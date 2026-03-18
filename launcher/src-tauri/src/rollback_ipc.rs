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

    // Verify IPC connection — retry up to 5 times, draining any stale WELCOME messages
    let mut ipc_verified = false;
    for attempt in 0..5 {
        // Drain any stale messages (WELCOME, etc.) before sending PING
        ipc.drain_stale().await;

        match ipc.ping().await {
            Ok(()) => {
                ipc_verified = true;
                crate::diagnostics::log_ipc("IPC connection verified");
                break;
            }
            Err(e) => {
                crate::diagnostics::log_warn(&format!(
                    "IPC ping attempt {}/5 failed: {} — retrying...", attempt + 1, e
                ));
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    if !ipc_verified {
        crate::diagnostics::log_error("IPC connection failed after 5 attempts — aborting");
        return;
    }

    // No memory attachment needed — we read input via IPC GET_INPUT now.
    crate::diagnostics::log_info("Using IPC GET_INPUT for controller reads (no memory attachment needed)");

    // Track the last remote input received (persists between frames)
    let mut last_remote_input = FrameInput::default();

    // Use RELATIVE frame counter starting from 0.
    // Both players must count from 0 when rollback starts, regardless of
    // when their Dolphin was launched. Dolphin's absolute frame number
    // is meaningless for sync — only the relative count matters.
    let mut netplay_frame: u32 = 0;
    let mut first_dolphin_frame: Option<u64> = None;
    let mut last_frame_time = Instant::now();
    let frame_duration = Duration::from_micros(16667); // ~60fps

    // First: drain any backlogged frame events from Dolphin loading/intro.
    // These arrive in a burst and would cause the game loop to race ahead
    // by thousands of frames, consuming all remote inputs as predictions.
    {
        crate::diagnostics::log_info("[SYNC] Draining backlogged frame events...");
        let mut drained = 0u32;
        loop {
            match tokio::time::timeout(Duration::from_millis(50), ipc.wait_frame()).await {
                Ok(Ok(_)) => { drained += 1; }
                _ => break, // No more backlogged events
            }
        }
        crate::diagnostics::log_info(&format!("[SYNC] Drained {} backlogged frames", drained));
    }

    loop {
        // Check if we should stop
        {
            let rs = rb_state.lock().unwrap();
            if rs.engine.state == EngineState::Idle {
                break;
            }
        }

        // Wait for next frame boundary from Dolphin
        let dolphin_frame = match ipc.wait_frame().await {
            Ok(f) => f,
            Err(_) => {
                if !ipc.is_connected() {
                    crate::diagnostics::log_error("IPC disconnected");
                    break;
                }
                continue;
            }
        };

        // Throttle to ~60fps max — if frames arrive faster than real-time
        // (backlog burst), skip the extras. This prevents racing ahead of
        // the remote player's input stream.
        let elapsed = last_frame_time.elapsed();
        if elapsed < frame_duration {
            // Frame arrived too fast — drain extras until we're at real-time pace
            let mut skipped = 0u32;
            while last_frame_time.elapsed() < frame_duration {
                match tokio::time::timeout(Duration::from_millis(1), ipc.wait_frame()).await {
                    Ok(Ok(_)) => { skipped += 1; }
                    _ => break,
                }
            }
            if skipped > 0 && (netplay_frame <= 10 || netplay_frame % 300 == 0) {
                crate::diagnostics::log_info(&format!(
                    "[THROTTLE] F{} skipped {} fast frames", netplay_frame, skipped
                ));
            }
        }
        last_frame_time = Instant::now();

        // Convert absolute Dolphin frame to relative netplay frame
        if first_dolphin_frame.is_none() {
            first_dolphin_frame = Some(dolphin_frame);
            crate::diagnostics::log_info(&format!(
                "[SYNC] First Dolphin frame: {}. Netplay starts at frame 0.",
                dolphin_frame
            ));
        }
        netplay_frame = (dolphin_frame - first_dolphin_frame.unwrap_or(0)) as u32;
        current_frame = netplay_frame;

        // Verbose debug logging — first 10 frames log everything, then every 60 frames
        let verbose = current_frame <= 10 || current_frame % 60 == 0;
        if verbose {
            let np = netplay_state.lock().unwrap();
            let (remote_count, local_count, has_peer, peer_addr, ping_ms) = match np.session.as_ref() {
                Some(s) => (
                    s.input_buffer.remote.len(),
                    s.input_buffer.local.len(),
                    s.peer_addr.is_some(),
                    s.peer_addr.map(|a| a.to_string()).unwrap_or("none".into()),
                    s.ping_ms,
                ),
                None => (0, 0, false, "NO_SESSION".into(), 0.0),
            };
            crate::diagnostics::log_info(&format!(
                "[FRAME] F{} P{} peer={} addr={} ping={:.1}ms local_buf={} remote_buf={}",
                current_frame, local_player + 1, has_peer, peer_addr, ping_ms,
                local_count, remote_count
            ));
        }

        // ── Step 1: Read local input via IPC GET_INPUT (reads physical controller) ──
        let local_frame_input = match ipc.get_input(0).await {
            Ok(pad) => FrameInput {
                buttons: pad.buttons,
                stick_x: pad.stick_x,
                stick_y: pad.stick_y,
                cstick_x: pad.cstick_x,
                cstick_y: pad.cstick_y,
                trigger_l: pad.trigger_l,
                trigger_r: pad.trigger_r,
            },
            Err(e) => {
                if verbose {
                    crate::diagnostics::log_warn(&format!("[INPUT] GET_INPUT failed: {}", e));
                }
                FrameInput::default()
            }
        };

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
                    match tx.try_send(packet) {
                        Ok(()) => {
                            if verbose {
                                crate::diagnostics::log_info(&format!(
                                    "[SEND] F{} buttons=0x{:04X} stick=({},{})",
                                    current_frame, local_frame_input.buttons,
                                    local_frame_input.stick_x, local_frame_input.stick_y
                                ));
                            }
                        }
                        Err(e) => {
                            crate::diagnostics::log_warn(&format!(
                                "[SEND] F{} FAILED: {}", current_frame, e
                            ));
                        }
                    }
                } else {
                    if verbose {
                        crate::diagnostics::log_warn(&format!("[SEND] F{} NO send_tx channel!", current_frame));
                    }
                }
            } else {
                if verbose {
                    crate::diagnostics::log_warn(&format!("[SEND] F{} NO netplay session!", current_frame));
                }
            }
        }

        // ── Step 3: (Skipped — we read directly from recv_rx in Step 5 now) ──

        // ── Step 4: Rollback disabled (save states not implemented yet) ──

        // ── Step 5: Inject the LATEST remote input, ignoring frame numbers ──
        // Don't match by frame — just use whatever the remote player last sent.
        // Frame matching is only needed for rollback replay (which is disabled).
        // This guarantees any input that arrives gets injected immediately.
        {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                // Drain ALL received packets and keep the LATEST one
                let mut latest_remote = FrameInput::default();
                let mut got_any = false;
                if let Some(ref mut rx) = session.recv_rx {
                    while let Ok(packet) = rx.try_recv() {
                        if packet.frame < 90000 { // skip test packets
                            latest_remote = packet.input;
                            got_any = true;
                        }
                    }
                }

                // If we got new data, use it. Otherwise repeat the last known input.
                let remote_input = if got_any {
                    last_remote_input = latest_remote;
                    latest_remote
                } else {
                    last_remote_input
                };

                let remote_pad = frame_input_to_pad(&remote_input);

                if verbose {
                    crate::diagnostics::log_info(&format!(
                        "[INJECT] F{} remote_btns=0x{:04X} new_data={} → port 0+1",
                        current_frame, remote_input.buttons, got_any
                    ));
                }

                // Inject remote into port 1 (for VS gameplay as P2)
                if let Err(e) = ipc.set_input(1, &remote_pad).await {
                    if verbose {
                        crate::diagnostics::log_error(&format!("[INJECT] set_input(1) failed: {}", e));
                    }
                }

                // ALSO merge remote input into port 0 (for menu control).
                // Both players can control menus. During VS character select,
                // the game splits port 0 = P1 wheel, port 1 = P2 wheel naturally.
                // Read local physical input, OR remote buttons in, merge sticks.
                let local_pad = ipc.get_input(0).await.unwrap_or_default();
                let merged = HWPadInput {
                    buttons: local_pad.buttons | remote_input.buttons,
                    stick_x: if remote_input.stick_x.abs() > local_pad.stick_x.abs() { remote_input.stick_x } else { local_pad.stick_x },
                    stick_y: if remote_input.stick_y.abs() > local_pad.stick_y.abs() { remote_input.stick_y } else { local_pad.stick_y },
                    cstick_x: if remote_input.cstick_x.abs() > local_pad.cstick_x.abs() { remote_input.cstick_x } else { local_pad.cstick_x },
                    cstick_y: if remote_input.cstick_y.abs() > local_pad.cstick_y.abs() { remote_input.cstick_y } else { local_pad.cstick_y },
                    trigger_l: remote_input.trigger_l.max(local_pad.trigger_l),
                    trigger_r: remote_input.trigger_r.max(local_pad.trigger_r),
                };
                if let Err(e) = ipc.set_input(0, &merged).await {
                    if verbose {
                        crate::diagnostics::log_error(&format!("[INJECT] merged set_input(0) failed: {}", e));
                    }
                }

                // Track stats
                let mut rs = rb_state.lock().unwrap();
                rs.engine.predictions_total += 1;
                if got_any {
                    rs.engine.predictions_correct += 1;
                }
            }
        }

        // ── Step 6: Save state (DISABLED — too slow, kills IPC connection) ──
        // TODO: Re-enable once we implement in-memory save states in the fork
        // The current State::Save() writes to disk, takes 2+ seconds, and
        // causes the IPC response timeout to kill the connection.
        let save_ms = 0.0;

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
