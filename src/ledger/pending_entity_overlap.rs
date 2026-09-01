//! Pending-entity overlap for `ledger start` collision (0223).
//!
//! Pure string matching: slash-normalize, trim trailing `/`, case-insensitive,
//! component-boundary prefix for path-shaped entities, exact match for slugs.
//! No `std::fs::canonicalize`.

use miette::Diagnostic;
use thiserror::Error;

use crate::ledger::types::Transaction;

/// Max overlapping paths printed on a collision report (sorted, then capped).
pub const COLLISION_PATH_CAP: usize = 20;

/// Greppable refuse line: `[Ledgerful] Collision: pending tx {id} owns {entity}`.
pub const COLLISION_GREP_PREFIX: &str = "[Ledgerful] Collision:";

/// One pending TX that overlaps the new start entity and/or dirty paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionHit {
    pub tx_id: String,
    pub entity: String,
    pub category: String,
    pub message: String,
    pub overlapping_paths: Vec<String>,
}

/// Refused `ledger start` because a PENDING entity overlaps.
#[derive(Debug, Error, Diagnostic)]
#[error("{message}")]
#[diagnostic(
    code(ledger::pending_entity_collision),
    help(
        "Commit or abort the pending transaction first, or pass --force on ledger start. start --force bypasses this lock; commit --force bypasses the verification gate."
    )
)]
pub struct PendingEntityCollision {
    pub message: String,
}

/// Slash-normalize, trim trailing `/` only, lowercase. Empty after that overlaps nothing.
/// Does not strip surrounding whitespace (spec: slash + trailing slash + case-fold).
pub fn normalize_overlap_key(raw: &str) -> String {
    raw.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// True when `entity` is path-shaped after normalize (contains `/`).
fn is_path_shaped(normalized: &str) -> bool {
    normalized.contains('/')
}

/// Overlap predicate (symmetric). Empty entity overlaps nothing.
///
/// Path-shaped (contains `/` after normalize): equal, or component-boundary
/// prefix (`A` overlaps `A/B`; `crates/foo` does **not** overlap `crates/foo-bar`).
/// Slug (no slash): identical key only — never prefix-matches `src/lib.rs`.
pub fn pending_entity_overlaps(entity: &str, candidate: &str) -> bool {
    let a = normalize_overlap_key(entity);
    let b = normalize_overlap_key(candidate);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if is_path_shaped(&a) && is_path_shaped(&b) {
        a == b || b.starts_with(&format!("{a}/")) || a.starts_with(&format!("{b}/"))
    } else {
        a == b
    }
}

/// PENDING rows whose entity overlaps `new_entity` (arm a) or any dirty path (arm b).
///
/// Hits are sorted by `tx_id`. Overlapping paths are sorted, deduped, capped.
pub fn find_start_collisions(
    pending: &[Transaction],
    new_entity: &str,
    dirty_paths: &[String],
) -> Vec<CollisionHit> {
    let mut hits = Vec::new();
    for tx in pending {
        let mut overlapping = Vec::new();
        if pending_entity_overlaps(&tx.entity, new_entity) {
            overlapping.push(new_entity.to_string());
        }
        for path in dirty_paths {
            if pending_entity_overlaps(&tx.entity, path) {
                overlapping.push(path.clone());
            }
        }
        overlapping.sort();
        overlapping.dedup();
        if overlapping.is_empty() {
            continue;
        }
        overlapping.truncate(COLLISION_PATH_CAP);
        hits.push(CollisionHit {
            tx_id: tx.tx_id.clone(),
            entity: tx.entity.clone(),
            category: tx.category.to_string(),
            message: tx.planned_action.clone().unwrap_or_default(),
            overlapping_paths: overlapping,
        });
    }
    hits.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));
    hits
}

/// Human + greppable collision body. First line of each hit is the grep line.
pub fn format_collision_report(hits: &[CollisionHit]) -> String {
    let mut blocks = Vec::with_capacity(hits.len());
    for hit in hits {
        let mut lines = vec![format!(
            "{COLLISION_GREP_PREFIX} pending tx {} owns {}",
            hit.tx_id, hit.entity
        )];
        lines.push(format!("category: {}", hit.category));
        lines.push(format!("message: {}", hit.message));
        if !hit.overlapping_paths.is_empty() {
            lines.push("overlapping paths:".to_string());
            for path in &hit.overlapping_paths {
                lines.push(format!("  {path}"));
            }
        }
        blocks.push(lines.join("\n"));
    }
    blocks.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, Transaction};
    use rstest::rstest;

    fn pending_tx(id: &str, entity: &str, message: &str) -> Transaction {
        Transaction {
            tx_id: id.to_string(),
            operation_id: None,
            status: "PENDING".into(),
            category: Category::Feature,
            entity: entity.into(),
            entity_normalized: entity.replace('\\', "/").to_lowercase(),
            planned_action: Some(message.into()),
            session_id: "test".into(),
            source: "test".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            resolved_at: None,
            detected_at: None,
            drift_count: 0,
            first_seen_at: None,
            last_seen_at: None,
            issue_ref: None,
            snapshot_id: None,
        }
    }

    #[rstest]
    #[case("crates/foo", "crates/foo", true)]
    #[case("crates/foo", "crates/foo/bar.rs", true)]
    #[case("crates/foo/bar.rs", "crates/foo", true)]
    #[case("crates/foo", "crates/foo-bar", false)]
    #[case("crates/foo", "crates/foo-bar/src/lib.rs", false)]
    #[case("crates/dedupe-chrome", "crates/other", false)]
    #[case("crates/dedupe-chrome", "crates/dedupe-chrome/foo.rs", true)]
    #[case(r"crates\foo", "crates/foo/bar.rs", true)]
    #[case("crates/foo/", "crates/foo/bar.rs", true)]
    #[case("Crates/Foo", "crates/foo/bar.rs", true)]
    #[case("crates/FOO", "CRATES/foo", true)]
    #[case("", "crates/foo", false)]
    #[case("crates/foo", "", false)]
    #[case("", "", false)]
    #[case("0221-agent-skill-card", "0221-agent-skill-card", true)]
    #[case("0221-Agent-Skill-Card", "0221-agent-skill-card", true)]
    #[case("0221-agent-skill-card", "src/lib.rs", false)]
    #[case("src", "src/lib.rs", false)]
    #[case("README.md", "README.md", true)]
    #[case("README.md", "readme.md", true)]
    #[case("docs", "docs/install.md", false)]
    #[case("foo ", "foo", false)]
    #[case(" foo", "foo", false)]
    fn pending_entity_overlap_matrix(
        #[case] entity: &str,
        #[case] candidate: &str,
        #[case] expect: bool,
    ) {
        assert_eq!(
            pending_entity_overlaps(entity, candidate),
            expect,
            "entity={entity:?} candidate={candidate:?}"
        );
        assert_eq!(
            pending_entity_overlaps(candidate, entity),
            expect,
            "symmetric entity={candidate:?} candidate={entity:?}"
        );
    }

    #[test]
    fn find_start_collisions_dirty_under_pending_refuses_adjacent_entity() {
        let pending = vec![pending_tx(
            "aaa-chrome",
            "crates/dedupe-chrome",
            "chrome work",
        )];
        let dirty = vec!["crates/dedupe-chrome/foo.rs".to_string()];
        let hits = find_start_collisions(&pending, "crates/other", &dirty);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tx_id, "aaa-chrome");
        assert_eq!(hits[0].entity, "crates/dedupe-chrome");
        assert_eq!(hits[0].category, "FEATURE");
        assert_eq!(hits[0].message, "chrome work");
        assert_eq!(
            hits[0].overlapping_paths,
            vec!["crates/dedupe-chrome/foo.rs"]
        );
        let report = format_collision_report(&hits);
        assert_eq!(
            report,
            "[Ledgerful] Collision: pending tx aaa-chrome owns crates/dedupe-chrome\ncategory: FEATURE\nmessage: chrome work\noverlapping paths:\n  crates/dedupe-chrome/foo.rs"
        );
    }

    #[test]
    fn format_collision_report_always_prints_message_when_planned_action_absent() {
        let mut tx = pending_tx("tx-empty", "crates/foo", "");
        tx.planned_action = None;
        let hits = find_start_collisions(&[tx], "crates/foo/bar.rs", &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message, "");
        let report = format_collision_report(&hits);
        assert_eq!(
            report,
            "[Ledgerful] Collision: pending tx tx-empty owns crates/foo\ncategory: FEATURE\nmessage: \noverlapping paths:\n  crates/foo/bar.rs"
        );
    }

    #[test]
    fn find_start_collisions_overlapping_new_entity_without_dirty() {
        let pending = vec![pending_tx("tx-chrome", "crates/dedupe-chrome", "lock")];
        let hits = find_start_collisions(&pending, "crates/dedupe-chrome/nested", &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].overlapping_paths,
            vec!["crates/dedupe-chrome/nested"]
        );
    }

    #[test]
    fn find_start_collisions_disjoint_dirty_and_entity_is_empty() {
        let pending = vec![pending_tx("tx-chrome", "crates/dedupe-chrome", "lock")];
        let dirty = vec!["crates/other/src/lib.rs".to_string()];
        let hits = find_start_collisions(&pending, "crates/other", &dirty);
        assert!(hits.is_empty(), "expected no collision, got {hits:?}");
    }

    #[test]
    fn find_start_collisions_slug_does_not_lock_src_lib() {
        let pending = vec![pending_tx("tx-slug", "0221-agent-skill-card", "docs")];
        let dirty = vec!["src/lib.rs".to_string()];
        let hits = find_start_collisions(&pending, "0222-hotspots", &dirty);
        assert!(hits.is_empty(), "slug must not prefix-match dirty files");
    }

    #[test]
    fn find_start_collisions_caps_and_sorts_paths() {
        let pending = vec![pending_tx("tx-a", "crates/foo", "lock")];
        let mut dirty = Vec::new();
        for i in (0..25).rev() {
            dirty.push(format!("crates/foo/f{i:02}.rs"));
        }
        let hits = find_start_collisions(&pending, "crates/bar", &dirty);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].overlapping_paths.len(), COLLISION_PATH_CAP);
        let mut sorted = hits[0].overlapping_paths.clone();
        sorted.sort();
        assert_eq!(hits[0].overlapping_paths, sorted);
    }

    #[test]
    fn find_start_collisions_sorts_hits_by_tx_id() {
        let pending = vec![
            pending_tx("z-tx", "crates/foo", "z"),
            pending_tx("a-tx", "crates/foo", "a"),
        ];
        let dirty = vec!["crates/foo/x.rs".to_string()];
        let hits = find_start_collisions(&pending, "crates/other", &dirty);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].tx_id, "a-tx");
        assert_eq!(hits[1].tx_id, "z-tx");
    }
}
