/// Send binary stream control command to Ultimate64 (port 64)
/// Tries REST API first, falls back to binary protocol if REST fails
pub fn send_stream_command(
    ultimate_ip: &str,
    my_ip: &str,
    port: u16,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let dest = format!("{}:{}", my_ip, port);
    let dest_bytes = dest.as_bytes();

    // Try REST API first
    let stream_name = match stream_cmd {
        0x20 => "video",
        0x21 => "audio",
        0x22 => "debug",
        _ => "video",
    };

    let path = format!("/v1/streams/{}:start?ip={}:{}", stream_name, my_ip, port);
    log::info!("DEBUG: settings.connection.password = {:?}", password);

    // Build request with optional password header
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
                log::warn!("REST API write failed: {}, trying binary fallback", e);
            } else {
                // Read response
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut response = [0u8; 256];
                let _ = stream.read(&mut response);

                let response_str = String::from_utf8_lossy(&response);
                let first_line = response_str.lines().next().unwrap_or("");
                log::info!("REST API response: {}", first_line);

                // Check if response indicates success (2xx status)
                if first_line.contains("200")
                    || first_line.contains("204")
                    || first_line.contains("201")
                {
                    return Ok(());
                } else {
                    log::warn!(
                        "REST API returned non-success: {}, trying binary fallback",
                        first_line
                    );
                }
            }
        }
        Err(e) => {
            log::warn!("REST API connect failed: {}, trying binary fallback", e);
        }
    }

    // Fallback to binary TCP protocol (port 64)
    log::info!("Trying binary TCP fallback to {}:64", ultimate_ip);

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

/// Send stop command to Ultimate64 (port 64)
/// Tries REST API first, falls back to binary protocol if REST fails
pub fn send_stop_command(
    ultimate_ip: &str,
    stream_cmd: u8,
    password: Option<&str>,
) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    // Try REST API first
    let stream_name = match stream_cmd {
        0x20 => "video",
        0x21 => "audio",
        0x22 => "debug",
        _ => "video",
    };

    let path = format!("/v1/streams/{}:stop", stream_name);

    // Build request with optional password header
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
                log::warn!("REST API stop write failed: {}, trying binary fallback", e);
            } else {
                // Read response
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut response = [0u8; 256];
                let _ = stream.read(&mut response);

                let response_str = String::from_utf8_lossy(&response);
                let first_line = response_str.lines().next().unwrap_or("");
                log::info!("REST API stop response: {}", first_line);

                // Check if response indicates success (2xx status)
                if first_line.contains("200")
                    || first_line.contains("204")
                    || first_line.contains("201")
                {
                    return Ok(());
                } else {
                    log::warn!(
                        "REST API stop returned non-success: {}, trying binary fallback",
                        first_line
                    );
                }
            }
        }
        Err(e) => {
            log::warn!(
                "REST API stop connect failed: {}, trying binary fallback",
                e
            );
        }
    }

    // Fallback to binary TCP protocol (port 64)
    log::debug!("Trying binary TCP stop fallback to {}:64", ultimate_ip);

    let stop_cmd = stream_cmd + 0x10;
    let cmd = [stop_cmd, 0xFF, 0x00, 0x00];

    log::debug!("Sending STOP command to {}:64 -> {:02X?}", ultimate_ip, cmd);

    let addr = resolve_host(ultimate_ip, 64)?;

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.write_all(&cmd)?;
    log::debug!("Stop command sent via binary TCP");

    Ok(())
}

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
