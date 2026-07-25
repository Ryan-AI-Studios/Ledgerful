use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Env var: when truthy (`1` or `true`, case-insensitive), reachability probes
/// must not open sockets (track 0082 RT-X3 / Action `LEDGERFUL_NO_NETWORK=1`).
pub const NO_NETWORK_ENV: &str = "LEDGERFUL_NO_NETWORK";

/// True when `LEDGERFUL_NO_NETWORK` is set to `1` or case-insensitive `true`.
///
/// Mirrors the host opt-in pattern used by
/// [`crate::local_model::cloud_policy::mcp_allow_cloud_egress_from_env`].
pub fn network_disabled_from_env() -> bool {
    std::env::var(NO_NETWORK_ENV)
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

/// Checks if a port is open and reachable at the given host and port.
///
/// When [`network_disabled_from_env`] is true, returns `false` without opening
/// a socket (honesty for offline / CI Action runs).
pub fn is_host_port_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    if network_disabled_from_env() {
        return false;
    }
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
///
/// When [`network_disabled_from_env`] is true, returns `false` without DNS or
/// TCP (see also the explicit short-circuit in
/// `ImpactOrchestrator::ai_enrichment_status`).
pub fn is_url_reachable(url: &str, timeout: Duration) -> bool {
    #[cfg(test)]
    {
        // Thread-local so parallel nextest processes/threads cannot race the
        // DoD-4 "probe never entered" assertion (0082 RT-X3).
        REACHABILITY_PROBE_CALLS.with(|c| {
            let n = c.get();
            c.set(n.saturating_add(1));
        });
    }
    if network_disabled_from_env() {
        return false;
    }
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
thread_local! {
    static REACHABILITY_PROBE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test-only: how many times [`is_url_reachable`] was entered on **this thread**
/// (including the no-network short-circuit path).
#[cfg(test)]
pub fn reachability_probe_call_count() -> u64 {
    REACHABILITY_PROBE_CALLS.with(|c| c.get())
}

/// Test-only: reset this thread's probe call counter.
#[cfg(test)]
pub fn reset_reachability_probe_call_count() {
    REACHABILITY_PROBE_CALLS.with(|c| c.set(0));
}

#[cfg(test)]
mod tests {
    use super::*;

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    #[test]
    #[serial_test::serial(env)]
    fn network_disabled_from_env_truthy_values() {
        let _g = TempEnv::set(NO_NETWORK_ENV, "1");
        assert!(network_disabled_from_env());
        drop(_g);
        let _g = TempEnv::set(NO_NETWORK_ENV, "TRUE");
        assert!(network_disabled_from_env());
        drop(_g);
        let _g = TempEnv::set(NO_NETWORK_ENV, "true");
        assert!(network_disabled_from_env());
        drop(_g);
        let _g = TempEnv::remove(NO_NETWORK_ENV);
        assert!(!network_disabled_from_env());
        drop(_g);
        let _g = TempEnv::set(NO_NETWORK_ENV, "0");
        assert!(!network_disabled_from_env());
    }

    #[test]
    #[serial_test::serial(env)]
    fn is_url_reachable_skips_socket_when_no_network() {
        let _g = TempEnv::set(NO_NETWORK_ENV, "1");
        // Would hang or succeed on a live port if a socket were opened; must short-circuit.
        assert!(!is_url_reachable(
            "http://127.0.0.1:65534",
            Duration::from_secs(30)
        ));
    }

    #[test]
    #[serial_test::serial(env)]
    fn test_is_url_reachable_invalid() {
        let _g = TempEnv::remove(NO_NETWORK_ENV);
        // Unused/invalid port should return false quickly
        assert!(!is_url_reachable(
            "http://127.0.0.1:65534",
            Duration::from_millis(50)
        ));
    }

    #[test]
    #[serial_test::serial(env)]
    fn test_parse_ipv6_url() {
        let _g = TempEnv::remove(NO_NETWORK_ENV);
        // We don't necessarily need the server to be up, just verify we don't panic
        // and host/port derivation is sane.
        let _ = is_url_reachable("http://[::1]:11434", Duration::from_millis(1));
    }
}
