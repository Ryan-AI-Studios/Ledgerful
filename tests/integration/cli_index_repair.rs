#![allow(non_snake_case)]

use camino::Utf8Path;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

use crate::common::{run_cli, setup_git_repo};

/// Hermetic populated index via binary spawn only — no process CWD mutation.
fn populated_index_root(tmp: &tempfile::TempDir) -> camino::Utf8PathBuf {
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
    setup_git_repo(tmp.path());
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn repair_preview_target() {}\n",
    )
    .expect("lib.rs");
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .expect("state dir");
    let (stdout, stderr, code) = run_cli(tmp.path(), &["index", "--incremental"]);
    assert_eq!(
        code, 0,
        "incremental index must succeed; stdout={stdout}; stderr={stderr}"
    );
    root.to_path_buf()
}

fn ledger_db(root: &Utf8Path) -> std::path::PathBuf {
    root.join(".ledgerful")
        .join("state")
        .join("ledger.db")
        .into_std_path_buf()
}

type KeyValueRows = Vec<(String, String)>;

fn snapshot_repair_tables(db: &Path) -> (KeyValueRows, KeyValueRows) {
    let conn = rusqlite::Connection::open(db).expect("open ledger.db");
    let mut meta_stmt = conn
        .prepare("SELECT key, value FROM index_metadata ORDER BY key")
        .expect("prepare metadata");
    let meta: Vec<(String, String)> = meta_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query metadata")
        .collect::<Result<_, _>>()
        .expect("collect metadata");
    let mut files_stmt = conn
        .prepare("SELECT file_path, parse_status FROM project_files ORDER BY file_path")
        .expect("prepare project_files");
    let files: Vec<(String, String)> = files_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query project_files")
        .collect::<Result<_, _>>()
        .expect("collect project_files");
    (meta, files)
}

fn assert_no_shadow(root: &Utf8Path) {
    let state = root.join(".ledgerful").join("state");
    assert!(
        !state.join("ledger_shadow.db").exists(),
        "preview must not create ledger_shadow.db"
    );
    assert!(
        !state.join("ledger_shadow.cozo").exists(),
        "preview must not create ledger_shadow.cozo"
    );
}

fn parse_preview(
    stdout: &str,
    stderr: &str,
    code: i32,
    require_empty_stderr: bool,
) -> serde_json::Value {
    assert_eq!(
        code, 0,
        "preview must exit 0; stdout={stdout}; stderr={stderr}"
    );
    if require_empty_stderr {
        let trimmed = stderr.trim();
        assert!(
            trimmed.is_empty() || trimmed.contains("Ledgerful database auto-migrated"),
            "fresh fixture stderr must be empty (or migration token only): {stderr}"
        );
    }
    assert!(
        stdout.ends_with('\n'),
        "pretty JSON must end with one trailing newline: {stdout:?}"
    );
    assert!(
        stdout.contains('\n'),
        "pretty JSON must be multi-line: {stdout}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("entire stdout must parse as one JSON object: {e}; stdout={stdout}");
    });
    assert!(parsed.is_object(), "expected JSON object, got {parsed}");
    assert_eq!(parsed["schemaVersion"], 1, "schemaVersion: {parsed}");
    assert_eq!(parsed["kind"], "indexRepairPreview", "kind: {parsed}");
    assert_eq!(parsed["executed"], false, "executed: {parsed}");
    assert_eq!(parsed["dryRun"], true, "dryRun: {parsed}");
    assert!(parsed.get("ok").is_none(), "ok must be omitted: {parsed}");
    assert_eq!(
        parsed["proposed"],
        serde_json::json!(["force full index", "replace metadata if successful"]),
        "proposed: {parsed}"
    );
    let assessment = parsed
        .get("assessment")
        .and_then(|a| a.as_object())
        .unwrap_or_else(|| panic!("assessment object required: {parsed}"));
    assert!(assessment.contains_key("state"), "state: {parsed}");
    assert!(assessment.contains_key("source"), "source: {parsed}");
    assert!(
        assessment.contains_key("indexedFiles"),
        "indexedFiles: {parsed}"
    );
    assert_eq!(assessment["staleFiles"], 0, "age-only staleFiles: {parsed}");
    assert_eq!(
        assessment["unindexedFiles"], 0,
        "age-only unindexedFiles: {parsed}"
    );
    assert!(
        !assessment.contains_key("emptyDiagnostics"),
        "emptyDiagnostics must be omitted: {parsed}"
    );
    let dumped = serde_json::to_string(&parsed).expect("dump");
    assert!(!dumped.contains(":null"), "never JSON null: {dumped}");
    parsed
}

#[test]
fn index_repair_preview_json__fresh__pretty_envelope_no_writes() {
    let tmp = tempdir().unwrap();
    let root = populated_index_root(&tmp);
    let db = ledger_db(&root);
    let before = snapshot_repair_tables(&db);

    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["index", "--repair-metadata", "--dry-run", "--json"],
    );
    let parsed = parse_preview(&stdout, &stderr, code, true);
    assert_eq!(parsed["assessment"]["state"], "FreshPopulated");

    let after = snapshot_repair_tables(&db);
    assert_eq!(
        before, after,
        "preview must not mutate index_metadata/project_files"
    );
    assert_no_shadow(&root);
}

#[test]
fn index_repair_preview_json__missing_last_indexed_at__still_emits_plan() {
    let tmp = tempdir().unwrap();
    let root = populated_index_root(&tmp);
    let db = ledger_db(&root);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "DELETE FROM index_metadata WHERE key = 'last_indexed_at'",
            [],
        )
        .unwrap();
    }
    let before = snapshot_repair_tables(&db);

    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["index", "--repair-metadata", "--dry-run", "--json"],
    );
    let parsed = parse_preview(&stdout, &stderr, code, false);
    let state = parsed["assessment"]["state"]
        .as_str()
        .unwrap_or_else(|| panic!("missing fixture still emits a state: {parsed}"));
    assert!(
        !state.is_empty(),
        "missing fixture still emits a plan (no NeverIndexed requirement): {parsed}"
    );

    let after = snapshot_repair_tables(&db);
    assert_eq!(before, after, "preview must not mutate tables");
    assert_no_shadow(&root);
}

#[test]
fn index_repair_preview_json__corrupt_timestamp__indeterminate_warning() {
    let tmp = tempdir().unwrap();
    let root = populated_index_root(&tmp);
    let db = ledger_db(&root);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE index_metadata SET value = 'not-a-date' WHERE key = 'last_indexed_at'",
            [],
        )
        .unwrap();
    }
    let before = snapshot_repair_tables(&db);

    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["index", "--repair-metadata", "--dry-run", "--json"],
    );
    let parsed = parse_preview(&stdout, &stderr, code, false);
    assert_eq!(parsed["assessment"]["state"], "Indeterminate");
    let warnings = parsed["assessment"]["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("warnings required: {parsed}"));
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|s| s.contains("Malformed timestamp"))),
        "corrupt fixture must warn Malformed timestamp: {parsed}"
    );

    let after = snapshot_repair_tables(&db);
    assert_eq!(before, after, "preview must not mutate tables");
    assert_no_shadow(&root);
}

#[test]
fn index_repair_preview_json__future_skew__indeterminate_warning() {
    let tmp = tempdir().unwrap();
    let root = populated_index_root(&tmp);
    let db = ledger_db(&root);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let updated = conn
            .execute(
                "UPDATE index_metadata SET value = '2099-01-01T00:00:00Z' WHERE key = 'last_indexed_at'",
                [],
            )
            .unwrap();
        assert_eq!(updated, 1, "must rewrite last_indexed_at for skew fixture");
    }
    let before = snapshot_repair_tables(&db);

    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["index", "--repair-metadata", "--dry-run", "--json"],
    );
    let parsed = parse_preview(&stdout, &stderr, code, false);
    assert_eq!(parsed["assessment"]["state"], "Indeterminate");
    let warnings = parsed["assessment"]["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("warnings required: {parsed}"));
    assert!(
        warnings.iter().any(|w| {
            w.as_str()
                .is_some_and(|s| s.contains("Future timestamp detected (clock skew)."))
        }),
        "skew fixture must warn clock skew: {parsed}"
    );

    let after = snapshot_repair_tables(&db);
    assert_eq!(before, after, "preview must not mutate tables");
    assert_no_shadow(&root);
}

#[test]
fn index_repair_preview_human__dry_run__locked_sentence() {
    let tmp = tempdir().unwrap();
    let root = populated_index_root(&tmp);
    let db = ledger_db(&root);
    let before = snapshot_repair_tables(&db);

    let (stdout, _stderr, code) = run_cli(tmp.path(), &["index", "--repair-metadata", "--dry-run"]);
    assert_eq!(code, 0, "human dry-run must exit 0; stdout={stdout}");
    assert!(
        stdout.contains("Current metadata state:"),
        "human dry-run must name current state: {stdout}"
    );
    assert!(
        stdout.contains("Would force full index and replace metadata if successful."),
        "human dry-run sentence frozen: {stdout}"
    );
    assert!(
        !stdout.contains("indexRepairPreview"),
        "human path must not print the JSON kind: {stdout}"
    );

    let after = snapshot_repair_tables(&db);
    assert_eq!(before, after, "human preview must not mutate tables");
    assert_no_shadow(&root);
}
