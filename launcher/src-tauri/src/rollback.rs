//! Rollback engine — the core of HowlingWind's netplay.
//!
//! Orchestrates frame advancement, input prediction, save states, and rollback.
//! This runs as a background task that polls Dolphin's memory and the netplay session.

use crate::dolphin_mem::{DolphinMemState, MatchOutcome};
use crate::hw_ipc::HWClient;
use crate::netplay::{FrameInput, NetplaySession, NetplayState, SyncPacket};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ── Configuration ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackConfig {
    /// Frames of intentional input delay (reduces rollbacks at the cost of responsiveness).
    pub input_delay: u32,
    /// Maximum number of frames we'll roll back. Beyond this, we stall.
    pub max_rollback: u32,
    /// How many save states to keep in the ring buffer.
    pub save_state_capacity: usize,
}

impl Default for RollbackConfig {
    fn default() -> Self {
        Self {
            input_delay: 2,
            max_rollback: 7,
            save_state_capacity: 10,
        }
    }
}

// ── Rollback Stats (exposed to frontend) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackStats {
    pub current_frame: u32,
    pub remote_frame: u32,
    pub frames_ahead: u32,
    pub rollback_count: u64,
    pub total_rollback_frames: u64,
    pub avg_rollback_ms: f64,
    pub max_rollback_ms: f64,
    pub stall_count: u64,
    pub last_rollback_depth: u32,
    pub prediction_success_rate: f64,
    pub save_state_ms: f64,
    pub load_state_ms: f64,
    /// Network ping to peer in milliseconds.
    pub ping_ms: f64,
    /// Desync detected — game states diverged between peers.
    pub desync_detected: bool,
    /// Frame where desync was first detected.
    pub desync_frame: u32,
    /// Our hash vs their hash at the desync frame.
    pub desync_local_hash: u32,
    pub desync_remote_hash: u32,
}

impl Default for RollbackStats {
    fn default() -> Self {
        Self {
            current_frame: 0,
            remote_frame: 0,
            frames_ahead: 0,
            rollback_count: 0,
            total_rollback_frames: 0,
            avg_rollback_ms: 0.0,
            max_rollback_ms: 0.0,
            stall_count: 0,
            last_rollback_depth: 0,
            prediction_success_rate: 100.0,
            save_state_ms: 0.0,
            load_state_ms: 0.0,
            ping_ms: 0.0,
            desync_detected: false,
            desync_frame: 0,
            desync_local_hash: 0,
            desync_remote_hash: 0,
        }
    }
}

// ── Engine State ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EngineState {
    Idle,
    WaitingForPeer,
    Running,
    Stalling,
    Paused,
    MatchOver,
}

/// The rollback engine manages the game loop during netplay.
pub struct RollbackEngine {
    pub config: RollbackConfig,
    pub state: EngineState,
    pub stats: RollbackStats,
    /// Current local frame (our latest simulated frame).
    pub local_frame: u32,
    /// Latest frame confirmed by remote.
    pub confirmed_frame: u32,
    /// Total predictions made.
    pub predictions_total: u64,
    /// Predictions that were correct.
    pub predictions_correct: u64,
    /// Timing for stats.
    pub total_rollback_time: Duration,
}

impl RollbackEngine {
    pub fn new(config: RollbackConfig) -> Self {
        Self {
            config,
            state: EngineState::Idle,
            stats: RollbackStats::default(),
            local_frame: 0,
            confirmed_frame: 0,
            predictions_total: 0,
            predictions_correct: 0,
            total_rollback_time: Duration::ZERO,
        }
    }

    /// Start the engine — call when both players are connected and game is loaded.
    pub fn start(&mut self) {
        self.state = EngineState::Running;
        self.local_frame = 0;
        self.confirmed_frame = 0;
        self.stats = RollbackStats::default();
        self.predictions_total = 0;
        self.predictions_correct = 0;
        self.total_rollback_time = Duration::ZERO;
    }

    /// Process one tick of the rollback loop.
    /// This is the hot path — called every frame (~16.67ms at 60fps).
    ///
    /// Returns what action the caller should take.
    #[cfg(windows)]
    pub fn tick(
        &mut self,
        dolphin: &mut DolphinMemState,
        netplay: &mut NetplaySession,
    ) -> TickAction {
        if self.state != EngineState::Running && self.state != EngineState::Stalling {
            return TickAction::None;
        }

        let mem = match &dolphin.memory {
            Some(m) => m,
            None => return TickAction::Error("Not attached to Dolphin".to_string()),
        };

        // Step 1: Process any received remote inputs
        let rollback_frames = netplay.process_received();

        // Step 2: Check if we need to rollback
        if !rollback_frames.is_empty() {
            let earliest_mismatch = rollback_frames[0];
            let rollback_depth = self.local_frame.saturating_sub(earliest_mismatch);

            if rollback_depth > 0 && rollback_depth <= self.config.max_rollback {
                let rb_start = Instant::now();

                // Load the lightweight game state for the mismatch frame
                if let Some(snapshot) = dolphin.game_state_ring.get(earliest_mismatch) {
                    // Restore game state (safe — only writes ~2-4KB)
                    if let Err(e) = mem.load_game_state(snapshot) {
                        return TickAction::Error(format!("Failed to load state: {}", e));
                    }

                    let load_time = rb_start.elapsed();
                    self.stats.load_state_ms = load_time.as_secs_f64() * 1000.0;

                    // Replay frames from mismatch to current
                    // (The actual frame replay happens through Dolphin's emulation —
                    //  we write the correct inputs and let Dolphin advance)
                    let frames_to_replay = self.local_frame - earliest_mismatch;

                    let rb_time = rb_start.elapsed();
                    self.stats.rollback_count += 1;
                    self.stats.total_rollback_frames += frames_to_replay as u64;
                    self.stats.last_rollback_depth = frames_to_replay;
                    self.total_rollback_time += rb_time;
                    let rb_ms = rb_time.as_secs_f64() * 1000.0;
                    if rb_ms > self.stats.max_rollback_ms {
                        self.stats.max_rollback_ms = rb_ms;
                    }
                    self.stats.avg_rollback_ms = self.total_rollback_time.as_secs_f64()
                        * 1000.0
                        / self.stats.rollback_count as f64;

                    return TickAction::Rollback {
                        from_frame: earliest_mismatch,
                        to_frame: self.local_frame,
                        depth: frames_to_replay,
                    };
                }
                // If we don't have the save state, we can't rollback — desync
            }
        }

        // Step 3: Check if we should stall (too far ahead of remote)
        if netplay.should_stall() {
            self.state = EngineState::Stalling;
            self.stats.stall_count += 1;
            return TickAction::Stall;
        } else if self.state == EngineState::Stalling {
            self.state = EngineState::Running;
        }

        // Step 4: Save lightweight game state for current frame
        let save_start = Instant::now();
        match mem.save_game_state(self.local_frame) {
            Ok(snapshot) => {
                self.stats.save_state_ms = save_start.elapsed().as_secs_f64() * 1000.0;
                dolphin.game_state_ring.push(snapshot);
            }
            Err(e) => {
                return TickAction::Error(format!("Failed to save state: {}", e));
            }
        }

        // Step 5: Get remote input (confirmed or predicted)
        let (remote_input, is_predicted) = netplay.get_remote_input();
        if is_predicted {
            self.predictions_total += 1;
        } else {
            self.predictions_total += 1;
            self.predictions_correct += 1;
        }

        // Update prediction success rate
        if self.predictions_total > 0 {
            self.stats.prediction_success_rate =
                (self.predictions_correct as f64 / self.predictions_total as f64) * 100.0;
        }

        // Step 6: Advance frame
        self.local_frame += 1;
        netplay.advance_frame();

        // Update stats
        self.stats.current_frame = self.local_frame;
        self.stats.remote_frame = netplay.input_buffer.latest_remote_frame;
        self.stats.frames_ahead = netplay.frames_ahead();

        // Step 7: Check for match end
        match check_match_over(mem) {
            Some(outcome) => {
                self.state = EngineState::MatchOver;
                TickAction::MatchOver(outcome)
            }
            None => TickAction::Advance {
                frame: self.local_frame,
                remote_input,
                is_predicted,
            },
        }
    }

    pub fn get_stats(&self) -> RollbackStats {
        self.stats.clone()
    }

    pub fn pause(&mut self) {
        if self.state == EngineState::Running {
            self.state = EngineState::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.state == EngineState::Paused {
            self.state = EngineState::Running;
        }
    }

    pub fn stop(&mut self) {
        self.state = EngineState::Idle;
    }
}

/// What action the game loop should take after a tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TickAction {
    /// Nothing to do (engine not running).
    None,
    /// Advance to the next frame normally.
    Advance {
        frame: u32,
        remote_input: FrameInput,
        is_predicted: bool,
    },
    /// Need to rollback and replay frames.
    Rollback {
        from_frame: u32,
        to_frame: u32,
        depth: u32,
    },
    /// Stalling — wait for remote to catch up.
    Stall,
    /// Match is over.
    MatchOver(MatchOutcome),
    /// An error occurred.
    Error(String),
}

/// Check if either player's health has hit 0.
#[cfg(windows)]
fn check_match_over(mem: &crate::dolphin_mem::DolphinMemory) -> Option<MatchOutcome> {
    let p1 = mem.read_player_state(0).ok()?;
    let p2 = mem.read_player_state(1).ok()?;
    let frame = mem.read_frame_counter().unwrap_or(0);

    // GNT4 health is a DAMAGE ACCUMULATOR: 0 = full HP, increases as damage taken
    // KO threshold is ~150+ damage
    const KO_THRESHOLD: u16 = 150;

    if frame < 120 {
        return None; // Don't trigger during intro/loading
    }

    if p1.health >= KO_THRESHOLD || p2.health >= KO_THRESHOLD {
        let result = if p1.health >= KO_THRESHOLD && p2.health >= KO_THRESHOLD {
            "draw"
        } else if p1.health >= KO_THRESHOLD {
            "p2_win" // P1 took too much damage
        } else {
            "p1_win" // P2 took too much damage
        };

        Some(MatchOutcome {
            result: result.to_string(),
            p1_health: p1.health,
            p2_health: p2.health,
            frame,
        })
    } else {
        None
    }
}

// ── Background Game Loop (Path A: Passive Rollback) ──

/// The background rollback loop runs at ~1000Hz (1ms polling).
/// It detects when Dolphin advances a frame by watching the frame counter,
/// then performs input exchange and state management.
///
/// Flow per detected frame:
/// 1. Read local player's input from Dolphin memory
/// 2. Send local input to remote via UDP
/// 3. Check for received remote inputs (may trigger rollback)
/// 4. Write remote input (confirmed or predicted) to Dolphin memory
/// 5. Save state for this frame
/// 6. If rollback needed: load old state → write correct inputs → let Dolphin replay
#[cfg(windows)]
pub fn run_game_loop(
    rb_state: Arc<Mutex<RollbackState>>,
    dolphin_state: Arc<Mutex<DolphinMemState>>,
    netplay_state: Arc<Mutex<NetplayState>>,
    local_player: u8, // 0 = P1, 1 = P2
) {
    use crate::dolphin_mem::GCPadStatus;
    use crate::netplay::FrameInput;

    let remote_player = if local_player == 0 { 1u8 } else { 0u8 };
    let mut last_anim_frame: u16 = 0;
    let mut current_frame: u32 = 0;
    let mut loop_count: u64 = 0;

    // Desync detection: store local state hashes and sync every N frames
    const SYNC_INTERVAL: u32 = 30; // Send sync packet every 30 frames (~0.5s)
    const MAX_HASH_HISTORY: usize = 300; // Keep ~5s of hash history
    let mut local_hashes: HashMap<u32, u32> = HashMap::with_capacity(MAX_HASH_HISTORY);

    // Round/match tracking — GNT4 is first to 3 round wins
    let mut p1_round_wins: u32 = 0;
    let mut p2_round_wins: u32 = 0;
    let rounds_to_win: u32 = 3; // First to 3 wins the match

    // Set tracking — ranked is best of 3 matches
    let mut p1_set_wins: u32 = 0;
    let mut p2_set_wins: u32 = 0;
    let is_ranked = {
        let rb = rb_state.lock().unwrap();
        rb.ranked
    };
    let matches_to_win_set: u32 = if is_ranked { 2 } else { 1 }; // Ranked: Bo3 sets, Unranked: single match

    // KO detection with delay — let the animation play out naturally
    let mut ko_detected_frame: Option<u32> = None;
    let mut ko_outcome: Option<MatchOutcome> = None;
    let mut last_ko_frame: u32 = 0; // Prevent re-triggering on same KO
    const KO_DELAY_FRAMES: u32 = 180; // ~3 seconds for KO animation
    const ROUND_RESET_COOLDOWN: u32 = 300; // ~5 seconds between rounds for reset animation

    loop {
        // Check if we should stop
        {
            let rb = rb_state.lock().unwrap();
            if rb.engine.state == EngineState::Idle {
                break;
            }
        }

        // Detect frame advancement by watching P1's animation frame counter (+0x25A).
        // This is proven to work in Practice mode. We use a u16 that wraps, so we
        // detect any change (not just increments).
        let frame_indicator = {
            let ds = dolphin_state.lock().unwrap();
            match &ds.memory {
                Some(mem) => {
                    let p1_ptr = mem.read_u32(0x80226358).unwrap_or(0);
                    if p1_ptr >= 0x80000000 && p1_ptr < 0x81800000 {
                        mem.read_u16(p1_ptr + 0x25A).unwrap_or(last_anim_frame)
                    } else {
                        last_anim_frame // No player struct — don't advance
                    }
                }
                None => { std::thread::sleep(Duration::from_millis(100)); continue; }
            }
        };

        // Only act when the frame counter changes (= new game frame)
        if frame_indicator == last_anim_frame {
            std::thread::sleep(Duration::from_micros(500));
            loop_count += 1;
            continue;
        }

        last_anim_frame = frame_indicator;
        current_frame += 1;

        // Debug: log periodically so we can verify the loop is running
        if current_frame <= 3 || current_frame % 120 == 0 {
            let np = netplay_state.lock().unwrap();
            let remote_count = np.session.as_ref().map(|s| s.input_buffer.remote.len()).unwrap_or(0);
            let has_peer = np.session.as_ref().map(|s| s.peer_addr.is_some()).unwrap_or(false);
            eprintln!("[rollback] F{} P{} peer={} remote_inputs={}", current_frame, local_player+1, has_peer, remote_count);
        }

        // ── Frame detected! Do rollback work ──

        // Step 1: Read local player's input from port 0 (physical controller)
        // The physical controller is ALWAYS port 0 in Dolphin, regardless of
        // whether this player is P1 or P2 in the game.
        let local_input = {
            let ds = dolphin_state.lock().unwrap();
            match &ds.memory {
                Some(mem) => mem.read_pad_input(0).unwrap_or_default(),
                None => GCPadStatus::default(),
            }
        };

        // Convert GCPadStatus → FrameInput for network
        let frame_input = FrameInput {
            buttons: local_input.buttons,
            stick_x: local_input.stick_x,
            stick_y: local_input.stick_y,
            cstick_x: local_input.cstick_x,
            cstick_y: local_input.cstick_y,
            trigger_l: local_input.trigger_l,
            trigger_r: local_input.trigger_r,
        };

        // Step 2: Send local input via netplay
        {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                // Record local input
                session.input_buffer.add_local(current_frame, frame_input);

                // Send over UDP (non-blocking, try_send doesn't need async runtime)
                let packet = crate::netplay::InputPacket {
                    frame: current_frame,
                    player_id: local_player,
                    input: frame_input,
                    checksum: 0,
                };
                if let Some(ref tx) = session.send_tx {
                    let _ = tx.try_send(packet);
                }
            }
        }

        // Step 3: Process received remote inputs
        let rollback_frames = {
            let mut np = netplay_state.lock().unwrap();
            match &mut np.session {
                Some(session) => session.process_received(),
                None => vec![],
            }
        };

        // Step 4: Handle rollback if needed
        if !rollback_frames.is_empty() {
            let earliest = rollback_frames[0];
            let depth = current_frame.saturating_sub(earliest);

            let mut rb = rb_state.lock().unwrap();
            if depth > 0 && depth <= rb.engine.config.max_rollback {
                let rb_start = Instant::now();

                // Load the lightweight game state (safe mid-emulation, ~2-4KB)
                let mut ds = dolphin_state.lock().unwrap();
                if let Some(snapshot) = ds.game_state_ring.get(earliest) {
                    if let Some(mem) = &ds.memory {
                        if let Err(_e) = mem.load_game_state(snapshot) {
                            rb.engine.stats.rollback_count += 1;
                            continue;
                        }

                        // Write correct inputs for the rollback frames
                        let np = netplay_state.lock().unwrap();
                        if let Some(session) = &np.session {
                            for f in earliest..current_frame {
                                if let Some(remote_input) = session.input_buffer.remote.get(&f) {
                                    let pad = GCPadStatus {
                                        buttons: remote_input.buttons,
                                        stick_x: remote_input.stick_x,
                                        stick_y: remote_input.stick_y,
                                        cstick_x: remote_input.cstick_x,
                                        cstick_y: remote_input.cstick_y,
                                        trigger_l: remote_input.trigger_l,
                                        trigger_r: remote_input.trigger_r,
                                    };
                                    let _ = mem.write_pad_buffer(remote_player, &pad);
                                }
                            }
                        }
                    }
                }

                // Update rollback stats
                let rb_ms = rb_start.elapsed().as_secs_f64() * 1000.0;
                rb.engine.stats.rollback_count += 1;
                rb.engine.stats.total_rollback_frames += depth as u64;
                rb.engine.stats.last_rollback_depth = depth;
                if rb_ms > rb.engine.stats.max_rollback_ms {
                    rb.engine.stats.max_rollback_ms = rb_ms;
                }
                rb.engine.total_rollback_time += rb_start.elapsed();
                if rb.engine.stats.rollback_count > 0 {
                    rb.engine.stats.avg_rollback_ms = rb.engine.total_rollback_time.as_secs_f64()
                        * 1000.0 / rb.engine.stats.rollback_count as f64;
                }
            }
        }

        // Step 5: Get remote input for current frame and inject it
        {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                let (remote_input, is_predicted) = session.input_buffer.get_remote(current_frame);
                let pad = GCPadStatus {
                    buttons: remote_input.buttons,
                    stick_x: remote_input.stick_x,
                    stick_y: remote_input.stick_y,
                    cstick_x: remote_input.cstick_x,
                    cstick_y: remote_input.cstick_y,
                    trigger_l: remote_input.trigger_l,
                    trigger_r: remote_input.trigger_r,
                };

                let ds = dolphin_state.lock().unwrap();
                if let Some(mem) = &ds.memory {
                    // Write remote player's input to their assigned port
                    let _ = mem.write_pad_buffer(remote_player, &pad);

                    // If we're P2 (guest), our physical controller is port 0 but the game
                    // expects our input on port 1. Redirect local input to our assigned port.
                    if local_player == 1 {
                        let _ = mem.write_pad_buffer(1, &local_input);
                    }
                }

                // Track prediction stats
                let mut rb = rb_state.lock().unwrap();
                rb.engine.predictions_total += 1;
                if !is_predicted {
                    rb.engine.predictions_correct += 1;
                }
                if rb.engine.predictions_total > 0 {
                    rb.engine.stats.prediction_success_rate =
                        (rb.engine.predictions_correct as f64 / rb.engine.predictions_total as f64) * 100.0;
                }
            }
        }

        // Step 6: Save lightweight game state for current frame (~2-4KB, <0.1ms)
        {
            let mut ds = dolphin_state.lock().unwrap();
            if let Some(mem) = &ds.memory {
                let save_start = Instant::now();
                if let Ok(snapshot) = mem.save_game_state(current_frame) {
                    let save_ms = save_start.elapsed().as_secs_f64() * 1000.0;
                    ds.game_state_ring.push(snapshot);

                    let mut rb = rb_state.lock().unwrap();
                    rb.engine.stats.save_state_ms = save_ms;
                }
            }
        }

        // Step 7: Update stats + ping
        {
            let mut np = netplay_state.lock().unwrap();
            if let Some(session) = &mut np.session {
                // Send ping every 60 frames (~1s)
                if current_frame % 60 == 0 && current_frame > 0 {
                    session.send_ping();
                }
                // Process any received pongs
                let ping = session.process_pongs();

                let mut rb = rb_state.lock().unwrap();
                rb.engine.stats.current_frame = current_frame;
                rb.engine.local_frame = current_frame;
                rb.engine.stats.remote_frame = session.input_buffer.latest_remote_frame;
                rb.engine.stats.frames_ahead = session.frames_ahead();
                rb.engine.stats.ping_ms = ping;
            } else {
                let mut rb = rb_state.lock().unwrap();
                rb.engine.stats.current_frame = current_frame;
                rb.engine.local_frame = current_frame;
            }
        }

        // Step 8: Desync detection — hash game state every SYNC_INTERVAL frames
        if current_frame % SYNC_INTERVAL == 0 && current_frame > 60 {
            // Read player state for hashing
            let hash_data = {
                let ds = dolphin_state.lock().unwrap();
                if let Some(mem) = &ds.memory {
                    let p1 = mem.read_player_state(0).ok();
                    let p2 = mem.read_player_state(1).ok();
                    match (p1, p2) {
                        (Some(p1s), Some(p2s)) => Some((p1s, p2s)),
                        _ => None,
                    }
                } else {
                    None
                }
            };

            if let Some((p1s, p2s)) = hash_data {
                // Build a deterministic byte buffer of critical game state
                let mut hash_buf = [0u8; 20];
                hash_buf[0..2].copy_from_slice(&p1s.health.to_le_bytes());
                hash_buf[2..4].copy_from_slice(&p2s.health.to_le_bytes());
                hash_buf[4..6].copy_from_slice(&p1s.chakra.to_le_bytes());
                hash_buf[6..8].copy_from_slice(&p2s.chakra.to_le_bytes());
                hash_buf[8..12].copy_from_slice(&p1s.vertical_speed.to_le_bytes());
                hash_buf[12..16].copy_from_slice(&p2s.vertical_speed.to_le_bytes());
                hash_buf[16..18].copy_from_slice(&p1s.block_meter.to_le_bytes());
                hash_buf[18..20].copy_from_slice(&p2s.block_meter.to_le_bytes());

                let state_hash = crc32fast::hash(&hash_buf);
                local_hashes.insert(current_frame, state_hash);

                // Prune old hashes to avoid unbounded growth
                if local_hashes.len() > MAX_HASH_HISTORY {
                    let cutoff = current_frame.saturating_sub((MAX_HASH_HISTORY as u32) * SYNC_INTERVAL);
                    local_hashes.retain(|&frame, _| frame > cutoff);
                }

                // Send sync packet to peer
                let sync_pkt = SyncPacket {
                    frame: current_frame,
                    state_hash,
                    p1_health: p1s.health,
                    p2_health: p2s.health,
                };
                {
                    let np = netplay_state.lock().unwrap();
                    if let Some(session) = &np.session {
                        if let Some(ref tx) = session.sync_send_tx {
                            let _ = tx.try_send(sync_pkt);
                        }
                    }
                }

                // Process received sync packets from peer
                {
                    let mut np = netplay_state.lock().unwrap();
                    if let Some(session) = &mut np.session {
                        if let Some((frame, local_h, remote_h)) = session.process_sync_packets(&local_hashes) {
                            // Desync detected — update stats
                            let mut rb = rb_state.lock().unwrap();
                            rb.engine.stats.desync_detected = true;
                            rb.engine.stats.desync_frame = frame;
                            rb.engine.stats.desync_local_hash = local_h;
                            rb.engine.stats.desync_remote_hash = remote_h;
                        }
                    }
                }
            }
        }

        // Step 9: Check for round/match end (with breathing room for animations)
        {
            let ds = dolphin_state.lock().unwrap();
            if let Some(mem) = &ds.memory {
                // Don't re-trigger if we just saw a KO recently
                if current_frame > last_ko_frame + ROUND_RESET_COOLDOWN {
                    if ko_detected_frame.is_none() {
                        // Check for KO
                        if let Some(outcome) = check_match_over(mem) {
                            ko_detected_frame = Some(current_frame);
                            ko_outcome = Some(outcome);
                        }
                    } else {
                        // KO detected — wait for animation to finish
                        let ko_frame = ko_detected_frame.unwrap();
                        if current_frame >= ko_frame + KO_DELAY_FRAMES {
                            // Round over! Count the win
                            if let Some(ref outcome) = ko_outcome {
                                match outcome.result.as_str() {
                                    "p1_win" => p1_round_wins += 1,
                                    "p2_win" => p2_round_wins += 1,
                                    "draw" => {} // No winner for this round
                                    _ => {}
                                }
                            }

                            last_ko_frame = current_frame;

                            // Check if someone won the match (first to 3 rounds)
                            if p1_round_wins >= rounds_to_win || p2_round_wins >= rounds_to_win {
                                let match_winner = if p1_round_wins >= rounds_to_win { "p1" } else { "p2" };

                                // Track set wins
                                if match_winner == "p1" {
                                    p1_set_wins += 1;
                                } else {
                                    p2_set_wins += 1;
                                }

                                // Check if the set is over (Bo3 for ranked, Bo1 for unranked)
                                if p1_set_wins >= matches_to_win_set || p2_set_wins >= matches_to_win_set {
                                    // Set over!
                                    let final_result = if p1_set_wins >= matches_to_win_set {
                                        "p1_win"
                                    } else {
                                        "p2_win"
                                    };

                                    let mut rb = rb_state.lock().unwrap();
                                    rb.engine.state = EngineState::MatchOver;
                                    rb.match_outcome = Some(MatchOutcome {
                                        result: final_result.to_string(),
                                        p1_health: ko_outcome.as_ref().map(|o| o.p1_health).unwrap_or(0),
                                        p2_health: ko_outcome.as_ref().map(|o| o.p2_health).unwrap_or(0),
                                        frame: current_frame,
                                    });
                                    rb.round_wins = Some((p1_round_wins, p2_round_wins));
                                    rb.set_wins = Some((p1_set_wins, p2_set_wins));
                                    rb.set_over = true;
                                } else {
                                    // Match over but set continues — reset round wins for next match
                                    let mut rb = rb_state.lock().unwrap();
                                    rb.round_wins = Some((p1_round_wins, p2_round_wins));
                                    rb.set_wins = Some((p1_set_wins, p2_set_wins));
                                    p1_round_wins = 0;
                                    p2_round_wins = 0;
                                    // Game will auto-reset to character select/next match
                                }
                            }
                            // else: next round continues, game will auto-reset

                            ko_detected_frame = None;
                            ko_outcome = None;
                        }
                    }
                }
            }
        }
    }
}

// ── Shared State for Tauri ──

pub struct RollbackState {
    pub engine: RollbackEngine,
    pub loop_handle: Option<std::thread::JoinHandle<()>>,
    pub match_outcome: Option<MatchOutcome>,
    pub round_wins: Option<(u32, u32)>, // (P1 wins, P2 wins)
    pub ranked: bool, // Whether this is a ranked match
    pub set_wins: Option<(u32, u32)>, // Ranked sets: (P1 match wins, P2 match wins)
    pub set_over: bool, // True when entire ranked set is complete
}

impl RollbackState {
    pub fn new() -> Self {
        Self {
            engine: RollbackEngine::new(RollbackConfig::default()),
            loop_handle: None,
            match_outcome: None,
            round_wins: None,
            ranked: true,
            set_wins: None,
            set_over: false,
        }
    }
}

// ── Tauri Commands ──

#[tauri::command]
pub fn rollback_start(
    input_delay: u32,
    max_rollback: u32,
    local_player: u8,
    ranked: bool,
    state: tauri::State<'_, Arc<Mutex<RollbackState>>>,
    dolphin_state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
    netplay_state: tauri::State<'_, Arc<Mutex<NetplayState>>>,
    dolphin_proc_state: tauri::State<'_, Arc<Mutex<crate::DolphinState>>>,
) -> Result<String, String> {
    crate::diagnostics::log_info(&format!(
        "rollback_start called: player={}, delay={}, max_rb={}, ranked={}",
        local_player, input_delay, max_rollback, ranked
    ));

    let mut rs = state.lock().map_err(|e| e.to_string())?;
    rs.engine.config.input_delay = input_delay;
    rs.engine.config.max_rollback = max_rollback;
    rs.engine.start();
    rs.match_outcome = None;
    rs.round_wins = None;
    rs.set_wins = None;
    rs.set_over = false;
    rs.ranked = ranked;

    // Wait for IPC client to be ready (background thread may still be connecting)
    let ipc_client = {
        let mut client = None;
        for attempt in 0..20 {
            let ds = dolphin_proc_state.lock().map_err(|e| e.to_string())?;
            if let Some(ref c) = ds.ipc_client {
                client = Some(c.clone());
                crate::diagnostics::log_info(&format!(
                    "IPC client found on attempt {}/20", attempt + 1
                ));
                break;
            }
            drop(ds); // Release lock before sleeping
            if attempt == 0 {
                crate::diagnostics::log_info("Waiting for IPC client to connect...");
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if client.is_none() {
            crate::diagnostics::log_warn("IPC client not available after 10s — falling back to legacy");
        }
        client
    };

    if let Some(ipc) = ipc_client {
        // ── IPC PATH: Use our Dolphin fork with true rollback ──
        crate::diagnostics::log_info("Using IPC-based rollback (HowlingWind Dolphin fork)");
        let rb_clone = Arc::clone(&*state);
        let ds_clone = Arc::clone(&*dolphin_state);
        let np_clone = Arc::clone(&*netplay_state);

        let handle = std::thread::Builder::new()
            .name("rollback-ipc-loop".to_string())
            .spawn(move || {
                crate::diagnostics::log_info("IPC rollback thread started, creating runtime...");
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        crate::diagnostics::log_error(&format!("Failed to create tokio runtime: {}", e));
                        return;
                    }
                };
                crate::diagnostics::log_info("Tokio runtime created, entering game loop...");
                rt.block_on(crate::rollback_ipc::run_ipc_game_loop(
                    ipc, rb_clone, ds_clone, np_clone, local_player,
                ));
            })
            .map_err(|e| format!("Failed to spawn IPC rollback thread: {}", e))?;

        rs.loop_handle = Some(handle);
        Ok(format!("Rollback engine started [IPC/FORK] (player {}, delay {}, max rb {})",
            local_player, input_delay, max_rollback))
    } else {
        // ── LEGACY PATH: External memory polling ──
        eprintln!("[rollback] Using legacy external memory rollback (stock Dolphin)");

        #[cfg(windows)]
        {
            let rb_clone = Arc::clone(&*state);
            let ds_clone = Arc::clone(&*dolphin_state);
            let np_clone = Arc::clone(&*netplay_state);

            let handle = std::thread::Builder::new()
                .name("rollback-loop".to_string())
                .spawn(move || {
                    run_game_loop(rb_clone, ds_clone, np_clone, local_player);
                })
                .map_err(|e| format!("Failed to spawn rollback thread: {}", e))?;

            rs.loop_handle = Some(handle);
        }

        Ok(format!("Rollback engine started [LEGACY] (player {}, delay {}, max rb {})",
            local_player, input_delay, max_rollback))
    }
}

#[tauri::command]
pub fn rollback_stats(
    state: tauri::State<'_, Arc<Mutex<RollbackState>>>,
) -> Result<RollbackStats, String> {
    let rs = state.lock().map_err(|e| e.to_string())?;
    Ok(rs.engine.get_stats())
}

#[tauri::command]
pub fn rollback_stop(
    state: tauri::State<'_, Arc<Mutex<RollbackState>>>,
) -> Result<(), String> {
    let mut rs = state.lock().map_err(|e| e.to_string())?;
    rs.engine.stop(); // Sets state to Idle, which causes the background loop to exit

    // Wait for the background thread to finish
    if let Some(handle) = rs.loop_handle.take() {
        drop(rs); // Release lock before join
        let _ = handle.join();
    }
    Ok(())
}

/// Check if a match has ended (polled by frontend).
#[tauri::command]
pub fn rollback_check_match_end(
    state: tauri::State<'_, Arc<Mutex<RollbackState>>>,
) -> Result<Option<MatchEndInfo>, String> {
    let rs = state.lock().map_err(|e| e.to_string())?;
    if let Some(ref outcome) = rs.match_outcome {
        Ok(Some(MatchEndInfo {
            outcome: outcome.clone(),
            round_wins: rs.round_wins,
            set_wins: rs.set_wins,
            set_over: rs.set_over,
            ranked: rs.ranked,
        }))
    } else {
        Ok(None)
    }
}

/// Clear the match outcome after frontend has consumed it.
#[tauri::command]
pub fn rollback_clear_match_end(
    state: tauri::State<'_, Arc<Mutex<RollbackState>>>,
) -> Result<(), String> {
    let mut rs = state.lock().map_err(|e| e.to_string())?;
    rs.match_outcome = None;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchEndInfo {
    pub outcome: MatchOutcome,
    pub round_wins: Option<(u32, u32)>,
    pub set_wins: Option<(u32, u32)>,
    pub set_over: bool,
    pub ranked: bool,
}

/// Run one tick of the rollback engine (manual mode, not using background loop).
/// Called from the frontend's game loop (via requestAnimationFrame or a timer).
#[tauri::command]
pub fn rollback_tick(
    rb_state: tauri::State<'_, Arc<Mutex<RollbackState>>>,
    dolphin_state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
    netplay_state: tauri::State<'_, Arc<Mutex<NetplayState>>>,
) -> Result<TickAction, String> {
    #[cfg(windows)]
    {
        let mut rb = rb_state.lock().map_err(|e| e.to_string())?;
        let mut dolphin = dolphin_state.lock().map_err(|e| e.to_string())?;
        let mut np = netplay_state.lock().map_err(|e| e.to_string())?;

        let session = np.session.as_mut().ok_or("No netplay session")?;
        let action = rb.engine.tick(&mut *dolphin, session);

        Ok(action)
    }

    #[cfg(not(windows))]
    Err("Rollback only supported on Windows".to_string())
}
