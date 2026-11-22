//! Edge-set extraction, identity assembly and completeness evaluation
//! (Track 0601).
//!
//! Every database read for one side of a comparison runs inside **one**
//! read transaction, so the edges, coverage, identity inputs and freshness
//! reads describe a single committed index snapshot. The worktree walk runs in
//! the same call but outside the transaction: SQLite does not snapshot files,
//! so `COMPLETE` is a point-in-time statement (R12).

use super::identity::{
    BASELINE_SCHEMA, BaselineIdentity, BaselineScope, EdgeCoverage, FileEdge, FileEdgeSet,
    InputCompleteness, PAYLOAD_KIND, input_digest,
};
use crate::config::model::Config;
use crate::index::orchestrator::{BINARY_EXTENSIONS, SUPPORTED_EXTENSIONS};
use crate::index::staleness::{
    HashBudget, IndexFreshnessState, apply_content_drift_override, assess_index_freshness_at,
    content_hash_drift_for_discovered, is_supported_code_path,
};
use crate::index::walker::RepoWalker;
use crate::state::storage::StorageManager;
use camino::Utf8Path;
use chrono::{DateTime, Utc};
use miette::{IntoDiagnostic, Result};

/// Counts that feed the completeness verdict, read in the same snapshot as
/// the edges.
#[derive(Debug, Clone, Copy)]
struct CompletenessInputs {
    files_parse_failed: u64,
    edges_capped: bool,
    parser_version_present: bool,
}

/// Extract the live edge set for the storage work root at the current clock.
pub(crate) fn extract_file_edge_set(
    storage: &StorageManager,
    config: &Config,
) -> Result<FileEdgeSet> {
    extract_file_edge_set_at(storage, config, storage.root(), Utc::now())
}

/// `_at` form: injectable repo root and clock for tests.
pub(crate) fn extract_file_edge_set_at(
    storage: &StorageManager,
    config: &Config,
    repo_root: &Utf8Path,
    now: DateTime<Utc>,
) -> Result<FileEdgeSet> {
    let guard = storage
        .get_connection()
        .unchecked_transaction()
        .into_diagnostic()?;
    let extracted = extract_in_snapshot(storage, config, repo_root, now);
    // Read-only transaction: commit releases the snapshot; rollback would do
    // the same. Commit keeps the intent explicit and surfaces a broken guard.
    match extracted {
        Ok(set) => {
            guard.commit().into_diagnostic()?;
            Ok(set)
        }
        Err(e) => Err(e),
    }
}

/// All reads for one side. Must be called with the snapshot guard alive.
fn extract_in_snapshot(
    storage: &StorageManager,
    config: &Config,
    repo_root: &Utf8Path,
    now: DateTime<Utc>,
) -> Result<FileEdgeSet> {
    let edges = read_edges(storage)?;
    let coverage = read_coverage(storage, &edges)?;
    let parser_version = read_metadata_value(storage, "parser_version")?;
    let index_head_hash = read_metadata_value(storage, "head_hash")?;
    let edge_sources = read_edge_sources(storage)?;
    let digest_rows = read_digest_rows(storage)?;
    let inputs = CompletenessInputs {
        files_parse_failed: coverage.files_parse_failed,
        edges_capped: has_capped_edges(storage)?,
        parser_version_present: parser_version.is_some(),
    };

    // The walk is filesystem I/O, not SQL; it runs while the database
    // snapshot is open but the transaction does not cover it (R12).
    let walk = RepoWalker::new(
        repo_root.to_path_buf(),
        SUPPORTED_EXTENSIONS,
        BINARY_EXTENSIONS,
    )
    .discover_files_strict();
    let completeness = evaluate_completeness_for(storage, config, repo_root, now, &inputs, walk)?;

    let identity = BaselineIdentity {
        baseline_schema: BASELINE_SCHEMA,
        payload_kind: PAYLOAD_KIND.to_string(),
        scope: BaselineScope::repo(),
        parser_version: parser_version.unwrap_or_default(),
        edge_sources,
        ledgerful_version: env!("LEDGERFUL_VERSION_LONG").to_string(),
        index_head_hash,
        input_digest: input_digest(&digest_rows),
        input_completeness: completeness,
    };

    Ok(FileEdgeSet {
        identity,
        coverage,
        edges,
    })
}

fn read_edges(storage: &StorageManager) -> Result<Vec<FileEdge>> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT cf.file_path, df.file_path, COUNT(*)
             FROM structural_edges se
             JOIN project_files cf ON cf.id = se.caller_file_id
             JOIN project_files df ON df.id = se.callee_file_id
             WHERE se.resolution_status = 'RESOLVED'
               AND se.callee_file_id IS NOT NULL
               AND se.caller_file_id != se.callee_file_id
               AND cf.parse_status != 'DELETED'
               AND df.parse_status != 'DELETED'
             GROUP BY cf.file_path, df.file_path",
        )
        .into_diagnostic()?;
    let rows = stmt
        .query_map([], |row| {
            let weight: i64 = row.get(2)?;
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, weight))
        })
        .into_diagnostic()?;

    let mut edges = Vec::new();
    for row in rows {
        let (src_path, dst_path, weight) = row.into_diagnostic()?;
        edges.push(FileEdge {
            src_path,
            dst_path,
            weight: u32::try_from(weight).into_diagnostic()?,
        });
    }
    edges.sort_unstable_by(|a, b| (&a.src_path, &a.dst_path).cmp(&(&b.src_path, &b.dst_path)));
    Ok(edges)
}

fn read_coverage(storage: &StorageManager, edges: &[FileEdge]) -> Result<EdgeCoverage> {
    let conn = storage.get_connection();
    let (files_indexed, files_parse_failed) = conn
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN parse_status = 'PARSE_FAILED' THEN 1 ELSE 0 END), 0)
             FROM project_files WHERE parse_status != 'DELETED'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .into_diagnostic()?;
    let (ambiguous, unresolved, capped, resolved_missing_callee, resolved_intra) = conn
        .query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN se.resolution_status = 'AMBIGUOUS' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN se.resolution_status = 'UNRESOLVED' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN se.resolution_status = 'CAPPED' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN se.resolution_status = 'RESOLVED'
                                   AND (df.id IS NULL OR df.parse_status = 'DELETED') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN se.resolution_status = 'RESOLVED'
                                   AND se.callee_file_id IS NOT NULL
                                   AND se.caller_file_id = se.callee_file_id THEN 1 ELSE 0 END), 0)
             FROM structural_edges se
             JOIN project_files cf ON cf.id = se.caller_file_id
             LEFT JOIN project_files df ON df.id = se.callee_file_id
             WHERE cf.parse_status != 'DELETED'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .into_diagnostic()?;

    Ok(EdgeCoverage {
        files_indexed: to_u64(files_indexed),
        files_parse_failed: to_u64(files_parse_failed),
        edges_resolved_cross_file: edges.iter().map(|e| u64::from(e.weight)).sum(),
        edges_resolved_intra_file: to_u64(resolved_intra),
        edges_ambiguous: to_u64(ambiguous),
        // A RESOLVED row without an active callee contributes to unresolved coverage.
        edges_unresolved: to_u64(unresolved) + to_u64(resolved_missing_callee),
        edges_capped: to_u64(capped),
    })
}

fn has_capped_edges(storage: &StorageManager) -> Result<bool> {
    storage
        .get_connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM structural_edges WHERE resolution_status = 'CAPPED')",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn read_metadata_value(storage: &StorageManager, key: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    storage
        .get_connection()
        .query_row(
            "SELECT value FROM index_metadata WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .into_diagnostic()
}

fn read_edge_sources(storage: &StorageManager) -> Result<Vec<String>> {
    let scip: i64 = storage
        .get_connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM structural_edges WHERE evidence LIKE 'scip:%')",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    let mut sources = vec!["native".to_string()];
    if scip != 0 {
        sources.push("scip".to_string());
    }
    sources.sort();
    Ok(sources)
}

fn read_digest_rows(storage: &StorageManager) -> Result<Vec<(String, Option<String>, String)>> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT file_path, content_hash, parser_version
             FROM project_files WHERE parse_status != 'DELETED'",
        )
        .into_diagnostic()?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .into_diagnostic()?
        .collect::<std::result::Result<Vec<_>, _>>()
        .into_diagnostic()?;
    Ok(rows
        .into_iter()
        .filter(|(path, _, _)| is_supported_code_path(path))
        .collect())
}

/// Evaluate completeness with an injected discovery result so the walk-error
/// mapping is deterministic in tests.
fn evaluate_completeness_for(
    storage: &StorageManager,
    config: &Config,
    repo_root: &Utf8Path,
    now: DateTime<Utc>,
    inputs: &CompletenessInputs,
    walk: Result<Vec<camino::Utf8PathBuf>>,
) -> Result<InputCompleteness> {
    let mut reasons: Vec<String> = Vec::new();

    let age = assess_index_freshness_at(storage, config.index.stale_threshold_days, now);
    let assessment = match walk {
        Ok(discovered) => {
            let drift = content_hash_drift_for_discovered(
                storage,
                repo_root,
                &discovered,
                HashBudget::Full,
            )?;
            apply_content_drift_override(age, &drift)
        }
        Err(e) => {
            tracing::debug!("baseline completeness: strict discovery failed: {e}");
            reasons.push("SCAN_INCOMPLETE".to_string());
            age
        }
    };
    match assessment.state {
        IndexFreshnessState::FreshPopulated => {}
        IndexFreshnessState::StalePopulated => reasons.push("INDEX_STALE".to_string()),
        IndexFreshnessState::ContentStalePopulated => reasons.push("CONTENT_DRIFT".to_string()),
        IndexFreshnessState::NeverIndexed
        | IndexFreshnessState::FreshEmpty
        | IndexFreshnessState::StaleEmpty => reasons.push("INDEX_EMPTY".to_string()),
        IndexFreshnessState::Indeterminate => reasons.push("INDEX_INDETERMINATE".to_string()),
    }

    if inputs.files_parse_failed > 0 {
        reasons.push("PARSE_FAILED_FILES".to_string());
    }
    if inputs.edges_capped {
        reasons.push("CAPPED_EDGES".to_string());
    }
    if !inputs.parser_version_present {
        reasons.push("INDEX_INDETERMINATE".to_string());
    }

    if reasons.is_empty() {
        Ok(InputCompleteness::complete())
    } else {
        Ok(InputCompleteness::incomplete(reasons))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::baseline::identity::CompletenessState;
    use crate::state::migrations::get_migrations;
    use rusqlite::{Connection, params};

    struct Fixture {
        _tmp: tempfile::TempDir,
        storage: StorageManager,
        root: camino::Utf8PathBuf,
        config: Config,
        now: DateTime<Utc>,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        std::fs::create_dir_all(root.join("src").as_std_path()).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        Fixture {
            _tmp: tmp,
            storage,
            root,
            config: Config::default(),
            now: Utc::now(),
        }
    }

    fn insert_file(
        storage: &StorageManager,
        path: &str,
        status: &str,
        content: Option<&str>,
    ) -> i64 {
        let hash = content.map(|c| blake3::hash(c.as_bytes()).to_hex().to_string());
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files
                (file_path, language, content_hash, file_size, mtime_ns, parser_version, parse_status, last_indexed_at)
             VALUES (?1, 'rust', ?2, NULL, NULL, '1', ?3, ?4)",
            params![path, hash, status, Utc::now().to_rfc3339()],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_symbol(storage: &StorageManager, file_id: i64, name: &str) -> i64 {
        let conn = storage.get_connection();
        let next: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0) + 1 FROM project_symbols",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let qualified = format!("{name}#{next}");
        conn.execute(
            "INSERT INTO project_symbols
                (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, 'function', ?4)",
            params![file_id, qualified, name, Utc::now().to_rfc3339()],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_edge(
        storage: &StorageManager,
        caller_file: i64,
        callee_file: Option<i64>,
        status: &str,
        evidence: Option<&str>,
    ) {
        let caller_symbol = insert_symbol(storage, caller_file, "caller");
        let callee_symbol = callee_file.map(|id| insert_symbol(storage, id, "callee"));
        storage
            .get_connection()
            .execute(
                "INSERT INTO structural_edges
                    (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id,
                     resolution_status, evidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    caller_symbol,
                    caller_file,
                    callee_symbol,
                    callee_file,
                    status,
                    evidence
                ],
            )
            .unwrap();
    }

    fn set_metadata(storage: &StorageManager, key: &str, value: &str) {
        storage
            .get_connection()
            .execute(
                "INSERT INTO index_metadata (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .unwrap();
    }

    /// Fresh, clean, complete side with one resolved cross-file edge.
    fn complete_side(f: &Fixture) -> (i64, i64) {
        let content_a = "pub fn caller() { helper(); }";
        let content_b = "pub fn helper() {}";
        std::fs::write(f.root.join("src/a.rs").as_std_path(), content_a).unwrap();
        std::fs::write(f.root.join("src/b.rs").as_std_path(), content_b).unwrap();
        let a = insert_file(&f.storage, "src/a.rs", "OK", Some(content_a));
        let b = insert_file(&f.storage, "src/b.rs", "OK", Some(content_b));
        insert_edge(&f.storage, a, Some(b), "RESOLVED", Some("native"));
        set_metadata(&f.storage, "parser_version", "1");
        set_metadata(&f.storage, "head_hash", "abc123");
        set_metadata(&f.storage, "last_indexed_at", &Utc::now().to_rfc3339());
        (a, b)
    }

    #[test]
    fn baseline_extract_only_resolved_cross_file_become_edges() {
        let f = fixture();
        let (a, b) = complete_side(&f);
        insert_edge(&f.storage, a, None, "UNRESOLVED", None);
        insert_edge(&f.storage, a, None, "RESOLVED", None);
        insert_edge(&f.storage, a, Some(a), "RESOLVED", None);
        insert_edge(&f.storage, a, Some(b), "AMBIGUOUS", None);
        insert_edge(&f.storage, a, Some(b), "CAPPED", None);

        let set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(
            set.edges,
            vec![FileEdge {
                src_path: "src/a.rs".to_string(),
                dst_path: "src/b.rs".to_string(),
                weight: 1,
            }]
        );
        assert_eq!(set.coverage.edges_ambiguous, 1);
        assert_eq!(set.coverage.edges_capped, 1);
        assert_eq!(set.coverage.edges_resolved_intra_file, 1);
        // 1 UNRESOLVED row + 1 RESOLVED row with a NULL callee.
        assert_eq!(set.coverage.edges_unresolved, 2);
        assert_eq!(
            set.identity.input_completeness.state,
            CompletenessState::Incomplete
        );
        assert!(
            set.identity
                .input_completeness
                .reasons
                .contains(&"CAPPED_EDGES".to_string())
        );
    }

    #[test]
    fn baseline_extract_weight_equals_row_count_per_pair() {
        let f = fixture();
        let (a, b) = complete_side(&f);
        insert_edge(&f.storage, a, Some(b), "RESOLVED", None);
        insert_edge(&f.storage, a, Some(b), "RESOLVED", None);
        let set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(set.edges.len(), 1);
        assert_eq!(set.edges[0].weight, 3);
        assert_eq!(set.coverage.edges_resolved_cross_file, 3);
    }

    #[test]
    fn baseline_extract_excludes_deleted_files() {
        let f = fixture();
        let (_a, b) = complete_side(&f);
        f.storage
            .get_connection()
            .execute(
                "UPDATE project_files SET parse_status = 'DELETED' WHERE id = ?1",
                [b],
            )
            .unwrap();
        let set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert!(set.edges.is_empty());
        assert_eq!(set.coverage.files_indexed, 1);
    }

    #[test]
    fn baseline_extract_excluded_rows_counted_in_coverage() {
        let f = fixture();
        let (a, b) = complete_side(&f);
        let deleted = insert_file(&f.storage, "src/deleted.rs", "DELETED", None);
        insert_edge(&f.storage, a, Some(deleted), "RESOLVED", None);
        insert_edge(&f.storage, deleted, Some(b), "RESOLVED", None);
        insert_edge(&f.storage, deleted, Some(b), "CAPPED", None);
        let conn = f.storage.get_connection();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute(
            "UPDATE structural_edges SET callee_file_id = 99999 WHERE caller_file_id = ?1 AND callee_file_id = ?2",
            params![a, deleted],
        ).unwrap();
        insert_edge(&f.storage, a, Some(deleted), "RESOLVED", None);
        let set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(set.coverage.edges_unresolved, 2);
        assert_eq!(set.coverage.edges_resolved_cross_file, 1);
        assert_eq!(set.coverage.edges_capped, 0);
        assert_eq!(set.identity.input_completeness.reasons, ["CAPPED_EDGES"]);
    }

    #[test]
    fn baseline_extract_is_deterministic_across_runs() {
        let f = fixture();
        let (_a, b) = complete_side(&f);
        let c = insert_file(&f.storage, "src/c.rs", "OK", Some("pub fn c() {}"));
        insert_edge(&f.storage, c, Some(b), "RESOLVED", None);
        let first = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        let second = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(first, second);
        let keys: Vec<_> = first
            .edges
            .iter()
            .map(|e| (e.src_path.as_str(), e.dst_path.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![("src/a.rs", "src/b.rs"), ("src/c.rs", "src/b.rs")]
        );
    }

    #[test]
    fn baseline_extract_edge_sources_scip_positive_and_negative() {
        let f = fixture();
        let (a, b) = complete_side(&f);
        let native = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(native.identity.edge_sources, vec!["native".to_string()]);

        insert_edge(&f.storage, a, Some(b), "RESOLVED", Some("scip:abc"));
        let scip = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(
            scip.identity.edge_sources,
            vec!["native".to_string(), "scip".to_string()]
        );
    }

    #[test]
    fn baseline_completeness_complete_for_fresh_clean_index() {
        let f = fixture();
        complete_side(&f);
        let set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(
            set.identity.input_completeness,
            InputCompleteness {
                state: CompletenessState::Complete,
                reasons: vec![],
            }
        );
    }

    #[test]
    fn baseline_completeness_reason_scan_incomplete() {
        let f = fixture();
        complete_side(&f);
        let inputs = CompletenessInputs {
            files_parse_failed: 0,
            edges_capped: false,
            parser_version_present: true,
        };
        let walk =
            crate::index::walker::discovery_with_injected_error(&f.root.join("blocked"), true);
        let completeness =
            evaluate_completeness_for(&f.storage, &f.config, &f.root, f.now, &inputs, walk)
                .unwrap();
        assert_eq!(completeness.state, CompletenessState::Incomplete);
        assert_eq!(completeness.reasons, vec!["SCAN_INCOMPLETE".to_string()]);
        let mut set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        set.identity.input_completeness = completeness;
        let refusal = super::super::create_baseline(&f.storage, "walk-error", &set).unwrap_err();
        assert!(refusal.to_string().contains("SCAN_INCOMPLETE"));
        assert!(
            crate::state::storage::baselines::list_baselines(&f.storage)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn baseline_completeness_missing_parser_version_is_indeterminate() {
        let f = fixture();
        complete_side(&f);
        f.storage
            .get_connection()
            .execute(
                "DELETE FROM index_metadata WHERE key = 'parser_version'",
                [],
            )
            .unwrap();
        let set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(
            set.identity.input_completeness.state,
            CompletenessState::Incomplete
        );
        assert!(
            set.identity
                .input_completeness
                .reasons
                .contains(&"INDEX_INDETERMINATE".to_string())
        );
    }

    #[test]
    fn baseline_completeness_reason_content_drift() {
        let f = fixture();
        complete_side(&f);
        std::fs::write(f.root.join("src/a.rs").as_std_path(), "pub fn changed() {}").unwrap();
        let set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(
            set.identity.input_completeness.state,
            CompletenessState::Incomplete
        );
        assert_eq!(
            set.identity.input_completeness.reasons,
            vec!["CONTENT_DRIFT".to_string()]
        );
    }

    #[test]
    fn baseline_completeness_reason_parse_failed_files() {
        let f = fixture();
        let (_a, b) = complete_side(&f);
        insert_file(&f.storage, "src/broken.rs", "PARSE_FAILED", Some("?"));
        insert_edge(&f.storage, b, Some(b), "RESOLVED", None);
        let set = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert!(
            set.identity
                .input_completeness
                .reasons
                .contains(&"PARSE_FAILED_FILES".to_string())
        );
    }

    #[test]
    fn baseline_input_digest_stable_and_changes_with_content_hash() {
        let f = fixture();
        complete_side(&f);
        let first = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        let second = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(first.identity.input_digest, second.identity.input_digest);
        f.storage
            .get_connection()
            .execute(
                "UPDATE project_files SET content_hash = 'changed' WHERE file_path = 'src/b.rs'",
                [],
            )
            .unwrap();
        let third = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_ne!(first.identity.input_digest, third.identity.input_digest);
    }

    #[test]
    fn baseline_input_digest_ignores_enrichment_rows() {
        let f = fixture();
        complete_side(&f);
        insert_file(&f.storage, "README.md", "OK", Some("# docs"));
        let first = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        f.storage
            .get_connection()
            .execute(
                "UPDATE project_files SET content_hash = 'other' WHERE file_path = 'README.md'",
                [],
            )
            .unwrap();
        let second = extract_file_edge_set_at(&f.storage, &f.config, &f.root, f.now).unwrap();
        assert_eq!(first.identity.input_digest, second.identity.input_digest);
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use rusqlite::{Connection, params};

    fn file_backed_storage() -> (tempfile::TempDir, StorageManager, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join(".ledgerful").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let db_path = state_dir.join("ledger.db");
        let storage = StorageManager::init(&db_path).unwrap();
        (tmp, storage, db_path)
    }

    fn seed_file(storage: &StorageManager, path: &str) -> i64 {
        storage
            .get_connection()
            .execute(
                "INSERT INTO project_files
                    (file_path, language, content_hash, parser_version, parse_status, last_indexed_at)
                 VALUES (?1, 'rust', 'hash', '1', 'OK', ?2)",
                params![path, Utc::now().to_rfc3339()],
            )
            .unwrap();
        storage.get_connection().last_insert_rowid()
    }

    fn seed_symbol(storage: &StorageManager, file_id: i64, label: &str) -> i64 {
        let conn = storage.get_connection();
        let next: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0) + 1 FROM project_symbols",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO project_symbols
                (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, 'function', ?4)",
            params![
                file_id,
                format!("{label}#{next}"),
                label,
                Utc::now().to_rfc3339()
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_edge(storage: &StorageManager, caller_file: i64, callee_file: i64) {
        let caller = seed_symbol(storage, caller_file, "caller");
        let callee = seed_symbol(storage, callee_file, "callee");
        storage
            .get_connection()
            .execute(
                "INSERT INTO structural_edges
                    (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id,
                     resolution_status, evidence)
                 VALUES (?1, ?2, ?3, ?4, 'RESOLVED', 'native')",
                params![caller, caller_file, callee, callee_file],
            )
            .unwrap();
    }

    #[test]
    fn baseline_extract_uses_one_read_snapshot() {
        let (tmp, storage, db_path) = file_backed_storage();
        let a = seed_file(&storage, "src/a.rs");
        let b = seed_file(&storage, "src/b.rs");
        let c = seed_file(&storage, "src/c.rs");
        seed_edge(&storage, a, b);
        let writer = StorageManager::init_from_conn(Connection::open(&db_path).unwrap());
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let reader =
            StorageManager::open_read_only_sqlite_only(&crate::state::layout::Layout::new(root))
                .unwrap();
        let mode: String = reader
            .get_connection()
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let snapshot = reader.get_connection().unchecked_transaction().unwrap();
        let before = read_edges(&reader).unwrap();
        assert_eq!(before.len(), 1);

        seed_edge(&writer, c, b);
        writer
            .get_connection()
            .execute(
                "INSERT INTO index_metadata (key,value) VALUES ('head_hash','new-head')",
                [],
            )
            .unwrap();

        let during = read_edges(&reader).unwrap();
        assert_eq!(during, before);
        assert_eq!(
            read_coverage(&reader, &during)
                .unwrap()
                .edges_resolved_cross_file,
            1
        );
        assert_eq!(read_metadata_value(&reader, "head_hash").unwrap(), None);
        snapshot.commit().unwrap();
        let after = read_edges(&reader).unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(
            read_coverage(&reader, &after)
                .unwrap()
                .edges_resolved_cross_file,
            2
        );
        assert_eq!(
            read_metadata_value(&reader, "head_hash")
                .unwrap()
                .as_deref(),
            Some("new-head")
        );
    }
}

#[cfg(test)]
mod completeness_tests {
    use super::*;
    use crate::commands::baseline::identity::CompletenessState;
    use crate::state::migrations::get_migrations;
    use rusqlite::{Connection, params};

    fn fixture() -> (
        tempfile::TempDir,
        StorageManager,
        camino::Utf8PathBuf,
        Config,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        std::fs::create_dir_all(root.join("src").as_std_path()).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        (
            tmp,
            StorageManager::init_from_conn(conn),
            root,
            Config::default(),
        )
    }

    fn seed(storage: &StorageManager, now: DateTime<Utc>) {
        storage
            .get_connection()
            .execute_batch("DELETE FROM structural_edges; DELETE FROM project_files; DELETE FROM index_metadata;")
            .unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO index_metadata (key, value) VALUES ('parser_version','1')",
                [],
            )
            .unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO index_metadata (key, value) VALUES ('last_indexed_at', ?1)",
                params![now.to_rfc3339()],
            )
            .unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO project_files
                    (file_path, language, content_hash, parser_version, parse_status, last_indexed_at)
                 VALUES ('src/a.rs','rust','h','1','OK', ?1)",
                params![now.to_rfc3339()],
            )
            .unwrap();
    }

    fn reasons(
        storage: &StorageManager,
        root: &Utf8Path,
        config: &Config,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        let inputs = CompletenessInputs {
            files_parse_failed: 0,
            edges_capped: false,
            parser_version_present: true,
        };
        let walk = RepoWalker::new(root.to_path_buf(), SUPPORTED_EXTENSIONS, BINARY_EXTENSIONS)
            .discover_files_strict();
        evaluate_completeness_for(storage, config, root, now, &inputs, walk)
            .unwrap()
            .reasons
    }

    #[test]
    fn baseline_completeness_reason_index_stale() {
        let (_tmp, storage, root, config) = fixture();
        let old = Utc::now() - chrono::Duration::days(400);
        seed(&storage, old);
        let reasons = reasons(&storage, &root, &config, Utc::now());
        assert!(reasons.contains(&"INDEX_STALE".to_string()), "{reasons:?}");
    }

    #[test]
    fn baseline_completeness_reason_index_empty() {
        let (_tmp, storage, root, config) = fixture();
        let reasons = reasons(&storage, &root, &config, Utc::now());
        assert!(reasons.contains(&"INDEX_EMPTY".to_string()), "{reasons:?}");
    }

    #[test]
    fn baseline_completeness_reason_capped_edges() {
        let (_tmp, storage, root, config) = fixture();
        let now = Utc::now();
        seed(&storage, now);
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files
                (file_path, language, content_hash, parser_version, parse_status, last_indexed_at)
             VALUES ('src/b.rs','rust','h','1','OK', ?1)",
            params![now.to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_symbols
                (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES ((SELECT id FROM project_files WHERE file_path='src/a.rs'), 'c1','c1','function', ?1)",
            params![now.to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO structural_edges
                (caller_symbol_id, caller_file_id, resolution_status)
             VALUES ((SELECT MAX(id) FROM project_symbols),
                     (SELECT id FROM project_files WHERE file_path='src/a.rs'), 'CAPPED')",
            [],
        )
        .unwrap();
        let inputs = CompletenessInputs {
            files_parse_failed: 0,
            edges_capped: true,
            parser_version_present: true,
        };
        let walk = RepoWalker::new(root.to_path_buf(), SUPPORTED_EXTENSIONS, BINARY_EXTENSIONS)
            .discover_files_strict();
        let completeness =
            evaluate_completeness_for(&storage, &config, &root, now, &inputs, walk).unwrap();
        assert_eq!(completeness.state, CompletenessState::Incomplete);
        assert!(
            completeness.reasons.contains(&"CAPPED_EDGES".to_string()),
            "{:?}",
            completeness.reasons
        );
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::{Connection, params};

    fn fixture() -> (tempfile::TempDir, StorageManager, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        std::fs::create_dir_all(root.join("src").as_std_path()).unwrap();
        for name in ["a.rs", "m.rs", "z.rs"] {
            std::fs::write(
                root.join("src").join(name).as_std_path(),
                format!("pub fn {name}() {{}}\n"),
            )
            .unwrap();
        }
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        (tmp, StorageManager::init_from_conn(conn), root)
    }

    fn seed_file(storage: &StorageManager, path: &str) -> i64 {
        storage
            .get_connection()
            .execute(
                "INSERT INTO project_files
                    (file_path, language, content_hash, parser_version, parse_status, last_indexed_at)
                 VALUES (?1, 'rust', 'h', '1', 'OK', '2026-11-21T00:00:00Z')",
                params![path],
            )
            .unwrap();
        storage.get_connection().last_insert_rowid()
    }

    fn seed_symbol(storage: &StorageManager, file_id: i64, label: &str) -> i64 {
        let conn = storage.get_connection();
        let next: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0) + 1 FROM project_symbols",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO project_symbols
                (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, 'function', '2026-11-21T00:00:00Z')",
            params![file_id, format!("{label}#{next}"), label],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_edge(storage: &StorageManager, caller: i64, callee: i64) {
        let c = seed_symbol(storage, caller, "c");
        let d = seed_symbol(storage, callee, "d");
        storage
            .get_connection()
            .execute(
                "INSERT INTO structural_edges
                    (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id,
                     resolution_status, evidence)
                 VALUES (?1, ?2, ?3, ?4, 'RESOLVED', 'native')",
                params![c, caller, d, callee],
            )
            .unwrap();
    }

    #[test]
    fn baseline_extract_paths_are_forward_slash_and_sorted_bytewise() {
        let (_tmp, storage, root) = fixture();
        // Insert in reverse order so the SQL/rust sort is load-bearing.
        let z = seed_file(&storage, "src/z.rs");
        let m = seed_file(&storage, "src/m.rs");
        let a = seed_file(&storage, "src/a.rs");
        seed_edge(&storage, z, a);
        seed_edge(&storage, m, a);
        seed_edge(&storage, a, z);

        storage
            .get_connection()
            .execute(
                "INSERT INTO index_metadata (key, value) VALUES ('parser_version', '1')",
                [],
            )
            .unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO index_metadata (key, value) VALUES ('last_indexed_at', '2026-11-21T00:00:00Z')",
                [],
            )
            .unwrap();

        let config = crate::config::model::Config::default();
        let set = extract_file_edge_set_at(&storage, &config, &root, Utc::now()).unwrap();
        let keys: Vec<_> = set
            .edges
            .iter()
            .map(|e| (e.src_path.as_str(), e.dst_path.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("src/a.rs", "src/z.rs"),
                ("src/m.rs", "src/a.rs"),
                ("src/z.rs", "src/a.rs"),
            ],
            "edge keys sort bytewise by (srcPath, dstPath)"
        );
        assert!(
            set.edges
                .iter()
                .all(|e| !e.src_path.contains('\\') && !e.dst_path.contains('\\')),
            "stored forward-slash paths pass through unchanged"
        );
    }
}
