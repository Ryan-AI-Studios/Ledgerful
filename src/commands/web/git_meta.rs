//! Git metadata enrichment for the hotspots API (Track TA29).
//!
//! Walks git history (first-parent, up to 1000 commits) to collect the most
//! recent commit timestamp and author name for each file. The result is cached
//! in `AppState` with a 5-minute TTL so interactive dashboard refreshes do
//! not re-walk 1000 commits on every request.
//!
//! Track TA30 persists this data in `project_files` during indexing; the TTL
//! cache here is the fallback for DBs without the m47 migration.
//!
//! The actual walk logic lives in `crate::git::metadata` (shared with TA30's
//! indexer backfill) to avoid code duplication.

pub use crate::git::metadata::{GitMetaCacheEntry, collect_git_metadata, lookup_git_meta};

use std::time::{Duration, Instant};

/// TTL for the in-memory git metadata cache. A stale cache (5 min old) is
/// acceptable — the data is informational, not transactional.
pub const GIT_META_CACHE_TTL: Duration = Duration::from_secs(300);

pub fn git_meta_cache_needs_refresh(cache: &GitMetaCacheEntry, now: Instant) -> bool {
    match cache {
        Some((fetched_at, _)) => now
            .checked_duration_since(*fetched_at)
            .is_some_and(|age| age >= GIT_META_CACHE_TTL),
        None => true,
    }
}

/// Convenience wrapper: build a git metadata map with the default 1000-commit
/// walk. Delegates to `crate::git::metadata::collect_git_metadata`.
pub fn build_git_metadata_map(
    repo_root: &camino::Utf8Path,
) -> miette::Result<std::collections::HashMap<String, (String, String)>> {
    collect_git_metadata(repo_root, 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn ttl_cache_fresh_entry_does_not_need_refresh() {
        let now = Instant::now();
        let cache: GitMetaCacheEntry = Some((now, HashMap::new()));
        let needs_refresh = git_meta_cache_needs_refresh(&cache, now);
        assert!(!needs_refresh, "fresh cache should not need refresh");
    }

    #[test]
    fn ttl_cache_expired_entry_needs_refresh() {
        let old = Instant::now();
        let now = old + Duration::from_secs(600);
        let cache: GitMetaCacheEntry = Some((old, HashMap::new()));
        let needs_refresh = git_meta_cache_needs_refresh(&cache, now);
        assert!(needs_refresh, "expired cache should need refresh");
    }

    #[test]
    fn ttl_cache_empty_needs_refresh() {
        let cache: GitMetaCacheEntry = None;
        let needs_refresh = git_meta_cache_needs_refresh(&cache, Instant::now());
        assert!(needs_refresh, "empty cache should need refresh");
    }

    #[test]
    fn ttl_cache_future_entry_does_not_panic_or_refresh() {
        let now = Instant::now();
        let cache: GitMetaCacheEntry = Some((now + Duration::from_secs(1), HashMap::new()));
        assert!(!git_meta_cache_needs_refresh(&cache, now));
    }
}
