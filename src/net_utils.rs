use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

/// Timeout for REST API operations to prevent hangs when device goes offline
pub const REST_TIMEOUT_SECS: u64 = 5;

/// Timeout for device operations that **run or load a program**.
///
/// Measured on live hardware: `POST /v1/runners:run_prg` on an Ultimate II+
/// (fw 3.14) takes a *fixed* ~8 s to answer — the firmware holds the HTTP
/// response until its reset + DMA-load sequence finishes — and then returns
/// `200 OK`. Program size is irrelevant (a 14-byte PRG and a 24 KB PRG both
/// took ~8 s), so this is a firmware handshake, not transfer cost.
///
/// Under the ordinary 5 s [`REST_TIMEOUT_SECS`] cap a perfectly healthy load
/// was therefore reported to the user as "Load timed out — device may be
/// offline", roughly three seconds before the device confirmed success. Keep
/// this comfortably above the observed figure.
pub const REST_RUN_TIMEOUT_SECS: u64 = 20;

/// Run a blocking device call under `spawn_blocking` (so it never pins the iced
/// runtime) plus a hard outer timeout (so a hung call surfaces as a clear error
/// instead of a frozen UI).
///
/// This is the single owner of the `timeout(spawn_blocking(…))` +
/// `Ok(Ok)/Ok(Err)/Err(_)` triage that was previously copy-pasted across the
/// codebase. `what` names the operation for the timeout message ("Load",
/// "Mount", "Disk boot", …).
///
/// Caveat worth knowing at every call site: a `spawn_blocking` task **cannot be
/// cancelled**. When the timeout fires the caller is freed, but the blocking
/// thread keeps running until the underlying call returns — so this bounds the
/// *UI* wait, not the thread. Calls that could otherwise hang forever must also
/// carry a timeout on the HTTP client itself (see [`build_device_client`]).
pub async fn run_blocking<F, T>(timeout_secs: u64, what: &str, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::task::spawn_blocking(f),
    )
    .await
    {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err(format!(
            "{} timed out after {}s — device may be offline",
            what, timeout_secs
        )),
    }
}

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
///
/// Why the explicit no-pooling / no-keepalive settings:
/// The Ultimate's embedded HTTP server closes TCP connections aggressively
/// to conserve memory. reqwest's default connection pool keeps idle
/// connections alive and tries to reuse them — but if the server has
/// already closed the connection, reqwest only notices when the send times
/// out (10s silent failure). Forcing a fresh connection per request
/// eliminates this and matches curl's behavior (which works reliably).
pub fn build_device_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .connect_timeout(std::time::Duration::from_secs(5))
        .pool_max_idle_per_host(0)
        .tcp_keepalive(None)
        .http1_only()
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))
}

/// Like [`build_device_client`], but with a sub-second budget.
///
/// For interactive input there is no point waiting seconds: a joystick event
/// that has not landed within a few hundred milliseconds is already stale — the
/// stick has moved on — and the caller will re-send the *current* position
/// anyway. A short cap keeps one slow request from freezing control.
pub fn build_device_client_ms(timeout_ms: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .connect_timeout(std::time::Duration::from_millis(timeout_ms))
        .pool_max_idle_per_host(0)
        .tcp_keepalive(None)
        .http1_only()
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

/// Send a device request and validate the HTTP status, classifying any failure
/// as a [`DeviceError`]. This is the single choke point for device REST calls:
/// transport errors become `Timeout`/`Network`, and non-2xx responses become
/// `Unauthorized`/`NotFound`/`Http`. Callers keep building the request (URL,
/// method, query, body) and attach the password via [`with_password`]; this
/// only owns the send + status classification.
pub async fn device_send(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, crate::device_error::DeviceError> {
    use crate::device_error::DeviceError;
    let resp = request
        .send()
        .await
        .map_err(|e| DeviceError::from_reqwest(&e))?;
    if resp.status().is_success() {
        Ok(resp)
    } else {
        Err(DeviceError::from_status(resp.status().as_u16()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_error::DeviceError;

    #[test]
    fn run_timeout_exceeds_the_measured_device_run_latency() {
        // run_prg answers in ~8s on an Ultimate II+ (fw 3.14). The ordinary cap
        // is below that, which is precisely the bug this constant fixes; the run
        // budget must stay clear of it.
        assert!(REST_TIMEOUT_SECS < 8, "guard assumption behind the bug");
        assert!(
            REST_RUN_TIMEOUT_SECS > 8,
            "run budget must exceed the ~8s device latency"
        );
    }

    #[test]
    fn password_header_attached_only_when_non_empty() {
        let client = reqwest::Client::new();
        let has_header = |p: Option<&str>| {
            with_password(client.get("http://example.invalid"), p)
                .build()
                .unwrap()
                .headers()
                .contains_key("X-password")
        };
        assert!(has_header(Some("secret")));
        assert!(!has_header(None), "no password → no header");
        assert!(!has_header(Some("")), "empty password → no header");
    }

    #[test]
    fn status_codes_classify_into_actionable_variants() {
        assert_eq!(DeviceError::from_status(403), DeviceError::Unauthorized);
        assert_eq!(DeviceError::from_status(404), DeviceError::NotFound);
        assert_eq!(DeviceError::from_status(500), DeviceError::Http(500));
    }

    #[tokio::test]
    async fn run_blocking_returns_the_closure_value() {
        let out: Result<u8, String> = run_blocking(5, "Test", || Ok(42)).await;
        assert_eq!(out, Ok(42));
    }

    #[tokio::test]
    async fn run_blocking_propagates_the_closure_error_verbatim() {
        let out: Result<(), String> = run_blocking(5, "Test", || Err("boom".to_string())).await;
        assert_eq!(out, Err("boom".to_string()));
    }

    #[tokio::test]
    async fn run_blocking_times_out_and_names_the_operation() {
        // Sleeps past its 1s budget; the caller must be freed with a message
        // that identifies which operation stalled.
        let out: Result<(), String> = run_blocking(1, "Disk boot", || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            Ok(())
        })
        .await;
        let err = out.expect_err("should have timed out");
        assert!(err.contains("Disk boot"), "got: {err}");
        assert!(err.contains("timed out"), "got: {err}");
    }
}
