//! Middleware for the Ledgerful web dashboard.

use crate::commands::web::auth::{extract_token_header, validate_token};
use crate::commands::web::error::WebError;
use crate::commands::web::state::{AppState, RATE_LIMIT_MAX_KEYS, RateLimitMap};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Restrict CORS to local dashboard origins. The production SPA is served from
/// the same origin, so this primarily supports the Next.js dev server on
/// http://localhost:3001 / http://127.0.0.1:3001 and manual local testing.
pub(crate) fn local_cors() -> CorsLayer {
    CorsLayer::new().allow_origin(AllowOrigin::predicate(
        |origin: &HeaderValue, _parts: &axum::http::request::Parts| {
            let Ok(text) = origin.to_str() else {
                return false;
            };
            let Ok(uri) = text.parse::<axum::http::Uri>() else {
                return false;
            };
            let Some(authority) = uri.authority() else {
                return false;
            };
            is_loopback_host(authority.host())
        },
    ))
}

pub(crate) fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    // `http::uri::Authority::host()` keeps brackets for IPv6 (e.g., "[::1]").
    // Strip them before parsing as `IpAddr`.
    let stripped = host.strip_prefix('[').and_then(|h| h.strip_suffix(']'));
    let candidate = stripped.unwrap_or(host);
    candidate
        .parse::<IpAddr>()
        .is_ok_and(|addr| addr.is_loopback())
}

/// Layer that requires a valid session token for all nested routes.
pub(crate) async fn token_layer(
    State(state): State<std::sync::Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, WebError> {
    let parts = request.into_parts();
    let provided = extract_token_header(&parts.0);
    validate_token(provided, &state.token)?;

    let request = Request::from_parts(parts.0, parts.1);
    Ok(next.run(request).await)
}

/// Validate the `Host` header and reject non-loopback authorities with 403.
///
/// Host validation is a **DNS-rebinding defense only**, not a network ACL.
/// Peer IP allowlisting (when `--allow-public` is used) is enforced separately
/// via [`peer_allowlist_layer`]. Fully-supported public Host/CORS rewrite is a
/// future track residual.
///
/// Runs before routing and before the token layer.
pub(crate) async fn host_validation_layer(
    request: Request,
    next: Next,
) -> Result<Response, WebError> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .ok_or(WebError::Forbidden)?;

    let authority = host
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| WebError::Forbidden)?;

    if !is_loopback_host(authority.host()) {
        return Err(WebError::Forbidden);
    }

    Ok(next.run(request).await)
}

/// When public mode is active (`peer_allowlist` is `Some`), reject peers whose
/// source IP is not in the configured allowlist. Loopback mode leaves the
/// allowlist as `None` and this layer is a no-op.
pub(crate) async fn peer_allowlist_layer(
    State(state): State<std::sync::Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, WebError> {
    if let Some(allowlist) = state.peer_allowlist.as_ref() {
        let ip = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.ip())
            .unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]));
        if !allowlist.contains(&ip) {
            return Err(WebError::Forbidden);
        }
    }
    Ok(next.run(request).await)
}

pub(crate) async fn csp_header_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let value = HeaderValue::from_static(
        "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
         style-src 'self' 'unsafe-inline'; script-src 'self'",
    );
    response
        .headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, value);
    response
}

pub(crate) async fn server_header_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let value = HeaderValue::from_static(concat!("ledgerful-web/", env!("CARGO_PKG_VERSION")));
    response.headers_mut().insert(header::SERVER, value);
    response
}

const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const RATE_LIMIT_MAX: usize = 60;

/// Per-IP, per-path sliding-window rate limiter. Bursts up to the configured
/// limit are allowed; exceeding the window returns 429. Keying by path prevents a
/// single noisy endpoint from starving the rest of the dashboard.
///
/// Auth failures share this per-(IP, path) bucket — there is no separate
/// auth-failure limiter (document residual: a dedicated auth-failure bucket
/// can be added later if needed; today a bad-token flood on `/api/*` is already
/// bounded by the same window).
///
/// The map is hard-capped at [`RATE_LIMIT_MAX_KEYS`] with eviction of expired
/// window entries before insert and drop of excess keys when still at capacity.
pub(crate) async fn rate_limit_layer(
    State(state): State<std::sync::Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, WebError> {
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.ip())
        .unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]));
    let path = request.uri().path().to_string();
    let now = Instant::now();
    let mut map = state.rate_limiter.lock().await;
    record_rate_limit(
        &mut map,
        ip,
        path,
        now,
        RATE_LIMIT_MAX_KEYS,
        RATE_LIMIT_WINDOW,
        RATE_LIMIT_MAX,
    )?;
    drop(map);
    Ok(next.run(request).await)
}

/// Pure rate-limit bookkeeping (testable without axum).
///
/// Returns `Err(TooManyRequests)` when the (ip, path) window is saturated.
pub(crate) fn record_rate_limit(
    map: &mut RateLimitMap,
    ip: IpAddr,
    path: String,
    now: Instant,
    max_keys: usize,
    window: Duration,
    max_per_window: usize,
) -> Result<(), WebError> {
    // Evict fully-expired keys before insert to free capacity.
    evict_expired_rate_limit_keys(map, now, window);

    let key = (ip, path);
    if !map.contains_key(&key) && map.len() >= max_keys {
        // Still at cap: drop arbitrary excess keys until under limit.
        while map.len() >= max_keys {
            if let Some(dead) = map.keys().next().cloned() {
                map.remove(&dead);
            } else {
                break;
            }
        }
    }

    let entries = map.entry(key).or_default();
    entries.retain(|t| now.duration_since(*t) < window);
    if entries.len() >= max_per_window {
        return Err(WebError::TooManyRequests);
    }
    entries.push(now);
    Ok(())
}

/// Drop map keys whose entire window has expired.
pub(crate) fn evict_expired_rate_limit_keys(
    map: &mut RateLimitMap,
    now: Instant,
    window: Duration,
) {
    map.retain(|_, entries| {
        entries.retain(|t| now.duration_since(*t) < window);
        !entries.is_empty()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn loopback_authority_parsing_accepts_ipv6_forms() {
        for host in [
            "127.0.0.1:52001",
            "localhost:52001",
            "[::1]:52001",
            "[0:0:0:0:0:0:0:1]:52001",
        ] {
            let authority = host.parse::<axum::http::uri::Authority>().unwrap();
            assert!(
                is_loopback_host(authority.host()),
                "expected loopback for {}",
                host
            );
        }
    }

    #[test]
    fn rate_limit_distinct_peers_have_distinct_buckets() {
        let mut map = RateLimitMap::new();
        let now = Instant::now();
        let ip_a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ip_b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        for _ in 0..RATE_LIMIT_MAX {
            record_rate_limit(
                &mut map,
                ip_a,
                "/api/status".into(),
                now,
                RATE_LIMIT_MAX_KEYS,
                RATE_LIMIT_WINDOW,
                RATE_LIMIT_MAX,
            )
            .unwrap();
        }
        // Peer A is saturated…
        assert!(
            record_rate_limit(
                &mut map,
                ip_a,
                "/api/status".into(),
                now,
                RATE_LIMIT_MAX_KEYS,
                RATE_LIMIT_WINDOW,
                RATE_LIMIT_MAX,
            )
            .is_err()
        );
        // …but peer B still has a fresh bucket.
        assert!(
            record_rate_limit(
                &mut map,
                ip_b,
                "/api/status".into(),
                now,
                RATE_LIMIT_MAX_KEYS,
                RATE_LIMIT_WINDOW,
                RATE_LIMIT_MAX,
            )
            .is_ok()
        );
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn rate_limit_map_bounded_under_unique_ip_path_flood() {
        let mut map = RateLimitMap::new();
        let now = Instant::now();
        let max_keys = 100usize;

        for i in 0..(max_keys * 3) {
            let ip = IpAddr::V4(Ipv4Addr::new(
                10,
                ((i / 65_536) % 256) as u8,
                ((i / 256) % 256) as u8,
                (i % 256) as u8,
            ));
            let path = format!("/p/{i}");
            let _ = record_rate_limit(&mut map, ip, path, now, max_keys, RATE_LIMIT_WINDOW, 60);
        }
        assert!(
            map.len() <= max_keys,
            "map grew to {} keys (cap {max_keys})",
            map.len()
        );
    }

    #[test]
    fn rate_limit_evicts_expired_keys_before_insert() {
        let mut map = RateLimitMap::new();
        let past = Instant::now() - Duration::from_secs(120);
        map.insert((IpAddr::V4(Ipv4Addr::LOCALHOST), "/old".into()), vec![past]);
        assert_eq!(map.len(), 1);
        record_rate_limit(
            &mut map,
            IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)),
            "/new".into(),
            Instant::now(),
            10,
            RATE_LIMIT_WINDOW,
            60,
        )
        .unwrap();
        assert!(!map.contains_key(&(IpAddr::V4(Ipv4Addr::LOCALHOST), "/old".into())));
        assert_eq!(map.len(), 1);
    }
}
