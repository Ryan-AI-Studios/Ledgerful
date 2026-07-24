//! Shared web dashboard state.

use crate::commands::web::api::KnowledgeGraphResponse;
use crate::commands::web::git_meta::GitMetaCacheEntry;
use crate::commands::web::server::csp::{embedded_csp, resolve_csp_for_spa_dir};
use crate::state::layout::Layout;
use axum::http::HeaderValue;
use camino::Utf8PathBuf;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

type KgCacheEntry = Option<(Instant, (usize, bool), KnowledgeGraphResponse)>;

/// Per-IP, per-path sliding-window request timestamps.
pub type RateLimitMap = HashMap<(IpAddr, String), Vec<Instant>>;

/// Maximum distinct (IP, path) keys retained by the rate limiter.
pub const RATE_LIMIT_MAX_KEYS: usize = 10_000;

/// Application-wide state shared by all axum handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    pub layout: Layout,
    pub token: String,
    pub spa_dir: Option<Utf8PathBuf>,
    /// Resolved Content-Security-Policy for this process instance.
    /// Embedded SPA → vendored hash manifest; `--spa-dir` → sidecar or fallback.
    pub csp_header: HeaderValue,
    pub start_time: Instant,
    pub kg_cache: Arc<Mutex<KgCacheEntry>>,
    pub rate_limiter: Arc<Mutex<RateLimitMap>>,
    /// Separate sliding-window map for failed auth attempts (DoD-4).
    /// Keyed by (IP, path); does not share counts with [`Self::rate_limiter`].
    pub auth_fail_limiter: Arc<Mutex<RateLimitMap>>,
    /// When set (public bind mode), only these peer IPs may connect.
    /// `None` means no peer filter (loopback bind / private mode).
    pub peer_allowlist: Option<HashSet<IpAddr>>,
    /// Git metadata cache for `/api/hotspots` (Track TA29). Maps
    /// `file_path → (iso8601_timestamp, author_name)`. 5-minute TTL.
    /// Track TA30 will replace this with persisted `project_files` columns.
    pub git_meta_cache: Arc<Mutex<GitMetaCacheEntry>>,
}

impl AppState {
    /// Construct application state.
    ///
    /// Constructor signature is unchanged for call sites: CSP is resolved from
    /// `spa_dir` (sidecar when `Some`, embedded vendored manifest when `None`).
    pub fn new(
        layout: Layout,
        token: String,
        spa_dir: Option<Utf8PathBuf>,
        peer_allowlist: Option<HashSet<IpAddr>>,
    ) -> Self {
        let csp_string = match &spa_dir {
            Some(dir) => resolve_csp_for_spa_dir(dir),
            None => embedded_csp().to_string(),
        };
        let csp_header = HeaderValue::from_str(&csp_string).unwrap_or_else(|_| {
            // HeaderValue rejects only a narrow set of control chars; fall back
            // to a minimal safe CSP rather than panicking at startup.
            tracing::error!("Resolved CSP header value is invalid; using script-src 'self' only");
            HeaderValue::from_static(
                "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
                 style-src 'self' 'unsafe-inline'; script-src 'self'; \
                 object-src 'none'; base-uri 'self'; frame-ancestors 'none'",
            )
        });

        Self {
            layout,
            token,
            spa_dir,
            csp_header,
            start_time: Instant::now(),
            kg_cache: Arc::new(Mutex::new(None)),
            rate_limiter: Arc::new(Mutex::new(HashMap::new())),
            auth_fail_limiter: Arc::new(Mutex::new(HashMap::new())),
            peer_allowlist,
            git_meta_cache: Arc::new(Mutex::new(None)),
        }
    }
}
