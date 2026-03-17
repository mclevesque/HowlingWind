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

    // Extract zip to a TEMP staging folder (not over the running app!)
    emit("extracting", "Extracting update...", downloaded, total, 100.0);

    let zip_path_clone = zip_path.clone();
    let app_dir_clone = app_dir.clone();
    let staging_dir = app_dir.join("_update_staging");
    let staging_clone = staging_dir.clone();

    tokio::task::spawn_blocking(move || {
        // Clean previous staging if exists
        if staging_clone.exists() {
            std::fs::remove_dir_all(&staging_clone).ok();
        }
        std::fs::create_dir_all(&staging_clone)
            .map_err(|e| format!("Failed to create staging dir: {}", e))?;

        let file = std::fs::File::open(&zip_path_clone)
            .map_err(|e| format!("Failed to open zip: {}", e))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("Invalid zip file: {}", e))?;

        // Detect top-level folder in zip (e.g. "HowlingWind/" or "HowlingWind\")
        let top_prefix = {
            let mut prefix = String::new();
            if let Ok(first) = archive.by_index(0) {
                // Normalize backslashes to forward slashes
                let name = first.name().replace('\\', "/");
                if let Some(slash) = name.find('/') {
                    prefix = name[..=slash].to_string();
                }
            }
            prefix
        };

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)
                .map_err(|e| format!("Zip entry error: {}", e))?;

            // Normalize backslashes to forward slashes
            let entry_name = entry.name().replace('\\', "/");
            // Strip the top-level folder prefix (e.g. "HowlingWind/file" -> "file")
            let relative = if !top_prefix.is_empty() && entry_name.starts_with(&top_prefix) {
                entry_name[top_prefix.len()..].to_string()
            } else {
                entry_name.clone()
            };

            if relative.is_empty() {
                continue; // Skip the top-level folder entry itself
            }

            let out_path = staging_clone.join(relative);

            if entry.is_dir() {
                std::fs::create_dir_all(&out_path).ok();
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).ok();
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

    // Write a batch script that waits for us to exit, copies files, and relaunches
    let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_name = exe_path.file_name().unwrap().to_string_lossy().to_string();
    let batch_path = app_dir.join("_apply_update.bat");
    let batch_contents = format!(
        r#"@echo off
title HowlingWind Updater
echo Applying update...
:: Wait for the app to close
:wait
tasklist /FI "PID eq %1" 2>NUL | find /I "%1" >NUL
if not errorlevel 1 (
    timeout /t 1 /nobreak >NUL
    goto wait
)
:: Copy staging files over app directory (skip games folder to preserve user ISOs)
for %%F in ("{staging}\*") do (
    if not "%%~nxF"=="games" (
        copy /Y "%%F" "{app_dir}\" >NUL 2>&1
    )
)
:: Copy subdirectories except games
for /D %%D in ("{staging}\*") do (
    if /I not "%%~nxD"=="games" (
        xcopy /E /Y /Q "%%D" "{app_dir}\%%~nxD\" >NUL 2>&1
    )
)
:: Ensure games directory exists but don't overwrite ISOs
if not exist "{app_dir}\games" mkdir "{app_dir}\games"
:: Clean up
rmdir /S /Q "{staging}" >NUL 2>&1
:: Relaunch
start "" "{exe}"
:: Delete this batch file
del "%~f0"
"#,
        staging = staging_dir.display(),
        app_dir = app_dir.display(),
        exe = exe_path.display(),
    );
    std::fs::write(&batch_path, &batch_contents)
        .map_err(|e| format!("Failed to write update script: {}", e))?;

    emit("done", "Update ready! Click restart to apply.", downloaded, total, 100.0);

    Ok("ready".to_string())
}

/// Launch the update batch script and exit so it can replace our files.
#[tauri::command]
pub fn apply_update_and_restart(app: tauri::AppHandle) -> Result<(), String> {
    let app_dir = get_app_dir();
    let batch_path = app_dir.join("_apply_update.bat");
    if !batch_path.exists() {
        return Err("No update staged. Download an update first.".to_string());
    }

    let pid = std::process::id();

    // Launch the batch script with our PID so it can wait for us to exit
    std::process::Command::new("cmd")
        .args(["/C", "start", "/min", "", &batch_path.to_string_lossy(), &pid.to_string()])
        .spawn()
        .map_err(|e| format!("Failed to launch updater: {}", e))?;

    // Exit the app so the batch script can overwrite our files
    app.exit(0);

    Ok(())
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
