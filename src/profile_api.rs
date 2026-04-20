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
/// Pre-clean modes for profile apply.
/// 0 = direct (no pre-clean), 1 = full reboot, 2 = load flash defaults + reset.
pub async fn apply_profile(
    host: String,
    profile: &DeviceProfile,
    diff: ConfigTree,
    password: Option<String>,
    connection: Option<Arc<Mutex<Rest>>>,
    pre_clean_mode: u8,
) -> Result<String, String> {
    let mut steps = Vec::new();

    // Track whether Step 0 already did a reboot — if so, skip the ROM-change
    // reboot later (double reboots back-to-back hang the device).
    let already_rebooted = pre_clean_mode == 1;

    match pre_clean_mode {
        1 => {
            // Legacy "Reboot & Apply" mode — the UI no longer exposes this
            // because REST reboot on this firmware is unpredictable (0s-3min
            // recovery, or hang requiring power-cycle). If still called,
            // fire-and-forget with a warning; don't block waiting.
            log::info!("Pre-apply reboot requested (fire-and-forget)...");
            let _ = crate::api::reboot_machine_async(&host, password.as_deref()).await;
            steps.push("Reboot sent — device may be offline briefly".to_string());
        }
        2 => {
            // Load flash defaults — fast way to restore saved config without rebooting.
            // Clears any runtime config changes from previous profiles.
            log::info!("Loading flash defaults before applying profile...");
            match config_api::flash_operation(host.clone(), "load_from_flash", password.clone())
                .await
            {
                Ok(_) => steps.push("Loaded flash defaults".to_string()),
                Err(e) => log::warn!("load_from_flash failed: {}", e),
            }
            tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
        }
        _ => {
            // Direct apply — no pre-clean
        }
    }

    // Step 1: Restore flash baseline if requested
    if profile.launch.restore_baseline_first {
        log::info!("Loading from flash to restore baseline...");
        config_api::flash_operation(host.clone(), "load_from_flash", password.clone()).await?;
        steps.push("Restored flash baseline".to_string());
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    }

    // Step 2: Apply the diff
    let diff_count: usize = diff.values().map(|v| v.len()).sum();
    let needs_reboot = requires_reboot(&diff);
    if diff_count == 0 {
        log::info!(
            "Profile '{}': no changes needed, device already matches",
            profile.name
        );
        steps.push("No changes needed".to_string());
    } else {
        log::info!(
            "Profile '{}': applying {} changed settings across {} categories{}",
            profile.name,
            diff_count,
            diff.len(),
            if needs_reboot {
                " (reboot required)"
            } else {
                ""
            },
        );
        let result = config_api::apply_all_config(host.clone(), diff, password.clone()).await?;
        steps.push(result);
        // Let the device settle after config changes before more requests
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }

    // Step 2b: If the diff contains ROM changes, the Ultimate needs to reboot
    // (ROM loading happens at Ultimate boot, not at C64 reset). We send the
    // reboot request but DON'T wait for the device to come back — on this
    // firmware, post-reboot recovery can take 0s or 3+ minutes unpredictably,
    // and blocking the app that long locks out the UI and makes users click
    // Apply again (which makes things worse). Fire-and-forget + warn the user.
    if needs_reboot && diff_count > 0 && !already_rebooted {
        log::info!(
            "ROM/firmware settings changed — sending reboot (device will be offline briefly)..."
        );
        match crate::api::reboot_machine_async(&host, password.as_deref()).await {
            Ok(_) => steps.push(
                "Reboot sent for ROM change — device may take up to 3 min to come back".to_string(),
            ),
            Err(e) => log::warn!("Reboot request failed: {}", e),
        }
    }

    // Step 3: Mount media
    let mount_results = apply_mounts(&host, profile, password.clone(), connection.clone()).await;
    for result in mount_results {
        match result {
            Ok(msg) => steps.push(msg),
            Err(e) => log::warn!("Mount warning: {}", e),
        }
    }

    // Step 4: Reset if requested (skip if we already rebooted for ROM changes)
    if profile.launch.reset_after_apply && !needs_reboot {
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

/// Check if the diff contains settings that require a full Ultimate reboot
/// (not just a C64 reset). ROM changes are loaded at Ultimate boot time,
/// so they only take effect after reboot.
/// Wait for the Ultimate to be fully ready to accept POST requests after a reboot.
///
/// Uses a 2-stage check:
/// - Stage 1: GET /v1/configs returns 200 (HTTP server is up)
/// - Stage 2: A probe POST succeeds without "connection closed" (config
///   subsystem is initialized — POST handlers actually work)
///
/// This avoids the bug where GET succeeds ~15s after reboot but POST still
/// closes connections for another 5-10s because the config subsystem is
/// still initializing.
async fn wait_for_device_ready(host: &str, password: Option<&str>) {
    log::info!("Waiting for device HTTP to come back (up to 3 min)...");
    let get_url = format!("{}/v1/configs", host);

    // Stage 1: GET /v1/configs works (HTTP server is up).
    // Some reboots (when a disk is mounted or a cart was active) can take
    // 2-3 minutes before HTTP responds. 60 × 3s = 180s max.
    let mut http_up = false;
    for attempt in 1..=60 {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Ok(client) = crate::net_utils::build_device_client(3) {
            let req = crate::net_utils::with_password(client.get(&get_url), password);
            if req.send().await.is_ok() {
                log::info!("HTTP server up (~{}s)", attempt * 3);
                http_up = true;
                break;
            }
        }
    }
    if !http_up {
        log::warn!("HTTP server did not come back after 3 min — continuing anyway");
        return;
    }

    // Small settle — once GET works, POST usually works too. The earlier
    // "POST probe" stage turned out to be unreliable (config endpoint can
    // succeed while writemem still fails, and vice-versa), so we drop it
    // and let the downstream retries (writemem_async, apply_all_config)
    // handle the residual settling window.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}

fn requires_reboot(diff: &ConfigTree) -> bool {
    for (category, items) in diff {
        for key in items.keys() {
            let lower_key = key.to_lowercase();
            let lower_cat = category.to_lowercase();
            // ROM file paths — loaded at boot
            if lower_key.contains("rom")
                && (lower_key.contains("kernal")
                    || lower_key.contains("basic")
                    || lower_key.contains("char")
                    || lower_key.contains("1541")
                    || lower_key.contains("1571")
                    || lower_key.contains("1581"))
            {
                return true;
            }
            // "ROM for XXXX mode" pattern in drive settings
            if lower_key.starts_with("rom for ") {
                return true;
            }
            // Cartridge changes may also need reboot
            if lower_key == "cartridge" && lower_cat.contains("cartridge") {
                return true;
            }
        }
    }
    false
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
/// Only fetches the single "Drive" key, not the entire category.
async fn ensure_drive_enabled(host: &str, drive: &str, password: Option<String>) {
    let category = if drive == "a" {
        "Drive A Settings"
    } else {
        "Drive B Settings"
    };

    // Fetch only the "Drive" key (not all 13 items in the category)
    match config_api::fetch_item_details(
        host.to_string(),
        category.to_string(),
        "Drive".to_string(),
        password.clone(),
    )
    .await
    {
        Ok((_name, details)) => {
            let is_enabled = details
                .current
                .as_str()
                .map(|s| s.to_lowercase() != "disabled")
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
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
        }
        Err(e) => log::warn!("Could not check drive {} state: {}", drive, e),
    }
}

/// Mount and optionally autoload (run) a disk on a drive.
///
/// When autoload=true: enable drive, (probe/recover cart), upload/mount, reset, LOAD"*",N,1, RUN
/// When autoload=false: just upload/mount
///
/// All REST calls go through the pool-free client — see [api::reset_machine_async] etc.
async fn mount_and_run(
    host: &str,
    path: &str,
    drive: &str,
    autoload: bool,
    password: Option<String>,
    _connection: Option<Arc<Mutex<Rest>>>,
) -> Result<String, String> {
    let filename = path.rsplit('/').next().unwrap_or(path).to_string();
    let pwd = password.as_deref();

    // Step 1: Enable drive if needed (includes its own settle delay)
    ensure_drive_enabled(host, drive, password.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    if autoload {
        let device_num = if drive == "a" { "8" } else { "9" };

        // Mount the disk first. Any prior cartridge keeps running — that's fine,
        // the reset below unloads it. Mount itself doesn't need writemem.
        if is_device_path(path) {
            crate::api::mount_disk(bare_host(host), path, drive, "readonly", password.clone())
                .await?;
        } else {
            let local_path = Path::new(path);
            if !local_path.exists() {
                return Err(format!("Local file not found: {}", path));
            }
            crate::api::upload_mount_disk_async(host, local_path, drive, "readonly", pwd).await?;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Reset — this unloads any running cartridge AND puts the machine at
        // the BASIC prompt with the mounted disk visible. Verified empirically:
        // after `run_crt` + `reset`, cartridge is gone and writemem works.
        crate::api::reset_machine_async(host, pwd).await?;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Type LOAD"*",N,1 then RUN. writemem_async retries transient errors
        // (Empty reply / Connection reset — usually brief DMA contention).
        let load_cmd = format!("load \"*\",{},1\n", device_num);
        crate::api::type_text_async(host, &load_cmd, pwd)
            .await
            .map_err(|e| format!("Type LOAD failed: {}", e))?;
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        crate::api::type_text_async(host, "run\n", pwd)
            .await
            .map_err(|e| format!("Type RUN failed: {}", e))?;

        Ok(format!("Mounted and running: {}", filename))
    } else {
        // Just mount, no autoload
        if is_device_path(path) {
            crate::api::mount_disk(bare_host(host), path, drive, "readwrite", password).await
        } else {
            let local_path = Path::new(path);
            if !local_path.exists() {
                return Err(format!("Local file not found: {}", path));
            }
            crate::api::upload_mount_disk_async(host, local_path, drive, "readwrite", pwd).await?;
            Ok(format!("Uploaded and mounted: {}", filename))
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
    _connection: Option<Arc<Mutex<Rest>>>,
) -> Result<String, String> {
    let pwd = password.as_deref();
    if is_device_path(path) {
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
        let filename = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string();
        let ext = local_path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {}", local_path.display(), e))?;
        match ext.as_deref() {
            Some("crt") => crate::api::upload_runner_async(host, "run_crt", data, pwd)
                .await
                .map(|_| format!("Running cartridge: {}", filename)),
            Some("prg") => crate::api::upload_runner_async(host, "run_prg", data, pwd)
                .await
                .map(|_| format!("Running PRG: {}", filename)),
            Some("sid") => crate::api::upload_runner_async(host, "sidplay", data, pwd)
                .await
                .map(|_| format!("Playing SID: {}", filename)),
            other => Err(format!(
                "Unsupported file type '{}' for cartridge slot — use .crt, .prg, or .sid",
                other.unwrap_or("(none)"),
            )),
        }
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
                                presets: details.presets,
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
