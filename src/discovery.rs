use crate::net_utils::get_local_ip;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;

/// Discovered Ultimate64/Ultimate-II+ device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub ip: String,
    pub product: String,
    pub firmware: String,
}

/// Identity JSON returned by both the UDP Ident service and `/v1/info`.
///
/// Note: the endpoint uses `firmware_version` (snake_case). A previous
/// `#[serde(rename = "firmwareVersion")]` here meant the field never matched,
/// so every discovered device showed firmware "Unknown".
#[derive(Debug, Deserialize)]
struct IdentResponse {
    product: Option<String>,
    firmware_version: Option<String>,
}

/// The Ultimate "Ident" service — an undocumented UDP request/response on
/// port 64, broadcast-capable, enabled by default since firmware 3.11
/// ("Ultimate Ident Service" under Network Settings → Services). Sending the
/// literal bytes `json` returns a JSON identity (a superset of `/v1/info`).
///
/// This is the primary discovery path because a single broadcast datagram finds
/// devices the TCP `/24` scan cannot:
/// - the LAN isn't a `/24` (broadcast ignores prefix length);
/// - the device is on a different subnet of the same L2 segment;
/// - Web Remote Control (port 80) is disabled — no REST to probe;
/// - a network password is set (REST `/v1/info` then returns 403 and is
///   discarded, so those devices were previously undiscoverable).
///
/// Kept alongside the TCP scan, not replacing it: broadcast does not cross AP
/// isolation or a genuinely separate VLAN. Credit: firmware-side report,
/// GideonZ/1541ultimate#782. Verified on a Commodore C64 Ultimate (fw 1.1.0).
const IDENT_PORT: u16 = 64;

/// Scan the local network for Ultimate devices. Runs the UDP Ident broadcast and
/// the TCP `/24` scan concurrently, then merges (Ident wins on duplicate IPs).
pub async fn discover_devices() -> Vec<DiscoveredDevice> {
    let local_ip = get_local_ip();
    if local_ip.is_none() {
        log::warn!("Could not determine local IP; relying on UDP Ident broadcast only");
    }

    // Run both discovery methods at once so total time is ~max, not sum.
    let (ident, scanned) = tokio::join!(
        discover_via_ident(Duration::from_millis(1500)),
        tcp_scan(local_ip.clone()),
    );

    log::info!(
        "Discovery: {} via Ident (UDP), {} via TCP scan",
        ident.len(),
        scanned.len()
    );

    // Merge: keep every Ident result, then add TCP results the broadcast missed.
    let mut devices = ident;
    let seen: HashSet<String> = devices.iter().map(|d| d.ip.clone()).collect();
    for d in scanned {
        if !seen.contains(&d.ip) {
            devices.push(d);
        }
    }

    log::info!("Found {} Ultimate device(s)", devices.len());
    devices
}

/// Broadcast the Ident probe and collect JSON replies for `wait`.
async fn discover_via_ident(wait: Duration) -> Vec<DiscoveredDevice> {
    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("Ident: could not bind UDP socket: {}", e);
            return Vec::new();
        }
    };
    if let Err(e) = socket.set_broadcast(true) {
        log::warn!("Ident: could not enable broadcast: {}", e);
    }

    // Send to the global broadcast plus the subnet-directed broadcast — some
    // networks drop 255.255.255.255 but forward the directed one.
    let mut targets = vec!["255.255.255.255".to_string()];
    if let Some(ip) = get_local_ip() {
        let p: Vec<&str> = ip.split('.').collect();
        if p.len() == 4 {
            targets.push(format!("{}.{}.{}.255", p[0], p[1], p[2]));
        }
    }
    for t in &targets {
        if let Err(e) = socket
            .send_to(b"json", format!("{}:{}", t, IDENT_PORT))
            .await
        {
            log::debug!("Ident: send to {} failed: {}", t, e);
        }
    }

    let mut devices = Vec::new();
    let mut seen = HashSet::new();
    let mut buf = vec![0u8; 2048];
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, addr))) => {
                let ip = addr.ip().to_string();
                if !seen.insert(ip.clone()) {
                    continue; // already answered (e.g. to both broadcasts)
                }
                if let Some(dev) = parse_ident(&buf[..n], &ip) {
                    devices.push(dev);
                }
            }
            _ => break, // timeout or socket error — done listening
        }
    }
    devices
}

/// Parse an Ident/`/v1/info` JSON body into a device if it looks like an Ultimate.
fn parse_ident(body: &[u8], ip: &str) -> Option<DiscoveredDevice> {
    let info = serde_json::from_slice::<IdentResponse>(body).ok()?;
    let product = info.product.unwrap_or_default();
    if !(product.contains("Ultimate") || product.contains("1541")) {
        return None;
    }
    Some(DiscoveredDevice {
        ip: ip.to_string(),
        product,
        firmware: info
            .firmware_version
            .unwrap_or_else(|| "Unknown".to_string()),
    })
}

/// TCP `/24` scan fallback: port-80 sweep then `/v1/info`. Empty when the local
/// IP is unknown. Broadcast covers most cases; this catches segments where
/// broadcast is filtered but the host shares the `/24`.
async fn tcp_scan(local_ip: Option<String>) -> Vec<DiscoveredDevice> {
    let Some(local_ip) = local_ip else {
        return Vec::new();
    };
    let parts: Vec<&str> = local_ip.split('.').collect();
    if parts.len() != 4 {
        log::warn!("Invalid local IP format: {}", local_ip);
        return Vec::new();
    }
    let subnet = format!("{}.{}.{}.", parts[0], parts[1], parts[2]);
    log::info!("Scanning subnet {}0/24 for Ultimate devices...", subnet);

    // Phase 1: parallel TCP port-80 scan (50ms timeout).
    let mut port_scan_handles = Vec::with_capacity(254);
    for i in 1..=254u8 {
        let ip = format!("{}{}", subnet, i);
        port_scan_handles.push(tokio::spawn(async move {
            check_port_open(&ip, 80, 50).await.then_some(ip)
        }));
    }
    let mut candidates = Vec::new();
    for handle in port_scan_handles {
        if let Ok(Some(ip)) = handle.await {
            candidates.push(ip);
        }
    }

    // Phase 2: verify the Ultimate REST API (parallel, 500ms timeout).
    let mut api_handles = Vec::with_capacity(candidates.len());
    for ip in candidates {
        api_handles.push(tokio::spawn(
            async move { check_ultimate_api(&ip, 500).await },
        ));
    }
    let mut devices = Vec::new();
    for handle in api_handles {
        if let Ok(Some(device)) = handle.await {
            devices.push(device);
        }
    }
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
        .is_ok_and(|r| r.is_ok())
}

/// Check if device responds to the Ultimate64 REST API (`/v1/info`).
async fn check_ultimate_api(ip: &str, timeout_ms: u64) -> Option<DiscoveredDevice> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .ok()?;

    let url = format!("http://{}/v1/info", ip);
    let response = client.get(&url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let text = response.text().await.ok()?;

    if let Some(dev) = parse_ident(text.as_bytes(), ip) {
        return Some(dev);
    }
    // Fallback: raw keyword sniff for odd response shapes.
    if text.contains("Ultimate") || text.contains("1541") {
        return Some(DiscoveredDevice {
            ip: ip.to_string(),
            product: "Ultimate Device".to_string(),
            firmware: "Unknown".to_string(),
        });
    }
    None
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

    #[test]
    fn parses_real_ident_json() {
        // Exact payload from a Commodore C64 Ultimate (fw 1.1.0) Ident reply.
        let body = br#"{
  "product" : "C64 Ultimate (V1.49) 1.1.0",
  "firmware_version" : "1.1.0",
  "fpga_version" : "122",
  "core_version" : "1.49",
  "hostname" : "C64-Ultimate-4329FC",
  "menu_header" : "*** C64 Ultimate (V1.49) 1.1.0 ***",
  "your_string" : "",
  "unique_id" : "77F617"
}"#;
        let dev = parse_ident(body, "10.0.0.47").expect("recognized as Ultimate");
        assert_eq!(dev.ip, "10.0.0.47");
        assert_eq!(dev.product, "C64 Ultimate (V1.49) 1.1.0");
        // The bug fix: firmware is read, not "Unknown".
        assert_eq!(dev.firmware, "1.1.0");
    }

    #[test]
    fn ignores_non_ultimate_json() {
        let body = br#"{ "product": "Some Router", "firmware_version": "9" }"#;
        assert!(parse_ident(body, "10.0.0.1").is_none());
    }

    /// Live check against a real device on the LAN. Ignored by default (needs
    /// hardware + broadcast on the local segment).
    #[tokio::test]
    #[ignore = "requires an Ultimate device reachable by UDP broadcast"]
    async fn discovers_live_device_via_ident() {
        let found = discover_via_ident(Duration::from_millis(1500)).await;
        for d in &found {
            println!("Ident: {} — {} (fw {})", d.ip, d.product, d.firmware);
        }
        assert!(!found.is_empty(), "no device answered the Ident broadcast");
        assert!(found.iter().all(|d| d.firmware != "Unknown"));
    }
}
