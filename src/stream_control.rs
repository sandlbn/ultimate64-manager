use crate::net_utils::resolve_host;
use crate::settings::StreamControlMethod;

/// Send binary stream control command to Ultimate64 (port 64)
/// Uses the configured stream control method
pub fn send_stream_command(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
    password: Option<&str>,
    method: StreamControlMethod,
) -> std::io::Result<()> {
    match method {
        StreamControlMethod::RestWithBinaryFallback => {
            send_stream_command_rest_with_binary_fallback(
                ultimate_ip,
                my_ip,
                port,
                stream_cmd,
                password,
            )
        }
        StreamControlMethod::BinaryWithRestFallback => {
            send_stream_command_binary_with_rest_fallback(
                ultimate_ip,
                my_ip,
                port,
                stream_cmd,
                password,
            )
        }
        StreamControlMethod::RestOnly => {
            send_stream_command_rest_only(ultimate_ip, my_ip, port, stream_cmd, password)
        }
        StreamControlMethod::BinaryOnly => {
            send_stream_command_binary_only(ultimate_ip, my_ip, port, stream_cmd)
        }
    }
}

/// Send stop command to Ultimate64
/// Uses the configured stream control method
pub fn send_stop_command(
    ultimate_ip: &str,
    stream_cmd: u8,
    password: Option<&str>,
    method: StreamControlMethod,
) -> std::io::Result<()> {
    match method {
        StreamControlMethod::RestWithBinaryFallback => {
            send_stop_command_rest_with_binary_fallback(ultimate_ip, stream_cmd, password)
        }
        StreamControlMethod::BinaryWithRestFallback => {
            send_stop_command_binary_with_rest_fallback(ultimate_ip, stream_cmd, password)
        }
        StreamControlMethod::RestOnly => {
            send_stop_command_rest_only(ultimate_ip, stream_cmd, password)
        }
        StreamControlMethod::BinaryOnly => send_stop_command_binary_only(ultimate_ip, stream_cmd),
    }
}

// ============================================================================
// REST API Implementation
// ============================================================================
//
// `PUT /v1/streams/{video|audio|debug}:start?ip=<host>[:<port>]` and
// `PUT /v1/streams/{…}:stop`, as the firmware's own OpenAPI document for
// Ultimate 64-class hardware describes them. Stopping a stream that is not
// running is documented as not an error, which is what lets the callers send
// a stop unconditionally before every start.

/// The REST name of the stream a port-64 command byte refers to, so both
/// transports are driven from the same identifier.
fn stream_name(stream_cmd: u8) -> &'static str {
    match stream_cmd {
        0x21 => "audio",
        0x22 => "debug",
        _ => "video",
    }
}

fn rest_start_path(stream_cmd: u8, my_ip: &str, port: u16) -> String {
    format!(
        "/v1/streams/{}:start?ip={}:{}",
        stream_name(stream_cmd),
        my_ip,
        port
    )
}

fn rest_stop_path(stream_cmd: u8) -> String {
    format!("/v1/streams/{}:stop", stream_name(stream_cmd))
}

/// The status code of a raw HTTP response, read from its status line.
///
/// Parsed from the second token rather than by searching the line for "200":
/// a substring match also accepts any other status whose reason phrase or
/// version happens to contain those digits.
fn http_status(response: &str) -> Option<u16> {
    let mut parts = response.lines().next()?.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// Issue one bodyless `PUT` and report whether the device accepted it.
///
/// `Ok(false)` covers every way of not succeeding — refused connection, no
/// answer, non-2xx — because the callers treat them all alike: fall back to
/// the port-64 protocol, or report failure when there is no fallback.
///
/// A hand-written request on a fresh socket, not a pooled HTTP client: this is
/// called from plain threads outside any async runtime, and the device serves
/// one connection at a time and closes it after each answer.
fn rest_put(ultimate_ip: &str, path: &str, password: Option<&str>) -> std::io::Result<bool> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let password_header = match password {
        Some(pw) if !pw.is_empty() => format!("X-Password: {}\r\n", pw),
        _ => String::new(),
    };

    let request = format!(
        "PUT {} HTTP/1.1\r\nHost: {}\r\n{}Content-Length: 0\r\nConnection: close\r\n\r\n",
        path, ultimate_ip, password_header
    );

    log::info!("REST API: PUT http://{}{}", ultimate_ip, path);

    let http_addr = resolve_host(ultimate_ip, 80)?;

    let mut stream = match TcpStream::connect_timeout(&http_addr, Duration::from_secs(5)) {
        Ok(stream) => stream,
        Err(e) => {
            log::warn!("REST API connect failed: {}", e);
            return Ok(false);
        }
    };
    if let Err(e) = stream.write_all(request.as_bytes()) {
        log::warn!("REST API write failed: {}", e);
        return Ok(false);
    }

    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut response = [0u8; 256];
    let read = stream.read(&mut response).unwrap_or(0);
    let response = String::from_utf8_lossy(&response[..read]);

    match http_status(&response) {
        Some(status) if (200..300).contains(&status) => Ok(true),
        Some(status) => {
            log::warn!("REST API {} returned HTTP {}", path, status);
            Ok(false)
        }
        None => {
            log::warn!("REST API {} gave no readable answer", path);
            Ok(false)
        }
    }
}

fn send_stream_command_rest(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<bool> {
    rest_put(
        ultimate_ip,
        &rest_start_path(stream_cmd, my_ip, port),
        password,
    )
}

fn send_stop_command_rest(
    ultimate_ip: &str,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<bool> {
    rest_put(ultimate_ip, &rest_stop_path(stream_cmd), password)
}

// ============================================================================
// Binary Protocol Implementation (Port 64)
// ============================================================================

fn send_stream_command_binary(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
) -> std::io::Result<()> {
    use std::io::Write;
    use std::net::TcpStream;
    use std::time::Duration;

    let dest = format!("{}:{}", my_ip, port);
    let dest_bytes = dest.as_bytes();

    log::info!("Binary TCP: Sending to {}:64", ultimate_ip);

    let mut cmd = Vec::with_capacity(6 + dest_bytes.len());
    cmd.push(stream_cmd); // 0x20 for video, 0x21 for audio
    cmd.push(0xFF); // Command marker
    cmd.push((2 + dest_bytes.len()) as u8); // Param length (duration + dest string)
    cmd.push(0x00); // Param length high byte
    cmd.push(0x00); // Duration low byte (0 = forever)
    cmd.push(0x00); // Duration high byte
    cmd.extend_from_slice(dest_bytes);

    log::info!(
        "Sending {} bytes to {}:64 -> {:02X?}",
        cmd.len(),
        ultimate_ip,
        cmd
    );
    log::info!("Destination string: {}", dest);

    let addr = resolve_host(ultimate_ip, 64)?;

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    let written = stream.write(&cmd)?;
    log::info!("Wrote {} bytes via binary TCP", written);

    Ok(())
}

fn send_stop_command_binary(ultimate_ip: &str, stream_cmd: u8) -> std::io::Result<()> {
    use std::io::Write;
    use std::net::TcpStream;
    use std::time::Duration;

    log::debug!("Binary TCP stop: Sending to {}:64", ultimate_ip);

    let stop_cmd = stream_cmd + 0x10;
    let cmd = [stop_cmd, 0xFF, 0x00, 0x00];

    log::debug!("Sending STOP command to {}:64 -> {:02X?}", ultimate_ip, cmd);

    let addr = resolve_host(ultimate_ip, 64)?;

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.write_all(&cmd)?;
    log::debug!("Stop command sent via binary TCP");

    Ok(())
}

fn send_stream_command_rest_with_binary_fallback(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<()> {
    // Try REST API first
    match send_stream_command_rest(ultimate_ip, my_ip, port, stream_cmd, password) {
        Ok(true) => return Ok(()),
        Ok(false) => log::warn!("REST API failed, trying binary fallback"),
        Err(e) => log::warn!("REST API error: {}, trying binary fallback", e),
    }

    // Fallback to binary protocol
    log::info!("Trying binary TCP fallback to {}:64", ultimate_ip);
    send_stream_command_binary(ultimate_ip, my_ip, port, stream_cmd)
}

fn send_stream_command_binary_with_rest_fallback(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<()> {
    // Try binary protocol first
    log::info!("Trying binary TCP to {}:64", ultimate_ip);
    match send_stream_command_binary(ultimate_ip, my_ip, port, stream_cmd) {
        Ok(()) => return Ok(()),
        Err(e) => log::warn!("Binary protocol failed: {}, trying REST fallback", e),
    }

    // Fallback to REST API
    log::info!("Trying REST API fallback");
    match send_stream_command_rest(ultimate_ip, my_ip, port, stream_cmd, password) {
        Ok(true) => Ok(()),
        Ok(false) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "REST API returned non-success status",
        )),
        Err(e) => Err(e),
    }
}

fn send_stream_command_rest_only(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<()> {
    match send_stream_command_rest(ultimate_ip, my_ip, port, stream_cmd, password) {
        Ok(true) => Ok(()),
        Ok(false) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "REST API returned non-success status",
        )),
        Err(e) => Err(e),
    }
}

fn send_stream_command_binary_only(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
) -> std::io::Result<()> {
    send_stream_command_binary(ultimate_ip, my_ip, port, stream_cmd)
}

fn send_stop_command_rest_with_binary_fallback(
    ultimate_ip: &str,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<()> {
    // Try REST API first
    match send_stop_command_rest(ultimate_ip, stream_cmd, password) {
        Ok(true) => return Ok(()),
        Ok(false) => log::warn!("REST API stop failed, trying binary fallback"),
        Err(e) => log::warn!("REST API stop error: {}, trying binary fallback", e),
    }

    // Fallback to binary protocol
    log::debug!("Trying binary TCP stop fallback to {}:64", ultimate_ip);
    send_stop_command_binary(ultimate_ip, stream_cmd)
}

fn send_stop_command_binary_with_rest_fallback(
    ultimate_ip: &str,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<()> {
    // Try binary protocol first
    log::debug!("Trying binary TCP stop to {}:64", ultimate_ip);
    match send_stop_command_binary(ultimate_ip, stream_cmd) {
        Ok(()) => return Ok(()),
        Err(e) => log::warn!("Binary stop failed: {}, trying REST fallback", e),
    }

    // Fallback to REST API
    match send_stop_command_rest(ultimate_ip, stream_cmd, password) {
        Ok(true) => Ok(()),
        Ok(false) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "REST API stop returned non-success status",
        )),
        Err(e) => Err(e),
    }
}

fn send_stop_command_rest_only(
    ultimate_ip: &str,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<()> {
    match send_stop_command_rest(ultimate_ip, stream_cmd, password) {
        Ok(true) => Ok(()),
        Ok(false) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "REST API stop returned non-success status",
        )),
        Err(e) => Err(e),
    }
}

fn send_stop_command_binary_only(ultimate_ip: &str, stream_cmd: u8) -> std::io::Result<()> {
    send_stop_command_binary(ultimate_ip, stream_cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The paths are fixed by the firmware's OpenAPI document, and the stream
    /// name set is closed — an unknown name is answered with a 404.
    #[test]
    fn rest_paths_match_the_documented_stream_routes() {
        assert_eq!(
            rest_start_path(0x20, "192.168.1.10", 11000),
            "/v1/streams/video:start?ip=192.168.1.10:11000"
        );
        assert_eq!(
            rest_start_path(0x21, "192.168.1.10", 11001),
            "/v1/streams/audio:start?ip=192.168.1.10:11001"
        );
        assert_eq!(rest_stop_path(0x22), "/v1/streams/debug:stop");
    }

    #[test]
    fn the_status_is_read_from_the_status_line() {
        assert_eq!(http_status("HTTP/1.1 200 OK\r\n\r\n{}"), Some(200));
        assert_eq!(http_status("HTTP/1.1 404 Not Found\r\n"), Some(404));
        assert_eq!(http_status(""), None, "a silent device is not a success");
        assert_eq!(http_status("garbage 200"), None);
    }

    /// A substring search for "200" used to decide success, which would wave
    /// through a failure that merely mentions the number.
    #[test]
    fn a_failure_mentioning_200_is_not_mistaken_for_success() {
        assert_eq!(
            http_status("HTTP/1.1 500 Limit of 200 reached\r\n"),
            Some(500)
        );
    }
}
