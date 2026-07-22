//! Shared web dashboard state.

use crate::commands::web::api::KnowledgeGraphResponse;
use crate::commands::web::git_meta::GitMetaCacheEntry;
use crate::state::layout::Layout;
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
    pub fn new(
        layout: Layout,
        token: String,
        spa_dir: Option<Utf8PathBuf>,
        peer_allowlist: Option<HashSet<IpAddr>>,
    ) -> Self {
        Self {
            layout,
            token,
            spa_dir,
            start_time: Instant::now(),
            kg_cache: Arc::new(Mutex::new(None)),
            rate_limiter: Arc::new(Mutex::new(HashMap::new())),
            auth_fail_limiter: Arc::new(Mutex::new(HashMap::new())),
            peer_allowlist,
            git_meta_cache: Arc::new(Mutex::new(None)),
        }
    }
}
