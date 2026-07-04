//! Middleware for the Ledgerful web dashboard.

use crate::commands::web::auth::{extract_token_header, validate_token};
use crate::commands::web::error::WebError;
use crate::commands::web::state::AppState;
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
    let entries = map.entry((ip, path)).or_default();
    entries.retain(|t| now.duration_since(*t) < RATE_LIMIT_WINDOW);
    if entries.len() >= RATE_LIMIT_MAX {
        return Err(WebError::TooManyRequests);
    }
    entries.push(now);
    drop(map);
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
