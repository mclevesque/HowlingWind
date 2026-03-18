//! HowlingWind IPC client — connects to our Dolphin fork on localhost:17492.
//!
//! This replaces the old external memory (ReadProcessMemory) approach with
//! direct control over the emulator via TCP IPC.

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex, Notify};
use std::collections::HashMap;

const HW_SERVER_PORT: u16 = 17492;
const CONNECT_TIMEOUT_MS: u64 = 5000;
const RECONNECT_DELAY_MS: u64 = 500;

/// Parsed response from the IPC server.
#[derive(Debug, Clone)]
pub enum HWResponse {
    Ok { command: String, args: String },
    Error { command: String, reason: String },
    Frame(u64),
    State(String),
    Pong,
    Event { event_type: String, data: String },
    Unknown(String),
}

impl HWResponse {
    fn parse(line: &str) -> Self {
        let line = line.trim();
        if line.starts_with("OK ") {
            let rest = &line[3..];
            let (cmd, args) = split_first(rest);
            HWResponse::Ok { command: cmd, args }
        } else if line.starts_with("ERR ") {
            let rest = &line[4..];
            let (cmd, reason) = split_first(rest);
            HWResponse::Error { command: cmd, reason }
        } else if line.starts_with("FRAME ") {
            let n = line[6..].trim().parse::<u64>().unwrap_or(0);
            HWResponse::Frame(n)
        } else if line.starts_with("STATE ") {
            HWResponse::State(line[6..].trim().to_string())
        } else if line == "PONG" {
            HWResponse::Pong
        } else if line.starts_with("EVENT ") {
            let rest = &line[6..];
            let (event_type, data) = split_first(rest);
            HWResponse::Event { event_type, data }
        } else if line.starts_with("HOWLINGWIND ") {
            // Welcome message — ignore
            HWResponse::Ok { command: "WELCOME".to_string(), args: line.to_string() }
        } else {
            HWResponse::Unknown(line.to_string())
        }
    }
}

fn split_first(s: &str) -> (String, String) {
    match s.find(' ') {
        Some(pos) => (s[..pos].to_string(), s[pos + 1..].to_string()),
        None => (s.to_string(), String::new()),
    }
}

/// Represents a GC controller input state for IPC.
#[derive(Debug, Clone, Default)]
pub struct HWPadInput {
    pub buttons: u16,
    pub stick_x: i8,
    pub stick_y: i8,
    pub cstick_x: i8,
    pub cstick_y: i8,
    pub trigger_l: u8,
    pub trigger_r: u8,
}

impl HWPadInput {
    /// Format as IPC command args: "buttons stickX stickY cstickX cstickY trigL trigR"
    pub fn to_ipc_args(&self) -> String {
        format!(
            "0x{:04X} {} {} {} {} {} {}",
            self.buttons,
            self.stick_x,
            self.stick_y,
            self.cstick_x,
            self.cstick_y,
            self.trigger_l,
            self.trigger_r
        )
    }
}

/// The IPC client for communicating with our Dolphin fork.
pub struct HWClient {
    /// Sender for commands to write to the TCP stream
    cmd_tx: mpsc::Sender<String>,
    /// Receiver for frame boundary events from Dolphin
    frame_rx: Arc<Mutex<mpsc::Receiver<u64>>>,
    /// Receiver for command responses
    resp_rx: Arc<Mutex<mpsc::Receiver<HWResponse>>>,
    /// Serializes command+response pairs to prevent response mismatch
    cmd_lock: Mutex<()>,
    /// Whether we're connected
    connected: Arc<std::sync::atomic::AtomicBool>,
    /// Current frame number (updated by frame events)
    current_frame: Arc<std::sync::atomic::AtomicU64>,
}

impl HWClient {
    /// Connect to the Dolphin fork's IPC server.
    /// Retries for up to `timeout_ms` before giving up.
    pub async fn connect(timeout_ms: u64) -> Result<Self, String> {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

        let stream = loop {
            match TcpStream::connect(format!("127.0.0.1:{}", HW_SERVER_PORT)).await {
                Ok(s) => break s,
                Err(e) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(format!("Failed to connect to Dolphin IPC: {}", e));
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(RECONNECT_DELAY_MS)).await;
                }
            }
        };

        eprintln!("[hw_ipc] Connected to Dolphin fork on port {}", HW_SERVER_PORT);

        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader);
        let writer = Arc::new(Mutex::new(writer));

        // Channels
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<String>(256);
        let (frame_tx, frame_rx) = mpsc::channel::<u64>(1024);
        let (resp_tx, resp_rx) = mpsc::channel::<HWResponse>(256);

        let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let current_frame = Arc::new(std::sync::atomic::AtomicU64::new(0));

        // Writer task: sends commands from cmd_tx to TCP
        let writer_clone = writer.clone();
        let connected_w = connected.clone();
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                let mut w = writer_clone.lock().await;
                if let Err(e) = w.write_all(cmd.as_bytes()).await {
                    eprintln!("[hw_ipc] Write error: {}", e);
                    connected_w.store(false, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
                let _ = w.flush().await;
            }
        });

        // Reader task: reads responses from TCP, routes to frame_tx or resp_tx
        let connected_r = connected.clone();
        let current_frame_r = current_frame.clone();
        tokio::spawn(async move {
            let mut lines = reader.lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let resp = HWResponse::parse(&line);
                        match &resp {
                            HWResponse::Event { event_type, data } if event_type == "FRAME_BOUNDARY" => {
                                if let Ok(f) = data.trim().parse::<u64>() {
                                    current_frame_r.store(f, std::sync::atomic::Ordering::Relaxed);
                                    let _ = frame_tx.try_send(f);
                                }
                            }
                            _ => {
                                let _ = resp_tx.try_send(resp);
                            }
                        }
                    }
                    Ok(None) => {
                        eprintln!("[hw_ipc] Connection closed by Dolphin");
                        connected_r.store(false, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    Err(e) => {
                        eprintln!("[hw_ipc] Read error: {}", e);
                        connected_r.store(false, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                }
            }
        });

        let client = Self {
            cmd_tx,
            frame_rx: Arc::new(Mutex::new(frame_rx)),
            resp_rx: Arc::new(Mutex::new(resp_rx)),
            cmd_lock: Mutex::new(()),
            connected,
            current_frame,
        };

        // Wait for and consume the WELCOME message from the server.
        // This MUST be drained before any commands are sent, otherwise
        // the first command (e.g. PING) will get the WELCOME as its response.
        {
            let mut rx = client.resp_rx.lock().await;
            let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
            loop {
                match tokio::time::timeout(
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                    rx.recv(),
                ).await {
                    Ok(Some(HWResponse::Ok { command, .. })) if command == "WELCOME" => {
                        eprintln!("[hw_ipc] Welcome message consumed");
                        break;
                    }
                    Ok(Some(_other)) => {
                        // Some other message arrived first — discard and keep waiting
                        continue;
                    }
                    Ok(None) => {
                        eprintln!("[hw_ipc] Channel closed before welcome");
                        break;
                    }
                    Err(_) => {
                        eprintln!("[hw_ipc] Timeout waiting for welcome (5s) — proceeding anyway");
                        break;
                    }
                }
            }
        }

        Ok(client)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Drain any stale messages from the response channel (WELCOME, etc.)
    /// Call this before sending the first command on a new runtime.
    pub async fn drain_stale(&self) {
        let mut rx = self.resp_rx.lock().await;
        while let Ok(msg) = rx.try_recv() {
            eprintln!("[hw_ipc] Drained stale message: {:?}", msg);
        }
    }

    pub fn current_frame(&self) -> u64 {
        self.current_frame.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Send a raw command and wait for a response.
    /// Serialized via cmd_lock to prevent response mismatch when multiple
    /// commands are sent concurrently.
    async fn send_command(&self, cmd: &str) -> Result<HWResponse, String> {
        // Hold the command lock to serialize send+receive pairs
        let _guard = self.cmd_lock.lock().await;

        let line = if cmd.ends_with('\n') { cmd.to_string() } else { format!("{}\n", cmd) };
        self.cmd_tx.send(line).await.map_err(|e| format!("Send failed: {}", e))?;

        // Wait for next non-event response
        let mut rx = self.resp_rx.lock().await;
        match tokio::time::timeout(
            tokio::time::Duration::from_millis(10000),
            rx.recv(),
        ).await {
            Ok(Some(resp)) => Ok(resp),
            Ok(None) => Err("Response channel closed".to_string()),
            Err(_) => Err("Timeout waiting for response".to_string()),
        }
    }

    // ── High-level commands ──

    pub async fn ping(&self) -> Result<(), String> {
        match self.send_command("PING").await? {
            HWResponse::Pong => Ok(()),
            other => Err(format!("Unexpected ping response: {:?}", other)),
        }
    }

    /// Save emulator state to a slot (0-15).
    pub async fn save_state(&self, slot: u32) -> Result<u64, String> {
        match self.send_command(&format!("SAVE_STATE {}", slot)).await? {
            HWResponse::Ok { args, .. } => {
                // args = "<slot> <frame>"
                let frame = args.split_whitespace().nth(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                Ok(frame)
            }
            HWResponse::Error { reason, .. } => Err(reason),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Load emulator state from a slot.
    pub async fn load_state(&self, slot: u32) -> Result<(), String> {
        match self.send_command(&format!("LOAD_STATE {}", slot)).await? {
            HWResponse::Ok { .. } => Ok(()),
            HWResponse::Error { reason, .. } => Err(reason),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Advance exactly one frame (when paused).
    pub async fn frame_advance(&self) -> Result<u64, String> {
        match self.send_command("FRAME_ADVANCE").await? {
            HWResponse::Ok { args, .. } => {
                let frame = args.trim().parse::<u64>().unwrap_or(0);
                Ok(frame)
            }
            HWResponse::Error { reason, .. } => Err(reason),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Set controller input for a port (0-3). Takes effect on next SI poll.
    pub async fn set_input(&self, port: u32, input: &HWPadInput) -> Result<(), String> {
        let cmd = format!("SET_INPUT {} {}", port, input.to_ipc_args());
        match self.send_command(&cmd).await? {
            HWResponse::Ok { .. } => Ok(()),
            HWResponse::Error { reason, .. } => Err(reason),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Read physical controller input directly from Dolphin (bypasses IPC override).
    pub async fn get_input(&self, port: u32) -> Result<HWPadInput, String> {
        let cmd = format!("GET_INPUT {}", port);
        match self.send_command(&cmd).await? {
            HWResponse::Ok { args, .. } => {
                // Parse: "<port> 0xBBBB <sx> <sy> <cx> <cy> <tl> <tr>"
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.len() < 8 {
                    return Err(format!("Invalid GET_INPUT response: {}", args));
                }
                let buttons = u16::from_str_radix(parts[1].trim_start_matches("0x").trim_start_matches("0X"), 16).unwrap_or(0);
                let stick_x = parts[2].parse::<i8>().unwrap_or(0);
                let stick_y = parts[3].parse::<i8>().unwrap_or(0);
                let cstick_x = parts[4].parse::<i8>().unwrap_or(0);
                let cstick_y = parts[5].parse::<i8>().unwrap_or(0);
                let trigger_l = parts[6].parse::<u8>().unwrap_or(0);
                let trigger_r = parts[7].parse::<u8>().unwrap_or(0);
                Ok(HWPadInput { buttons, stick_x, stick_y, cstick_x, cstick_y, trigger_l, trigger_r })
            }
            HWResponse::Error { reason, .. } => Err(reason),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Clear input override for a port, returning to physical controller.
    pub async fn clear_input(&self, port: u32) -> Result<(), String> {
        match self.send_command(&format!("CLEAR_INPUT {}", port)).await? {
            HWResponse::Ok { .. } => Ok(()),
            HWResponse::Error { reason, .. } => Err(reason),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Get current frame number.
    pub async fn get_frame(&self) -> Result<u64, String> {
        match self.send_command("GET_FRAME").await? {
            HWResponse::Frame(f) => Ok(f),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Pause emulation.
    pub async fn pause(&self) -> Result<(), String> {
        match self.send_command("PAUSE").await? {
            HWResponse::Ok { .. } => Ok(()),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Resume emulation.
    pub async fn resume(&self) -> Result<(), String> {
        match self.send_command("RESUME").await? {
            HWResponse::Ok { .. } => Ok(()),
            other => Err(format!("Unexpected: {:?}", other)),
        }
    }

    /// Wait for the next frame boundary event. Returns the frame number.
    pub async fn wait_frame(&self) -> Result<u64, String> {
        let mut rx = self.frame_rx.lock().await;
        match tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            rx.recv(),
        ).await {
            Ok(Some(f)) => Ok(f),
            Ok(None) => Err("Frame channel closed".to_string()),
            Err(_) => Err("No frame event within 100ms".to_string()),
        }
    }

    /// Perform a rollback: load state, replay N frames with corrected inputs.
    /// This is the core of our rollback netplay.
    ///
    /// Steps:
    /// 1. Pause emulation
    /// 2. Load state from `slot`
    /// 3. For each frame in `inputs`: set inputs, advance one frame
    /// 4. Resume emulation
    ///
    /// `inputs` is a vec of (frame, port0_input, port1_input) tuples.
    pub async fn rollback(
        &self,
        slot: u32,
        inputs: &[(u64, HWPadInput, HWPadInput)],
    ) -> Result<u64, String> {
        // Pause
        self.pause().await?;

        // Load the saved state
        self.load_state(slot).await?;

        // Replay frames with corrected inputs
        let mut last_frame = 0;
        for (frame, p1_input, p2_input) in inputs {
            self.set_input(0, p1_input).await?;
            self.set_input(1, p2_input).await?;
            last_frame = self.frame_advance().await?;
        }

        // Clear overrides and resume
        self.clear_input(0).await?;
        self.clear_input(1).await?;
        self.resume().await?;

        Ok(last_frame)
    }
}
