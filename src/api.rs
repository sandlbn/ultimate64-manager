// Ultimate64 REST API client

use reqwest::Client;
use std::sync::Arc;
use tokio::sync::Mutex;
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

    let client = Client::new();

    // Build request with file query parameter
    let mut request = client.put(&url).query(&[("file", file_path)]);

    // Add X-password header if password is configured
    if let Some(ref pwd) = password {
        if !pwd.is_empty() {
            request = request.header("X-password", pwd.as_str());
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if response.status().is_success() {
        let filename = file_path.rsplit('/').next().unwrap_or(file_path);
        Ok(format!("{}: {}", runner_description(runner), filename))
    } else {
        Err(format!("Runner failed: HTTP {}", response.status()))
    }
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

    let client = Client::new();
    let mut request = client
        .put(&url)
        .query(&[("image", file_path), ("mode", mode)]);

    // Add X-password header if password is configured
    if let Some(ref pwd) = password {
        if !pwd.is_empty() {
            request = request.header("X-password", pwd.as_str());
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if response.status().is_success() {
        let filename = file_path.rsplit('/').next().unwrap_or(file_path);
        Ok(format!("Mounted: {}", filename))
    } else {
        Err(format!("Mount failed: HTTP {}", response.status()))
    }
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
            let c = conn.blocking_lock();

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
