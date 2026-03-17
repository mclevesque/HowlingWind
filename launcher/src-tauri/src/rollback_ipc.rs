//! IPC-based rollback engine — uses our Dolphin fork's built-in IPC server.
//!
//! This replaces the external memory polling approach with direct control:
//! - Frame boundary events from Dolphin (no polling needed)
//! - SI-level input injection (SET_INPUT at exact polling moment)
//! - Full emulator save states (SAVE_STATE/LOAD_STATE)
//! - Frame stepping for rollback resimulation (FRAME_ADVANCE)

use crate::hw_ipc::{HWClient, HWPadInput};
use crate::netplay::{FrameInput, NetplayState};
use crate::rollback::{EngineState, RollbackConfig, RollbackState, RollbackStats};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Save state slot management — we use a ring buffer of Dolphin's slots (0-15)
const MAX_SLOTS: u32 = 16;

fn slot_for_frame(frame: u32) -> u32 {
    frame % MAX_SLOTS
}

/// Run the IPC-based rollback game loop.
/// This is the new hot path when using our HowlingWind Dolphin fork.
///
/// Key differences from the external memory approach:
/// 1. No polling — we wait for FRAME_BOUNDARY events from Dolphin
/// 2. Input injection via SET_INPUT at SI level (exact timing)
/// 3. True rollback: LOAD_STATE → SET_INPUT → FRAME_ADVANCE loop
/// 4. Full emulator state save/load (not just 2-4KB)
pub async fn run_ipc_game_loop(
    ipc: Arc<HWClient>,
    rb_state: Arc<Mutex<RollbackState>>,
    netplay_state: Arc<Mutex<NetplayState>>,
    local_player: u8, // 0 = P1, 1 = P2
) {
    let remote_player = if local_player == 0 { 1u8 } else { 0u8 };
    let mut current_frame: u32 = 0;

    // Local input history for rollback resimulation
    let mut local_input_history: HashMap<u32, HWPadInput> = HashMap::with_capacity(300);
    let mut remote_input_history: HashMap<u32, HWPadInput> = HashMap::with_capacity(300);

    // Config
    let config = {
        let rs = rb_state.lock().unwrap();
        rs.engine.config.clone()
    };

    eprintln!("[rollback_ipc] Starting IPC game loop (player {}, delay {}, max_rb {})",
        local_player + 1, config.input_delay, config.max_rollback);

    // Wait for IPC connection to be ready
    if let Err(e) = ipc.ping().await {
        eprintln!("[rollback_ipc] IPC ping failed: {}", e);
        return;
    }
    eprintln!("[rollback_ipc] IPC connection verified");

    loop {
        // Check if we should stop
        {
            let rs = rb_state.lock().unwrap();
            if rs.engine.state == EngineState::Idle {
                break;
            }
        }

        // Wait for next frame boundary from Dolphin
        let frame = match ipc.wait_frame().await {
            Ok(f) => f as u32,
            Err(_) => {
                // Timeout — check if still connected
                if !ipc.is_connected() {
                    eprintln!("[rollback_ipc] IPC disconnected");
                    break;
                }
                continue;
            }
        };

        current_frame = frame;

        // Debug logging
        if current_frame <= 3 || current_frame % 120 == 0 {
            let np = netplay_state.lock().unwrap();
            let remote_count = np.session.as_ref()
                .map(|s| s.input_buffer.remote.len()).unwrap_or(0);
            let has_peer = np.session.as_ref()
                .map(|s| s.peer_addr.is_some()).unwrap_or(false);
            eprintln!("[rollback_ipc] F{} P{} peer={} remote_inputs={}",
                current_frame, local_player + 1, has_peer, remote_count);
        }

        // ── Step 1: Save state for this frame ──
        let save_start = Instant::now();
        let slot = slot_for_frame(current_frame);
        if let Err(e) = ipc.save_state(slot).await {
            eprintln!("[rollback_ipc] Save state failed: {}", e);
        }
        let save_ms = save_start.elapsed().as_secs_f64() * 1000.0;

        // ── Step 2: Read local input (physical controller is always port 0) ──
        // With IPC, Dolphin handles reading the physical controller automatically.
        // We need to TELL Dolphin what the remote player's input should be.
        // The local player's input flows naturally through the controller.
        // But we still need to READ it to send it over the network.
        //
        // For now, we let the physical controller pass through for the local player
        // and inject the remote player's input via SET_INPUT.

        // Get local input from the netplay session (read from PAD buffer on Dolphin side)
        // Actually, with our fork, Dolphin reads physical input directly via SI.
        // We need to send a "get current input" command or hook it differently.
        //
        // For the first iteration: the local player's physical controller input
        // goes through normally. We only need to SET_INPUT for the remote port.

        // ── Step 3: Send local input over network ──
        // NOTE: In the IPC model, Dolphin reads the local controller directly.
        // We need to extract what it read. For now, we let the controller pass
        // through and just handle the remote side.
        // TODO: Add READ_INPUT command to IPC for reading what the controller sent.

        // Get the network input for this frame
        let local_input_for_net = {
            let np = netplay_state.lock().unwrap();
            if let Some(session) = &np.session {
                session.input_buffer.local.get(&current_frame).cloned()
            } else {
                None
            }
        };

        // If we have local input, send it
        if let Some(input) = local_input_for_net {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                let packet = crate::netplay::InputPacket {
                    frame: current_frame,
                    player_id: local_player,
                    input,
                    checksum: 0,
                };
                if let Some(ref tx) = session.send_tx {
                    let _ = tx.try_send(packet);
                }
            }
        }

        // ── Step 4: Process received remote inputs ──
        let rollback_frames = {
            let mut np = netplay_state.lock().unwrap();
            match &mut np.session {
                Some(session) => session.process_received(),
                None => vec![],
            }
        };

        // ── Step 5: Handle rollback if needed ──
        if !rollback_frames.is_empty() {
            let earliest = rollback_frames[0];
            let depth = current_frame.saturating_sub(earliest);

            if depth > 0 && depth <= config.max_rollback {
                let rb_start = Instant::now();

                // TRUE ROLLBACK: load state → replay with correct inputs
                let rollback_slot = slot_for_frame(earliest);
                if let Err(e) = ipc.load_state(rollback_slot).await {
                    eprintln!("[rollback_ipc] Load state failed: {}", e);
                } else {
                    // Pause emulation for controlled replay
                    let _ = ipc.pause().await;

                    // Collect inputs FIRST (hold lock briefly), then do IPC calls
                    let replay_inputs: Vec<(HWPadInput, HWPadInput)> = {
                        let np = netplay_state.lock().unwrap();
                        if let Some(session) = &np.session {
                            (earliest..current_frame).map(|f| {
                                let local_input = session.input_buffer.local.get(&f);
                                let remote_input = session.input_buffer.remote.get(&f);
                                let local_pad = local_input.map(|i| frame_input_to_pad(i)).unwrap_or_default();
                                let remote_pad = remote_input.map(|i| frame_input_to_pad(i)).unwrap_or_default();
                                if local_player == 0 {
                                    (local_pad, remote_pad)
                                } else {
                                    (remote_pad, local_pad)
                                }
                            }).collect()
                        } else {
                            vec![]
                        }
                    }; // Lock dropped here

                    // Now replay with IPC (no lock held)
                    for (p1_pad, p2_pad) in &replay_inputs {
                        let _ = ipc.set_input(0, p1_pad).await;
                        let _ = ipc.set_input(1, p2_pad).await;
                        let _ = ipc.frame_advance().await;
                    }

                    // Clear input overrides and resume
                    let _ = ipc.clear_input(0).await;
                    let _ = ipc.clear_input(1).await;
                    let _ = ipc.resume().await;

                    // Update rollback stats
                    let rb_ms = rb_start.elapsed().as_secs_f64() * 1000.0;
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

        // ── Step 6: Inject remote player's current frame input ──
        {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                let (remote_input, is_predicted) = session.input_buffer.get_remote(current_frame);
                let remote_pad = frame_input_to_pad(&remote_input);

                // Inject remote input on the remote player's port
                let _ = ipc.set_input(remote_player as u32, &remote_pad).await;

                // If we're P2, also redirect our local input (port 0 → port 1)
                // Actually with the fork, the local controller goes through SI directly.
                // We only need to inject the REMOTE player's input.

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

        // ── Step 7: Update stats ──
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

        // Prune old input history
        if current_frame > 300 {
            let cutoff = current_frame - 300;
            local_input_history.retain(|&f, _| f > cutoff);
            remote_input_history.retain(|&f, _| f > cutoff);
        }
    }

    eprintln!("[rollback_ipc] Game loop ended at frame {}", current_frame);

    // Clear any input overrides
    let _ = ipc.clear_input(0).await;
    let _ = ipc.clear_input(1).await;
}

/// Convert FrameInput (network format) to HWPadInput (IPC format).
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
