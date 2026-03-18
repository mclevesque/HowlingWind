//! Hybrid rollback engine — reads input from Dolphin memory, injects via IPC.
//!
//! This combines the best of both approaches:
//! - LOCAL INPUT: Read from Dolphin's memory (read_pad_input, proven reliable)
//! - REMOTE INPUT: Injected via IPC SET_INPUT at SI level (frame-perfect)
//! - SAVE/LOAD: Via IPC (full emulator state, not just 2-4KB)
//! - FRAME STEPPING: Via IPC FRAME_ADVANCE (true rollback resimulation)

use crate::dolphin_mem::{DolphinMemState, GCPadStatus};
use crate::hw_ipc::{HWClient, HWPadInput};
use crate::netplay::{FrameInput, NetplayState};
use crate::rollback::{EngineState, RollbackConfig, RollbackState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_SLOTS: u32 = 16;

fn slot_for_frame(frame: u32) -> u32 {
    frame % MAX_SLOTS
}

/// Convert GCPadStatus (memory read) → FrameInput (network format).
fn pad_to_frame_input(pad: &GCPadStatus) -> FrameInput {
    FrameInput {
        buttons: pad.buttons,
        stick_x: pad.stick_x,
        stick_y: pad.stick_y,
        cstick_x: pad.cstick_x,
        cstick_y: pad.cstick_y,
        trigger_l: pad.trigger_l,
        trigger_r: pad.trigger_r,
    }
}

/// Convert FrameInput (network format) → HWPadInput (IPC format).
fn frame_input_to_pad(input: &FrameInput) -> HWPadInput {
    HWPadInput {
        buttons: input.buttons,
        stick_x: input.stick_x,
        stick_y: input.stick_y,
        cstick_x: input.cstick_x,
        cstick_y: input.cstick_y,
        trigger_l: input.trigger_l,
        trigger_r: input.trigger_r,
    }
}

/// Run the hybrid rollback game loop.
///
/// HOW IT WORKS:
/// 1. Wait for FRAME_BOUNDARY event from Dolphin IPC
/// 2. Read local controller input from Dolphin's memory (port 0 = physical controller)
/// 3. Send local input to remote player via UDP
/// 4. Get remote input (confirmed or predicted)
/// 5. Inject remote input via IPC SET_INPUT (frame-perfect SI-level injection)
/// 6. If guest (P2): also redirect local port 0 input to port 1 via IPC
/// 7. Save emulator state via IPC
/// 8. If rollback needed: LOAD_STATE → SET_INPUT + FRAME_ADVANCE loop
pub async fn run_ipc_game_loop(
    ipc: Arc<HWClient>,
    rb_state: Arc<Mutex<RollbackState>>,
    dolphin_state: Arc<Mutex<DolphinMemState>>,
    netplay_state: Arc<Mutex<NetplayState>>,
    local_player: u8, // 0 = P1 (host), 1 = P2 (guest)
) {
    let remote_player = if local_player == 0 { 1u8 } else { 0u8 };
    let mut current_frame: u32 = 0;
    let mut mem_attached = false;

    let config = {
        let rs = rb_state.lock().unwrap();
        rs.engine.config.clone()
    };

    crate::diagnostics::log_info(&format!(
        "IPC game loop starting: player={} ({}), delay={}, max_rb={}",
        local_player + 1,
        if local_player == 0 { "HOST/P1" } else { "GUEST/P2" },
        config.input_delay, config.max_rollback
    ));

    // Verify IPC connection
    if let Err(e) = ipc.ping().await {
        crate::diagnostics::log_error(&format!("IPC ping failed: {}", e));
        return;
    }
    crate::diagnostics::log_ipc("IPC connection verified");

    // Try to attach to Dolphin's memory for reading local controller input.
    // We retry until attached — Dolphin needs a moment to start.
    #[cfg(windows)]
    fn try_attach_mem(ds: &mut DolphinMemState) -> bool {
        if ds.memory.is_some() { return true; }
        match crate::dolphin_mem::find_dolphin_pid() {
            Ok(pid) => {
                match crate::dolphin_mem::DolphinMemory::attach(pid) {
                    Ok(mem) => {
                        ds.memory = Some(mem);
                        true
                    }
                    Err(_) => false,
                }
            }
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    fn try_attach_mem(_ds: &mut DolphinMemState) -> bool { false }

    loop {
        // Check if we should stop
        {
            let rs = rb_state.lock().unwrap();
            if rs.engine.state == EngineState::Idle {
                break;
            }
        }

        // Ensure memory is attached (for reading local input)
        if !mem_attached {
            let mut ds = dolphin_state.lock().unwrap();
            mem_attached = try_attach_mem(&mut ds);
            if !mem_attached {
                // Can't read input yet — wait and retry
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            crate::diagnostics::log_info("Memory attached to Dolphin for input reading");
        }

        // Wait for next frame boundary from Dolphin
        let frame = match ipc.wait_frame().await {
            Ok(f) => f as u32,
            Err(_) => {
                if !ipc.is_connected() {
                    crate::diagnostics::log_error("IPC disconnected");
                    break;
                }
                continue;
            }
        };

        current_frame = frame;

        // Debug logging
        if current_frame <= 5 || current_frame % 120 == 0 {
            let np = netplay_state.lock().unwrap();
            let remote_count = np.session.as_ref()
                .map(|s| s.input_buffer.remote.len()).unwrap_or(0);
            let has_peer = np.session.as_ref()
                .map(|s| s.peer_addr.is_some()).unwrap_or(false);
            crate::diagnostics::log_info(&format!(
                "F{} P{} peer={} remote_inputs={}",
                current_frame, local_player + 1, has_peer, remote_count
            ));
        }

        // ── Step 1: Read local input from Dolphin memory (port 0 = physical controller) ──
        let local_input = {
            let ds = dolphin_state.lock().unwrap();
            match &ds.memory {
                Some(mem) => mem.read_pad_input(0).unwrap_or_default(),
                None => {
                    mem_attached = false;
                    GCPadStatus::default()
                }
            }
        };
        let local_frame_input = pad_to_frame_input(&local_input);

        // ── Step 2: Send local input to remote via UDP ──
        {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                // Record in input buffer (for rollback replay)
                session.input_buffer.add_local(current_frame, local_frame_input);

                // Send over network
                let packet = crate::netplay::InputPacket {
                    frame: current_frame,
                    player_id: local_player,
                    input: local_frame_input,
                    checksum: 0,
                };
                if let Some(ref tx) = session.send_tx {
                    let _ = tx.try_send(packet);
                }
            }
        }

        // ── Step 3: Process received remote inputs ──
        let rollback_frames = {
            let mut np = netplay_state.lock().unwrap();
            match &mut np.session {
                Some(session) => session.process_received(),
                None => vec![],
            }
        };

        // ── Step 4: Handle rollback if needed ──
        if !rollback_frames.is_empty() {
            let earliest = rollback_frames[0];
            let depth = current_frame.saturating_sub(earliest);

            if depth > 0 && depth <= config.max_rollback {
                let rb_start = Instant::now();

                let rollback_slot = slot_for_frame(earliest);
                if let Err(e) = ipc.load_state(rollback_slot).await {
                    crate::diagnostics::log_error(&format!("Rollback load_state failed: {}", e));
                } else {
                    let _ = ipc.pause().await;

                    // Collect inputs (brief lock)
                    let replay_inputs: Vec<(HWPadInput, HWPadInput)> = {
                        let np = netplay_state.lock().unwrap();
                        if let Some(session) = &np.session {
                            (earliest..current_frame).map(|f| {
                                let local = session.input_buffer.local.get(&f);
                                let remote = session.input_buffer.remote.get(&f);
                                let local_pad = local.map(|i| frame_input_to_pad(i)).unwrap_or_default();
                                let remote_pad = remote.map(|i| frame_input_to_pad(i)).unwrap_or_default();
                                // P1 = port 0, P2 = port 1
                                if local_player == 0 {
                                    (local_pad, remote_pad) // Host: local=P1, remote=P2
                                } else {
                                    (remote_pad, local_pad) // Guest: remote=P1, local=P2
                                }
                            }).collect()
                        } else {
                            vec![]
                        }
                    };

                    // Replay frames with correct inputs
                    for (p1_pad, p2_pad) in &replay_inputs {
                        let _ = ipc.set_input(0, p1_pad).await;
                        let _ = ipc.set_input(1, p2_pad).await;
                        let _ = ipc.frame_advance().await;
                    }

                    let _ = ipc.clear_input(0).await;
                    let _ = ipc.clear_input(1).await;
                    let _ = ipc.resume().await;

                    let rb_ms = rb_start.elapsed().as_secs_f64() * 1000.0;
                    crate::diagnostics::log_rollback(earliest, current_frame, rb_ms);

                    let mut rs = rb_state.lock().unwrap();
                    rs.engine.stats.rollback_count += 1;
                    rs.engine.stats.total_rollback_frames += depth as u64;
                    rs.engine.stats.last_rollback_depth = depth;
                    if rb_ms > rs.engine.stats.max_rollback_ms {
                        rs.engine.stats.max_rollback_ms = rb_ms;
                    }
                    rs.engine.total_rollback_time += rb_start.elapsed();
                    if rs.engine.stats.rollback_count > 0 {
                        rs.engine.stats.avg_rollback_ms = rs.engine.total_rollback_time.as_secs_f64()
                            * 1000.0 / rs.engine.stats.rollback_count as f64;
                    }
                }
            }
        }

        // ── Step 5: Inject BOTH players' inputs via IPC every frame ──
        // This is critical: we override ALL controller ports so the physical
        // controller doesn't bleed through. The guest's physical controller
        // is on port 0 but they need to be P2 (port 1).
        {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                let (remote_input, is_predicted) = session.input_buffer.get_remote(current_frame);
                let remote_pad = frame_input_to_pad(&remote_input);
                let local_pad = frame_input_to_pad(&local_frame_input);

                if local_player == 0 {
                    // HOST: local = P1 (port 0), remote = P2 (port 1)
                    let _ = ipc.set_input(0, &local_pad).await;
                    let _ = ipc.set_input(1, &remote_pad).await;
                } else {
                    // GUEST: remote = P1 (port 0), local = P2 (port 1)
                    // This BLOCKS the physical controller from controlling P1!
                    let _ = ipc.set_input(0, &remote_pad).await;
                    let _ = ipc.set_input(1, &local_pad).await;
                }

                // Track prediction stats
                let mut rs = rb_state.lock().unwrap();
                rs.engine.predictions_total += 1;
                if !is_predicted {
                    rs.engine.predictions_correct += 1;
                }
                if rs.engine.predictions_total > 0 {
                    rs.engine.stats.prediction_success_rate =
                        (rs.engine.predictions_correct as f64 / rs.engine.predictions_total as f64) * 100.0;
                }
            }
        }

        // ── Step 6: Save state for this frame ──
        let save_start = Instant::now();
        let slot = slot_for_frame(current_frame);
        if let Err(e) = ipc.save_state(slot).await {
            if current_frame <= 5 {
                crate::diagnostics::log_error(&format!("Save state failed F{}: {}", current_frame, e));
            }
        }
        let save_ms = save_start.elapsed().as_secs_f64() * 1000.0;

        // ── Step 7: Update stats + ping ──
        {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                if current_frame % 60 == 0 && current_frame > 0 {
                    session.send_ping();
                }
                let ping = session.process_pongs();

                let mut rs = rb_state.lock().unwrap();
                rs.engine.stats.current_frame = current_frame;
                rs.engine.local_frame = current_frame;
                rs.engine.stats.remote_frame = session.input_buffer.latest_remote_frame;
                rs.engine.stats.frames_ahead = session.frames_ahead();
                rs.engine.stats.ping_ms = ping;
                rs.engine.stats.save_state_ms = save_ms;
            }
        }
    }

    crate::diagnostics::log_info(&format!("IPC game loop ended at frame {}", current_frame));
    let _ = ipc.clear_input(0).await;
    let _ = ipc.clear_input(1).await;
}
