use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Discovered Ultimate64/Ultimate-II+ device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub ip: String,
    pub product: String,
    pub firmware: String,
}

/// Response from Ultimate64 /v1/info endpoint
#[derive(Debug, Deserialize)]
struct InfoResponse {
    product: Option<String>,
    #[serde(rename = "firmwareVersion")]
    firmware_version: Option<String>,
}

/// Scan local network for Ultimate64 devices
/// Uses parallel scanning - completes in 1-2 seconds
pub async fn discover_devices() -> Vec<DiscoveredDevice> {
    let local_ip = match get_local_ip() {
        Some(ip) => ip,
        None => {
            log::warn!("Could not determine local IP for network scan");
            return Vec::new();
        }
    };

    let parts: Vec<&str> = local_ip.split('.').collect();
    if parts.len() != 4 {
        log::warn!("Invalid local IP format: {}", local_ip);
        return Vec::new();
    }

    let subnet = format!("{}.{}.{}.", parts[0], parts[1], parts[2]);
    log::info!("Scanning subnet {}0/24 for Ultimate devices...", subnet);

    // Phase 1: Fast parallel TCP port scan (50ms timeout)
    let mut port_scan_handles = Vec::with_capacity(254);

    for i in 1..=254u8 {
        let ip = format!("{}{}", subnet, i);
        port_scan_handles.push(tokio::spawn(async move {
            if check_port_open(&ip, 80, 50).await {
                Some(ip)
            } else {
                None
            }
        }));
    }

    // Collect IPs with port 80 open
    let mut candidates = Vec::new();
    for handle in port_scan_handles {
        if let Ok(Some(ip)) = handle.await {
            candidates.push(ip);
        }
    }

    log::info!("Found {} devices with port 80 open", candidates.len());

    // Phase 2: Verify Ultimate64 API (parallel, 500ms timeout)
    let mut api_handles = Vec::with_capacity(candidates.len());

    for ip in candidates {
        api_handles.push(tokio::spawn(
            async move { check_ultimate_api(&ip, 500).await },
        ));
    }

    // Collect verified Ultimate devices
    let mut devices = Vec::new();
    for handle in api_handles {
        if let Ok(Some(device)) = handle.await {
            devices.push(device);
        }
    }

    log::info!("Found {} Ultimate device(s)", devices.len());
    devices
}

/// Quick TCP port check
async fn check_port_open(ip: &str, port: u16, timeout_ms: u64) -> bool {
    let addr: SocketAddr = match format!("{}:{}", ip, port).parse() {
        Ok(a) => a,
        Err(_) => return false,
    };

    timeout(Duration::from_millis(timeout_ms), TcpStream::connect(&addr))
        .await
        .is_ok()
}

/// Check if device responds to Ultimate64 REST API
async fn check_ultimate_api(ip: &str, timeout_ms: u64) -> Option<DiscoveredDevice> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .ok()?;

    let url = format!("http://{}/v1/info", ip);

    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return None,
    };

    if !response.status().is_success() {
        return None;
    }

    let text = match response.text().await {
        Ok(t) => t,
        Err(_) => return None,
    };

    // Try to parse as JSON
    if let Ok(info) = serde_json::from_str::<InfoResponse>(&text) {
        let product = info.product.unwrap_or_default();

        // Verify it's an Ultimate device
        if product.contains("Ultimate") || product.contains("1541") {
            return Some(DiscoveredDevice {
                ip: ip.to_string(),
                product,
                firmware: info
                    .firmware_version
                    .unwrap_or_else(|| "Unknown".to_string()),
            });
        }
    }

    // Fallback: check raw text for Ultimate keywords
    if text.contains("Ultimate") || text.contains("1541") {
        return Some(DiscoveredDevice {
            ip: ip.to_string(),
            product: "Ultimate Device".to_string(),
            firmware: "Unknown".to_string(),
        });
    }

    None
}

/// Get local IP address
fn get_local_ip() -> Option<String> {
    use std::net::UdpSocket;

    // Connect to a public IP (doesn't actually send data)
    // This trick reveals our local IP
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_local_ip() {
        let ip = get_local_ip();
        assert!(ip.is_some());
        println!("Local IP: {:?}", ip);
    }
}
