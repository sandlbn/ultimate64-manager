// Ultimate64 REST API client
// Handles API calls to the Ultimate64 device

use reqwest::Client;

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

/// Mount a disk image on the Ultimate64
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

/// Reset the C64 machine
/// PUT /v1/machine:reset
pub async fn reset_machine(host: &str, password: Option<String>) -> Result<(), String> {
    let url = format!("http://{}:80/v1/machine:reset", host);
    log::info!("API: reset machine");

    let client = Client::new();
    let mut request = client.put(&url);

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
        Ok(())
    } else {
        Err(format!("Reset failed: HTTP {}", response.status()))
    }
}

/// Run a disk image: mount, reset, and auto-load
/// This mounts the disk readonly, resets the machine
pub async fn run_disk(
    host: &str,
    file_path: &str,
    drive: &str,
    password: Option<String>,
) -> Result<String, String> {
    // 1. Mount the disk (readonly)
    mount_disk(host, file_path, drive, "readonly", password.clone()).await?;

    // Small delay
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // 2. Reset the machine
    reset_machine(host, password).await?;

    let filename = file_path.rsplit('/').next().unwrap_or(file_path);
    let drive_num = if drive == "a" { 8 } else { 9 };
    Ok(format!(
        "Mounted & reset: {} - Type LOAD\"*\",{},1 then RUN",
        filename, drive_num
    ))
}
