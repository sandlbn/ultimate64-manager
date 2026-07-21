//! Typed error for Ultimate64 device REST calls.
//!
//! The device REST modules (`api`, `config_api`, `profile_api`, …) historically
//! returned `Result<_, String>`, which loses the distinction between "wrong
//! password", "device offline (timeout)", and "not found" — a distinction the
//! auto-reconnect / status logic actually needs. `DeviceError` classifies the
//! failure once, at the HTTP boundary, via [`crate::net_utils::device_send`].
//!
//! For call sites that only surface a message, `impl From<DeviceError> for
//! String` lets a `Result<_, DeviceError>` helper be used with `?` inside an
//! existing `Result<_, String>` function — so adopting the typed error needs no
//! churn at the ~100 UI call sites; they keep flattening to a string, while the
//! paths that care (status/reconnect) can match on the variant.

use std::fmt;

/// A classified failure from a device REST request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceError {
    /// No connection configured (host/URL missing).
    NotConnected,
    /// HTTP 403 — network password missing or wrong.
    Unauthorized,
    /// HTTP 404 — endpoint/resource not found.
    NotFound,
    /// Request exceeded its deadline (device likely offline or rebooting).
    Timeout,
    /// Any other non-success HTTP status.
    Http(u16),
    /// Transport/connection error (DNS, refused, reset, EOF, …).
    Network(String),
    /// Could not build the HTTP client.
    Build(String),
}

impl DeviceError {
    /// Map a non-success HTTP status code to a typed error.
    pub fn from_status(code: u16) -> Self {
        match code {
            403 => DeviceError::Unauthorized,
            404 => DeviceError::NotFound,
            other => DeviceError::Http(other),
        }
    }

    /// Classify a `reqwest` transport error (timeout vs. everything else).
    pub fn from_reqwest(e: &reqwest::Error) -> Self {
        if e.is_timeout() {
            DeviceError::Timeout
        } else {
            DeviceError::Network(e.to_string())
        }
    }

    /// Whether a retry might succeed — the device rebooting, DMA contention, or
    /// the embedded HTTP server dropping an idle connection. Used by the
    /// writemem retry loop and the transient-outage reconnect window.
    pub fn is_transient(&self) -> bool {
        match self {
            DeviceError::Timeout => true,
            DeviceError::Http(502..=504) => true,
            DeviceError::Network(msg) => {
                let s = msg.to_lowercase();
                s.contains("empty reply")
                    || s.contains("connection")
                    || s.contains("reset")
                    || s.contains("broken")
                    || s.contains("eof")
                    || s.contains("timed out")
                    || s.contains("deadline")
            }
            _ => false,
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceError::NotConnected => write!(f, "Not connected"),
            DeviceError::Unauthorized => {
                write!(f, "Unauthorized — check the device network password")
            }
            DeviceError::NotFound => write!(f, "Not found"),
            DeviceError::Timeout => write!(f, "Request timed out — device may be offline"),
            DeviceError::Http(code) => write!(f, "HTTP {}", code),
            DeviceError::Network(msg) => write!(f, "Network error: {}", msg),
            DeviceError::Build(msg) => write!(f, "HTTP client error: {}", msg),
        }
    }
}

impl std::error::Error for DeviceError {}

/// Flatten to a message. Lets a `DeviceError`-returning helper be used with `?`
/// inside a `Result<_, String>` function without a manual `.map_err`.
impl From<DeviceError> for String {
    fn from(e: DeviceError) -> String {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification() {
        assert_eq!(DeviceError::from_status(403), DeviceError::Unauthorized);
        assert_eq!(DeviceError::from_status(404), DeviceError::NotFound);
        assert_eq!(DeviceError::from_status(500), DeviceError::Http(500));
    }

    #[test]
    fn transient_detection() {
        assert!(DeviceError::Timeout.is_transient());
        assert!(DeviceError::Http(503).is_transient());
        assert!(DeviceError::Network("connection reset by peer".into()).is_transient());
        assert!(!DeviceError::Unauthorized.is_transient());
        assert!(!DeviceError::Http(400).is_transient());
        assert!(!DeviceError::Network("malformed body".into()).is_transient());
    }

    #[test]
    fn flattens_to_string_via_from() {
        let s: String = DeviceError::Unauthorized.into();
        assert!(s.contains("password"));
    }
}
