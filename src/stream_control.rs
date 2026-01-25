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

fn send_stream_command_rest(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<bool> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let stream_name = match stream_cmd {
        0x20 => "video",
        0x21 => "audio",
        0x22 => "debug",
        _ => "video",
    };

    let path = format!("/v1/streams/{}:start?ip={}:{}", stream_name, my_ip, port);
    log::info!("DEBUG: settings.connection.password = {:?}", password);

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

    match TcpStream::connect_timeout(&http_addr, Duration::from_secs(5)) {
        Ok(mut stream) => {
            if let Err(e) = stream.write_all(request.as_bytes()) {
                log::warn!("REST API write failed: {}", e);
                return Ok(false);
            }

            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut response = [0u8; 256];
            let _ = stream.read(&mut response);

            let response_str = String::from_utf8_lossy(&response);
            let first_line = response_str.lines().next().unwrap_or("");
            log::info!("REST API response: {}", first_line);

            if first_line.contains("200")
                || first_line.contains("204")
                || first_line.contains("201")
            {
                Ok(true)
            } else {
                log::warn!("REST API returned non-success: {}", first_line);
                Ok(false)
            }
        }
        Err(e) => {
            log::warn!("REST API connect failed: {}", e);
            Ok(false)
        }
    }
}

fn send_stop_command_rest(
    ultimate_ip: &str,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<bool> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let stream_name = match stream_cmd {
        0x20 => "video",
        0x21 => "audio",
        0x22 => "debug",
        _ => "video",
    };

    let path = format!("/v1/streams/{}:stop", stream_name);

    let password_header = match password {
        Some(pw) if !pw.is_empty() => format!("X-Password: {}\r\n", pw),
        _ => String::new(),
    };

    let request = format!(
        "PUT {} HTTP/1.1\r\nHost: {}\r\n{}Content-Length: 0\r\nConnection: close\r\n\r\n",
        path, ultimate_ip, password_header
    );

    log::info!("REST API stop: PUT http://{}{}", ultimate_ip, path);

    let http_addr = resolve_host(ultimate_ip, 80)?;

    match TcpStream::connect_timeout(&http_addr, Duration::from_secs(5)) {
        Ok(mut stream) => {
            if let Err(e) = stream.write_all(request.as_bytes()) {
                log::warn!("REST API stop write failed: {}", e);
                return Ok(false);
            }

            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut response = [0u8; 256];
            let _ = stream.read(&mut response);

            let response_str = String::from_utf8_lossy(&response);
            let first_line = response_str.lines().next().unwrap_or("");
            log::info!("REST API stop response: {}", first_line);

            if first_line.contains("200")
                || first_line.contains("204")
                || first_line.contains("201")
            {
                Ok(true)
            } else {
                log::warn!("REST API stop returned non-success: {}", first_line);
                Ok(false)
            }
        }
        Err(e) => {
            log::warn!("REST API stop connect failed: {}", e);
            Ok(false)
        }
    }
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

// ============================================================================
// Utility Functions
// ============================================================================

/// Detect local IP address that can reach the network
pub fn get_local_ip() -> Option<String> {
    // Method: Try to get IP by creating a UDP socket (doesn't actually send anything)
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        // Connect to a public IP (doesn't send data, just determines route)
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                return Some(addr.ip().to_string());
            }
        }
    }
    None
}

/// Resolve hostname to SocketAddr (supports both IP addresses and hostnames)
pub fn resolve_host(host: &str, port: u16) -> std::io::Result<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;

    let addr_str = format!("{}:{}", host, port);
    addr_str.to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Could not resolve hostname: {}", host),
        )
    })
}
