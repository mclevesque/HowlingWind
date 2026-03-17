//! Built-in crash and desync diagnostics for HowlingWind.
//!
//! Automatically logs critical events to a file that can be shared for debugging.
//! Creates `howlingwind-debug.log` next to the executable.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

/// Initialize the diagnostic log file.
pub fn init() {
    let log_path = get_log_path();
    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
    {
        Ok(file) => {
            *LOG_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
            log_info(&format!("HowlingWind diagnostics started"));
            log_info(&format!("Version: {}", env!("CARGO_PKG_VERSION")));
            log_info(&format!("OS: {}", std::env::consts::OS));
            log_info(&format!("Arch: {}", std::env::consts::ARCH));
            eprintln!("[diagnostics] Logging to {:?}", log_path);
        }
        Err(e) => {
            eprintln!("[diagnostics] Failed to create log file: {}", e);
        }
    }
}

fn get_log_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("howlingwind-debug.log");
        }
    }
    PathBuf::from("howlingwind-debug.log")
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let hours = (secs / 3600) % 24;
    let mins = (secs / 60) % 60;
    let s = secs % 60;
    let ms = now.subsec_millis();
    format!("{:02}:{:02}:{:02}.{:03}", hours, mins, s, ms)
}

fn write_log(level: &str, msg: &str) {
    let line = format!("[{}] [{}] {}\n", timestamp(), level, msg);

    // Write to file
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(ref mut file) = *guard {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }

    // Also write to stderr for console
    eprint!("{}", line);
}

pub fn log_info(msg: &str) {
    write_log("INFO", msg);
}

pub fn log_warn(msg: &str) {
    write_log("WARN", msg);
}

pub fn log_error(msg: &str) {
    write_log("ERROR", msg);
}

/// Log a desync event with full details.
pub fn log_desync(frame: u32, local_hash: u32, remote_hash: u32) {
    write_log("DESYNC", &format!(
        "Frame {} — local_hash=0x{:08X} remote_hash=0x{:08X}",
        frame, local_hash, remote_hash
    ));
}

/// Log a rollback event.
pub fn log_rollback(from_frame: u32, to_frame: u32, duration_ms: f64) {
    write_log("ROLLBACK", &format!(
        "Frames {}->{} (depth {}) took {:.2}ms",
        from_frame, to_frame, to_frame - from_frame, duration_ms
    ));
}

/// Log IPC connection status.
pub fn log_ipc(msg: &str) {
    write_log("IPC", msg);
}

/// Log network events.
pub fn log_net(msg: &str) {
    write_log("NET", msg);
}

/// Log a crash/panic with backtrace.
pub fn log_crash(msg: &str) {
    write_log("CRASH", msg);
    // Force flush
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(ref mut file) = *guard {
            let _ = file.flush();
        }
    }
}

/// Install a panic hook that logs crashes to the diagnostic file.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("{}", info);
        log_crash(&msg);
        default_hook(info);
    }));
}

/// Tauri command: get the log file path for the user.
#[tauri::command]
pub fn get_debug_log_path() -> String {
    get_log_path().to_string_lossy().to_string()
}

/// Tauri command: read the last N lines of the debug log.
#[tauri::command]
pub fn read_debug_log(lines: Option<u32>) -> Result<String, String> {
    let path = get_log_path();
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read log: {}", e))?;

    let max_lines = lines.unwrap_or(100) as usize;
    let all_lines: Vec<&str> = content.lines().collect();
    let start = if all_lines.len() > max_lines { all_lines.len() - max_lines } else { 0 };
    Ok(all_lines[start..].join("\n"))
}
