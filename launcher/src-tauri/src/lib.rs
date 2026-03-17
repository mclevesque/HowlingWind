use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};
use tauri::Manager;

mod netplay;
mod dolphin_mem;
mod rollback;
mod stun;
mod updater;
mod hw_ipc;
mod rollback_ipc;
mod diagnostics;

#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW, MoveWindow,
    SetParent, SetWindowLongW, ShowWindow, GWL_STYLE, SW_HIDE, SW_SHOW, WS_CAPTION,
    WS_CHILD, WS_POPUP, WS_THICKFRAME, WS_VISIBLE,
};

#[cfg(windows)]
#[derive(Clone, Copy)]
struct SendHwnd(HWND);

#[cfg(windows)]
unsafe impl Send for SendHwnd {}
#[cfg(windows)]
unsafe impl Sync for SendHwnd {}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GameInfo {
    pub id: String,       // e.g. "gnt4", "gntsp"
    pub name: String,     // e.g. "Naruto GNT4"
    pub game_id: String,  // Dolphin game ID e.g. "G4NJDA"
    pub iso_path: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppSettings {
    pub dolphin_path: String,
    pub iso_path: String,
    pub player_name: String,
    pub input_delay: u32,
    pub max_rollback: u32,
    #[serde(default)]
    pub selected_game: String, // "gnt4" or "gntsp"
    #[serde(default = "default_resolution")]
    pub resolution: u32, // 1=native, 2=2x, 3=3x, 4=4x
}

fn default_resolution() -> u32 { 2 }

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            dolphin_path: String::new(),
            iso_path: String::new(),
            player_name: "Player".to_string(),
            input_delay: 2,
            max_rollback: 7,
            selected_game: "gnt4".to_string(),
            resolution: 2,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ControllerInfo {
    pub index: u32,
    pub name: String,
    pub controller_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GCPadMapping {
    pub device: String,
    pub a: String,
    pub b: String,
    pub x: String,
    pub y: String,
    pub z: String,
    pub start: String,
    pub l: String,
    pub r: String,
    pub stick_up: String,
    pub stick_down: String,
    pub stick_left: String,
    pub stick_right: String,
    pub cstick_up: String,
    pub cstick_down: String,
    pub cstick_left: String,
    pub cstick_right: String,
    pub dpad_up: String,
    pub dpad_down: String,
    pub dpad_left: String,
    pub dpad_right: String,
}

impl Default for GCPadMapping {
    fn default() -> Self {
        Self {
            device: "DInput/0/Keyboard Mouse".to_string(),
            a: "X".to_string(),
            b: "Z".to_string(),
            x: "C".to_string(),
            y: "S".to_string(),
            z: "D".to_string(),
            start: "RETURN".to_string(),
            l: "Q".to_string(),
            r: "W".to_string(),
            stick_up: "UP".to_string(),
            stick_down: "DOWN".to_string(),
            stick_left: "LEFT".to_string(),
            stick_right: "RIGHT".to_string(),
            cstick_up: "I".to_string(),
            cstick_down: "K".to_string(),
            cstick_left: "J".to_string(),
            cstick_right: "L".to_string(),
            dpad_up: "T".to_string(),
            dpad_down: "G".to_string(),
            dpad_left: "F".to_string(),
            dpad_right: "H".to_string(),
        }
    }
}

struct DolphinState {
    process: Option<Child>,
    #[cfg(windows)]
    embedded_hwnd: Option<SendHwnd>,
    /// IPC client connected to our HowlingWind Dolphin fork (if using fork)
    ipc_client: Option<Arc<hw_ipc::HWClient>>,
}

fn settings_path(app: &tauri::AppHandle) -> PathBuf {
    let config_dir = app.path().app_config_dir()
        .unwrap_or_else(|_| std::env::current_exe().unwrap_or_default().parent()
            .unwrap_or(std::path::Path::new(".")).to_path_buf());
    fs::create_dir_all(&config_dir).ok();
    config_dir.join("settings.json")
}

fn auto_detect_paths() -> (String, String) {
    // Look for HowlingWind Dolphin fork FIRST, then fall back to stock Dolphin.
    let mut dolphin_candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // Priority 1: Our fork next to the launcher (release layout)
            dolphin_candidates.push(exe_dir.join("howlingwind-dolphin").join("Dolphin.exe"));

            // Priority 2: Walk up directories (dev mode + project layout)
            let mut dir = exe_dir.to_path_buf();
            for _ in 0..5 {
                // Check for fork (flat release layout)
                let fork_flat = dir.join("howlingwind-dolphin").join("Dolphin.exe");
                if fork_flat.exists() && !dolphin_candidates.contains(&fork_flat) {
                    dolphin_candidates.push(fork_flat);
                }
                // Check for fork (build output layout)
                let fork_build = dir.join("howlingwind-dolphin").join("Binary").join("x64").join("Release").join("Dolphin.exe");
                if fork_build.exists() {
                    dolphin_candidates.push(fork_build);
                }
                // Stock Dolphin as fallback (LOWER priority than fork)
                let stock = dir.join("dolphin").join("Dolphin-x64").join("Dolphin.exe");
                if stock.exists() {
                    dolphin_candidates.push(stock);
                }
                if !dir.pop() { break; }
            }
            // Fallback: stock Dolphin next to exe
            dolphin_candidates.push(exe_dir.join("dolphin").join("Dolphin-x64").join("Dolphin.exe"));
            dolphin_candidates.push(exe_dir.join("Dolphin-x64").join("Dolphin.exe"));
            dolphin_candidates.push(exe_dir.join("Dolphin.exe"));
        }
    }

    // Fallback: hardcoded project path
    // No hardcoded fallback — rely on relative path detection from exe location

    let dolphin = dolphin_candidates
        .iter()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    eprintln!("[auto_detect] Dolphin candidates: {:?}", dolphin_candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>());
    eprintln!("[auto_detect] Selected: {}", if dolphin.is_empty() { "NONE" } else { &dolphin });

    // Look for the ISO similarly
    let mut iso_candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // Search games/ folder for any .iso file (GNT4.iso, NGNT4.iso, etc.)
            let mut dir = exe_dir.to_path_buf();
            for _ in 0..5 {
                let games_dir = dir.join("games");
                if games_dir.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&games_dir) {
                        for entry in entries.flatten() {
                            let p = entry.path();
                            if let Some(ext) = p.extension() {
                                if ext.eq_ignore_ascii_case("iso") || ext.eq_ignore_ascii_case("gcm") {
                                    iso_candidates.push(p);
                                }
                            }
                        }
                    }
                    if !iso_candidates.is_empty() { break; }
                }
                if !dir.pop() { break; }
            }
            // Also check directly next to exe
            let games_dir = exe_dir.join("games");
            if games_dir.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&games_dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if let Some(ext) = p.extension() {
                            if ext.eq_ignore_ascii_case("iso") || ext.eq_ignore_ascii_case("gcm") {
                                if !iso_candidates.contains(&p) {
                                    iso_candidates.push(p);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // No hardcoded fallback — rely on relative path detection from exe location

    let iso = iso_candidates
        .iter()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    (dolphin, iso)
}

fn load_settings(app: &tauri::AppHandle) -> AppSettings {
    let path = settings_path(app);
    if path.exists() {
        let data = fs::read_to_string(&path).unwrap_or_default();
        let mut settings: AppSettings = serde_json::from_str(&data).unwrap_or_default();

        // ALWAYS re-detect Dolphin path to prefer our fork over stock
        // The fork has an IPC server for proper rollback; stock Dolphin does not.
        let (detected_dolphin, _) = auto_detect_paths();
        if !detected_dolphin.is_empty() && detected_dolphin != settings.dolphin_path {
            // Check if detected path is our fork (contains "howlingwind-dolphin")
            if detected_dolphin.contains("howlingwind-dolphin") {
                eprintln!("[settings] Overriding saved Dolphin path with fork: {}", detected_dolphin);
                settings.dolphin_path = detected_dolphin;
            }
        }

        let dolphin_missing = settings.dolphin_path.is_empty() || !PathBuf::from(&settings.dolphin_path).exists();
        let iso_missing = settings.iso_path.is_empty() || !PathBuf::from(&settings.iso_path).exists();
        if dolphin_missing || iso_missing {
            let (dolphin, iso) = auto_detect_paths();
            if dolphin_missing { settings.dolphin_path = dolphin; }
            if iso_missing { settings.iso_path = iso; }
        }
        settings
    } else {
        let (dolphin, iso) = auto_detect_paths();
        AppSettings {
            dolphin_path: dolphin,
            iso_path: iso,
            ..AppSettings::default()
        }
    }
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: AppSettings) -> Result<(), String> {
    let path = settings_path(&app);
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> AppSettings {
    load_settings(&app)
}

// ── Game scanning ──

fn read_game_id_from_iso(iso_path: &str) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(iso_path).ok()?;
    let mut buf = [0u8; 6];
    f.read_exact(&mut buf).ok()?;
    let id = String::from_utf8_lossy(&buf).to_string();
    // Validate it looks like a game ID (alphanumeric)
    if id.chars().all(|c| c.is_ascii_alphanumeric()) {
        Some(id)
    } else {
        None
    }
}

fn game_name_from_id(game_id: &str) -> String {
    match game_id {
        "G4NJDA" => "Naruto: Gekitou Ninja Taisen! 4".to_string(),
        "SG4JDA" => "Naruto: GNT Special".to_string(),
        _ => format!("Unknown ({})", game_id),
    }
}

fn game_short_id(game_id: &str) -> String {
    match game_id {
        "G4NJDA" => "gnt4".to_string(),
        "SG4JDA" => "gntsp".to_string(),
        _ => game_id.to_lowercase(),
    }
}

#[tauri::command]
fn get_local_ip() -> Result<String, String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| format!("Failed to bind socket: {}", e))?;
    // Connect to a public IP to determine our local network IP (no data sent)
    socket.connect("8.8.8.8:80")
        .map_err(|e| format!("Failed to determine local IP: {}", e))?;
    let local_addr = socket.local_addr()
        .map_err(|e| format!("Failed to get local addr: {}", e))?;
    Ok(local_addr.ip().to_string())
}

#[tauri::command]
fn scan_games() -> Vec<GameInfo> {
    let mut games: Vec<GameInfo> = Vec::new();
    let mut games_dirs: Vec<PathBuf> = Vec::new();

    // Check relative to exe
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let mut dir = exe_dir.to_path_buf();
            for _ in 0..5 {
                let candidate = dir.join("games");
                if candidate.is_dir() {
                    games_dirs.push(candidate);
                    break;
                }
                if !dir.pop() { break; }
            }
        }
    }

    // No hardcoded fallback — rely on relative path detection from exe location

    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for dir in games_dirs {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    let ext = ext.to_string_lossy().to_lowercase();
                    if ext == "iso" || ext == "gcm" || ext == "ciso" {
                        let path_str = path.to_string_lossy().to_string();
                        if let Some(gid) = read_game_id_from_iso(&path_str) {
                            if seen_ids.insert(gid.clone()) {
                                games.push(GameInfo {
                                    id: game_short_id(&gid),
                                    name: game_name_from_id(&gid),
                                    game_id: gid,
                                    iso_path: path_str,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    games
}

// ── Controller detection + input reading ──

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GamepadState {
    pub connected: bool,
    pub buttons: Vec<bool>,    // 16 buttons
    pub axes: Vec<f64>,        // 6 axes: LX, LY, RX, RY, LT, RT
    pub button_names: Vec<String>,  // Dolphin-format names for each pressed button/axis
}

#[cfg(windows)]
mod controllers {
    use super::{ControllerInfo, GamepadState};

    type XInputGetStateFn = unsafe extern "system" fn(u32, *mut XInputState) -> u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XInputGamepad {
        pub buttons: u16,
        pub left_trigger: u8,
        pub right_trigger: u8,
        pub thumb_lx: i16,
        pub thumb_ly: i16,
        pub thumb_rx: i16,
        pub thumb_ry: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XInputState {
        pub packet_number: u32,
        pub gamepad: XInputGamepad,
    }

    // XInput button masks
    const XINPUT_GAMEPAD_DPAD_UP: u16 = 0x0001;
    const XINPUT_GAMEPAD_DPAD_DOWN: u16 = 0x0002;
    const XINPUT_GAMEPAD_DPAD_LEFT: u16 = 0x0004;
    const XINPUT_GAMEPAD_DPAD_RIGHT: u16 = 0x0008;
    const XINPUT_GAMEPAD_START: u16 = 0x0010;
    const XINPUT_GAMEPAD_BACK: u16 = 0x0020;
    const XINPUT_GAMEPAD_LEFT_THUMB: u16 = 0x0040;
    const XINPUT_GAMEPAD_RIGHT_THUMB: u16 = 0x0080;
    const XINPUT_GAMEPAD_LEFT_SHOULDER: u16 = 0x0100;
    const XINPUT_GAMEPAD_RIGHT_SHOULDER: u16 = 0x0200;
    const XINPUT_GAMEPAD_A: u16 = 0x1000;
    const XINPUT_GAMEPAD_B: u16 = 0x2000;
    const XINPUT_GAMEPAD_X: u16 = 0x4000;
    const XINPUT_GAMEPAD_Y: u16 = 0x8000;

    fn load_xinput() -> Option<XInputGetStateFn> {
        let xinput_names = ["xinput1_4.dll", "xinput1_3.dll", "xinput9_1_0.dll"];
        for dll_name in &xinput_names {
            let wide: Vec<u16> = dll_name.encode_utf16().chain(std::iter::once(0)).collect();
            let handle = unsafe {
                windows_sys::Win32::System::LibraryLoader::LoadLibraryW(wide.as_ptr())
            };
            if !handle.is_null() {
                let proc_name = b"XInputGetState\0";
                let proc = unsafe {
                    windows_sys::Win32::System::LibraryLoader::GetProcAddress(handle, proc_name.as_ptr())
                };
                if let Some(p) = proc {
                    return Some(unsafe { std::mem::transmute(p) });
                }
            }
        }
        None
    }

    pub fn detect_controllers() -> Vec<ControllerInfo> {
        let mut controllers = Vec::new();
        if let Some(get_state) = load_xinput() {
            for i in 0..4u32 {
                let mut state = std::mem::MaybeUninit::<XInputState>::zeroed();
                let result = unsafe { get_state(i, state.as_mut_ptr()) };
                if result == 0 {
                    controllers.push(ControllerInfo {
                        index: i,
                        name: format!("Xbox Controller {}", i + 1),
                        controller_type: "xinput".to_string(),
                    });
                }
            }
        }
        controllers
    }

    pub fn read_gamepad(index: u32) -> GamepadState {
        let get_state = match load_xinput() {
            Some(f) => f,
            None => return GamepadState { connected: false, buttons: vec![], axes: vec![], button_names: vec![] },
        };

        let mut state = std::mem::MaybeUninit::<XInputState>::zeroed();
        let result = unsafe { get_state(index, state.as_mut_ptr()) };
        if result != 0 {
            return GamepadState { connected: false, buttons: vec![], axes: vec![], button_names: vec![] };
        }

        let state = unsafe { state.assume_init() };
        let gp = &state.gamepad;

        // Button order matches standard layout
        let button_masks: [(u16, &str); 16] = [
            (XINPUT_GAMEPAD_A, "Button A"),
            (XINPUT_GAMEPAD_B, "Button B"),
            (XINPUT_GAMEPAD_X, "Button X"),
            (XINPUT_GAMEPAD_Y, "Button Y"),
            (XINPUT_GAMEPAD_LEFT_SHOULDER, "Shoulder L"),
            (XINPUT_GAMEPAD_RIGHT_SHOULDER, "Shoulder R"),
            (0, "Trigger L"),  // handled via analog
            (0, "Trigger R"),  // handled via analog
            (XINPUT_GAMEPAD_BACK, "Back"),
            (XINPUT_GAMEPAD_START, "Start"),
            (XINPUT_GAMEPAD_LEFT_THUMB, "Thumb L"),
            (XINPUT_GAMEPAD_RIGHT_THUMB, "Thumb R"),
            (XINPUT_GAMEPAD_DPAD_UP, "Pad N"),
            (XINPUT_GAMEPAD_DPAD_DOWN, "Pad S"),
            (XINPUT_GAMEPAD_DPAD_LEFT, "Pad W"),
            (XINPUT_GAMEPAD_DPAD_RIGHT, "Pad E"),
        ];

        let mut buttons = Vec::with_capacity(16);
        let mut pressed_names = Vec::new();
        let deadzone = 8000i16;
        let trigger_threshold = 30u8;

        for (i, (mask, name)) in button_masks.iter().enumerate() {
            let pressed = if i == 6 {
                gp.left_trigger > trigger_threshold
            } else if i == 7 {
                gp.right_trigger > trigger_threshold
            } else {
                gp.buttons & mask != 0
            };
            buttons.push(pressed);
            if pressed {
                pressed_names.push(name.to_string());
            }
        }

        // Axes: normalize to -1.0..1.0
        let norm = |v: i16| -> f64 { v as f64 / 32767.0 };
        let lx = norm(gp.thumb_lx);
        let ly = norm(gp.thumb_ly);
        let rx = norm(gp.thumb_rx);
        let ry = norm(gp.thumb_ry);
        let lt = gp.left_trigger as f64 / 255.0;
        let rt = gp.right_trigger as f64 / 255.0;

        // Report axis directions as pressed names
        if gp.thumb_lx > deadzone { pressed_names.push("Left X+".to_string()); }
        if gp.thumb_lx < -deadzone { pressed_names.push("Left X-".to_string()); }
        if gp.thumb_ly > deadzone { pressed_names.push("Left Y+".to_string()); }
        if gp.thumb_ly < -deadzone { pressed_names.push("Left Y-".to_string()); }
        if gp.thumb_rx > deadzone { pressed_names.push("Right X+".to_string()); }
        if gp.thumb_rx < -deadzone { pressed_names.push("Right X-".to_string()); }
        if gp.thumb_ry > deadzone { pressed_names.push("Right Y+".to_string()); }
        if gp.thumb_ry < -deadzone { pressed_names.push("Right Y-".to_string()); }

        GamepadState {
            connected: true,
            buttons,
            axes: vec![lx, ly, rx, ry, lt, rt],
            button_names: pressed_names,
        }
    }
}

#[cfg(not(windows))]
mod controllers {
    use super::{ControllerInfo, GamepadState};
    pub fn detect_controllers() -> Vec<ControllerInfo> { Vec::new() }
    pub fn read_gamepad(_index: u32) -> GamepadState {
        GamepadState { connected: false, buttons: vec![], axes: vec![], button_names: vec![] }
    }
}

#[tauri::command]
fn get_controllers() -> Vec<ControllerInfo> {
    controllers::detect_controllers()
}

#[tauri::command]
fn poll_gamepad(index: u32) -> GamepadState {
    controllers::read_gamepad(index)
}

// ── GCPad config read/write ──

fn gcpad_ini_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(appdata)
        .join("Dolphin Emulator")
        .join("Config")
        .join("GCPadNew.ini")
}

/// Get the portable Dolphin config path (next to the Dolphin exe)
fn gcpad_ini_path_portable(dolphin_path: &str) -> Option<PathBuf> {
    let dolphin = PathBuf::from(dolphin_path);
    dolphin.parent().map(|dir| dir.join("User").join("Config").join("GCPadNew.ini"))
}

/// Write default XInput GCPad mapping to all relevant Dolphin config locations.
/// Called automatically before launching Dolphin if a controller is detected.
/// Configure Dolphin to use a GC adapter (Wii U / Mayflash in Wii U mode).
/// This writes to Dolphin.ini to enable the SI adapter and to GCPadNew.ini
/// to set port 1 to use the adapter instead of SDL/emulated input.
fn configure_gc_adapter(dolphin_path: &str) {
    let dolphin = PathBuf::from(dolphin_path);

    // Find Dolphin.ini (portable or user dir)
    let ini_paths: Vec<PathBuf> = [
        dolphin.parent().map(|d| d.join("portable.ini")).filter(|p| p.exists())
            .and_then(|_| dolphin.parent().map(|d| d.join("User").join("Config").join("Dolphin.ini"))),
        dolphin.parent().map(|d| d.join("User").join("Config").join("Dolphin.ini")),
        {
            let appdata = std::env::var("APPDATA").unwrap_or_default();
            if appdata.is_empty() { None } else { Some(PathBuf::from(appdata).join("Dolphin Emulator").join("Config").join("Dolphin.ini")) }
        },
    ].into_iter().flatten().collect();

    for ini_path in &ini_paths {
        if let Some(parent) = ini_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut content = fs::read_to_string(ini_path).unwrap_or_default();

        // Ensure [Core] section has SIDevice0 = 12 (GC Adapter)
        if !content.contains("SIDevice0 = 12") {
            if content.contains("[Core]") {
                content = content.replace("[Core]", "[Core]\nSIDevice0 = 12");
            } else {
                content.push_str("\n[Core]\nSIDevice0 = 12\n");
            }
            let _ = fs::write(ini_path, &content);
            eprintln!("[gcpad] Configured GC adapter in {:?}", ini_path);
        }
    }
}

fn ensure_gcpad_config(dolphin_path: &str) {
    // Try to read existing device name from any GCPadNew.ini
    let existing_device = {
        let mut device = String::new();
        let paths_to_check = [
            Some(gcpad_ini_path()),
            gcpad_ini_path_portable(dolphin_path),
        ];
        for path in paths_to_check.iter().flatten() {
            if let Ok(content) = fs::read_to_string(path) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("Device") {
                        if let Some((_, val)) = trimmed.split_once('=') {
                            let val = val.trim();
                            if val.contains("SDL") || val.contains("XInput") {
                                device = val.to_string();
                                break;
                            }
                        }
                    }
                }
                if !device.is_empty() { break; }
            }
        }
        device
    };

    // Use existing SDL device, or try to auto-detect.
    // Dolphin uses SDL for controller input. The device name varies by controller:
    //   Xbox: "SDL/0/Xbox One S Controller", "SDL/0/Xbox 360 Controller"
    //   PS4/PS5: "SDL/0/PS4 Controller", "SDL/0/DualSense Wireless Controller"
    //   GC Adapter: Uses "GameCube Adapter" mode in Dolphin (separate from GCPad)
    //   Generic: "SDL/0/USB Gamepad" etc.
    let device_name = if !existing_device.is_empty() {
        existing_device
    } else {
        // Check if any controller is connected via XInput
        let controllers = controllers::detect_controllers();
        if controllers.is_empty() {
            // No XInput controller — might be GC adapter or DInput device.
            // Try common SDL names that Dolphin recognizes.
            // If none work, Dolphin will fall back to keyboard.
            // GC adapters are handled separately by Dolphin's "GameCube Adapter" setting.
            eprintln!("[gcpad] No XInput controller found. Trying SDL auto-detect...");

            // Check if a GC adapter is connected (Zadig/WUP driver)
            // If so, configure Dolphin to use the GC Adapter directly
            configure_gc_adapter(dolphin_path);
            return; // Don't write GCPad config — GC adapter bypasses it
        }
        // XInput controller found — use the controller name from detection
        // Dolphin's SDL backend prefixes with "SDL/0/"
        let ctrl_name = &controllers[0].name;
        format!("SDL/0/{}", ctrl_name)
    };

    // SDL button names: A=South, B=East, X=West, Y=North
    let sdl_mapping = GCPadMapping {
        device: device_name,
        a: "Button S".to_string(),         // A (south) → GC A
        b: "Button E".to_string(),         // B (east) → GC B
        x: "Button W".to_string(),         // X (west) → GC X
        y: "Button N".to_string(),         // Y (north) → GC Y
        z: "Trigger R".to_string(),        // RT → GC Z
        start: "Start".to_string(),
        l: "Trigger L".to_string(),        // LT → GC L
        r: "Shoulder R".to_string(),       // RB → GC R
        stick_up: "Left Y+".to_string(),
        stick_down: "Left Y-".to_string(),
        stick_left: "Left X-".to_string(),
        stick_right: "Left X+".to_string(),
        cstick_up: "Right Y+".to_string(),
        cstick_down: "Right Y-".to_string(),
        cstick_left: "Right X-".to_string(),
        cstick_right: "Right X+".to_string(),
        dpad_up: "Pad N".to_string(),
        dpad_down: "Pad S".to_string(),
        dpad_left: "Pad W".to_string(),
        dpad_right: "Pad E".to_string(),
    };

    let section = format_gcpad_section(1, &sdl_mapping);
    // Keep pads 2-4 as keyboard defaults
    let mut output = section;
    for p in 2..=4u32 {
        output.push_str(&format!("\n[GCPad{}]\nDevice = DInput/0/Keyboard Mouse", p));
    }
    output.push('\n');

    // Write to all possible config locations
    let paths_to_write: Vec<PathBuf> = [
        Some(gcpad_ini_path()),
        gcpad_ini_path_portable(dolphin_path),
    ]
    .into_iter()
    .flatten()
    .collect();

    for path in paths_to_write {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Only write if no existing config at all
        let should_write = !path.exists();

        if should_write {
            let _ = fs::write(&path, &output);
        }
    }
}

fn parse_gcpad_section(lines: &[&str]) -> GCPadMapping {
    let mut mapping = GCPadMapping::default();

    for line in lines {
        let line = line.trim();
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('`');
            match key {
                "Device" => mapping.device = value.to_string(),
                "Buttons/A" => mapping.a = value.to_string(),
                "Buttons/B" => mapping.b = value.to_string(),
                "Buttons/X" => mapping.x = value.to_string(),
                "Buttons/Y" => mapping.y = value.to_string(),
                "Buttons/Z" => mapping.z = value.to_string(),
                "Buttons/Start" => mapping.start = value.to_string(),
                "Triggers/L" => mapping.l = value.to_string(),
                "Triggers/R" => mapping.r = value.to_string(),
                "Main Stick/Up" => mapping.stick_up = value.to_string(),
                "Main Stick/Down" => mapping.stick_down = value.to_string(),
                "Main Stick/Left" => mapping.stick_left = value.to_string(),
                "Main Stick/Right" => mapping.stick_right = value.to_string(),
                "C-Stick/Up" => mapping.cstick_up = value.to_string(),
                "C-Stick/Down" => mapping.cstick_down = value.to_string(),
                "C-Stick/Left" => mapping.cstick_left = value.to_string(),
                "C-Stick/Right" => mapping.cstick_right = value.to_string(),
                "D-Pad/Up" => mapping.dpad_up = value.to_string(),
                "D-Pad/Down" => mapping.dpad_down = value.to_string(),
                "D-Pad/Left" => mapping.dpad_left = value.to_string(),
                "D-Pad/Right" => mapping.dpad_right = value.to_string(),
                _ => {}
            }
        }
    }

    mapping
}

fn format_gcpad_section(pad_num: u32, mapping: &GCPadMapping) -> String {
    let wrap = |s: &str| {
        if s.is_empty() {
            String::new()
        } else {
            format!("`{}`", s)
        }
    };

    format!(
        "[GCPad{}]\n\
         Device = {}\n\
         Buttons/A = {}\n\
         Buttons/B = {}\n\
         Buttons/X = {}\n\
         Buttons/Y = {}\n\
         Buttons/Z = {}\n\
         Buttons/Start = {}\n\
         Main Stick/Up = {}\n\
         Main Stick/Down = {}\n\
         Main Stick/Left = {}\n\
         Main Stick/Right = {}\n\
         Main Stick/Modifier = `Shift`\n\
         Main Stick/Calibration = 100.00 141.42 100.00 141.42 100.00 141.42 100.00 141.42\n\
         C-Stick/Up = {}\n\
         C-Stick/Down = {}\n\
         C-Stick/Left = {}\n\
         C-Stick/Right = {}\n\
         C-Stick/Modifier = `Ctrl`\n\
         C-Stick/Calibration = 100.00 141.42 100.00 141.42 100.00 141.42 100.00 141.42\n\
         Triggers/L = {}\n\
         Triggers/R = {}\n\
         D-Pad/Up = {}\n\
         D-Pad/Down = {}\n\
         D-Pad/Left = {}\n\
         D-Pad/Right = {}",
        pad_num,
        mapping.device,
        wrap(&mapping.a),
        wrap(&mapping.b),
        wrap(&mapping.x),
        wrap(&mapping.y),
        wrap(&mapping.z),
        wrap(&mapping.start),
        wrap(&mapping.stick_up),
        wrap(&mapping.stick_down),
        wrap(&mapping.stick_left),
        wrap(&mapping.stick_right),
        wrap(&mapping.cstick_up),
        wrap(&mapping.cstick_down),
        wrap(&mapping.cstick_left),
        wrap(&mapping.cstick_right),
        wrap(&mapping.l),
        wrap(&mapping.r),
        wrap(&mapping.dpad_up),
        wrap(&mapping.dpad_down),
        wrap(&mapping.dpad_left),
        wrap(&mapping.dpad_right),
    )
}

#[tauri::command]
fn get_gcpad_mapping(pad: u32) -> GCPadMapping {
    let path = gcpad_ini_path();
    if !path.exists() {
        return GCPadMapping::default();
    }

    let content = fs::read_to_string(&path).unwrap_or_default();
    let section_header = format!("[GCPad{}]", pad);
    let lines: Vec<&str> = content.lines().collect();

    let mut section_lines = Vec::new();
    let mut in_section = false;

    for line in &lines {
        if line.starts_with('[') {
            if in_section {
                break;
            }
            if line.trim() == section_header {
                in_section = true;
                continue;
            }
        }
        if in_section {
            section_lines.push(*line);
        }
    }

    if section_lines.is_empty() {
        GCPadMapping::default()
    } else {
        parse_gcpad_section(&section_lines)
    }
}

#[tauri::command]
fn save_gcpad_mapping(pad: u32, mapping: GCPadMapping) -> Result<(), String> {
    let path = gcpad_ini_path();

    // Read existing content or start fresh
    let content = if path.exists() {
        fs::read_to_string(&path).unwrap_or_default()
    } else {
        String::new()
    };

    // Parse all sections, replacing the target pad
    let mut sections: Vec<(u32, String)> = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut current_section: Option<u32> = None;
    let mut current_lines: Vec<String> = Vec::new();

    for line in &lines {
        if line.starts_with("[GCPad") && line.ends_with(']') {
            // Save previous section
            if let Some(num) = current_section {
                sections.push((num, current_lines.join("\n")));
            }
            // Parse section number
            let num_str = line.trim_start_matches("[GCPad").trim_end_matches(']');
            current_section = num_str.parse().ok();
            current_lines = Vec::new();
        } else if current_section.is_some() {
            current_lines.push(line.to_string());
        }
    }
    // Don't forget last section
    if let Some(num) = current_section {
        sections.push((num, current_lines.join("\n")));
    }

    // Build output: replace the target pad, keep others
    let new_section = format_gcpad_section(pad, &mapping);
    let mut found = false;
    let mut output_parts: Vec<String> = Vec::new();

    for (num, _) in &sections {
        if *num == pad {
            output_parts.push(new_section.clone());
            found = true;
        } else {
            // Re-read original section from content
            let header = format!("[GCPad{}]", num);
            let start = content.find(&header).unwrap_or(0);
            let end = content[start + header.len()..]
                .find("\n[GCPad")
                .map(|i| start + header.len() + i)
                .unwrap_or(content.len());
            output_parts.push(content[start..end].trim_end().to_string());
        }
    }

    if !found {
        output_parts.push(new_section);
    }

    // Ensure all 4 pads exist
    for p in 1..=4u32 {
        if !output_parts.iter().any(|s| s.contains(&format!("[GCPad{}]", p))) {
            output_parts.push(format!("[GCPad{}]\nDevice = DInput/0/Keyboard Mouse", p));
        }
    }

    // Sort sections by pad number
    output_parts.sort_by_key(|s| {
        s.lines()
            .next()
            .and_then(|l| l.trim_start_matches("[GCPad").trim_end_matches(']').parse::<u32>().ok())
            .unwrap_or(0)
    });

    let final_output = output_parts.join("\n") + "\n";

    // Ensure directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    fs::write(&path, final_output).map_err(|e| format!("Failed to write GCPadNew.ini: {}", e))?;
    Ok(())
}

/// Open Dolphin's controller configuration UI (launches Dolphin without a game).
#[tauri::command]
fn open_dolphin_config(app: tauri::AppHandle) -> Result<(), String> {
    let settings = load_settings(&app);
    if settings.dolphin_path.is_empty() {
        return Err("Dolphin path not set.".to_string());
    }
    if !PathBuf::from(&settings.dolphin_path).exists() {
        return Err(format!("Dolphin not found at: {}", settings.dolphin_path));
    }
    std::process::Command::new(&settings.dolphin_path)
        .spawn()
        .map_err(|e| format!("Failed to launch Dolphin: {}", e))?;
    Ok(())
}

/// Get all controller devices that Dolphin has seen, by reading:
/// 1. GCPadNew.ini (previously configured devices)
/// 2. Dolphin's log file (detected devices from last run)
/// 3. XInput detection (live check)
#[tauri::command]
fn get_dolphin_devices(app: tauri::AppHandle) -> Vec<String> {
    let mut devices: Vec<String> = Vec::new();

    // Always include keyboard
    devices.push("DInput/0/Keyboard Mouse".to_string());

    // Read devices from existing GCPadNew.ini
    let ini_path = gcpad_ini_path();
    if ini_path.exists() {
        if let Ok(content) = fs::read_to_string(&ini_path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("Device") {
                    if let Some((_, val)) = trimmed.split_once('=') {
                        let val = val.trim().to_string();
                        if !val.is_empty() && !devices.contains(&val) {
                            devices.push(val);
                        }
                    }
                }
            }
        }
    }

    // Also check portable Dolphin config
    let settings = load_settings(&app);
    if !settings.dolphin_path.is_empty() {
        if let Some(portable_path) = gcpad_ini_path_portable(&settings.dolphin_path) {
            if portable_path.exists() {
                if let Ok(content) = fs::read_to_string(&portable_path) {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("Device") {
                            if let Some((_, val)) = trimmed.split_once('=') {
                                let val = val.trim().to_string();
                                if !val.is_empty() && !devices.contains(&val) {
                                    devices.push(val);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Read Dolphin's log for device detection lines
        // Dolphin logs: "Added device: SDL/0/Xbox One S Controller"
        let dolphin_dir = PathBuf::from(&settings.dolphin_path).parent().map(|p| p.to_path_buf());
        let log_paths = [
            // Portable log
            dolphin_dir.as_ref().map(|d| d.join("User").join("Logs").join("dolphin.log")),
            // AppData log
            Some(PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
                .join("Dolphin Emulator")
                .join("Logs")
                .join("dolphin.log")),
        ];

        for log_path in log_paths.iter().flatten() {
            if log_path.exists() {
                if let Ok(content) = fs::read_to_string(log_path) {
                    for line in content.lines() {
                        // Dolphin logs device additions
                        if let Some(dev) = line.strip_suffix("").and_then(|_| {
                            if line.contains("Added device:") {
                                line.split("Added device:").nth(1).map(|s| s.trim().to_string())
                            } else {
                                None
                            }
                        }) {
                            if !dev.is_empty() && !devices.contains(&dev) {
                                devices.push(dev);
                            }
                        }
                    }
                }
            }
        }
    }

    // Add XInput devices if detected
    let xinput_controllers = controllers::detect_controllers();
    for ctrl in &xinput_controllers {
        let dev = format!("XInput/{}/Gamepad", ctrl.index);
        if !devices.contains(&dev) {
            devices.push(dev);
        }
    }

    devices
}

// ── Gecko Codes (auto-unlock everything per game) ──

const GNT4_GAME_ID: &str = "G4NJDA";
const GNTSP_GAME_ID: &str = "SG4JDA";

fn gecko_codes_for_game(game_id: &str) -> Option<String> {
    match game_id {
        "G4NJDA" => Some(
            "[Gecko_Enabled]\n\
             $Unlock Everything\n\
             $Skip Intro Videos\n\
             \n\
             [Gecko]\n\
             $Unlock Everything\n\
             C200CA80 00000012\n\
             3860FFFF 3FC08022\n\
             907E3258 907E325C\n\
             907E3260 907E3264\n\
             907E3268 907E326C\n\
             907E3270 907E3274\n\
             907E3278 907E327C\n\
             907E3280 907E3284\n\
             907E32FC 907E3300\n\
             907E3304 907E3308\n\
             907E330C 907E3310\n\
             907E3314 907E3318\n\
             907E331C 907E3320\n\
             907E3324 907E3328\n\
             907E332C 907E3330\n\
             907E3334 907E3338\n\
             907E333C 3FE00002\n\
             387FFF03 907E32E8\n\
             38600000 00000000\n\
             $Skip Intro Videos\n\
             0400CB14 60000000\n\
             0400CB28 60000000\n\
             0400CB3C 60000000\n".to_string()
        ),
        "SG4JDA" => Some(
            "[Gecko_Enabled]\n\
             $Unlock All Characters\n\
             $Skip Intro Cutscenes\n\
             \n\
             [Gecko]\n\
             $Unlock All Characters\n\
             043e896c fecbfebf\n\
             043e8970 7f901f01\n\
             $Skip Intro Cutscenes\n\
             C21633E0 00000003\n\
             2C12000b 40820008\n\
             3A400001 9421FFF0\n\
             60000000 00000000\n".to_string()
        ),
        _ => None,
    }
}

/// Ensure Gecko codes are installed for the given game.
/// Both players must have the same codes for netplay sync.
fn ensure_gecko_codes(dolphin_path: &str, game_id: &str) {
    let gecko_ini = match gecko_codes_for_game(game_id) {
        Some(codes) => codes,
        None => return,
    };

    // Dolphin reads game settings from multiple locations
    let ini_name = format!("{}.ini", game_id);

    let mut paths: Vec<PathBuf> = Vec::new();

    // AppData location
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    if !appdata.is_empty() {
        paths.push(PathBuf::from(&appdata).join("Dolphin Emulator").join("GameSettings").join(&ini_name));
    }

    // Portable Dolphin location (next to exe)
    let dolphin = PathBuf::from(dolphin_path);
    if let Some(dir) = dolphin.parent() {
        paths.push(dir.join("User").join("GameSettings").join(&ini_name));
    }

    for path in paths {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Always write — these are our standard codes that both players need
        let _ = fs::write(&path, &gecko_ini);
    }

    // Also ensure cheats are enabled in Dolphin.ini
    let dolphin_ini_paths: Vec<PathBuf> = [
        Some(PathBuf::from(&appdata).join("Dolphin Emulator").join("Config").join("Dolphin.ini")),
        dolphin.parent().map(|d| d.join("User").join("Config").join("Dolphin.ini")),
    ]
    .into_iter()
    .flatten()
    .collect();

    for ini_path in dolphin_ini_paths {
        if ini_path.exists() {
            if let Ok(content) = fs::read_to_string(&ini_path) {
                if !content.contains("EnableCheats = True") {
                    // Add or replace EnableCheats setting
                    let new_content = if content.contains("EnableCheats") {
                        content.replace("EnableCheats = False", "EnableCheats = True")
                    } else if content.contains("[Core]") {
                        content.replace("[Core]", "[Core]\nEnableCheats = True")
                    } else {
                        format!("[Core]\nEnableCheats = True\n{}", content)
                    };
                    let _ = fs::write(&ini_path, new_content);
                }
            }
        } else {
            // Create Dolphin.ini with cheats enabled + sensible defaults
            // Preserve GC adapter on Port 2 (SIDevice1=12) if user has one
            if let Some(parent) = ini_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            // Copy from AppData Dolphin.ini if it exists (preserves user's controller/device settings)
            let appdata_ini = PathBuf::from(&appdata).join("Dolphin Emulator").join("Config").join("Dolphin.ini");
            if appdata_ini.exists() && appdata_ini != ini_path {
                if let Ok(content) = fs::read_to_string(&appdata_ini) {
                    let new_content = if content.contains("EnableCheats = True") {
                        content
                    } else if content.contains("EnableCheats") {
                        content.replace("EnableCheats = False", "EnableCheats = True")
                    } else if content.contains("[Core]") {
                        content.replace("[Core]", "[Core]\nEnableCheats = True")
                    } else {
                        format!("[Core]\nEnableCheats = True\n{}", content)
                    };
                    let _ = fs::write(&ini_path, new_content);
                    continue;
                }
            }
            // Fallback: minimal config with GC adapter on Port 2
            let _ = fs::write(&ini_path, "[Core]\nEnableCheats = True\nSIDevice0 = 6\nSIDevice1 = 12\nSIDevice2 = 0\nSIDevice3 = 0\n");
        }
    }
}

// ── Graphics settings ──

fn set_dolphin_resolution(dolphin_path: &str, resolution: u32) {
    let appdata = std::env::var("APPDATA").unwrap_or_default();

    let mut gfx_paths: Vec<PathBuf> = Vec::new();
    if !appdata.is_empty() {
        gfx_paths.push(PathBuf::from(&appdata).join("Dolphin Emulator").join("Config").join("GFX.ini"));
    }
    let dolphin = PathBuf::from(dolphin_path);
    if let Some(dir) = dolphin.parent() {
        gfx_paths.push(dir.join("User").join("Config").join("GFX.ini"));
    }

    for gfx_path in gfx_paths {
        if let Some(parent) = gfx_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let content = if gfx_path.exists() {
            fs::read_to_string(&gfx_path).unwrap_or_default()
        } else {
            String::new()
        };

        let efb_scale_line = format!("InternalResolution = {}", resolution);

        let new_content = if content.contains("InternalResolution") {
            // Replace existing line
            let mut result = String::new();
            for line in content.lines() {
                if line.trim().starts_with("InternalResolution") {
                    result.push_str(&efb_scale_line);
                } else {
                    result.push_str(line);
                }
                result.push('\n');
            }
            result
        } else if content.contains("[Settings]") {
            content.replace("[Settings]", &format!("[Settings]\n{}", efb_scale_line))
        } else {
            format!("[Settings]\n{}\n{}", efb_scale_line, content)
        };

        let _ = fs::write(&gfx_path, new_content);
    }
}

// ── Window management ──

#[cfg(windows)]
fn get_window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let actual = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if actual > 0 {
            String::from_utf16_lossy(&buf[..actual as usize])
        } else {
            String::new()
        }
    }
}

#[cfg(windows)]
fn find_dolphin_render_window() -> Option<HWND> {
    use std::cell::Cell;

    thread_local! {
        static RESULT: Cell<HWND> = Cell::new(ptr::null_mut());
    }

    RESULT.set(ptr::null_mut());

    unsafe extern "system" fn callback(hwnd: HWND, _: LPARAM) -> BOOL {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if style & WS_VISIBLE != 0 {
            let title = get_window_title(hwnd);
            if title.contains("Dolphin") && (title.contains("Direct3D") || title.contains("OpenGL") || title.contains("Vulkan") || title.contains("GNT") || title.contains("NARUTO")) {
                RESULT.set(hwnd);
                return 0;
            }
        }
        1
    }

    unsafe { EnumWindows(Some(callback), 0) };

    let result = RESULT.get();
    if !result.is_null() { Some(result) } else { None }
}

/// Find ANY window with "Dolphin" in the title (visible or not).
#[cfg(windows)]
fn find_any_dolphin_window() -> Option<HWND> {
    use std::cell::Cell;

    thread_local! {
        static RESULT2: Cell<HWND> = Cell::new(ptr::null_mut());
    }

    RESULT2.set(ptr::null_mut());

    unsafe extern "system" fn callback(hwnd: HWND, _: LPARAM) -> BOOL {
        let title = get_window_title(hwnd);
        if title.contains("Dolphin") && (title.contains("Direct3D") || title.contains("OpenGL") || title.contains("Vulkan") || title.contains("GNT") || title.contains("NARUTO")) {
            RESULT2.set(hwnd);
            return 0;
        }
        1
    }

    unsafe { EnumWindows(Some(callback), 0) };

    let result = RESULT2.get();
    if !result.is_null() { Some(result) } else { None }
}

/// Aggressively poll for Dolphin's window. Immediately hides it when found to prevent flash.
#[cfg(windows)]
fn find_and_hide_dolphin_window(max_attempts: u32) -> Option<HWND> {
    for _ in 0..max_attempts {
        if let Some(hwnd) = find_dolphin_render_window() {
            // Immediately hide to prevent visual flash
            unsafe { ShowWindow(hwnd, SW_HIDE); }
            return Some(hwnd);
        }
        if let Some(hwnd) = find_any_dolphin_window() {
            unsafe { ShowWindow(hwnd, SW_HIDE); }
            return Some(hwnd);
        }
        // Poll fast — 100ms intervals to catch the window ASAP
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    None
}

#[cfg(windows)]
fn embed_window(child_hwnd: HWND, parent_hwnd: HWND, width: i32, height: i32) {
    unsafe {
        // First hide the Dolphin window to prevent flash
        ShowWindow(child_hwnd, SW_HIDE);

        // Strip window chrome and make it a child
        let style = GetWindowLongW(child_hwnd, GWL_STYLE) as u32;
        let new_style = (style & !(WS_CAPTION | WS_THICKFRAME | WS_POPUP)) | WS_CHILD;
        SetWindowLongW(child_hwnd, GWL_STYLE, new_style as i32);

        // Reparent into HowlingWind
        SetParent(child_hwnd, parent_hwnd);
        MoveWindow(child_hwnd, 0, 0, width, height, 1);

        // Now show it embedded
        ShowWindow(child_hwnd, SW_SHOW);
    }
}

// ── Dolphin process management ──

/// Spawn Dolphin with its window initially hidden using Windows STARTUPINFO.
#[cfg(windows)]
fn spawn_dolphin_hidden(dolphin_path: &str, iso_path: &str, mode: &str) -> Result<Child, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // CREATE_NO_WINDOW doesn't apply to GUI apps, but we can use
    // STARTF_USESHOWWINDOW with SW_HIDE via the raw command
    let mut cmd = Command::new(dolphin_path);
    cmd.arg("--batch")
        .arg("--exec")
        .arg(iso_path);

    // DETACHED_PROCESS = 0x00000008 - detach from parent console
    // without restricting USB/device access (CREATE_NO_WINDOW breaks GC adapter)
    cmd.creation_flags(0x00000008);

    cmd.spawn().map_err(|e| format!("Failed to launch Dolphin: {}", e))
}

#[cfg(not(windows))]
fn spawn_dolphin_hidden(dolphin_path: &str, iso_path: &str, _mode: &str) -> Result<Child, String> {
    use std::process::Command;
    let mut cmd = Command::new(dolphin_path);
    cmd.arg("--batch").arg("--exec").arg(iso_path);
    cmd.spawn().map_err(|e| format!("Failed to launch Dolphin: {}", e))
}

#[tauri::command]
fn launch_dolphin(
    app: tauri::AppHandle,
    mode: String,
    iso_override: Option<String>,
    state: tauri::State<'_, Arc<Mutex<DolphinState>>>,
) -> Result<(), String> {
    let settings = load_settings(&app);

    diagnostics::log_info(&format!("launch_dolphin: mode={}, dolphin_path={}, iso_path={}",
        mode, settings.dolphin_path, settings.iso_path));

    if settings.dolphin_path.is_empty() {
        return Err("Dolphin path not set. Go to Settings to configure it.".to_string());
    }

    // Use iso_override if provided (from game selector), otherwise fall back to settings
    let iso_path = iso_override.unwrap_or_else(|| settings.iso_path.clone());

    if iso_path.is_empty() {
        return Err("No ISO selected. Pick a game or set a path in Settings.".to_string());
    }
    if !PathBuf::from(&settings.dolphin_path).exists() {
        return Err(format!("Dolphin not found at: {}", settings.dolphin_path));
    }
    if !PathBuf::from(&iso_path).exists() {
        return Err(format!("ISO not found at: {}", iso_path));
    }

    // Detect which game this ISO is so we can apply the right Gecko codes
    let detected_game_id = read_game_id_from_iso(&iso_path)
        .unwrap_or_else(|| GNT4_GAME_ID.to_string());

    // Kill any existing Dolphin process
    {
        let mut ds = state.lock().map_err(|e| e.to_string())?;
        if let Some(ref mut proc) = ds.process {
            let _ = proc.kill();
        }
        ds.process = None;
        #[cfg(windows)]
        { ds.embedded_hwnd = None; }
    }

    // Auto-configure controller mapping in Dolphin before launch
    ensure_gcpad_config(&settings.dolphin_path);

    // Install Gecko codes for the detected game
    ensure_gecko_codes(&settings.dolphin_path, &detected_game_id);

    // Apply resolution setting
    set_dolphin_resolution(&settings.dolphin_path, settings.resolution);

    let child = spawn_dolphin_hidden(&settings.dolphin_path, &iso_path, &mode)?;

    {
        let mut ds = state.lock().map_err(|e| e.to_string())?;
        ds.process = Some(child);
    }

    // Connect to our HowlingWind Dolphin fork's IPC server (background)
    {
        let state_ipc = Arc::clone(state.inner());
        let app_ipc = app.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime for IPC connect");
            rt.block_on(async move {
            // Wait a moment for the IPC server to start inside Dolphin
            tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

            match hw_ipc::HWClient::connect(10000).await {
                Ok(client) => {
                    crate::diagnostics::log_ipc("Connected to HowlingWind Dolphin fork");
                    let client = Arc::new(client);
                    if let Ok(mut ds) = state_ipc.lock() {
                        ds.ipc_client = Some(client);
                    }
                    // Notify frontend that IPC is ready
                    use tauri::Emitter;
                    if let Some(window) = app_ipc.get_webview_window("main") {
                        let _ = window.emit("ipc-connected", true);
                    }
                }
                Err(e) => {
                    crate::diagnostics::log_warn(&format!("IPC connect failed (stock Dolphin?): {}", e));
                    // Not fatal — we fall back to external memory approach
                }
            }
            }); // end block_on
        }); // end thread::spawn
    }

    #[cfg(windows)]
    {
        let state_inner = Arc::clone(state.inner());
        let app_clone = app.clone();

        std::thread::spawn(move || {
            // Start polling immediately — find_and_hide_dolphin_window will
            // hide the window the instant it appears (100ms poll interval)
            if let Some(dolphin_hwnd) = find_and_hide_dolphin_window(150) {
                if let Some(window) = app_clone.get_webview_window("main") {
                    let parent_hwnd: HWND = match window.hwnd() {
                        Ok(raw) => raw.0 as HWND,
                        Err(_) => return, // Window closed before we could embed
                    };

                    let size = window.inner_size().unwrap_or_default();
                    let width = size.width as i32;
                    let height = size.height as i32;

                    // embed_window hides first, reparents, then shows
                    embed_window(dolphin_hwnd, parent_hwnd, width, height);

                    if let Ok(mut ds) = state_inner.lock() {
                        ds.embedded_hwnd = Some(SendHwnd(dolphin_hwnd));
                    }

                    use tauri::Emitter;
                    let _ = window.emit("game-embedded", true);
                }
            }
        });
    }

    Ok(())
}

#[tauri::command]
fn stop_dolphin(state: tauri::State<'_, Arc<Mutex<DolphinState>>>) -> Result<(), String> {
    let mut ds = state.lock().map_err(|e| e.to_string())?;

    // Un-embed the Dolphin window first to avoid leaving a black hole
    // in the Tauri window when the child process is killed
    #[cfg(windows)]
    {
        if let Some(SendHwnd(hwnd)) = ds.embedded_hwnd {
            unsafe {
                // Remove WS_CHILD, restore to standalone window
                let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
                let new_style = (style & !WS_CHILD) | WS_POPUP;
                SetWindowLongW(hwnd, GWL_STYLE, new_style as i32);
                // Reparent to desktop (null parent)
                SetParent(hwnd, std::ptr::null_mut());
                // Hide it immediately so there's no flash
                ShowWindow(hwnd, SW_HIDE);
            }
        }
        ds.embedded_hwnd = None;
    }

    if let Some(ref mut proc) = ds.process {
        let _ = proc.kill();
    }
    ds.process = None;
    Ok(())
}

#[tauri::command]
fn resize_embedded(
    width: u32,
    height: u32,
    state: tauri::State<'_, Arc<Mutex<DolphinState>>>,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let ds = state.lock().map_err(|e| e.to_string())?;
        if let Some(SendHwnd(hwnd)) = ds.embedded_hwnd {
            unsafe {
                MoveWindow(hwnd, 0, 0, width as i32, height as i32, 1);
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn ipc_status(state: tauri::State<'_, Arc<Mutex<DolphinState>>>) -> Result<serde_json::Value, String> {
    let ds = state.lock().map_err(|e| e.to_string())?;
    let connected = ds.ipc_client.as_ref().map(|c| c.is_connected()).unwrap_or(false);
    let frame = ds.ipc_client.as_ref().map(|c| c.current_frame()).unwrap_or(0);
    Ok(serde_json::json!({
        "connected": connected,
        "frame": frame,
        "using_fork": ds.ipc_client.is_some(),
    }))
}

/// Kill Dolphin process if it's still running.
fn kill_dolphin_process(state: &Arc<Mutex<DolphinState>>) {
    if let Ok(mut ds) = state.lock() {
        if let Some(ref mut proc) = ds.process {
            let _ = proc.kill();
            let _ = proc.wait(); // Reap the process to avoid zombies
        }
        ds.process = None;
        #[cfg(windows)]
        { ds.embedded_hwnd = None; }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize diagnostics logging and panic hook
    diagnostics::init();
    diagnostics::install_panic_hook();
    diagnostics::log_info("HowlingWind launcher starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(Mutex::new(DolphinState {
            process: None,
            #[cfg(windows)]
            embedded_hwnd: None,
            ipc_client: None,
        })))
        .manage(Arc::new(Mutex::new(netplay::NetplayState::new())))
        .manage(Arc::new(Mutex::new(dolphin_mem::DolphinMemState::new())))
        .manage(Arc::new(Mutex::new(rollback::RollbackState::new())))
        .invoke_handler(tauri::generate_handler![
            save_settings,
            get_settings,
            launch_dolphin,
            stop_dolphin,
            resize_embedded,
            get_controllers,
            poll_gamepad,
            get_gcpad_mapping,
            save_gcpad_mapping,
            open_dolphin_config,
            get_dolphin_devices,
            scan_games,
            netplay::netplay_start,
            netplay::netplay_connect,
            netplay::netplay_status,
            netplay::netplay_stop,
            dolphin_mem::dolphin_mem_attach,
            dolphin_mem::dolphin_mem_read_player,
            dolphin_mem::dolphin_mem_read_frame,
            dolphin_mem::dolphin_mem_save_state,
            dolphin_mem::dolphin_mem_load_state,
            dolphin_mem::dolphin_mem_check_winner,
            dolphin_mem::dolphin_mem_read_input,
            dolphin_mem::dolphin_mem_write_input,
            dolphin_mem::dolphin_mem_detach,
            dolphin_mem::dolphin_test_rollback,
            dolphin_mem::dolphin_test_rewind,
            dolphin_mem::dolphin_debug_scan,
            dolphin_mem::dolphin_mem_scan_u16,
            dolphin_mem::dolphin_mem_hex_dump,
            dolphin_mem::dolphin_mem_list_regions,
            dolphin_mem::dolphin_auto_discover,
            dolphin_mem::dolphin_apply_gecko_live,
            dolphin_mem::dolphin_full_debug,
            dolphin_mem::test_save_load_speed,
            dolphin_mem::dolphin_fast_input_scan,
            rollback::rollback_start,
            rollback::rollback_stats,
            rollback::rollback_stop,
            rollback::rollback_tick,
            rollback::rollback_check_match_end,
            rollback::rollback_clear_match_end,
            stun::stun_discover,
            stun::stun_hole_punch,
            updater::check_for_updates,
            updater::download_update,
            updater::apply_update_and_restart,
            updater::get_app_version,
            get_local_ip,
            ipc_status,
            diagnostics::get_debug_log_path,
            diagnostics::read_debug_log,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Kill Dolphin when HowlingWind window is destroyed
                let state = window.app_handle().state::<Arc<Mutex<DolphinState>>>();
                kill_dolphin_process(state.inner());
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running HowlingWind");
}
