//! Profile API layer.
//!
//! Handles applying device profiles to connected Ultimate64/Elite-II devices
//! via the REST API, including config application, media mounting, and
//! baseline restoration.
//!
//! Key design: the baseline is captured once (at repo init or on demand) and
//! stored locally. All diffs are computed locally — no device reads at apply
//! time. To restore original config, load_from_flash is used.

use crate::config_api;
use crate::device_profile::{ConfigTree, DeviceProfile};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::Rest;

/// Apply a pre-computed config diff plus mounts to the device.
///
/// The caller is responsible for computing the diff against the stored baseline.
/// This function only sends what it receives — no device reads, no diffing.
///
/// Flow:
/// 1. Optionally restore flash baseline first (load_from_flash)
/// 2. Send the diff settings via REST API
/// 3. Mount media according to profile mounts
/// 4. Optionally reset machine
pub async fn apply_profile(
    host: String,
    profile: &DeviceProfile,
    diff: ConfigTree,
    password: Option<String>,
    connection: Option<Arc<Mutex<Rest>>>,
) -> Result<String, String> {
    let mut steps = Vec::new();

    // Step 1: Restore flash baseline if requested
    if profile.launch.restore_baseline_first {
        log::info!("Loading from flash to restore baseline...");
        config_api::flash_operation(host.clone(), "load_from_flash", password.clone()).await?;
        steps.push("Restored flash baseline".to_string());
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    }

    // Step 2: Apply the diff
    let diff_count: usize = diff.values().map(|v| v.len()).sum();
    if diff_count == 0 {
        log::info!(
            "Profile '{}': no changes needed, device already matches",
            profile.name
        );
        steps.push("No changes needed".to_string());
    } else {
        log::info!(
            "Profile '{}': applying {} changed settings across {} categories",
            profile.name,
            diff_count,
            diff.len()
        );
        let result = config_api::apply_all_config(host.clone(), diff, password.clone()).await?;
        steps.push(result);
    }

    // Step 3: Mount media
    let mount_results = apply_mounts(&host, profile, password.clone(), connection.clone()).await;
    for result in mount_results {
        match result {
            Ok(msg) => steps.push(msg),
            Err(e) => log::warn!("Mount warning: {}", e),
        }
    }

    // Step 4: Reset if requested
    if profile.launch.reset_after_apply {
        log::info!("Resetting machine after profile apply...");
        let reset_url = format!("{}/v1/machine:reset", host);
        let client = crate::net_utils::build_device_client(5)?;
        let request = crate::net_utils::with_password(client.put(&reset_url), password.as_deref());
        match request.send().await {
            Ok(_) => steps.push("Machine reset".to_string()),
            Err(e) => log::warn!("Reset request failed (may still succeed): {}", e),
        }
    }

    Ok(format!(
        "Applied profile '{}': {}",
        profile.name,
        steps.join(", ")
    ))
}

/// Extract bare host (IP or hostname) from a URL like "http://192.168.1.91".
/// The api.rs functions expect just the host — they prepend http:// themselves.
pub fn bare_host_pub(host_url: &str) -> &str {
    bare_host(host_url)
}

fn bare_host(host_url: &str) -> &str {
    host_url
        .strip_prefix("http://")
        .or_else(|| host_url.strip_prefix("https://"))
        .unwrap_or(host_url)
}

/// Check if a path is a device-side path (on the Ultimate64 filesystem).
/// Device paths start with /Usb, /Sd, /Flash, /Temp, /Net, etc.
fn is_device_path(path: &str) -> bool {
    let p = path.trim_start_matches('/');
    let lower = p.to_lowercase();
    lower.starts_with("usb")
        || lower.starts_with("sd")
        || lower.starts_with("flash")
        || lower.starts_with("temp")
        || lower.starts_with("net")
        || lower.starts_with("ramdisk")
}

/// Enable a drive via the config API if it's currently disabled.
async fn ensure_drive_enabled(host: &str, drive: &str, password: Option<String>) {
    let category = if drive == "a" {
        "Drive A Settings"
    } else {
        "Drive B Settings"
    };

    // Check current state
    match config_api::fetch_category_items(host.to_string(), category.to_string(), password.clone())
        .await
    {
        Ok((_cat, items)) => {
            let is_enabled = items
                .iter()
                .find(|i| i.name == "Drive")
                .map(|i| {
                    i.current_value
                        .as_str()
                        .map(|s| s.to_lowercase() != "disabled")
                        .unwrap_or(true)
                })
                .unwrap_or(true);

            if !is_enabled {
                log::info!("Drive {} is disabled, enabling...", drive);
                let mut changes = HashMap::new();
                let mut cat_changes = HashMap::new();
                cat_changes.insert(
                    "Drive".to_string(),
                    serde_json::Value::String("Enabled".to_string()),
                );
                changes.insert(category.to_string(), cat_changes);
                match config_api::save_batch_changes(host.to_string(), changes, password).await {
                    Ok(_) => log::info!("Drive {} enabled", drive),
                    Err(e) => log::warn!("Failed to enable drive {}: {}", drive, e),
                }
                // Let the device settle after config change
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        Err(e) => log::warn!("Could not check drive {} state: {}", drive, e),
    }
}

/// Mount and optionally autoload (run) a disk on a drive.
///
/// When autoload=true: enable drive, upload/mount, reset, LOAD"*",N,1, RUN
/// When autoload=false: just upload/mount
///
/// Handles both local files (upload via crate) and device paths (REST API).
async fn mount_and_run(
    host: &str,
    path: &str,
    drive: &str,
    autoload: bool,
    password: Option<String>,
    connection: Option<Arc<Mutex<Rest>>>,
) -> Result<String, String> {
    let filename = path.rsplit('/').next().unwrap_or(path).to_string();

    // Step 1: Enable drive if needed
    ensure_drive_enabled(host, drive, password.clone()).await;

    if autoload {
        // Full run sequence: mount + reset + LOAD + RUN
        let conn = connection.ok_or_else(|| "Autoload requires device connection".to_string())?;

        let device_num = if drive == "a" { "8" } else { "9" };

        // Pre-reset: make sure the machine is in a known state BEFORE mounting.
        // After apply_profile changes config, the device may have a cartridge
        // loaded, be showing the Ultimate menu, or be running a previous program.
        // A clean reset here guarantees the follow-up LOAD/RUN reaches BASIC.
        {
            let conn_pre = conn.clone();
            tokio::task::spawn_blocking(move || {
                let c = conn_pre.blocking_lock();
                c.reset().map_err(|e| format!("Pre-reset failed: {}", e))
            })
            .await
            .map_err(|e| format!("Task error: {}", e))??;
            // Give the machine time to boot to BASIC before uploading the disk
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        if is_device_path(path) {
            // Device path: mount via REST, then run sequence via crate
            crate::api::mount_disk(bare_host(host), path, drive, "readonly", password.clone())
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let dev = device_num.to_string();
            let fname = filename.clone();
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                // Second reset: re-enter BASIC with the mounted disk visible
                c.reset().map_err(|e| format!("Reset failed: {}", e))?;
                std::thread::sleep(std::time::Duration::from_secs(3));
                let load_cmd = format!("load \"*\",{},1\n", dev);
                c.type_text(&load_cmd)
                    .map_err(|e| format!("Type LOAD failed: {}", e))?;
                std::thread::sleep(std::time::Duration::from_secs(5));
                c.type_text("run\n")
                    .map_err(|e| format!("Type RUN failed: {}", e))?;
                Ok(format!("Mounted and running: {}", fname))
            })
            .await
            .map_err(|e| format!("Task error: {}", e))?
        } else {
            // Local file: upload+mount via crate, then run sequence
            let local_path = Path::new(path);
            if !local_path.exists() {
                return Err(format!("Local file not found: {}", path));
            }
            let path_buf = local_path.to_path_buf();
            let drive_str = drive.to_string();
            let dev = device_num.to_string();
            let fname = filename.clone();

            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                // Upload and mount
                c.mount_disk_image(
                    &path_buf,
                    drive_str,
                    ultimate64::drives::MountMode::ReadOnly,
                    false,
                )
                .map_err(|e| format!("Upload+mount failed: {}", e))?;
                std::thread::sleep(std::time::Duration::from_millis(500));
                // Second reset: re-enter BASIC with the mounted disk visible
                c.reset().map_err(|e| format!("Reset failed: {}", e))?;
                std::thread::sleep(std::time::Duration::from_secs(3));
                let load_cmd = format!("load \"*\",{},1\n", dev);
                c.type_text(&load_cmd)
                    .map_err(|e| format!("Type LOAD failed: {}", e))?;
                std::thread::sleep(std::time::Duration::from_secs(5));
                c.type_text("run\n")
                    .map_err(|e| format!("Type RUN failed: {}", e))?;
                Ok(format!("Uploaded and running: {}", fname))
            })
            .await
            .map_err(|e| format!("Task error: {}", e))?
        }
    } else {
        // Just mount, no autoload
        if is_device_path(path) {
            crate::api::mount_disk(bare_host(host), path, drive, "readwrite", password).await
        } else {
            let local_path = Path::new(path);
            if !local_path.exists() {
                return Err(format!("Local file not found: {}", path));
            }
            let conn = connection
                .ok_or_else(|| "Cannot upload local file without device connection".to_string())?;
            let path_buf = local_path.to_path_buf();
            let drive_str = drive.to_string();
            let fname = filename.clone();

            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                c.mount_disk_image(
                    &path_buf,
                    drive_str,
                    ultimate64::drives::MountMode::ReadWrite,
                    false,
                )
                .map_err(|e| format!("Upload+mount failed: {}", e))?;
                Ok(format!("Uploaded and mounted: {}", fname))
            })
            .await
            .map_err(|e| format!("Task error: {}", e))?
        }
    }
}

/// Mount media according to profile mount mappings.
async fn apply_mounts(
    host: &str,
    profile: &DeviceProfile,
    password: Option<String>,
    connection: Option<Arc<Mutex<Rest>>>,
) -> Vec<Result<String, String>> {
    let mut results = Vec::new();

    if let Some(mount) = &profile.mounts.drive_a {
        if !mount.path.is_empty() {
            results.push(
                mount_and_run(
                    host,
                    &mount.path,
                    "a",
                    mount.autoload,
                    password.clone(),
                    connection.clone(),
                )
                .await,
            );
        }
    }

    if let Some(mount) = &profile.mounts.drive_b {
        if !mount.path.is_empty() {
            results.push(
                mount_and_run(
                    host,
                    &mount.path,
                    "b",
                    mount.autoload,
                    password.clone(),
                    connection.clone(),
                )
                .await,
            );
        }
    }

    if let Some(mount) = &profile.mounts.cartridge {
        if !mount.path.is_empty() {
            results.push(
                run_program_entry(host, &mount.path, password.clone(), connection.clone()).await,
            );
        }
    }

    results
}

/// Run a program from the "cartridge" slot — accepts any runnable file type.
///
/// For device paths, uses the REST runners API (works for .prg / .crt).
/// For local files, reads the bytes and calls the ultimate64 crate's
/// corresponding method based on extension:
/// - `.crt` → `conn.run_crt(&data)`
/// - `.prg` → `conn.run_prg(&data)`
/// - `.sid` → `conn.sid_play(&data, None)`
async fn run_program_entry(
    host: &str,
    path: &str,
    password: Option<String>,
    connection: Option<Arc<Mutex<Rest>>>,
) -> Result<String, String> {
    if is_device_path(path) {
        // Route through the existing REST runners based on extension
        let ext = Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "prg" => crate::api::run_prg(bare_host(host), path, password).await,
            "sid" => crate::api::sidplay(bare_host(host), path, password).await,
            _ => crate::api::run_crt(bare_host(host), path, password).await,
        }
    } else {
        let local_path = Path::new(path);
        if !local_path.exists() {
            return Err(format!("Local file not found: {}", path));
        }
        let conn = connection
            .ok_or_else(|| "Cannot upload local file without device connection".to_string())?;
        let path_buf = local_path.to_path_buf();
        let filename = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string();
        let ext = local_path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());

        tokio::task::spawn_blocking(move || {
            let data = std::fs::read(&path_buf)
                .map_err(|e| format!("Failed to read {}: {}", path_buf.display(), e))?;
            let c = conn.blocking_lock();
            match ext.as_deref() {
                Some("crt") => c
                    .run_crt(&data)
                    .map(|_| format!("Running cartridge: {}", filename))
                    .map_err(|e| format!("run_crt failed: {}", e)),
                Some("prg") => c
                    .run_prg(&data)
                    .map(|_| format!("Running PRG: {}", filename))
                    .map_err(|e| format!("run_prg failed: {}", e)),
                Some("sid") => c
                    .sid_play(&data, None)
                    .map(|_| format!("Playing SID: {}", filename))
                    .map_err(|e| format!("sid_play failed: {}", e)),
                other => Err(format!(
                    "Unsupported file type '{}' for cartridge slot — use .crt, .prg, or .sid",
                    other.unwrap_or("(none)"),
                )),
            }
        })
        .await
        .map_err(|e| format!("Task error: {}", e))?
    }
}

/// Read full current device config + schema (valid enum values, ranges).
/// This is the ONLY time we read from the device — done once and stored.
pub async fn read_current_config(
    host: String,
    password: Option<String>,
) -> Result<(ConfigTree, crate::device_profile::ConfigSchema), String> {
    let categories = config_api::fetch_categories(host.clone(), password.clone()).await?;

    let mut config = ConfigTree::new();
    let mut schema = crate::device_profile::ConfigSchema::new();

    for (i, category) in categories.iter().enumerate() {
        log::info!(
            "Snapshotting category {}/{}: '{}'",
            i + 1,
            categories.len(),
            category
        );
        match config_api::fetch_category_items(host.clone(), category.clone(), password.clone())
            .await
        {
            Ok((_cat, items)) => {
                let mut cat_map = HashMap::new();
                let mut schema_map = HashMap::new();
                for item in items {
                    cat_map.insert(item.name.clone(), item.current_value);
                    if let Some(details) = item.details {
                        schema_map.insert(
                            item.name,
                            crate::device_profile::ItemSchema {
                                options: details.options,
                                min: details.min,
                                max: details.max,
                                format: details.format,
                                default: details.default,
                            },
                        );
                    }
                }
                config.insert(category.clone(), cat_map);
                if !schema_map.is_empty() {
                    schema.insert(category.clone(), schema_map);
                }
            }
            Err(e) => {
                log::error!("Failed to fetch category '{}': {}", category, e);
                return Err(format!("Failed to read '{}': {}", category, e));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }

    log::info!(
        "Snapshot complete: {} categories, {} schema entries",
        config.len(),
        schema.values().map(|v| v.len()).sum::<usize>()
    );
    Ok((config, schema))
}

/// Read current config and create a DeviceProfile from it.
pub async fn snapshot_current_config(
    host: String,
    name: String,
    password: Option<String>,
) -> Result<DeviceProfile, String> {
    let (config, _schema) = read_current_config(host, password).await?;
    let id = crate::device_profile::slugify(&name);
    let mut profile = DeviceProfile::new(&id, &name);
    profile.config = config;
    profile.profile_mode = crate::device_profile::ProfileMode::Full;
    profile.source_format = crate::device_profile::SourceFormat::Api;
    profile.metadata.notes = "Snapshot from device".to_string();
    Ok(profile)
}
