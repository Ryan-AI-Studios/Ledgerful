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
    ResolveCallerOutcome, SCIP_EDGE_EVIDENCE, ScipNativeResolver, is_definition_role,
    load_native_spans_for_file, resolve_caller_for_reference,
};
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use tracing::{debug, warn};

/// Cap on path-aware disagreement `debug!` samples (0157 D4/D10).
const DISAGREEMENT_SAMPLE_CAP: usize = 3;

/// Result of SCIP edge augmentation (for JSON / logging).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ScipEdgeStats {
    pub edges_added: usize,
    pub edges_updated: usize,
    /// No caller after resolve **or** no callee map. Excludes disagreement /
    /// invalid-occ (exclusive accounting — 0157 D1b).
    pub edges_skipped_unmapped: usize,
    pub edges_skipped_duplicate: usize,
    /// Both enclosing and occurrence ranges mapped to different native ids.
    pub edges_skipped_enclosing_disagreement: usize,
    /// Nest-prefer recovery: enc/occ disagreed but one span strictly contained
    /// the other — chose innermost (0166). Exclusive with disagreement.
    pub edges_recovered_nest_prefer: usize,
    /// Pass-2 occurrence classic range invalid / empty.
    pub edges_skipped_invalid_occ_range: usize,
    pub definitions_mapped: usize,
    pub definitions_seen: usize,
    /// Pass-1 definition classic range invalid.
    pub definitions_skipped_invalid_range: usize,
    /// Non-empty enclosing_range failed parse; resolution fell back to occ.
    pub invalid_enclosing_fallback: usize,
    /// Pass-2 reference occurrences considered (non-def, non-local, non-empty).
    pub references_seen: usize,
    pub files_skipped: usize,
    pub files_processed: usize,
    /// Documents where every non-local occurrence had an empty classic range
    /// (typed-range contingency under scip 0.8.1 — D11 detect-only).
    pub documents_all_classic_ranges_empty: usize,
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

        let spans = match spans_cache.entry(file_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                match load_native_spans_for_file(conn, file_id) {
                    Ok(spans) => e.insert(spans),
                    Err(err) => {
                        // Do not report success over a DB read failure (Codex R2 P3).
                        return Err(err);
                    }
                }
            }
        };

        for occurrence in &document.occurrences {
            if occurrence.symbol.is_empty() || occurrence.symbol.starts_with("local ") {
                continue;
            }
            if !is_definition_role(occurrence.symbol_roles) {
                continue;
            }
            let range = match parse_scip_range(&occurrence.range) {
                Ok(r) => r,
                Err(_) => {
                    stats.definitions_skipped_invalid_range += 1;
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
    let mut disagreement_samples: usize = 0;
    let mut typed_empty_docs: usize = 0;

    for document in documents {
        // D11: classic ranges empty for all non-local non-empty symbols?
        if document_all_classic_ranges_empty(document) {
            typed_empty_docs += 1;
        }

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

            stats.references_seen += 1;

            let result =
                resolve_caller_for_reference(&occurrence.range, &occurrence.enclosing_range, spans);
            if result.used_invalid_enclosing_fallback {
                stats.invalid_enclosing_fallback += 1;
            }

            match result.outcome {
                ResolveCallerOutcome::EnclosingDisagreement {
                    enclosing_id,
                    occurrence_id,
                } => {
                    stats.edges_skipped_enclosing_disagreement += 1;
                    // D10: only format/debug under sample cap
                    if disagreement_samples < DISAGREEMENT_SAMPLE_CAP {
                        debug!(
                            path = %document.relative_path,
                            enclosing_id,
                            occurrence_id,
                            "SCIP enclosing_range disagreement sample"
                        );
                        disagreement_samples += 1;
                    }
                    // D1b: exclusive — never also unmapped
                    continue;
                }
                ResolveCallerOutcome::InvalidOccurrenceRange => {
                    stats.edges_skipped_invalid_occ_range += 1;
                    continue;
                }
                ResolveCallerOutcome::Unmapped => {
                    stats.edges_skipped_unmapped += 1;
                    continue;
                }
                ResolveCallerOutcome::Resolved(caller_id) => {
                    // Count recovery as soon as nest-prefer chose innermost
                    // (even if callee is later unmapped — recovery is visible).
                    if result.recovered_nest_prefer {
                        stats.edges_recovered_nest_prefer += 1;
                    }
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
        }
    }

    stats.documents_all_classic_ranges_empty = typed_empty_docs;

    // Deterministic insert order; dedup by (caller, callee) only — call_kind
    // is not part of identity for SCIP precedence.
    pending.sort_by_key(|e| (e.caller_symbol_id, e.callee_symbol_id));
    pending.dedup_by_key(|e| (e.caller_symbol_id, e.callee_symbol_id));

    for edge in pending {
        apply_edge_with_precedence(conn, &edge, &mut stats)?;
    }

    emit_skip_summary_warn(&stats);
    emit_typed_empty_range_warn(&stats);

    Ok(stats)
}

/// D3: one summary WARN iff disagreement and/or invalid-range counts > 0.
/// Unmapped-only / duplicate-only must not trigger this WARN.
fn emit_skip_summary_warn(stats: &ScipEdgeStats) {
    let policy_skips = stats.edges_skipped_enclosing_disagreement
        + stats.edges_skipped_invalid_occ_range
        + stats.definitions_skipped_invalid_range;
    if policy_skips == 0 {
        return;
    }
    warn!(
        edges_skipped_enclosing_disagreement = stats.edges_skipped_enclosing_disagreement,
        edges_skipped_invalid_occ_range = stats.edges_skipped_invalid_occ_range,
        definitions_skipped_invalid_range = stats.definitions_skipped_invalid_range,
        edges_skipped_unmapped = stats.edges_skipped_unmapped,
        references_seen = stats.references_seen,
        invalid_enclosing_fallback = stats.invalid_enclosing_fallback,
        "SCIP augment skipped edges: enclosing_range disagreements and/or invalid ranges \
         (see scip.edges_skipped_* / references_seen on index --json; RUST_LOG=debug for ≤3 samples)"
    );
}

/// D11: ≤1 process-level WARN when any document had only empty classic ranges.
fn emit_typed_empty_range_warn(stats: &ScipEdgeStats) {
    if stats.documents_all_classic_ranges_empty == 0 {
        return;
    }
    warn!(
        documents = stats.documents_all_classic_ranges_empty,
        "SCIP document(s) have empty classic occurrence ranges; indexer may emit typed-only \
         ranges unreadable under scip 0.8.1 (typed consumers not implemented)"
    );
}

/// True when the document has ≥1 non-local non-empty symbol occurrence and
/// **every** such occurrence's classic `range` is empty.
fn document_all_classic_ranges_empty(document: &scip::types::Document) -> bool {
    let mut any = false;
    for occ in &document.occurrences {
        if occ.symbol.is_empty() || occ.symbol.starts_with("local ") {
            continue;
        }
        any = true;
        if !occ.range.is_empty() {
            return false;
        }
    }
    any
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
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing_subscriber::Layer;
    use tracing_subscriber::fmt::MakeWriter;

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

    /// Buffer make-writer for WARN capture tests.
    #[derive(Clone, Default)]
    struct BufWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = BufGuard;

        fn make_writer(&'a self) -> Self::Writer {
            BufGuard {
                buf: Arc::clone(&self.buf),
            }
        }
    }

    struct BufGuard {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for BufGuard {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf
                .lock()
                .map_err(|e| io::Error::other(e.to_string()))?
                .extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Serialize tracing-subscriber capture tests (set_default races under
    /// nextest/cargo test multi-thread otherwise).
    static CAPTURE_LOCK: Mutex<()> = Mutex::new(());

    /// Capture tracing events at `max_level` and more severe while running `f`.
    ///
    /// Uses `LevelFilter::from_level` (ERROR..max_level inclusive). Tests that
    /// need DEBUG samples must pass `Level::DEBUG` so `debug!` events reach the
    /// buffer — a bare `filter_fn(*level <= max)` is easy to get wrong and races
    /// with sibling capture tests without `CAPTURE_LOCK`.
    fn with_level_capture<T>(max_level: Level, f: impl FnOnce() -> T) -> (T, String) {
        use tracing_subscriber::filter::LevelFilter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let _serial = CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let buf = BufWriter::default();
        let capture = Arc::clone(&buf.buf);
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(buf)
            .without_time()
            .with_target(true)
            .with_level(true)
            .with_filter(LevelFilter::from_level(max_level));
        let _guard = tracing_subscriber::registry().with(layer).set_default();
        let out = f();
        let text = String::from_utf8_lossy(&capture.lock().unwrap()).to_string();
        (out, text)
    }

    /// Capture WARN-level events while running `f`; returns (result, warn text).
    fn with_warn_capture<T>(f: impl FnOnce() -> T) -> (T, String) {
        with_level_capture(Level::WARN, f)
    }

    /// Capture DEBUG-and-above events while running `f` (DoD-6 sample pins).
    fn with_debug_capture<T>(f: impl FnOnce() -> T) -> (T, String) {
        with_level_capture(Level::DEBUG, f)
    }

    fn count_related_scip_warns(text: &str) -> usize {
        text.lines()
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                lower.contains("warn")
                    && (lower.contains("scip")
                        || lower.contains("enclosing")
                        || lower.contains("skipped edges")
                        || lower.contains("classic"))
            })
            .count()
    }

    fn count_disagreement_sample_debugs(text: &str) -> usize {
        // Match only the D10 sample event message — not the summary WARN that
        // mentions RUST_LOG=debug / "≤3 samples" in its help text.
        text.lines()
            .filter(|line| line.to_ascii_lowercase().contains("disagreement sample"))
            .count()
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
        assert_eq!(stats.references_seen, 1);
        assert_eq!(stats.edges_skipped_enclosing_disagreement, 0);

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

    /// Disagreement path must increment ONLY enclosing_disagreement (D1b).
    #[test]
    fn exclusive_disagreement_does_not_increment_unmapped() {
        use crate::scip::resolver::SCIP_ROLE_DEFINITION;
        let conn = setup_db();
        let path = "src/disagree.rs";
        let fid = insert_file(&conn, path);
        // Two non-overlapping spans so enc/occ disagree
        let _mod_sym = insert_symbol(&conn, fid, "mod_fn", 1, 20);
        let _fn_sym = insert_symbol(&conn, fid, "inner_fn", 30, 50);
        // Map a definition so callee is present (proves exclusivity even when
        // callee would otherwise be resolvable).
        let def = scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 target/".to_string(),
            symbol_roles: SCIP_ROLE_DEFINITION,
            range: vec![9, 0, 5], // native line 10 → mod_fn
            ..Default::default()
        };
        // occ at native 40 (inner_fn); enc starts at native 10 (mod_fn)
        let r#ref = scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 target/".to_string(),
            symbol_roles: 0,
            range: vec![39, 0, 5],
            enclosing_range: vec![9, 0, 19, 0],
            ..Default::default()
        };
        let docs = vec![scip::types::Document {
            relative_path: path.to_string(),
            occurrences: vec![def, r#ref],
            ..Default::default()
        }];
        let stats = augment_edges_from_scip(&conn, &docs, &|rel| {
            if rel == path { Some(fid) } else { None }
        })
        .unwrap();

        assert_eq!(stats.edges_skipped_enclosing_disagreement, 1);
        assert_eq!(stats.edges_recovered_nest_prefer, 0);
        assert_eq!(stats.edges_skipped_unmapped, 0);
        assert_eq!(stats.edges_added, 0);
        assert_eq!(stats.references_seen, 1);
    }

    /// Nest-prefer: enc maps to outer mod, occ to inner fn → recover edge, not disagree.
    #[test]
    fn nest_prefer_recovers_edge_without_disagreement() {
        use crate::scip::resolver::SCIP_ROLE_DEFINITION;
        let conn = setup_db();
        let path = "src/nest.rs";
        let fid = insert_file(&conn, path);
        // Outer mod 1–100, inner fn 20–30, callee 40–50 (also under mod)
        let _mod_sym = insert_symbol(&conn, fid, "mod_item", 1, 100);
        let _fn_sym = insert_symbol(&conn, fid, "inner_fn", 20, 30);
        let _callee = insert_symbol(&conn, fid, "callee_fn", 40, 50);

        let def = scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 nest_target/".to_string(),
            symbol_roles: SCIP_ROLE_DEFINITION,
            range: vec![44, 0, 5], // native 45 → callee_fn
            ..Default::default()
        };
        // occ at native 25 (inner_fn); enc starts at native 1 (mod only)
        let r#ref = scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 nest_target/".to_string(),
            symbol_roles: 0,
            range: vec![24, 0, 5],
            enclosing_range: vec![0, 0, 99, 0],
            ..Default::default()
        };
        let docs = vec![scip::types::Document {
            relative_path: path.to_string(),
            occurrences: vec![def, r#ref],
            ..Default::default()
        }];
        let stats = augment_edges_from_scip(&conn, &docs, &|rel| {
            if rel == path { Some(fid) } else { None }
        })
        .unwrap();

        assert_eq!(stats.edges_recovered_nest_prefer, 1);
        assert_eq!(stats.edges_skipped_enclosing_disagreement, 0);
        assert!(
            stats.edges_added >= 1,
            "expected recovered nest path to insert edge; edges_added={}",
            stats.edges_added
        );
        assert_eq!(stats.references_seen, 1);
        assert_eq!(stats.edges_skipped_unmapped, 0);
    }

    /// Resolved caller + missing callee → unmapped only.
    #[test]
    fn resolved_missing_callee_increments_unmapped_only() {
        let conn = setup_db();
        let path = "src/miss_callee.rs";
        let fid = insert_file(&conn, path);
        let _caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);
        // No definition occurrence → callee unmapped
        let r#ref = scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 external/".to_string(),
            symbol_roles: 0,
            range: vec![9, 0, 5],
            ..Default::default()
        };
        let docs = vec![scip::types::Document {
            relative_path: path.to_string(),
            occurrences: vec![r#ref],
            ..Default::default()
        }];
        let stats = augment_edges_from_scip(&conn, &docs, &|rel| {
            if rel == path { Some(fid) } else { None }
        })
        .unwrap();

        assert_eq!(stats.edges_skipped_unmapped, 1);
        assert_eq!(stats.edges_skipped_enclosing_disagreement, 0);
        assert_eq!(stats.edges_skipped_invalid_occ_range, 0);
        assert_eq!(stats.edges_added, 0);
        assert_eq!(stats.references_seen, 1);
    }

    /// ≥100 synthetic disagreements → counter == N and ≤1 related summary WARN.
    #[test]
    fn many_disagreements_aggregate_to_one_summary_warn() {
        use crate::scip::resolver::SCIP_ROLE_DEFINITION;
        const N: usize = 100;

        let conn = setup_db();
        let path = "src/flood.rs";
        let fid = insert_file(&conn, path);
        let _mod_sym = insert_symbol(&conn, fid, "mod_fn", 1, 20);
        let _fn_sym = insert_symbol(&conn, fid, "inner_fn", 30, 50);

        let mut occurrences = Vec::with_capacity(N + 1);
        // One definition so maps exist (not required for disagreement counting)
        occurrences.push(scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 flood_target/".to_string(),
            symbol_roles: SCIP_ROLE_DEFINITION,
            range: vec![9, 0, 5],
            ..Default::default()
        });
        for i in 0..N {
            occurrences.push(scip::types::Occurrence {
                symbol: format!("rust-analyzer cargo test 0.1 flood_target/{i}"),
                symbol_roles: 0,
                range: vec![39, 0, 5],              // native 40 → inner_fn
                enclosing_range: vec![9, 0, 19, 0], // native 10 → mod_fn
                ..Default::default()
            });
        }
        let docs = vec![scip::types::Document {
            relative_path: path.to_string(),
            occurrences,
            ..Default::default()
        }];

        let (stats, warn_text) = with_warn_capture(|| {
            augment_edges_from_scip(&conn, &docs, &|rel| {
                if rel == path { Some(fid) } else { None }
            })
            .unwrap()
        });

        assert_eq!(stats.edges_skipped_enclosing_disagreement, N);
        assert_eq!(stats.edges_skipped_unmapped, 0);
        assert_eq!(stats.references_seen, N);
        assert_eq!(stats.edges_added, 0);

        let related = count_related_scip_warns(&warn_text);
        assert!(
            related <= 1,
            "expected ≤1 related SCIP WARN for {N} disagreements, got {related}; text={warn_text:?}"
        );
        assert!(
            related == 1,
            "expected exactly 1 summary WARN when disagreements > 0; text={warn_text:?}"
        );
        assert!(
            warn_text.contains("skipped edges") || warn_text.contains("enclosing"),
            "summary WARN should mention skip/enclosing; text={warn_text:?}"
        );
    }

    /// Unmapped-only batch must not emit the summary WARN (D3 / M2).
    #[test]
    fn unmapped_only_emits_zero_summary_warn() {
        let conn = setup_db();
        let path = "src/unmapped_only.rs";
        let fid = insert_file(&conn, path);
        let _caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);

        let mut occurrences = Vec::new();
        for i in 0..20 {
            occurrences.push(scip::types::Occurrence {
                symbol: format!("rust-analyzer cargo test 0.1 ext/{i}"),
                symbol_roles: 0,
                range: vec![9, 0, 5],
                ..Default::default()
            });
        }
        let docs = vec![scip::types::Document {
            relative_path: path.to_string(),
            occurrences,
            ..Default::default()
        }];

        let (stats, warn_text) = with_warn_capture(|| {
            augment_edges_from_scip(&conn, &docs, &|rel| {
                if rel == path { Some(fid) } else { None }
            })
            .unwrap()
        });

        assert_eq!(stats.edges_skipped_unmapped, 20);
        assert_eq!(stats.edges_skipped_enclosing_disagreement, 0);
        assert_eq!(stats.edges_skipped_invalid_occ_range, 0);
        assert_eq!(stats.definitions_skipped_invalid_range, 0);

        let related = count_related_scip_warns(&warn_text);
        assert_eq!(
            related, 0,
            "unmapped-only must not emit summary WARN; text={warn_text:?}"
        );
    }

    /// D11: all-empty classic ranges → ≤1 WARN, no per-occ flood.
    #[test]
    fn typed_empty_classic_ranges_one_warn() {
        let conn = setup_db();
        let path = "src/typed_only.rs";
        let fid = insert_file(&conn, path);
        let _caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);

        let mut occurrences = Vec::new();
        for i in 0..10 {
            occurrences.push(scip::types::Occurrence {
                symbol: format!("rust-analyzer cargo test 0.1 typed/{i}"),
                symbol_roles: 0,
                range: vec![], // empty classic range
                ..Default::default()
            });
        }
        let docs = vec![scip::types::Document {
            relative_path: path.to_string(),
            occurrences,
            ..Default::default()
        }];

        let (stats, warn_text) = with_warn_capture(|| {
            augment_edges_from_scip(&conn, &docs, &|rel| {
                if rel == path { Some(fid) } else { None }
            })
            .unwrap()
        });

        assert_eq!(stats.documents_all_classic_ranges_empty, 1);
        assert_eq!(stats.references_seen, 10);
        assert_eq!(stats.edges_skipped_invalid_occ_range, 10);

        // Summary WARN (invalid_occ) + D11 typed-empty WARN ≤ 2; never 10+
        let related = count_related_scip_warns(&warn_text);
        assert!(
            related <= 2,
            "expected ≤2 WARNs (summary + D11), got {related}; text={warn_text:?}"
        );
        assert!(
            warn_text.to_ascii_lowercase().contains("classic")
                || warn_text.to_ascii_lowercase().contains("typed"),
            "D11 WARN should mention classic/typed ranges; text={warn_text:?}"
        );
    }

    #[test]
    fn invalid_enclosing_fallback_counted() {
        use crate::scip::resolver::SCIP_ROLE_DEFINITION;
        let conn = setup_db();
        let path = "src/fallback.rs";
        let fid = insert_file(&conn, path);
        let _caller = insert_symbol(&conn, fid, "caller_fn", 1, 20);
        let _callee = insert_symbol(&conn, fid, "callee_fn", 30, 40);

        let def = scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 fb/".to_string(),
            symbol_roles: SCIP_ROLE_DEFINITION,
            range: vec![29, 0, 5],
            ..Default::default()
        };
        let r#ref = scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 fb/".to_string(),
            symbol_roles: 0,
            range: vec![9, 0, 5],
            enclosing_range: vec![1, 2], // invalid length → fallback
            ..Default::default()
        };
        let docs = vec![scip::types::Document {
            relative_path: path.to_string(),
            occurrences: vec![def, r#ref],
            ..Default::default()
        }];
        let stats = augment_edges_from_scip(&conn, &docs, &|rel| {
            if rel == path { Some(fid) } else { None }
        })
        .unwrap();

        assert_eq!(stats.invalid_enclosing_fallback, 1);
        assert_eq!(stats.edges_added, 1);
        assert_eq!(stats.edges_skipped_enclosing_disagreement, 0);
    }

    /// DoD-6: ≥5 disagreements → counter == N, but ≤3 path-aware debug samples
    /// (path + enclosing_id / occurrence_id context). Never flood 5+ sample lines.
    #[test]
    fn disagreement_debug_samples_capped_at_three() {
        use crate::scip::resolver::SCIP_ROLE_DEFINITION;
        const N: usize = 10;
        let path = "src/sample_cap.rs";

        let conn = setup_db();
        let fid = insert_file(&conn, path);
        let _mod_sym = insert_symbol(&conn, fid, "mod_fn", 1, 20);
        let _fn_sym = insert_symbol(&conn, fid, "inner_fn", 30, 50);

        let mut occurrences = Vec::with_capacity(N + 1);
        occurrences.push(scip::types::Occurrence {
            symbol: "rust-analyzer cargo test 0.1 sample_cap/".to_string(),
            symbol_roles: SCIP_ROLE_DEFINITION,
            range: vec![9, 0, 5],
            ..Default::default()
        });
        for i in 0..N {
            occurrences.push(scip::types::Occurrence {
                symbol: format!("rust-analyzer cargo test 0.1 sample_cap/{i}"),
                symbol_roles: 0,
                range: vec![39, 0, 5],              // native 40 → inner_fn
                enclosing_range: vec![9, 0, 19, 0], // native 10 → mod_fn
                ..Default::default()
            });
        }
        let docs = vec![scip::types::Document {
            relative_path: path.to_string(),
            occurrences,
            ..Default::default()
        }];

        let (stats, debug_text) = with_debug_capture(|| {
            augment_edges_from_scip(&conn, &docs, &|rel| {
                if rel == path { Some(fid) } else { None }
            })
            .unwrap()
        });

        assert_eq!(stats.edges_skipped_enclosing_disagreement, N);
        assert_eq!(stats.references_seen, N);

        let sample_lines = count_disagreement_sample_debugs(&debug_text);
        assert_eq!(
            sample_lines, DISAGREEMENT_SAMPLE_CAP,
            "expected exactly {DISAGREEMENT_SAMPLE_CAP} sample debug lines for {N} disagreements, \
             got {sample_lines}; text={debug_text:?}"
        );
        assert!(
            sample_lines < N,
            "samples must not equal disagreement count ({N}); got {sample_lines}"
        );

        // Sample messages must include relative_path and enc/occ id context.
        assert!(
            debug_text.contains(path),
            "sample debug must include relative_path {path:?}; text={debug_text:?}"
        );
        let lower = debug_text.to_ascii_lowercase();
        assert!(
            lower.contains("enclosing_id") || lower.contains("enclosing"),
            "sample debug must include enclosing id context; text={debug_text:?}"
        );
        assert!(
            lower.contains("occurrence_id") || lower.contains("occurrence"),
            "sample debug must include occurrence id context; text={debug_text:?}"
        );
    }
}
