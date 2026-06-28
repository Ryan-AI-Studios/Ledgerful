use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Checks if a port is open and reachable at the given host and port.
pub fn is_host_port_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    // Some hosts might have brackets if IPv6, (host, port).to_socket_addrs() handles it
    // but we need to ensure the brackets are passed correctly if they were parsed out.
    if let Ok(addrs) = (host, port).to_socket_addrs() {
        for addr in addrs {
            if TcpStream::connect_timeout(&addr, timeout).is_ok() {
                return true;
            }
        }
    }
    false
}

/// Helper to parse a base URL (e.g. "http://127.0.0.1:8081" or "http://[::1]:11434")
/// and check if it is reachable.
pub fn is_url_reachable(url: &str, timeout: Duration) -> bool {
    let stripped = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);

    // Get only the host:port part before any path
    let host_port = stripped.split('/').next().unwrap_or(stripped);

    // Correctly handle IPv6 literals in brackets [::1]:8080
    let (host, port) = if let Some(last_colon) = host_port.rfind(':') {
        let (host_part, port_part) = host_port.split_at(last_colon);
        let port_str = &port_part[1..];

        // If the host starts with '[' and the colon is after ']', it's a bracketed IPv6
        if host_part.starts_with('[') {
            if host_part.ends_with(']') {
                (host_part, port_str.parse::<u16>().unwrap_or(80))
            } else {
                // Malformed or no port? [::1:8080 or similar
                (
                    host_port,
                    if url.starts_with("https://") { 443 } else { 80 },
                )
            }
        } else {
            // Standard host:port
            (host_part, port_str.parse::<u16>().unwrap_or(80))
        }
    } else {
        // No colon found
        (
            host_port,
            if url.starts_with("https://") { 443 } else { 80 },
        )
    };

    is_host_port_reachable(host, port, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_url_reachable_invalid() {
        // Unused/invalid port should return false quickly
        assert!(!is_url_reachable(
            "http://127.0.0.1:65534",
            Duration::from_millis(50)
        ));
    }

    #[test]
    fn test_parse_ipv6_url() {
        // We don't necessarily need the server to be up, just verify we don't panic
        // and host/port derivation is sane.
        let _ = is_url_reachable("http://[::1]:11434", Duration::from_millis(1));
    }
}
