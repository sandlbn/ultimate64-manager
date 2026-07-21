// Ultimate64 REST API client

use crate::device_error::DeviceError;
use crate::net_utils::REST_TIMEOUT_SECS;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use ultimate64::Rest;

/// Run a PRG file from the Ultimate64 filesystem
/// PUT /v1/runners:run_prg?file=<path>
pub async fn run_prg(
    host: &str,
    file_path: &str,
    password: Option<String>,
) -> Result<String, String> {
    run_file(host, file_path, "run_prg", password).await
}

/// Run a CRT file from the Ultimate64 filesystem  
/// PUT /v1/runners:run_crt?file=<path>
pub async fn run_crt(
    host: &str,
    file_path: &str,
    password: Option<String>,
) -> Result<String, String> {
    run_file(host, file_path, "run_crt", password).await
}

/// Play a SID file from the Ultimate64 filesystem
/// PUT /v1/runners:sidplay?file=<path>
pub async fn sidplay(
    host: &str,
    file_path: &str,
    password: Option<String>,
) -> Result<String, String> {
    run_file(host, file_path, "sidplay", password).await
}

/// Play a MOD file from the Ultimate64 filesystem
/// PUT /v1/runners:modplay?file=<path>
pub async fn modplay(
    host: &str,
    file_path: &str,
    password: Option<String>,
) -> Result<String, String> {
    run_file(host, file_path, "modplay", password).await
}

/// Generic runner function for Ultimate64 API
async fn run_file(
    host: &str,
    file_path: &str,
    runner: &str,
    password: Option<String>,
) -> Result<String, String> {
    let url = format!("http://{}:80/v1/runners:{}", host, runner);
    log::info!("API: {} -> {}", runner, file_path);

    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let request = crate::net_utils::with_password(
        client.put(&url).query(&[("file", file_path)]),
        password.as_deref(),
    );
    crate::net_utils::device_send(request).await?;

    let filename = file_path.rsplit('/').next().unwrap_or(file_path);
    Ok(format!("{}: {}", runner_description(runner), filename))
}

fn runner_description(runner: &str) -> &'static str {
    match runner {
        "run_prg" => "Running",
        "run_crt" => "Started",
        "sidplay" => "Playing SID",
        "modplay" => "Playing MOD",
        _ => "Executed",
    }
}

/// Mount a disk image on the Ultimate64 (for files already on the device)
/// PUT /v1/drives/<drive>:mount?image=<path>&mode=<mode>
pub async fn mount_disk(
    host: &str,
    file_path: &str,
    drive: &str,
    mode: &str,
    password: Option<String>,
) -> Result<String, String> {
    let url = format!("http://{}:80/v1/drives/{}:mount", host, drive);
    log::info!("API: mount {} to drive {} ({})", file_path, drive, mode);

    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let request = crate::net_utils::with_password(
        client
            .put(&url)
            .query(&[("image", file_path), ("mode", mode)]),
        password.as_deref(),
    );
    crate::net_utils::device_send(request).await?;

    let filename = file_path.rsplit('/').next().unwrap_or(file_path);
    Ok(format!("Mounted: {}", filename))
}

/// Run a disk image with full sequence: mount, reset, LOAD"*",8,1, RUN
/// Uses Rest connection for type_text (from ultimate64 crate)
pub async fn run_disk(
    host: &str,
    file_path: &str,
    drive: &str,
    password: Option<String>,
    connection: Option<Arc<Mutex<Rest>>>,
) -> Result<String, String> {
    let device_num = if drive == "a" { "8" } else { "9" };
    let filename = file_path
        .rsplit('/')
        .next()
        .unwrap_or(file_path)
        .to_string();

    log::info!(
        "API: run_disk {} on drive {} (device {})",
        filename,
        drive,
        device_num
    );

    // 1. Mount the disk (readonly) via HTTP API
    mount_disk(host, file_path, drive, "readonly", password.clone()).await?;

    // Small delay for mount to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // 2. Reset and type commands using Rest connection
    if let Some(conn) = connection {
        let device = device_num.to_string();

        tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap();

            // Reset the machine
            c.reset().map_err(|e| format!("Reset failed: {}", e))?;

            // Wait for C64 to boot up
            std::thread::sleep(std::time::Duration::from_secs(3));

            // Type LOAD"*",8,1 (or 9)
            let load_cmd = format!("load\"*\",{},1\n", device);
            c.type_text(&load_cmd)
                .map_err(|e| format!("Type LOAD failed: {}", e))?;

            // Wait for program to load
            std::thread::sleep(std::time::Duration::from_secs(5));

            // Type RUN
            c.type_text("run\n")
                .map_err(|e| format!("Type RUN failed: {}", e))?;

            Ok::<String, String>(format!("Running: {}", filename))
        })
        .await
        .map_err(|e| format!("Task error: {}", e))?
    } else {
        // No connection available - just mount (reset requires connection or separate HTTP call)
        Ok(format!(
            "Mounted: {} (no connection for auto-run)",
            filename
        ))
    }
}

// ─────────────────────────────────────────────────────────────────
//  Memory read/write operations (via ultimate64 crate)
// ─────────────────────────────────────────────────────────────────

/// Maximum bytes per REST write chunk (mirrors the C++ SOCKET_BUFFER_SIZE guard).
const RAW_CHUNK: usize = 256;

/// Maximum bytes per single memory read request. Stays well under u16::MAX
/// so the underlying `read_mem(u16, u16)` call from the ultimate64 crate
/// never overflows, and keeps each round-trip's response size predictable.
const READ_CHUNK: usize = 0x8000; // 32 KB

pub async fn read_memory_async(
    connection: Arc<Mutex<Rest>>,
    address: u16,
    length: u32,
) -> Result<Vec<u8>, String> {
    if length == 0 {
        return Ok(Vec::new());
    }

    // Single u16-fit read fast-path — no extra round-trip overhead and
    // matches what the editor used to do before u32-widening.
    if length <= u16::MAX as u32 {
        return read_chunk(connection, address, length as u16).await;
    }

    // Larger reads (specifically `length == 65536`) need to be split: the
    // ultimate64 crate's read_mem signature is `(u16, u16)` so it can't
    // express 65536 directly. Walk the address space in 32 KB chunks; the
    // C64 wraps modulo 65536 so an oversized read will safely loop back to
    // earlier addresses if the user did something exotic.
    let mut out = Vec::with_capacity(length as usize);
    let mut remaining = length;
    let mut cursor = address;
    while remaining > 0 {
        let take = remaining.min(READ_CHUNK as u32) as u16;
        let bytes = read_chunk(connection.clone(), cursor, take).await?;
        out.extend_from_slice(&bytes);
        cursor = cursor.wrapping_add(take);
        remaining -= take as u32;
    }
    Ok(out)
}

async fn read_chunk(
    connection: Arc<Mutex<Rest>>,
    address: u16,
    length: u16,
) -> Result<Vec<u8>, String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.lock().unwrap();
            conn.read_mem(address, length)
                .map_err(|e| format!("Read failed: {}", e))
        }),
    )
    .await;
    match result {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Read timed out — device may be offline".to_string()),
    }
}

pub async fn write_byte_async(
    connection: Arc<Mutex<Rest>>,
    address: u16,
    value: u8,
) -> Result<(), String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.lock().unwrap();
            conn.write_mem(address, &[value])
                .map_err(|e| format!("Write failed: {}", e))
        }),
    )
    .await;
    match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Write timed out — device may be offline".to_string()),
    }
}

pub async fn fill_memory_async(
    connection: Arc<Mutex<Rest>>,
    address: u16,
    length: u32,
    value: u8,
) -> Result<(), String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS * 2),
        tokio::task::spawn_blocking(move || {
            let conn = connection.lock().unwrap();
            let fill_data: Vec<u8> = vec![value; RAW_CHUNK];
            // Walk in u32 to support length == 65536 (full address space).
            // The device address itself stays u16; `wrapping_add` matches
            // C64 semantics where memory wraps at the 64 KB boundary.
            let mut offset: u32 = 0;
            while offset < length {
                let remaining = (length - offset) as usize;
                let write_size = remaining.min(RAW_CHUNK);
                let current_addr = address.wrapping_add(offset as u16);
                conn.write_mem(current_addr, &fill_data[..write_size])
                    .map_err(|e| format!("Fill failed at ${:04X}: {}", current_addr, e))?;
                offset += write_size as u32;
            }
            Ok(())
        }),
    )
    .await;
    match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Fill timed out — device may be offline".to_string()),
    }
}

pub async fn write_memory_async(
    connection: Arc<Mutex<Rest>>,
    address: u16,
    data: Vec<u8>,
) -> Result<(), String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS * 4),
        tokio::task::spawn_blocking(move || {
            let conn = connection.lock().unwrap();
            let mut offset = 0usize;
            while offset < data.len() {
                let write_size = (data.len() - offset).min(RAW_CHUNK);
                let current_addr = address.wrapping_add(offset as u16);
                conn.write_mem(current_addr, &data[offset..offset + write_size])
                    .map_err(|e| format!("Write failed at ${:04X}: {}", current_addr, e))?;
                offset += write_size;
            }
            Ok(())
        }),
    )
    .await;
    match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Write timed out — device may be offline".to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────
//  Pool-free async REST helpers (for profile apply)
//
//  The ultimate64 crate's Rest struct uses a default reqwest Client with
//  connection pooling enabled. The Ultimate's embedded HTTP server always
//  closes connections (Connection: close), so reused idle sockets are dead
//  — which surfaces as "Connection reset by peer" / "Empty reply from server"
//  during profile apply.
//
//  These helpers use build_device_client() which disables the pool.
//  `host` in these helpers is a URL including scheme (e.g. "http://10.0.0.139").
// ─────────────────────────────────────────────────────────────────

pub async fn reset_machine_async(host: &str, password: Option<&str>) -> Result<(), String> {
    let url = format!("{}/v1/machine:reset", host);
    let client = crate::net_utils::build_device_client(5)?;
    let req = crate::net_utils::with_password(client.put(&url), password);
    crate::net_utils::device_send(req).await?;
    Ok(())
}

pub async fn reboot_machine_async(host: &str, password: Option<&str>) -> Result<(), String> {
    let url = format!("{}/v1/machine:reboot", host);
    let client = crate::net_utils::build_device_client(10)?;
    let req = crate::net_utils::with_password(client.put(&url), password);
    // Connection drop during reboot is expected — treat network errors as success.
    match req.send().await {
        Ok(_) => Ok(()),
        Err(e) => {
            let s = e.to_string().to_lowercase();
            if s.contains("connection")
                || s.contains("reset")
                || s.contains("broken")
                || s.contains("eof")
            {
                Ok(())
            } else {
                Err(format!("Reboot failed: {}", e))
            }
        }
    }
}

/// Single writemem POST.
async fn writemem_once(
    host: &str,
    address: u16,
    data: Vec<u8>,
    password: Option<&str>,
) -> Result<(), DeviceError> {
    let url = format!("{}/v1/machine:writemem?address={:x}", host, address);
    let client = crate::net_utils::build_device_client(5).map_err(DeviceError::Build)?;
    let req = crate::net_utils::with_password(
        client
            .post(&url)
            .header("Content-Type", "application/octet-stream")
            .body(data),
        password,
    );
    crate::net_utils::device_send(req).await?;
    Ok(())
}

/// writemem with retries on transient errors (DMA contention, cart activity).
pub async fn writemem_async(
    host: &str,
    address: u16,
    data: Vec<u8>,
    password: Option<&str>,
) -> Result<(), String> {
    let mut last_err: Option<DeviceError> = None;
    for attempt in 0..4 {
        if attempt > 0 {
            let delay = 500 + 500 * attempt as u64;
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            log::debug!("writemem retry {} at 0x{:x}", attempt, address);
        }
        match writemem_once(host, address, data.clone(), password).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if !e.is_transient() {
                    return Err(e.to_string());
                }
                last_err = Some(e);
            }
        }
    }
    Err(format!(
        "writemem 0x{:x} failed after 4 attempts: {}",
        address,
        last_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}

/// Type text via the keyboard buffer — mirrors ultimate64::Rest::type_text
/// but uses pool-free writes.
pub async fn type_text_async(host: &str, text: &str, password: Option<&str>) -> Result<(), String> {
    const KEYBOARD_LSTX: u16 = 0xc5;
    const KEYBOARD_NDX: u16 = 0xc6;
    const KEYBOARD_BUFFER: u16 = 0x277;

    let petscii: Vec<u8> = text
        .chars()
        .map(|c| ultimate64::petscii::Petscii::from_str_lossy(&c.to_string())[0])
        .collect();

    for chunk in petscii.chunks(10) {
        writemem_async(host, KEYBOARD_LSTX, vec![0, 0], password).await?;
        writemem_async(host, KEYBOARD_BUFFER, chunk.to_vec(), password).await?;
        writemem_async(host, KEYBOARD_NDX, vec![chunk.len() as u8], password).await?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Ok(())
}

/// Upload and mount a local disk image via multipart POST.
pub async fn upload_mount_disk_async(
    host: &str,
    local_path: &Path,
    drive: &str,
    mode: &str,
    password: Option<&str>,
) -> Result<(), String> {
    let ext = local_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .ok_or_else(|| "Missing disk image extension".to_string())?;
    let disktype = match ext.as_str() {
        "d64" | "d71" | "d81" | "g64" | "g71" => ext.as_str(),
        _ => return Err(format!("Unsupported disk image type: {}", ext)),
    };
    let file_name = local_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("disk")
        .to_string();
    let bytes = tokio::fs::read(local_path)
        .await
        .map_err(|e| format!("Read {}: {}", local_path.display(), e))?;

    let form = reqwest::multipart::Form::new()
        .part(
            "file",
            reqwest::multipart::Part::bytes(bytes).file_name(file_name),
        )
        .text("mode", mode.to_string())
        .text("type", disktype.to_string());

    let url = format!("{}/v1/drives/{}:mount", host, drive);
    let client = crate::net_utils::build_device_client(60)?;
    let req = crate::net_utils::with_password(client.post(&url).multipart(form), password);
    crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("Upload+mount failed: {}", e))?;
    Ok(())
}

/// Upload bytes to a runner endpoint (run_crt, run_prg, sidplay).
/// Remove the disk image currently mounted on the specified drive.
///
/// `drive` is `"a"` or `"b"`. Maps to the device's
/// `PUT /v1/drives/<drive>:remove` endpoint. The call is idempotent — an
/// empty drive returns 200 OK without error, so callers can always-fire it
/// to "ensure empty" without checking first.
pub async fn unmount_disk_async(
    host: &str,
    drive: &str,
    password: Option<&str>,
) -> Result<(), String> {
    let url = format!("{}/v1/drives/{}:remove", host, drive);
    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let req = crate::net_utils::with_password(client.put(&url), password);
    crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("unmount Drive {}: {}", drive.to_uppercase(), e))?;
    Ok(())
}

pub async fn upload_runner_async(
    host: &str,
    runner: &str,
    data: Vec<u8>,
    password: Option<&str>,
) -> Result<(), String> {
    let url = format!("{}/v1/runners:{}", host, runner);
    let client = crate::net_utils::build_device_client(60)?;
    let req = crate::net_utils::with_password(
        client
            .post(&url)
            .header("Content-Type", "application/octet-stream")
            .body(data),
        password,
    );
    crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("{}: {}", runner, e))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
//  Drive control (mode / power / reset). `host` includes the scheme,
//  e.g. "http://10.0.0.139"; `drive` is "a" or "b".
// ─────────────────────────────────────────────────────────────────

/// Switch a drive's emulated hardware type live.
/// `mode` is one of "1541", "1571", "1581".
/// PUT /v1/drives/<drive>:set_mode?mode=<mode>
pub async fn set_drive_mode_async(
    host: &str,
    drive: &str,
    mode: &str,
    password: Option<&str>,
) -> Result<String, String> {
    let url = format!("{}/v1/drives/{}:set_mode", host, drive);
    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let req = crate::net_utils::with_password(client.put(&url).query(&[("mode", mode)]), password);
    crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("Drive {} set mode: {}", drive.to_uppercase(), e))?;
    Ok(format!("Drive {} → {}", drive.to_uppercase(), mode))
}

/// Power a drive on or off.
/// PUT /v1/drives/<drive>:on | :off
pub async fn drive_power_async(
    host: &str,
    drive: &str,
    on: bool,
    password: Option<&str>,
) -> Result<String, String> {
    let action = if on { "on" } else { "off" };
    let url = format!("{}/v1/drives/{}:{}", host, drive, action);
    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let req = crate::net_utils::with_password(client.put(&url), password);
    crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("Drive {} power {}: {}", drive.to_uppercase(), action, e))?;
    Ok(format!("Drive {} powered {}", drive.to_uppercase(), action))
}

/// Reset a drive.
/// PUT /v1/drives/<drive>:reset
pub async fn drive_reset_async(
    host: &str,
    drive: &str,
    password: Option<&str>,
) -> Result<String, String> {
    let url = format!("{}/v1/drives/{}:reset", host, drive);
    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let req = crate::net_utils::with_password(client.put(&url), password);
    crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("Drive {} reset: {}", drive.to_uppercase(), e))?;
    Ok(format!("Drive {} reset", drive.to_uppercase()))
}

/// Load a PRG into C64 memory *without* running it.
/// PUT /v1/runners:load_prg?file=<path>
pub async fn load_prg_async(
    host: &str,
    file_path: &str,
    password: Option<&str>,
) -> Result<String, String> {
    let url = format!("{}/v1/runners:load_prg", host);
    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let req =
        crate::net_utils::with_password(client.put(&url).query(&[("file", file_path)]), password);
    crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("load_prg: {}", e))?;
    let filename = file_path.rsplit('/').next().unwrap_or(file_path);
    Ok(format!("Loaded: {}", filename))
}

/// Read the debug register ($D7FF, U64 only).
/// GET /v1/machine:debugreg  → body is the value (hex or decimal text).
pub async fn read_debugreg_async(host: &str, password: Option<&str>) -> Result<u8, String> {
    let url = format!("{}/v1/machine:debugreg", host);
    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let req = crate::net_utils::with_password(client.get(&url), password);
    let resp = crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("debugreg read: {}", e))?;
    let body = resp
        .text()
        .await
        .map_err(|e| format!("debugreg read body: {}", e))?;
    parse_debugreg_value(&body)
        .ok_or_else(|| format!("debugreg: could not parse value from '{}'", body.trim()))
}

/// Write the debug register ($D7FF, U64 only).
/// PUT /v1/machine:debugreg?value=<hex>
pub async fn write_debugreg_async(
    host: &str,
    value: u8,
    password: Option<&str>,
) -> Result<String, String> {
    let url = format!("{}/v1/machine:debugreg", host);
    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
    let value_str = format!("{:02x}", value);
    let req = crate::net_utils::with_password(
        client.put(&url).query(&[("value", value_str.as_str())]),
        password,
    );
    crate::net_utils::device_send(req)
        .await
        .map_err(|e| format!("debugreg write: {}", e))?;
    Ok(format!("Debug register = ${:02X}", value))
}

/// Parse a debug-register value returned by the device. Accepts a bare
/// number, an optional `0x`/`$` hex prefix, or a JSON-ish `{"value": N}`
/// fragment — returns the low byte.
fn parse_debugreg_value(body: &str) -> Option<u8> {
    let mut s = body.trim();
    // Pull a value out of a trivial `"value": 123` shape if present.
    if let Some(pos) = s.find(':') {
        if s.contains('{') || s.contains("value") {
            s = s[pos + 1..]
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != 'x' && c != '$');
        }
    }
    let s = s.trim();
    let parsed = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("$")) {
        u32::from_str_radix(hex.trim(), 16).ok()
    } else {
        // Try decimal first, then fall back to hex for bare hex like "ff".
        s.parse::<u32>()
            .ok()
            .or_else(|| u32::from_str_radix(s, 16).ok())
    };
    parsed.map(|v| (v & 0xff) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_debugreg_decimal() {
        assert_eq!(parse_debugreg_value("255"), Some(255));
        assert_eq!(parse_debugreg_value(" 16 "), Some(16));
    }

    #[test]
    fn parse_debugreg_hex_prefixes() {
        assert_eq!(parse_debugreg_value("0xff"), Some(0xff));
        assert_eq!(parse_debugreg_value("$1A"), Some(0x1a));
    }

    #[test]
    fn parse_debugreg_json_fragment() {
        assert_eq!(parse_debugreg_value("{\"value\": 42}"), Some(42));
    }

    #[test]
    fn parse_debugreg_rejects_garbage() {
        assert_eq!(parse_debugreg_value("hello"), None);
    }
}
