//! Ingest SCIP reference occurrences as `structural_edges` on native ids (0095).
//!
//! Precedence (DoD-8): when SCIP and native both have an edge for the same
//! `(caller_symbol_id, callee_symbol_id)` — **regardless of `call_kind`** —
//! prefer SCIP evidence: update the existing row's `evidence` to the SCIP
//! marker rather than inserting a duplicate. Native method edges are often
//! `METHOD_CALL` while SCIP emits `DIRECT`; matching only on call_kind would
//! leave both rows and skip the evidence upgrade.

use crate::index::call_graph::{CallKind, ResolutionStatus};
use crate::scip::range::parse_scip_range;
use crate::scip::resolver::{
    SCIP_EDGE_EVIDENCE, ScipNativeResolver, is_definition_role, load_native_spans_for_file,
    resolve_caller_for_reference,
};
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use tracing::warn;

/// Result of SCIP edge augmentation (for JSON / logging).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ScipEdgeStats {
    pub edges_added: usize,
    pub edges_updated: usize,
    pub edges_skipped_unmapped: usize,
    pub edges_skipped_duplicate: usize,
    pub definitions_mapped: usize,
    pub definitions_seen: usize,
    pub files_skipped: usize,
    pub files_processed: usize,
}

/// Pending edge before insert / precedence update.
struct PendingEdge {
    caller_symbol_id: i64,
    caller_file_id: i64,
    callee_symbol_id: i64,
    callee_file_id: Option<i64>,
    call_kind: String,
    evidence: String,
}

/// Build the resolver from Definition occurrences, then insert reference edges.
///
/// Does **not** write `project_symbols`. Call only after native symbols and
/// native call-graph edges exist.
pub fn augment_edges_from_scip(
    conn: &Connection,
    documents: &[scip::types::Document],
    path_to_file_id: &dyn Fn(&str) -> Option<i64>,
) -> Result<ScipEdgeStats> {
    let mut stats = ScipEdgeStats::default();
    let mut resolver = ScipNativeResolver::new();

    // Cache native spans per file_id
    let mut spans_cache: std::collections::HashMap<
        i64,
        Vec<crate::scip::resolver::NativeSymbolSpan>,
    > = std::collections::HashMap::new();

    // Pass 1: definition → native mapping
    for document in documents {
        let Some(file_id) = path_to_file_id(&document.relative_path) else {
            stats.files_skipped += 1;
            continue;
        };
        stats.files_processed += 1;

        let spans = spans_cache
            .entry(file_id)
            .or_insert_with(|| load_native_spans_for_file(conn, file_id).unwrap_or_default());

        for occurrence in &document.occurrences {
            if occurrence.symbol.is_empty() || occurrence.symbol.starts_with("local ") {
                continue;
            }
            if !is_definition_role(occurrence.symbol_roles) {
                continue;
            }
            let range = match parse_scip_range(&occurrence.range) {
                Ok(r) => r,
                Err(e) => {
                    warn!("SCIP definition range invalid: {e}");
                    continue;
                }
            };
            resolver.try_map_definition(&occurrence.symbol, &range, spans);
        }
    }

    stats.definitions_mapped = resolver.definitions_mapped;
    stats.definitions_seen = resolver.definitions_seen;

    // Pass 2: reference occurrences → edges
    let mut pending: Vec<PendingEdge> = Vec::new();

    for document in documents {
        let Some(file_id) = path_to_file_id(&document.relative_path) else {
            continue;
        };
        let spans = match spans_cache.get(&file_id) {
            Some(s) => s.as_slice(),
            None => continue,
        };

        for occurrence in &document.occurrences {
            if occurrence.symbol.is_empty() || occurrence.symbol.starts_with("local ") {
                continue;
            }
            // References only (not definitions)
            if is_definition_role(occurrence.symbol_roles) {
                continue;
            }

            let Some(caller_id) =
                resolve_caller_for_reference(&occurrence.range, &occurrence.enclosing_range, spans)
            else {
                stats.edges_skipped_unmapped += 1;
                continue;
            };

            let Some(callee_id) = resolver.get(&occurrence.symbol) else {
                stats.edges_skipped_unmapped += 1;
                continue;
            };

            // Resolve callee file_id for the edge row
            let callee_file_id: Option<i64> = conn
                .query_row(
                    "SELECT file_id FROM project_symbols WHERE id = ?1",
                    [callee_id],
                    |row| row.get(0),
                )
                .ok();

            pending.push(PendingEdge {
                caller_symbol_id: caller_id,
                caller_file_id: file_id,
                callee_symbol_id: callee_id,
                callee_file_id,
                call_kind: CallKind::Direct.as_str().to_string(),
                evidence: SCIP_EDGE_EVIDENCE.to_string(),
            });
        }
    }

    // Deterministic insert order; dedup by (caller, callee) only — call_kind
    // is not part of identity for SCIP precedence.
    pending.sort_by_key(|e| (e.caller_symbol_id, e.callee_symbol_id));
    pending.dedup_by_key(|e| (e.caller_symbol_id, e.callee_symbol_id));

    for edge in pending {
        apply_edge_with_precedence(conn, &edge, &mut stats)?;
    }

    Ok(stats)
}

/// Insert or update per DoD-8 precedence: SCIP evidence wins over native.
///
/// Match key is `(caller_symbol_id, callee_symbol_id)` regardless of
/// `call_kind`, so a native `METHOD_CALL` row is upgraded rather than
/// duplicated when SCIP would emit `DIRECT`.
fn apply_edge_with_precedence(
    conn: &Connection,
    edge: &PendingEdge,
    stats: &mut ScipEdgeStats,
) -> Result<()> {
    // Any existing edge for this caller→callee pair (any call_kind)
    let existing: Vec<(i64, Option<String>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, evidence FROM structural_edges \
                 WHERE caller_symbol_id = ?1 AND callee_symbol_id = ?2 \
                 ORDER BY id",
            )
            .into_diagnostic()?;
        let rows = stmt
            .query_map(
                rusqlite::params![edge.caller_symbol_id, edge.callee_symbol_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .into_diagnostic()?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r.into_diagnostic()?);
        }
        v
    };

    if existing.is_empty() {
        conn.execute(
            "INSERT INTO structural_edges \
             (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, \
              unresolved_callee, call_kind, resolution_status, confidence, evidence) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                edge.caller_symbol_id,
                edge.caller_file_id,
                edge.callee_symbol_id,
                edge.callee_file_id,
                edge.call_kind,
                ResolutionStatus::Resolved.as_str(),
                1.0_f64,
                edge.evidence,
            ],
        )
        .into_diagnostic()?;
        stats.edges_added += 1;
        return Ok(());
    }

    // Prefer SCIP: update first row's evidence if not already SCIP; leave
    // call_kind as-is (native METHOD_CALL stays METHOD_CALL). No new insert.
    let (id, evidence) = &existing[0];
    let already_scip = evidence.as_deref().is_some_and(|e| e.starts_with("scip:"));
    if already_scip {
        stats.edges_skipped_duplicate += 1;
    } else {
        conn.execute(
            "UPDATE structural_edges SET evidence = ?1, resolution_status = ?2, confidence = ?3 \
             WHERE id = ?4",
            rusqlite::params![
                edge.evidence,
                ResolutionStatus::Resolved.as_str(),
                1.0_f64,
                id
            ],
        )
        .into_diagnostic()?;
        stats.edges_updated += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_files (file_path, content_hash, language, file_size, last_indexed_at) \
             VALUES (?1, 'h', 'rust', 0, datetime('now'))",
            [path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_symbol(conn: &Connection, file_id: i64, name: &str, start: i32, end: i32) -> i64 {
        conn.execute(
            "INSERT INTO project_symbols \
             (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, is_public, \
              line_start, line_end, confidence, last_indexed_at) \
             VALUES (?1, ?2, ?3, 'Function', 'INTERNAL', 1, ?4, ?5, 1.0, datetime('now'))",
            rusqlite::params![file_id, name, name, start, end],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_native_edge(conn: &Connection, caller: i64, callee: i64, file_id: i64) {
        conn.execute(
            "INSERT INTO structural_edges \
             (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, \
              call_kind, resolution_status, confidence, evidence) \
             VALUES (?1, ?2, ?3, ?2, 'DIRECT', 'RESOLVED', 0.9, 'call_expr:foo')",
            rusqlite::params![caller, file_id, callee],
        )
        .unwrap();
    }

    fn insert_native_edge_kind(
        conn: &Connection,
        caller: i64,
        callee: i64,
        file_id: i64,
        call_kind: &str,
    ) {
        conn.execute(
            "INSERT INTO structural_edges \
             (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, \
              call_kind, resolution_status, confidence, evidence) \
             VALUES (?1, ?2, ?3, ?2, ?4, 'RESOLVED', 0.9, 'call_expr:method')",
            rusqlite::params![caller, file_id, callee, call_kind],
        )
        .unwrap();
    }

    /// Synthetic SCIP document: definition of `callee` + reference inside `caller`.
    /// Ranges are SCIP 0-based; native symbols use 1-based lines.
    fn synthetic_doc(path: &str, scip_symbol: &str) -> scip::types::Document {
        use crate::scip::resolver::SCIP_ROLE_DEFINITION;
        // 0-based line 29 → native line 30 (callee_fn span 30–40)
        let def = scip::types::Occurrence {
            symbol: scip_symbol.to_string(),
            symbol_roles: SCIP_ROLE_DEFINITION,
            range: vec![29, 0, 5],
            ..Default::default()
        };
        // 0-based line 9 → native line 10 (inside caller_fn 1–20)
        let r#ref = scip::types::Occurrence {
            symbol: scip_symbol.to_string(),
            symbol_roles: 0, // reference
            range: vec![9, 0, 5],
            ..Default::default()
        };
        scip::types::Document {
            relative_path: path.to_string(),
            occurrences: vec![def, r#ref],
            ..Default::default()
        }
    }

    #[test]
    fn precedence_updates_native_evidence_to_scip() {
        let conn = setup_db();
        let fid = insert_file(&conn, "src/a.rs");
        let caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);
        let callee = insert_symbol(&conn, fid, "callee_fn", 30, 40);
        insert_native_edge(&conn, caller, callee, fid);

        let edge = PendingEdge {
            caller_symbol_id: caller,
            caller_file_id: fid,
            callee_symbol_id: callee,
            callee_file_id: Some(fid),
            call_kind: "DIRECT".to_string(),
            evidence: SCIP_EDGE_EVIDENCE.to_string(),
        };
        let mut stats = ScipEdgeStats::default();
        apply_edge_with_precedence(&conn, &edge, &mut stats).unwrap();

        assert_eq!(stats.edges_updated, 1);
        assert_eq!(stats.edges_added, 0);

        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM structural_edges WHERE caller_symbol_id = ?1",
                [caller],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(evidence, SCIP_EDGE_EVIDENCE);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM structural_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "must not duplicate");
    }

    #[test]
    fn precedence_updates_method_call_regardless_of_call_kind() {
        // Native METHOD_CALL + SCIP DIRECT for same (caller, callee) → update, no dup
        let conn = setup_db();
        let fid = insert_file(&conn, "src/method.rs");
        let caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);
        let callee = insert_symbol(&conn, fid, "callee_fn", 30, 40);
        insert_native_edge_kind(&conn, caller, callee, fid, "METHOD_CALL");

        let edge = PendingEdge {
            caller_symbol_id: caller,
            caller_file_id: fid,
            callee_symbol_id: callee,
            callee_file_id: Some(fid),
            call_kind: CallKind::Direct.as_str().to_string(),
            evidence: SCIP_EDGE_EVIDENCE.to_string(),
        };
        let mut stats = ScipEdgeStats::default();
        apply_edge_with_precedence(&conn, &edge, &mut stats).unwrap();

        assert_eq!(stats.edges_updated, 1);
        assert_eq!(stats.edges_added, 0);

        let (evidence, kind): (String, String) = conn
            .query_row(
                "SELECT evidence, call_kind FROM structural_edges WHERE caller_symbol_id = ?1",
                [caller],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(evidence, SCIP_EDGE_EVIDENCE);
        assert_eq!(kind, "METHOD_CALL", "leave native call_kind as-is");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM structural_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "must not insert DIRECT duplicate");
    }

    #[test]
    fn insert_when_no_native_edge() {
        let conn = setup_db();
        let fid = insert_file(&conn, "src/b.rs");
        let caller = insert_symbol(&conn, fid, "a", 1, 10);
        let callee = insert_symbol(&conn, fid, "b", 20, 30);

        let edge = PendingEdge {
            caller_symbol_id: caller,
            caller_file_id: fid,
            callee_symbol_id: callee,
            callee_file_id: Some(fid),
            call_kind: "DIRECT".to_string(),
            evidence: SCIP_EDGE_EVIDENCE.to_string(),
        };
        let mut stats = ScipEdgeStats::default();
        apply_edge_with_precedence(&conn, &edge, &mut stats).unwrap();
        assert_eq!(stats.edges_added, 1);

        let status: String = conn
            .query_row(
                "SELECT resolution_status FROM structural_edges WHERE caller_symbol_id = ?1",
                [caller],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "RESOLVED");
    }

    #[test]
    fn no_project_symbols_inserts_from_edge_path() {
        // Sanity: augment with empty documents must not touch project_symbols
        let conn = setup_db();
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_symbols", [], |r| r.get(0))
            .unwrap();
        let stats = augment_edges_from_scip(&conn, &[], &|_| None).unwrap();
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(stats.edges_added, 0);
    }

    #[test]
    fn augment_from_synthetic_scip_inserts_scip_ref_edge() {
        let conn = setup_db();
        let path = "src/lib.rs";
        let fid = insert_file(&conn, path);
        let _caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);
        let _callee = insert_symbol(&conn, fid, "callee_fn", 30, 40);

        let docs = vec![synthetic_doc(
            path,
            "rust-analyzer cargo test 0.1 caller_fn/",
        )];
        let stats = augment_edges_from_scip(&conn, &docs, &|rel| {
            if rel == path { Some(fid) } else { None }
        })
        .unwrap();

        assert_eq!(stats.definitions_mapped, 1);
        assert_eq!(stats.edges_added, 1);
        assert_eq!(stats.edges_updated, 0);

        let evidence: String = conn
            .query_row("SELECT evidence FROM structural_edges LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(evidence, SCIP_EDGE_EVIDENCE);

        let status: String = conn
            .query_row(
                "SELECT resolution_status FROM structural_edges LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "RESOLVED");
    }

    #[test]
    fn augment_prefers_scip_evidence_on_existing_native_edge() {
        let conn = setup_db();
        let path = "src/pair.rs";
        let fid = insert_file(&conn, path);
        let caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);
        let callee = insert_symbol(&conn, fid, "callee_fn", 30, 40);
        insert_native_edge_kind(&conn, caller, callee, fid, "METHOD_CALL");

        let docs = vec![synthetic_doc(path, "rust-analyzer cargo test 0.1 pair/")];
        let stats = augment_edges_from_scip(&conn, &docs, &|rel| {
            if rel == path { Some(fid) } else { None }
        })
        .unwrap();

        assert_eq!(stats.definitions_mapped, 1);
        assert_eq!(stats.edges_updated, 1);
        assert_eq!(stats.edges_added, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM structural_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM structural_edges WHERE caller_symbol_id = ?1",
                [caller],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(evidence, SCIP_EDGE_EVIDENCE);
    }

    #[test]
    fn reapply_is_idempotent_after_existing_scip_edges() {
        // Simulates second execute_scip_index call with residual scip:% edges
        // (hash would formerly have SkippedStale; now always re-applies).
        let conn = setup_db();
        let path = "src/again.rs";
        let fid = insert_file(&conn, path);
        let _caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);
        let _callee = insert_symbol(&conn, fid, "callee_fn", 30, 40);

        let docs = vec![synthetic_doc(path, "rust-analyzer cargo test 0.1 again/")];
        let path_res = |rel: &str| if rel == path { Some(fid) } else { None };

        let first = augment_edges_from_scip(&conn, &docs, &path_res).unwrap();
        assert_eq!(first.edges_added, 1);

        let second = augment_edges_from_scip(&conn, &docs, &path_res).unwrap();
        assert_eq!(second.edges_added, 0);
        assert_eq!(second.edges_skipped_duplicate, 1);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM structural_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
