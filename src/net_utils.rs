use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

/// Timeout for REST API operations to prevent hangs when device goes offline
pub const REST_TIMEOUT_SECS: u64 = 5;

/// Detect local IP address that can reach the network.
///
/// Creates a UDP socket and "connects" to a public IP (no data is sent)
/// to determine which local interface would be used.
pub fn get_local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}

/// Resolve hostname to SocketAddr (supports both IP addresses and hostnames)
pub fn resolve_host(host: &str, port: u16) -> std::io::Result<SocketAddr> {
    let addr_str = format!("{}:{}", host, port);
    addr_str.to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Could not resolve hostname: {}", host),
        )
    })
}

/// Build a reqwest client configured for Ultimate64 device communication.
pub fn build_device_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))
}

/// Build a reqwest client configured for external API calls (GitHub, etc).
pub fn build_external_client(
    user_agent: &str,
    timeout_secs: u64,
) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))
}

/// Attach the X-password header to a request if a non-empty password is provided.
pub fn with_password(
    request: reqwest::RequestBuilder,
    password: Option<&str>,
) -> reqwest::RequestBuilder {
    match password {
        Some(pwd) if !pwd.is_empty() => request.header("X-password", pwd),
        _ => request,
    }
}
