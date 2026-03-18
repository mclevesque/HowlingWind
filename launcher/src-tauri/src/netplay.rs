//! P2P UDP networking for rollback netplay.
//!
//! Handles input exchange between two players over UDP.
//! Uses a simple packet format optimized for low latency.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, RwLock};

// ── Packet Format ──

const PACKET_MAGIC: u32 = 0x484F574C; // "HOWL"
const PACKET_SIZE: usize = 20;

const SYNC_MAGIC: u32 = 0x53594E43; // "SYNC"
const SYNC_PACKET_SIZE: usize = 16;

const PING_MAGIC: u32 = 0x50494E47; // "PING"
const PONG_MAGIC: u32 = 0x504F4E47; // "PONG"
const PING_PACKET_SIZE: usize = 12; // magic(4) + timestamp_us(8)

/// Input state for a single frame.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FrameInput {
    pub buttons: u16,    // GC button bitmask
    pub stick_x: i8,     // Main stick X (-128..127)
    pub stick_y: i8,     // Main stick Y
    pub cstick_x: i8,    // C-stick X
    pub cstick_y: i8,    // C-stick Y
    pub trigger_l: u8,   // L analog
    pub trigger_r: u8,   // R analog
}

impl Default for FrameInput {
    fn default() -> Self {
        Self {
            buttons: 0,
            stick_x: 0,
            stick_y: 0,
            cstick_x: 0,
            cstick_y: 0,
            trigger_l: 0,
            trigger_r: 0,
        }
    }
}

/// Wire format for a single input packet (20 bytes).
#[derive(Debug, Clone)]
pub struct InputPacket {
    pub frame: u32,
    pub player_id: u8,
    pub input: FrameInput,
    pub checksum: u32,
}

impl InputPacket {
    pub fn serialize(&self) -> [u8; PACKET_SIZE] {
        let mut buf = [0u8; PACKET_SIZE];
        buf[0..4].copy_from_slice(&PACKET_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&self.frame.to_le_bytes());
        buf[8] = self.player_id;
        buf[9..11].copy_from_slice(&self.input.buttons.to_le_bytes());
        buf[11] = self.input.stick_x as u8;
        buf[12] = self.input.stick_y as u8;
        buf[13] = self.input.cstick_x as u8;
        buf[14] = self.input.cstick_y as u8;
        buf[15] = self.input.trigger_l;
        buf[16] = self.input.trigger_r;
        // bytes 17-19: checksum (first 3 bytes of u32)
        let crc = crc32fast::hash(&buf[4..17]);
        buf[17..20].copy_from_slice(&crc.to_le_bytes()[0..3]);
        buf
    }

    pub fn deserialize(buf: &[u8; PACKET_SIZE]) -> Option<Self> {
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != PACKET_MAGIC {
            return None;
        }

        // Verify checksum
        let expected_crc = crc32fast::hash(&buf[4..17]);
        let received_crc_bytes = [buf[17], buf[18], buf[19], 0];
        let received_crc = u32::from_le_bytes(received_crc_bytes) & 0x00FFFFFF;
        if (expected_crc & 0x00FFFFFF) != received_crc {
            return None;
        }

        Some(InputPacket {
            frame: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            player_id: buf[8],
            input: FrameInput {
                buttons: u16::from_le_bytes([buf[9], buf[10]]),
                stick_x: buf[11] as i8,
                stick_y: buf[12] as i8,
                cstick_x: buf[13] as i8,
                cstick_y: buf[14] as i8,
                trigger_l: buf[15],
                trigger_r: buf[16],
            },
            checksum: expected_crc,
        })
    }
}

/// Sync packet for desync detection (16 bytes).
/// Sent every N frames to verify game state matches.
#[derive(Debug, Clone)]
pub struct SyncPacket {
    pub frame: u32,
    pub state_hash: u32, // CRC32 of critical game state
    pub p1_health: u16,
    pub p2_health: u16,
}

impl SyncPacket {
    pub fn serialize(&self) -> [u8; SYNC_PACKET_SIZE] {
        let mut buf = [0u8; SYNC_PACKET_SIZE];
        buf[0..4].copy_from_slice(&SYNC_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&self.frame.to_le_bytes());
        buf[8..12].copy_from_slice(&self.state_hash.to_le_bytes());
        buf[12..14].copy_from_slice(&self.p1_health.to_le_bytes());
        buf[14..16].copy_from_slice(&self.p2_health.to_le_bytes());
        buf
    }

    pub fn deserialize(buf: &[u8]) -> Option<Self> {
        if buf.len() < SYNC_PACKET_SIZE {
            return None;
        }
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != SYNC_MAGIC {
            return None;
        }
        Some(SyncPacket {
            frame: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            state_hash: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            p1_health: u16::from_le_bytes([buf[12], buf[13]]),
            p2_health: u16::from_le_bytes([buf[14], buf[15]]),
        })
    }
}

// ── Input Buffer ──

/// Ring buffer of inputs indexed by frame number.
/// Stores both local and remote inputs for rollback.
pub struct InputBuffer {
    /// Local player's confirmed inputs by frame.
    pub local: HashMap<u32, FrameInput>,
    /// Remote player's confirmed inputs by frame.
    pub remote: HashMap<u32, FrameInput>,
    /// Predicted inputs for frames where remote hasn't arrived yet.
    pub predicted: HashMap<u32, FrameInput>,
    /// The latest frame for which we have confirmed remote input.
    pub latest_remote_frame: u32,
    /// The latest frame for which we have local input.
    pub latest_local_frame: u32,
    /// Maximum number of frames we'll keep in history.
    pub max_history: u32,
}

impl InputBuffer {
    pub fn new(max_history: u32) -> Self {
        Self {
            local: HashMap::new(),
            remote: HashMap::new(),
            predicted: HashMap::new(),
            latest_remote_frame: 0,
            latest_local_frame: 0,
            max_history,
        }
    }

    /// Record local input for a frame.
    pub fn add_local(&mut self, frame: u32, input: FrameInput) {
        self.local.insert(frame, input);
        if frame > self.latest_local_frame {
            self.latest_local_frame = frame;
        }
        self.gc_old_frames(frame);
    }

    /// Record confirmed remote input. Returns true if this caused a prediction mismatch.
    pub fn add_remote(&mut self, frame: u32, input: FrameInput) -> bool {
        let mismatch = if let Some(predicted) = self.predicted.get(&frame) {
            predicted.buttons != input.buttons
                || predicted.stick_x != input.stick_x
                || predicted.stick_y != input.stick_y
                || predicted.cstick_x != input.cstick_x
                || predicted.cstick_y != input.cstick_y
                || predicted.trigger_l != input.trigger_l
                || predicted.trigger_r != input.trigger_r
        } else {
            false
        };

        self.remote.insert(frame, input);
        self.predicted.remove(&frame);

        if frame > self.latest_remote_frame {
            self.latest_remote_frame = frame;
        }

        mismatch
    }

    /// Get the remote input for a frame, predicting if not yet received.
    /// Returns (input, is_predicted).
    pub fn get_remote(&mut self, frame: u32) -> (FrameInput, bool) {
        if let Some(input) = self.remote.get(&frame) {
            return (*input, false);
        }

        if let Some(input) = self.predicted.get(&frame) {
            return (*input, true);
        }

        // Generate prediction: repeat last known input
        let predicted = if let Some(input) = self.remote.get(&self.latest_remote_frame) {
            *input
        } else {
            FrameInput::default()
        };

        self.predicted.insert(frame, predicted);
        (predicted, true)
    }

    fn gc_old_frames(&mut self, current_frame: u32) {
        if current_frame <= self.max_history {
            return;
        }
        let cutoff = current_frame - self.max_history;
        self.local.retain(|&f, _| f >= cutoff);
        self.remote.retain(|&f, _| f >= cutoff);
        self.predicted.retain(|&f, _| f >= cutoff);
    }
}

// ── Network Session ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionState {
    Disconnected,
    Connecting,
    Connected,
    Playing,
}

/// A netplay session managing UDP communication with one peer.
pub struct NetplaySession {
    pub state: SessionState,
    pub local_player_id: u8,  // 0 = P1, 1 = P2
    pub local_port: u16,
    pub peer_addr: Option<SocketAddr>,
    /// Shared peer address — updated by set_peer(), read by send tasks
    shared_peer_addr: Arc<RwLock<Option<SocketAddr>>>,
    pub input_buffer: InputBuffer,
    pub input_delay: u32,     // frames of intentional delay
    pub max_rollback: u32,    // max frames to rollback
    pub current_frame: u32,
    pub socket: Option<Arc<UdpSocket>>,
    pub send_tx: Option<mpsc::Sender<InputPacket>>,
    recv_rx: Option<mpsc::Receiver<InputPacket>>,
    pub sync_send_tx: Option<mpsc::Sender<SyncPacket>>,
    pub sync_rx: Option<mpsc::Receiver<SyncPacket>>,
    pub desync_info: Option<(u32, u32, u32)>,
    pub ping_ms: f64,
    pub last_ping_sent_us: u64,
    pub pong_rx: Option<mpsc::Receiver<u64>>,
    pub raw_send_tx: Option<mpsc::Sender<Vec<u8>>>,
}

impl NetplaySession {
    pub fn new(player_id: u8, input_delay: u32, max_rollback: u32) -> Self {
        Self {
            state: SessionState::Disconnected,
            local_player_id: player_id,
            local_port: 0,
            peer_addr: None,
            shared_peer_addr: Arc::new(RwLock::new(None)),
            input_buffer: InputBuffer::new(max_rollback + 30),
            input_delay,
            max_rollback,
            current_frame: 0,
            socket: None,
            send_tx: None,
            recv_rx: None,
            sync_send_tx: None,
            sync_rx: None,
            desync_info: None,
            ping_ms: 0.0,
            last_ping_sent_us: 0,
            pong_rx: None,
            raw_send_tx: None,
        }
    }

    /// Bind to a UDP port and start listening.
    pub async fn bind(&mut self, port: u16) -> Result<u16, String> {
        let addr = format!("0.0.0.0:{}", port);
        let socket = UdpSocket::bind(&addr)
            .await
            .map_err(|e| format!("Failed to bind UDP socket: {}", e))?;

        let local_addr = socket.local_addr().map_err(|e| e.to_string())?;
        self.local_port = local_addr.port();

        let socket = Arc::new(socket);
        self.socket = Some(socket.clone());

        // Create channels for sending/receiving
        let (send_tx, mut send_rx) = mpsc::channel::<InputPacket>(256);
        let (recv_tx, recv_rx) = mpsc::channel::<InputPacket>(256);
        let (sync_tx, sync_rx) = mpsc::channel::<SyncPacket>(32);
        let (sync_send_tx, mut sync_send_rx) = mpsc::channel::<SyncPacket>(32);
        let (pong_tx, pong_rx) = mpsc::channel::<u64>(16);
        let (raw_send_tx, mut raw_send_rx) = mpsc::channel::<Vec<u8>>(64);

        self.send_tx = Some(send_tx);
        self.recv_rx = Some(recv_rx);
        self.sync_send_tx = Some(sync_send_tx);
        self.sync_rx = Some(sync_rx);
        self.pong_rx = Some(pong_rx);
        self.raw_send_tx = Some(raw_send_tx);

        // All send tasks share this — gets updated when set_peer() is called
        let peer = self.shared_peer_addr.clone();

        // Spawn send task (input packets)
        let send_socket = socket.clone();
        let peer_for_input = peer.clone();
        tokio::spawn(async move {
            while let Some(packet) = send_rx.recv().await {
                if let Some(addr) = *peer_for_input.read().await {
                    let data = packet.serialize();
                    let _ = send_socket.send_to(&data, addr).await;
                }
            }
        });

        // Spawn sync send task (desync detection packets)
        let sync_send_socket = socket.clone();
        let peer_for_sync = peer.clone();
        tokio::spawn(async move {
            while let Some(sync_packet) = sync_send_rx.recv().await {
                if let Some(addr) = *peer_for_sync.read().await {
                    let data = sync_packet.serialize();
                    let _ = sync_send_socket.send_to(&data, addr).await;
                }
            }
        });

        // Spawn raw send task (ping/pong packets)
        let raw_send_socket = socket.clone();
        let peer_for_raw = peer.clone();
        tokio::spawn(async move {
            while let Some(data) = raw_send_rx.recv().await {
                if let Some(addr) = *peer_for_raw.read().await {
                    let _ = raw_send_socket.send_to(&data, addr).await;
                }
            }
        });

        // Spawn receive task — handles input (20), sync (16), and ping/pong (12) packets
        let recv_socket = socket.clone();
        let pong_reply_socket = socket.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            loop {
                match recv_socket.recv_from(&mut buf).await {
                    Ok((n, from_addr)) => {
                        if n == PACKET_SIZE {
                            if let Some(packet) = InputPacket::deserialize(
                                &<[u8; PACKET_SIZE]>::try_from(&buf[..PACKET_SIZE]).unwrap()
                            ) {
                                if recv_tx.send(packet).await.is_err() {
                                    break;
                                }
                            }
                        } else if n == SYNC_PACKET_SIZE {
                            if let Some(sync) = SyncPacket::deserialize(&buf[..n]) {
                                let _ = sync_tx.send(sync).await;
                            }
                        } else if n == PING_PACKET_SIZE {
                            let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                            if magic == PING_MAGIC {
                                let mut pong = [0u8; PING_PACKET_SIZE];
                                pong[0..4].copy_from_slice(&PONG_MAGIC.to_le_bytes());
                                pong[4..12].copy_from_slice(&buf[4..12]);
                                let _ = pong_reply_socket.send_to(&pong, from_addr).await;
                            } else if magic == PONG_MAGIC {
                                let ts = u64::from_le_bytes([
                                    buf[4], buf[5], buf[6], buf[7],
                                    buf[8], buf[9], buf[10], buf[11],
                                ]);
                                let _ = pong_tx.send(ts).await;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        self.state = SessionState::Connecting;
        Ok(self.local_port)
    }

    /// Set the peer's address to send inputs to.
    pub fn set_peer(&mut self, addr: SocketAddr) {
        self.peer_addr = Some(addr);
        // Update the shared peer address so send tasks can see it.
        // Use try_write first (non-blocking), fall back to spawning if contended.
        let shared = self.shared_peer_addr.clone();
        match shared.try_write() {
            Ok(mut guard) => {
                *guard = Some(addr);
            }
            Err(_) => {
                // Lock is contended — spawn a task to update it
                let shared2 = shared.clone();
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        *shared2.write().await = Some(addr);
                    });
                }
            }
        }
        self.state = SessionState::Connected;
    }

    /// Send local input for the current frame.
    pub async fn send_input(&mut self, input: FrameInput) -> Result<(), String> {
        let frame = self.current_frame;
        self.input_buffer.add_local(frame, input);

        let packet = InputPacket {
            frame,
            player_id: self.local_player_id,
            input,
            checksum: 0,
        };

        if let Some(ref tx) = self.send_tx {
            tx.send(packet)
                .await
                .map_err(|e| format!("Failed to send input: {}", e))?;
        }

        Ok(())
    }

    /// Process received packets. Returns frames that need rollback (if any).
    pub fn process_received(&mut self) -> Vec<u32> {
        let mut rollback_frames = Vec::new();
        let mut received_count = 0u32;

        if let Some(ref mut rx) = self.recv_rx {
            while let Ok(packet) = rx.try_recv() {
                // Skip test packets from controller test phase
                if packet.frame >= 90000 {
                    continue;
                }
                received_count += 1;
                let mismatch = self.input_buffer.add_remote(packet.frame, packet.input);
                if mismatch {
                    rollback_frames.push(packet.frame);
                }
                // Log first 10 received packets for debugging
                if self.input_buffer.remote.len() <= 10 {
                    crate::diagnostics::log_info(&format!(
                        "[UDP_RECV] frame={} player={} btns=0x{:04X} mismatch={} total_remote={}",
                        packet.frame, packet.player_id, packet.input.buttons,
                        mismatch, self.input_buffer.remote.len()
                    ));
                }
            }
        }

        if received_count > 0 && self.input_buffer.remote.len() <= 20 {
            crate::diagnostics::log_info(&format!(
                "[UDP_RECV] batch: {} packets, {} rollback triggers",
                received_count, rollback_frames.len()
            ));
        }

        rollback_frames.sort();
        rollback_frames
    }

    /// Advance to the next frame.
    pub fn advance_frame(&mut self) {
        self.current_frame += 1;
    }

    /// Get remote input for current frame (confirmed or predicted).
    pub fn get_remote_input(&mut self) -> (FrameInput, bool) {
        let frame = self.current_frame;
        self.input_buffer.get_remote(frame)
    }

    /// How many frames ahead we are of the remote player.
    pub fn frames_ahead(&self) -> u32 {
        self.current_frame.saturating_sub(self.input_buffer.latest_remote_frame)
    }

    /// Whether we should stall (wait for remote) to avoid getting too far ahead.
    pub fn should_stall(&self) -> bool {
        self.frames_ahead() > self.max_rollback
    }

    /// Process received sync packets. Returns desync info if hash mismatch detected.
    pub fn process_sync_packets(&mut self, local_hashes: &HashMap<u32, u32>) -> Option<(u32, u32, u32)> {
        if let Some(ref mut rx) = self.sync_rx {
            while let Ok(sync) = rx.try_recv() {
                if let Some(&local_hash) = local_hashes.get(&sync.frame) {
                    if local_hash != sync.state_hash {
                        self.desync_info = Some((sync.frame, local_hash, sync.state_hash));
                        return Some((sync.frame, local_hash, sync.state_hash));
                    }
                }
            }
        }
        None
    }

    /// Send a ping packet with current timestamp in microseconds.
    pub fn send_ping(&mut self) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        self.last_ping_sent_us = now_us;

        let mut ping = [0u8; PING_PACKET_SIZE];
        ping[0..4].copy_from_slice(&PING_MAGIC.to_le_bytes());
        ping[4..12].copy_from_slice(&now_us.to_le_bytes());

        if let Some(ref tx) = self.raw_send_tx {
            let _ = tx.try_send(ping.to_vec());
        }
    }

    /// Process received pong packets and update rolling ping average.
    pub fn process_pongs(&mut self) -> f64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        if let Some(ref mut rx) = self.pong_rx {
            while let Ok(sent_ts) = rx.try_recv() {
                let rtt_us = now_us.saturating_sub(sent_ts);
                let rtt_ms = rtt_us as f64 / 1000.0;
                if self.ping_ms == 0.0 {
                    self.ping_ms = rtt_ms;
                } else {
                    self.ping_ms = self.ping_ms * 0.8 + rtt_ms * 0.2;
                }
            }
        }
        self.ping_ms
    }
}

// ── Tauri Commands ──

use std::sync::Mutex as StdMutex;

pub struct NetplayState {
    pub session: Option<NetplaySession>,
    pub runtime: Option<tokio::runtime::Runtime>,
}

impl NetplayState {
    pub fn new() -> Self {
        Self {
            session: None,
            runtime: None,
        }
    }
}

/// Start a netplay session. Returns the local UDP port.
#[tauri::command]
pub fn netplay_start(
    player_id: u8,
    input_delay: u32,
    max_rollback: u32,
    port: u16,
    state: tauri::State<'_, Arc<StdMutex<NetplayState>>>,
) -> Result<u16, String> {
    let mut ns = state.lock().map_err(|e| e.to_string())?;

    if ns.runtime.is_none() {
        ns.runtime = Some(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to create async runtime: {}", e))?,
        );
    }

    let mut session = NetplaySession::new(player_id, input_delay, max_rollback);

    let bound_port = ns.runtime.as_ref().unwrap().block_on(session.bind(port))?;

    ns.session = Some(session);
    Ok(bound_port)
}

/// Connect to a peer by address.
#[tauri::command]
pub fn netplay_connect(
    peer_address: String,
    state: tauri::State<'_, Arc<StdMutex<NetplayState>>>,
) -> Result<(), String> {
    let mut ns = state.lock().map_err(|e| e.to_string())?;
    let session = ns.session.as_mut().ok_or("No netplay session active")?;

    let addr: SocketAddr = peer_address
        .parse()
        .map_err(|e| format!("Invalid peer address: {}", e))?;

    session.set_peer(addr);
    Ok(())
}

/// Get current netplay status.
#[tauri::command]
pub fn netplay_status(
    state: tauri::State<'_, Arc<StdMutex<NetplayState>>>,
) -> Result<NetplayStatusInfo, String> {
    let ns = state.lock().map_err(|e| e.to_string())?;

    Ok(match &ns.session {
        Some(session) => NetplayStatusInfo {
            state: format!("{:?}", session.state),
            local_port: session.local_port,
            current_frame: session.current_frame,
            latest_remote_frame: session.input_buffer.latest_remote_frame,
            frames_ahead: session.frames_ahead(),
            peer_connected: session.peer_addr.is_some(),
        },
        None => NetplayStatusInfo {
            state: "None".to_string(),
            local_port: 0,
            current_frame: 0,
            latest_remote_frame: 0,
            frames_ahead: 0,
            peer_connected: false,
        },
    })
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NetplayStatusInfo {
    pub state: String,
    pub local_port: u16,
    pub current_frame: u32,
    pub latest_remote_frame: u32,
    pub frames_ahead: u32,
    pub peer_connected: bool,
}

/// Stop netplay session.
#[tauri::command]
pub fn netplay_stop(
    state: tauri::State<'_, Arc<StdMutex<NetplayState>>>,
) -> Result<(), String> {
    let mut ns = state.lock().map_err(|e| e.to_string())?;
    ns.session = None;
    Ok(())
}

/// Run a 5-round sync handshake to verify bidirectional connectivity and measure RTT.
/// Returns { success: bool, avg_ping_ms: f64, rounds_completed: u32 }
/// This should be called AFTER netplay_connect, BEFORE launching Dolphin.
#[tauri::command]
pub async fn netplay_sync_test(
    state: tauri::State<'_, Arc<StdMutex<NetplayState>>>,
) -> Result<serde_json::Value, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    const NUM_SYNC_ROUNDS: u32 = 5;
    const SYNC_TIMEOUT_MS: u64 = 3000;

    // Get raw_send_tx and pong_rx from session
    let (raw_tx, has_peer) = {
        let ns = state.lock().map_err(|e| e.to_string())?;
        let session = ns.session.as_ref().ok_or("No session")?;
        (session.raw_send_tx.clone(), session.peer_addr.is_some())
    };

    if !has_peer {
        return Ok(serde_json::json!({
            "success": false,
            "avg_ping_ms": 0.0,
            "rounds_completed": 0,
            "error": "No peer connected"
        }));
    }

    let raw_tx = raw_tx.ok_or("No raw send channel")?;

    let mut rtts = Vec::new();
    let mut rounds_ok = 0u32;

    for round in 0..NUM_SYNC_ROUNDS {
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        // Send ping
        let mut ping = [0u8; PING_PACKET_SIZE];
        ping[0..4].copy_from_slice(&PING_MAGIC.to_le_bytes());
        ping[4..12].copy_from_slice(&now_us.to_le_bytes());
        let _ = raw_tx.send(ping.to_vec()).await;

        // Wait for pong with timeout
        let deadline = tokio::time::Instant::now()
            + tokio::time::Duration::from_millis(SYNC_TIMEOUT_MS);

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                crate::diagnostics::log_warn(&format!("Sync round {} timed out", round + 1));
                break;
            }

            // Check for pong
            {
                let mut ns = state.lock().map_err(|e| e.to_string())?;
                if let Some(session) = &mut ns.session {
                    if let Some(ref mut pong_rx) = session.pong_rx {
                        if let Ok(sent_ts) = pong_rx.try_recv() {
                            let reply_us = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_micros() as u64;
                            let rtt_ms = (reply_us.saturating_sub(sent_ts)) as f64 / 1000.0;
                            rtts.push(rtt_ms);
                            rounds_ok += 1;
                            crate::diagnostics::log_info(&format!(
                                "Sync round {}/{}: {:.1}ms",
                                round + 1, NUM_SYNC_ROUNDS, rtt_ms
                            ));
                            break;
                        }
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    }

    let avg_ping = if rtts.is_empty() {
        0.0
    } else {
        rtts.iter().sum::<f64>() / rtts.len() as f64
    };

    let success = rounds_ok >= 3; // At least 3 of 5 rounds must succeed
    let recommended_delay = if avg_ping > 0.0 {
        ((avg_ping / 16.67 / 2.0).ceil() as u32).max(1).min(7)
    } else {
        2
    };

    crate::diagnostics::log_info(&format!(
        "Sync test: {}/{} rounds OK, avg {:.1}ms, recommended delay {}f",
        rounds_ok, NUM_SYNC_ROUNDS, avg_ping, recommended_delay
    ));

    Ok(serde_json::json!({
        "success": success,
        "avg_ping_ms": avg_ping,
        "rounds_completed": rounds_ok,
        "recommended_delay": recommended_delay,
    }))
}

/// Read what the remote player is pressing right now (from the UDP input channel).
/// Also sends our local controller state so the remote can see ours.
/// Used for the pre-game controller test.
#[tauri::command]
pub fn netplay_poll_remote_input(
    state: tauri::State<'_, Arc<StdMutex<NetplayState>>>,
) -> Result<serde_json::Value, String> {
    let mut ns = state.lock().map_err(|e| e.to_string())?;
    let session = ns.session.as_mut().ok_or("No session")?;

    // Read any incoming input packets
    let mut remote_buttons: u16 = 0;
    let mut remote_stick_x: i8 = 0;
    let mut remote_stick_y: i8 = 0;
    let mut got_remote = false;

    if let Some(ref mut rx) = session.recv_rx {
        while let Ok(packet) = rx.try_recv() {
            remote_buttons = packet.input.buttons;
            remote_stick_x = packet.input.stick_x;
            remote_stick_y = packet.input.stick_y;
            got_remote = true;
        }
    }

    Ok(serde_json::json!({
        "got_remote": got_remote,
        "buttons": remote_buttons,
        "stick_x": remote_stick_x,
        "stick_y": remote_stick_y,
    }))
}

/// Send our current controller state to the remote player for the input test.
#[tauri::command]
pub async fn netplay_send_test_input(
    buttons: u16,
    stick_x: i8,
    stick_y: i8,
    state: tauri::State<'_, Arc<StdMutex<NetplayState>>>,
) -> Result<(), String> {
    let ns = state.lock().map_err(|e| e.to_string())?;
    let session = ns.session.as_ref().ok_or("No session")?;

    let packet = InputPacket {
        frame: 99999, // Special frame number = test mode
        player_id: session.local_player_id,
        input: FrameInput {
            buttons,
            stick_x,
            stick_y,
            cstick_x: 0,
            cstick_y: 0,
            trigger_l: 0,
            trigger_r: 0,
        },
        checksum: 0,
    };

    if let Some(ref tx) = session.send_tx {
        tx.try_send(packet).map_err(|e| e.to_string())?;
    }

    Ok(())
}
