//! Shared web dashboard state.

use crate::commands::web::api::KnowledgeGraphResponse;
use crate::commands::web::git_meta::GitMetaCacheEntry;
use crate::state::layout::Layout;
use camino::Utf8PathBuf;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

type KgCacheEntry = Option<(Instant, (usize, bool), KnowledgeGraphResponse)>;
type RateLimitMap = HashMap<(IpAddr, String), Vec<Instant>>;

/// Application-wide state shared by all axum handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    pub layout: Layout,
    pub token: String,
    pub spa_dir: Option<Utf8PathBuf>,
    pub start_time: Instant,
    pub kg_cache: Arc<Mutex<KgCacheEntry>>,
    pub rate_limiter: Arc<Mutex<RateLimitMap>>,
    /// Git metadata cache for `/api/hotspots` (Track TA29). Maps
    /// `file_path → (iso8601_timestamp, author_name)`. 5-minute TTL.
    /// Track TA30 will replace this with persisted `project_files` columns.
    pub git_meta_cache: Arc<Mutex<GitMetaCacheEntry>>,
}

impl AppState {
    pub fn new(layout: Layout, token: String, spa_dir: Option<Utf8PathBuf>) -> Self {
        Self {
            layout,
            token,
            spa_dir,
            start_time: Instant::now(),
            kg_cache: Arc::new(Mutex::new(None)),
            rate_limiter: Arc::new(Mutex::new(HashMap::new())),
            git_meta_cache: Arc::new(Mutex::new(None)),
        }
    }
}
