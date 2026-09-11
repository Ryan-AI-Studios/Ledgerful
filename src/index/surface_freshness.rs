//! Per-surface freshness rows for inspection (`index --check`, change-context).
//!
//! File content-hash freshness and derived-graph freshness are different
//! facts. This module classifies them without walking the tree or writing
//! state. Callers pass a precomputed `files_stale` (or `None` to omit
//! `files`/`symbols`) and table/Cozo probes.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::state::reports::ImpactFreshness;

/// Exact `scan --pr` `scopeNote` when `changeCount > 0`.
pub const SCOPE_NOTE: &str =
    "selected range is nonempty; treeClean is range emptiness, not working-tree dirtiness";

/// Pinned symbols reason (not an independent parser clock).
pub const SYMBOLS_REASON: &str = "derived from indexed source files; matches file content-hash";

pub const REFRESH_INDEX_INCREMENTAL: &str = "ledgerful index --incremental";
pub const REFRESH_SCAN_IMPACT: &str = "ledgerful scan --impact";

const COMPARED_HEAD_UNKNOWN: &str = "compared head unknown; not force-stale";
const INDEX_HEAD_UNKNOWN: &str = "index head unknown; not force-stale";

/// Shared 0135 head-pair matrix (verify wraps this as `MappingFreshness`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingHeadPair {
    Ok,
    Empty,
    HeadMismatch,
    ComparedHeadMissing,
}

/// Classify mapping/route row-count + heads. `row_count == 0` is Empty
/// (verify: missing table and empty table are the same class).
pub fn classify_mapping_head_pair(
    row_count: i64,
    indexed_head: Option<&str>,
    compared_head: Option<&str>,
) -> MappingHeadPair {
    if row_count == 0 {
        return MappingHeadPair::Empty;
    }
    match (compared_head, indexed_head) {
        (Some(compared), Some(indexed)) if compared != indexed => MappingHeadPair::HeadMismatch,
        (Some(_), Some(_)) => MappingHeadPair::Ok,
        // Indexed head missing + populated: unknown ≠ force-stale (0135).
        (Some(_), None) => MappingHeadPair::Ok,
        (None, Some(_)) => MappingHeadPair::ComparedHeadMissing,
        (None, None) => MappingHeadPair::Ok,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceFreshnessStatus {
    Available,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceFreshnessSource {
    ContentHash,
    IndexHead,
    TableMissing,
    NotConfigured,
    QueryFailed,
    NeverIndexed,
    ImpactCache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceFreshness {
    pub id: String,
    pub status: SurfaceFreshnessStatus,
    pub source: SurfaceFreshnessSource,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compared_head: Option<String>,
}

impl SurfaceFreshnessStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Stale => "stale",
            Self::Unavailable => "unavailable",
        }
    }
}

impl SurfaceFreshness {
    pub fn is_available(&self) -> bool {
        self.status == SurfaceFreshnessStatus::Available
    }

    pub fn human_lag_line(&self) -> String {
        format!(
            "Surface {}: {} — {}",
            self.id,
            self.status.as_str(),
            self.reason
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceTableProbe {
    pub exists: bool,
    pub rows: i64,
    pub query_failed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingsProbe {
    NotConfigured,
    QueryFailed,
    TableMissing,
    Rows(usize),
}

#[derive(Debug, Clone)]
pub struct ClassifySurfaceFreshness<'a> {
    pub files_stale: Option<usize>,
    pub compared_head: Option<&'a str>,
    pub indexed_head: Option<&'a str>,
    pub mapping: SurfaceTableProbe,
    pub routes: SurfaceTableProbe,
    pub embeddings: EmbeddingsProbe,
    pub permission_denied: bool,
}

pub fn classify_surface_freshness(input: ClassifySurfaceFreshness<'_>) -> Vec<SurfaceFreshness> {
    let mut rows = Vec::new();
    if let Some(stale) = input.files_stale {
        rows.push(files_row(stale, input.permission_denied));
        rows.push(symbols_row(stale, input.permission_denied));
    }
    rows.push(derived_table_row(
        "mapping",
        "mappings",
        input.mapping,
        input.indexed_head,
        input.compared_head,
        input.permission_denied,
    ));
    rows.push(derived_table_row(
        "routes",
        "routes",
        input.routes,
        input.indexed_head,
        input.compared_head,
        input.permission_denied,
    ));
    rows.push(embeddings_row(
        input.embeddings,
        input.indexed_head,
        input.compared_head,
        input.permission_denied,
    ));
    rows
}

pub fn impact_freshness_row(
    freshness: &ImpactFreshness,
    permission_denied: bool,
) -> SurfaceFreshness {
    let refresh = refresh_cmd(REFRESH_SCAN_IMPACT, false, permission_denied);
    match freshness {
        ImpactFreshness::Missing => SurfaceFreshness {
            id: "impact".into(),
            status: SurfaceFreshnessStatus::Unavailable,
            source: SurfaceFreshnessSource::ImpactCache,
            reason: "no latest-impact.json".into(),
            refresh,
            indexed_head: None,
            compared_head: None,
        },
        ImpactFreshness::CurrentClean | ImpactFreshness::CurrentDirty => SurfaceFreshness {
            id: "impact".into(),
            status: SurfaceFreshnessStatus::Available,
            source: SurfaceFreshnessSource::ImpactCache,
            reason: "latest-impact.json matches live HEAD".into(),
            refresh: None,
            indexed_head: None,
            compared_head: None,
        },
        ImpactFreshness::Stale { reason } => SurfaceFreshness {
            id: "impact".into(),
            status: SurfaceFreshnessStatus::Stale,
            source: SurfaceFreshnessSource::ImpactCache,
            reason: reason.clone(),
            refresh,
            indexed_head: None,
            compared_head: None,
        },
        ImpactFreshness::Corrupt { reason } => SurfaceFreshness {
            id: "impact".into(),
            status: SurfaceFreshnessStatus::Unavailable,
            source: SurfaceFreshnessSource::ImpactCache,
            reason: reason.clone(),
            refresh,
            indexed_head: None,
            compared_head: None,
        },
    }
}

pub fn probe_named_table(conn: &Connection, table: &str) -> SurfaceTableProbe {
    debug_assert!(
        table == "test_mapping" || table == "api_routes" || table == "index_metadata",
        "unexpected table probe: {table}"
    );
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        return SurfaceTableProbe {
            exists: false,
            rows: 0,
            query_failed: false,
        };
    }
    let sql = format!("SELECT count(*) FROM {table}");
    match conn.query_row(&sql, [], |row| row.get::<_, i64>(0)) {
        Ok(rows) => SurfaceTableProbe {
            exists: true,
            rows,
            query_failed: false,
        },
        Err(_) => SurfaceTableProbe {
            exists: true,
            rows: 0,
            query_failed: true,
        },
    }
}

pub fn read_indexed_head(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT value FROM index_metadata WHERE key = 'head_hash'",
        [],
        |row| row.get(0),
    )
    .ok()
}

pub fn embeddings_probe_from_storage(
    configured: bool,
    storage: &crate::state::storage::StorageManager,
) -> EmbeddingsProbe {
    if !configured {
        return EmbeddingsProbe::NotConfigured;
    }
    let Some(cozo) = storage.cozo() else {
        return EmbeddingsProbe::QueryFailed;
    };
    let relations = match cozo.get_relations() {
        Ok(r) => r,
        Err(_) => return EmbeddingsProbe::QueryFailed,
    };
    if !relations.contains(&"snippet_embedding".to_string()) {
        return EmbeddingsProbe::TableMissing;
    }
    match crate::semantic::vector_store::count_snippet_embedding_rows(cozo) {
        Ok(n) => EmbeddingsProbe::Rows(n),
        Err(_) => EmbeddingsProbe::QueryFailed,
    }
}

pub fn format_human_lag_lines(rows: &[SurfaceFreshness]) -> Vec<String> {
    rows.iter()
        .filter(|r| !r.is_available())
        .map(SurfaceFreshness::human_lag_line)
        .collect()
}

fn files_row(stale: usize, permission_denied: bool) -> SurfaceFreshness {
    if stale == 0 {
        SurfaceFreshness {
            id: "files".into(),
            status: SurfaceFreshnessStatus::Available,
            source: SurfaceFreshnessSource::ContentHash,
            reason: "content-hash matches indexed files".into(),
            refresh: None,
            indexed_head: None,
            compared_head: None,
        }
    } else {
        SurfaceFreshness {
            id: "files".into(),
            status: SurfaceFreshnessStatus::Stale,
            source: SurfaceFreshnessSource::ContentHash,
            reason: format!("{stale} file(s) drifted from indexed content-hash"),
            refresh: refresh_cmd(REFRESH_INDEX_INCREMENTAL, false, permission_denied),
            indexed_head: None,
            compared_head: None,
        }
    }
}

fn symbols_row(stale: usize, permission_denied: bool) -> SurfaceFreshness {
    let mut row = files_row(stale, permission_denied);
    row.id = "symbols".into();
    row.reason = SYMBOLS_REASON.to_string();
    row
}

fn derived_table_row(
    id: &str,
    noun: &str,
    probe: SurfaceTableProbe,
    indexed_head: Option<&str>,
    compared_head: Option<&str>,
    permission_denied: bool,
) -> SurfaceFreshness {
    if probe.query_failed {
        return SurfaceFreshness {
            id: id.into(),
            status: SurfaceFreshnessStatus::Unavailable,
            source: SurfaceFreshnessSource::QueryFailed,
            reason: format!("{id} table query failed"),
            refresh: refresh_cmd(REFRESH_INDEX_INCREMENTAL, false, permission_denied),
            indexed_head: omit_head(indexed_head),
            compared_head: omit_head(compared_head),
        };
    }
    if !probe.exists {
        return SurfaceFreshness {
            id: id.into(),
            status: SurfaceFreshnessStatus::Unavailable,
            source: SurfaceFreshnessSource::TableMissing,
            reason: format!("{id} table missing"),
            refresh: refresh_cmd(REFRESH_INDEX_INCREMENTAL, false, permission_denied),
            indexed_head: omit_head(indexed_head),
            compared_head: omit_head(compared_head),
        };
    }
    if probe.rows == 0 {
        let reason = if indexed_head.is_some() {
            format!("0 {noun} registered; up to date with index head")
        } else {
            format!("0 {noun} registered; {INDEX_HEAD_UNKNOWN}")
        };
        return SurfaceFreshness {
            id: id.into(),
            status: SurfaceFreshnessStatus::Available,
            source: SurfaceFreshnessSource::IndexHead,
            reason,
            refresh: None,
            indexed_head: omit_head(indexed_head),
            compared_head: omit_head(compared_head),
        };
    }

    match classify_mapping_head_pair(probe.rows, indexed_head, compared_head) {
        MappingHeadPair::HeadMismatch => SurfaceFreshness {
            id: id.into(),
            status: SurfaceFreshnessStatus::Stale,
            source: SurfaceFreshnessSource::IndexHead,
            reason: head_mismatch_reason(indexed_head, compared_head),
            refresh: refresh_cmd(REFRESH_INDEX_INCREMENTAL, false, permission_denied),
            indexed_head: omit_head(indexed_head),
            compared_head: omit_head(compared_head),
        },
        MappingHeadPair::ComparedHeadMissing => SurfaceFreshness {
            id: id.into(),
            status: SurfaceFreshnessStatus::Available,
            source: SurfaceFreshnessSource::IndexHead,
            reason: COMPARED_HEAD_UNKNOWN.into(),
            refresh: None,
            indexed_head: omit_head(indexed_head),
            compared_head: None,
        },
        MappingHeadPair::Ok | MappingHeadPair::Empty => {
            let reason = if indexed_head.is_none() {
                INDEX_HEAD_UNKNOWN.to_string()
            } else {
                format!("{id} matches compared head")
            };
            SurfaceFreshness {
                id: id.into(),
                status: SurfaceFreshnessStatus::Available,
                source: SurfaceFreshnessSource::IndexHead,
                reason,
                refresh: None,
                indexed_head: omit_head(indexed_head),
                compared_head: omit_head(compared_head),
            }
        }
    }
}

fn embeddings_row(
    probe: EmbeddingsProbe,
    indexed_head: Option<&str>,
    compared_head: Option<&str>,
    permission_denied: bool,
) -> SurfaceFreshness {
    match probe {
        EmbeddingsProbe::NotConfigured => SurfaceFreshness {
            id: "embeddings".into(),
            status: SurfaceFreshnessStatus::Unavailable,
            source: SurfaceFreshnessSource::NotConfigured,
            reason: "embedding backend not configured".into(),
            refresh: None,
            indexed_head: omit_head(indexed_head),
            compared_head: omit_head(compared_head),
        },
        EmbeddingsProbe::QueryFailed => SurfaceFreshness {
            id: "embeddings".into(),
            status: SurfaceFreshnessStatus::Unavailable,
            source: SurfaceFreshnessSource::QueryFailed,
            reason: "embedding store query failed".into(),
            refresh: refresh_cmd(REFRESH_INDEX_INCREMENTAL, false, permission_denied),
            indexed_head: omit_head(indexed_head),
            compared_head: omit_head(compared_head),
        },
        EmbeddingsProbe::TableMissing => SurfaceFreshness {
            id: "embeddings".into(),
            status: SurfaceFreshnessStatus::Unavailable,
            source: SurfaceFreshnessSource::TableMissing,
            reason: "snippet_embedding relation missing".into(),
            refresh: refresh_cmd(REFRESH_INDEX_INCREMENTAL, false, permission_denied),
            indexed_head: omit_head(indexed_head),
            compared_head: omit_head(compared_head),
        },
        EmbeddingsProbe::Rows(n) => derived_table_row(
            "embeddings",
            "embeddings",
            SurfaceTableProbe {
                exists: true,
                rows: n as i64,
                query_failed: false,
            },
            indexed_head,
            compared_head,
            permission_denied,
        ),
    }
}

fn head_mismatch_reason(indexed_head: Option<&str>, compared_head: Option<&str>) -> String {
    let indexed = indexed_head.unwrap_or("unknown");
    let compared = compared_head.unwrap_or("unknown");
    format!("index head_hash ({indexed}) ≠ compared head ({compared})")
}

fn omit_head(head: Option<&str>) -> Option<String> {
    head.filter(|h| !h.is_empty()).map(|h| h.to_string())
}

fn refresh_cmd(cmd: &str, available: bool, permission_denied: bool) -> Option<String> {
    if available || permission_denied {
        None
    } else {
        Some(cmd.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping_stale_input() -> ClassifySurfaceFreshness<'static> {
        ClassifySurfaceFreshness {
            files_stale: Some(0),
            compared_head: Some("96d46c10"),
            indexed_head: Some("250c7afe"),
            mapping: SurfaceTableProbe {
                exists: true,
                rows: 3,
                query_failed: false,
            },
            routes: SurfaceTableProbe {
                exists: true,
                rows: 0,
                query_failed: false,
            },
            embeddings: EmbeddingsProbe::NotConfigured,
            permission_denied: false,
        }
    }

    fn row<'a>(rows: &'a [SurfaceFreshness], id: &str) -> &'a SurfaceFreshness {
        rows.iter().find(|r| r.id == id).expect("row")
    }

    #[test]
    fn surface_freshness_files_available_mapping_stale() {
        let rows = classify_surface_freshness(mapping_stale_input());
        let files = row(&rows, "files");
        assert_eq!(files.status, SurfaceFreshnessStatus::Available);
        assert_eq!(files.source, SurfaceFreshnessSource::ContentHash);
        let mapping = row(&rows, "mapping");
        assert_eq!(mapping.status, SurfaceFreshnessStatus::Stale);
        assert_eq!(mapping.source, SurfaceFreshnessSource::IndexHead);
        assert!(
            mapping.reason.contains("compared head"),
            "expected compared-head vocab: {}",
            mapping.reason
        );
        assert_eq!(mapping.refresh.as_deref(), Some(REFRESH_INDEX_INCREMENTAL));
        assert_eq!(row(&rows, "symbols").reason, SYMBOLS_REASON);
    }

    #[test]
    fn surface_freshness_does_not_force_stale_on_missing_index_head() {
        let mut input = mapping_stale_input();
        input.indexed_head = None;
        let rows = classify_surface_freshness(input);
        let mapping = row(&rows, "mapping");
        assert_eq!(mapping.status, SurfaceFreshnessStatus::Available);
        assert!(mapping.reason.contains(INDEX_HEAD_UNKNOWN));
        assert!(mapping.refresh.is_none());
    }

    #[test]
    fn surface_freshness_zero_row_routes_available() {
        let rows = classify_surface_freshness(mapping_stale_input());
        let routes = row(&rows, "routes");
        assert_eq!(routes.status, SurfaceFreshnessStatus::Available);
        assert_eq!(
            routes.reason,
            "0 routes registered; up to date with index head"
        );
        assert!(routes.refresh.is_none());
    }

    #[test]
    fn surface_freshness_compared_head_unknown_not_force_stale() {
        let mut input = mapping_stale_input();
        input.compared_head = None;
        let rows = classify_surface_freshness(input);
        let mapping = row(&rows, "mapping");
        assert_eq!(mapping.status, SurfaceFreshnessStatus::Available);
        assert_eq!(mapping.reason, COMPARED_HEAD_UNKNOWN);
        assert!(mapping.refresh.is_none());
        assert!(mapping.compared_head.is_none());
    }

    #[test]
    fn surface_freshness_embeddings_not_configured() {
        let rows = classify_surface_freshness(mapping_stale_input());
        let embeddings = row(&rows, "embeddings");
        assert_eq!(embeddings.status, SurfaceFreshnessStatus::Unavailable);
        assert_eq!(embeddings.source, SurfaceFreshnessSource::NotConfigured);
        assert!(embeddings.refresh.is_none());
    }

    #[test]
    fn surface_freshness_routes_table_missing_unavailable() {
        let mut input = mapping_stale_input();
        input.routes = SurfaceTableProbe {
            exists: false,
            rows: 0,
            query_failed: false,
        };
        let rows = classify_surface_freshness(input);
        let routes = row(&rows, "routes");
        assert_eq!(routes.status, SurfaceFreshnessStatus::Unavailable);
        assert_eq!(routes.source, SurfaceFreshnessSource::TableMissing);
    }

    #[test]
    fn surface_freshness_omits_files_when_stale_unknown() {
        let mut input = mapping_stale_input();
        input.files_stale = None;
        let rows = classify_surface_freshness(input);
        assert!(rows.iter().all(|r| r.id != "files" && r.id != "symbols"));
        assert!(rows.iter().any(|r| r.id == "mapping"));
    }

    #[test]
    fn surface_freshness_permission_denied_omits_refresh() {
        let mut input = mapping_stale_input();
        input.permission_denied = true;
        let rows = classify_surface_freshness(input);
        assert!(row(&rows, "mapping").refresh.is_none());
    }

    #[test]
    fn classify_mapping_head_pair_matches_0135() {
        assert_eq!(
            classify_mapping_head_pair(0, Some("a"), Some("b")),
            MappingHeadPair::Empty
        );
        assert_eq!(
            classify_mapping_head_pair(2, Some("a"), Some("a")),
            MappingHeadPair::Ok
        );
        assert_eq!(
            classify_mapping_head_pair(2, Some("a"), Some("b")),
            MappingHeadPair::HeadMismatch
        );
        assert_eq!(
            classify_mapping_head_pair(2, None, Some("b")),
            MappingHeadPair::Ok
        );
        assert_eq!(
            classify_mapping_head_pair(2, Some("a"), None),
            MappingHeadPair::ComparedHeadMissing
        );
        assert_eq!(
            classify_mapping_head_pair(2, None, None),
            MappingHeadPair::Ok
        );
    }

    #[test]
    fn index_check_does_not_write_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("t.db");
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute_batch(
            "CREATE TABLE test_mapping (id INTEGER);
             CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO test_mapping (id) VALUES (1);
             INSERT INTO index_metadata (key, value) VALUES ('head_hash', 'abc');",
        )
        .expect("seed");
        drop(conn);
        let before = std::fs::metadata(&db)
            .expect("meta")
            .modified()
            .expect("mtime");
        let conn = rusqlite::Connection::open(&db).expect("reopen");
        let mapping = probe_named_table(&conn, "test_mapping");
        let head = read_indexed_head(&conn);
        drop(conn);
        assert!(mapping.exists);
        assert_eq!(mapping.rows, 1);
        assert_eq!(head.as_deref(), Some("abc"));
        let after = std::fs::metadata(&db)
            .expect("meta2")
            .modified()
            .expect("mtime2");
        assert_eq!(before, after, "probe must not rewrite the db file");
    }
}
