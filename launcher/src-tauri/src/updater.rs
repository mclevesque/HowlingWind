//! Auto-updater for HowlingWind.
//!
//! Checks a remote JSON endpoint for the latest version. If newer, downloads
//! the update zip, extracts it in-place, and signals the frontend to restart.
//!
//! Update JSON format (hosted on GitHub raw):
//! ```json
//! {
//!   "version": "0.2.0",
//!   "url": "https://github.com/.../releases/download/v0.2.0/HowlingWind-v0.2.0.zip",
//!   "notes": "Rollback improvements and bug fixes"
//! }
//! ```

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// Where to check for updates.
const UPDATE_CHECK_URL: &str =
    "https://raw.githubusercontent.com/mclevesque/HowlingWind/main/update.json";

/// Current app version (from Cargo.toml).
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

#[derive(Debug, Clone, Serialize)]
struct DownloadProgress {
    downloaded: u64,
    total: u64,
    percent: f64,
    phase: String, // "downloading" | "extracting" | "done" | "error"
    message: String,
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

/// Get the app's install directory (where exe lives).
fn get_app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
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

/// Download update with progress events, extract, and signal completion.
#[tauri::command]
pub async fn download_update(app: tauri::AppHandle, url: String) -> Result<String, String> {
    use tauri::Emitter;

    let emit = |phase: &str, msg: &str, downloaded: u64, total: u64, percent: f64| {
        let _ = app.emit(
            "update-progress",
            DownloadProgress {
                downloaded,
                total,
                percent,
                phase: phase.to_string(),
                message: msg.to_string(),
            },
        );
    };

    // Start streaming download
    let response = reqwest::get(&url)
        .await
        .map_err(|e| {
            emit("error", &format!("Download failed: {}", e), 0, 0, 0.0);
            format!("Download failed: {}", e)
        })?;

    if !response.status().is_success() {
        let msg = format!("Download returned status {}", response.status());
        emit("error", &msg, 0, 0, 0.0);
        return Err(msg);
    }

    let total = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;

    emit("downloading", "Starting download...", 0, total, 0.0);

    let app_dir = get_app_dir();
    let zip_path = app_dir.join("HowlingWind-update.zip");
    let mut file = std::fs::File::create(&zip_path)
        .map_err(|e| format!("Failed to create update file: {}", e))?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download stream error: {}", e))?;
        file.write_all(&chunk)
            .map_err(|e| format!("Failed to write update: {}", e))?;
        downloaded += chunk.len() as u64;
        let percent = if total > 0 {
            (downloaded as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        emit("downloading", "Downloading update...", downloaded, total, percent);
    }

    drop(file); // Close the file before extracting

    // Extract zip
    emit("extracting", "Extracting update...", downloaded, total, 100.0);

    let zip_path_clone = zip_path.clone();
    let app_dir_clone = app_dir.clone();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&zip_path_clone)
            .map_err(|e| format!("Failed to open zip: {}", e))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("Invalid zip file: {}", e))?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)
                .map_err(|e| format!("Zip entry error: {}", e))?;

            let out_path = match entry.enclosed_name() {
                Some(path) => app_dir_clone.join(path),
                None => continue,
            };

            if entry.is_dir() {
                std::fs::create_dir_all(&out_path).ok();
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                // Skip overwriting our own exe (Windows locks it)
                if out_path.ends_with("HowlingWind.exe") {
                    let bak = out_path.with_extension("exe.bak");
                    // Try to rename current exe to .bak so we can write the new one
                    std::fs::rename(&out_path, &bak).ok();
                }
                let mut outfile = std::fs::File::create(&out_path)
                    .map_err(|e| format!("Failed to extract {}: {}", out_path.display(), e))?;
                std::io::copy(&mut entry, &mut outfile)
                    .map_err(|e| format!("Extract copy error: {}", e))?;
            }
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("Extract task error: {}", e))?
    .map_err(|e| {
        emit("error", &e, downloaded, total, 100.0);
        e
    })?;

    // Clean up zip
    std::fs::remove_file(&zip_path).ok();

    emit("done", "Update complete! Restart HowlingWind to use the new version.", downloaded, total, 100.0);

    Ok("Update extracted successfully. Please restart HowlingWind.".to_string())
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
