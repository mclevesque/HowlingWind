//! Auto-updater for HowlingWind.
//!
//! Checks a remote JSON endpoint for the latest version. If newer, downloads
//! the update zip and extracts it alongside the current exe.
//!
//! Update JSON format (hosted on GitHub raw, gist, or any static host):
//! ```json
//! {
//!   "version": "0.2.0",
//!   "url": "https://github.com/.../releases/download/v0.2.0/HowlingWind-v0.2.0.zip",
//!   "notes": "Rollback improvements and bug fixes"
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where to check for updates. Change this to your actual hosting URL.
/// A GitHub raw file, gist, or any static JSON endpoint works.
const UPDATE_CHECK_URL: &str =
    "https://raw.githubusercontent.com/HowlingWind/HowlingWind/main/update.json";

/// Current app version (from Cargo.toml via tauri.conf.json).
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub download_url: String,
    pub notes: String,
}

/// Compare two semver strings. Returns true if `remote` is newer than `local`.
fn is_newer(local: &str, remote: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.trim_start_matches('v')
            .split('.')
            .filter_map(|p| p.parse::<u32>().ok())
            .collect()
    };
    let l = parse(local);
    let r = parse(remote);
    for i in 0..3 {
        let lv = l.get(i).copied().unwrap_or(0);
        let rv = r.get(i).copied().unwrap_or(0);
        if rv > lv {
            return true;
        }
        if rv < lv {
            return false;
        }
    }
    false
}

/// Check for updates by fetching the remote JSON.
#[tauri::command]
pub async fn check_for_updates() -> Result<UpdateCheckResult, String> {
    let response = reqwest::get(UPDATE_CHECK_URL)
        .await
        .map_err(|e| format!("Failed to check for updates: {}", e))?;

    if !response.status().is_success() {
        return Ok(UpdateCheckResult {
            current_version: CURRENT_VERSION.to_string(),
            latest_version: CURRENT_VERSION.to_string(),
            update_available: false,
            download_url: String::new(),
            notes: String::new(),
        });
    }

    let info: UpdateInfo = response
        .json()
        .await
        .map_err(|e| format!("Invalid update info: {}", e))?;

    let update_available = is_newer(CURRENT_VERSION, &info.version);

    Ok(UpdateCheckResult {
        current_version: CURRENT_VERSION.to_string(),
        latest_version: info.version,
        update_available,
        download_url: info.url,
        notes: info.notes,
    })
}

/// Download the update zip to a temp location and return the path.
#[tauri::command]
pub async fn download_update(url: String) -> Result<String, String> {
    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("Download failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Download returned status {}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read download: {}", e))?;

    // Save to temp dir next to exe
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let update_path = exe_dir.join("HowlingWind-update.zip");
    std::fs::write(&update_path, &bytes)
        .map_err(|e| format!("Failed to save update: {}", e))?;

    Ok(update_path.to_string_lossy().to_string())
}

/// Get the current app version.
#[tauri::command]
pub fn get_app_version() -> String {
    CURRENT_VERSION.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison() {
        assert!(is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.1.0", "1.0.0"));
        assert!(!is_newer("0.2.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("1.0.0", "0.9.9"));
    }
}
