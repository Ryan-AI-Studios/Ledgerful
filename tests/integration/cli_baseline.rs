//! End-to-end tests for `ledgerful baseline` (Track 0601).
//!
//! Every test that touches `ledger.db` uses `run_cli_isolated`, which strips
//! `LEDGERFUL_STATE_DIR`/`LEDGERFUL_CONFIG_HOME` and points HOME at a
//! synthetic directory, so an operator shell cannot redirect state.

use crate::common::{git_add_and_commit, run_cli_isolated, setup_git_repo};
use ledgerful::state::migrations::get_migrations_count;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).expect("write fixture file");
}

/// Two-file Rust fixture proven by Phase 0 to produce a resolved cross-file
/// edge after `ledgerful init` + `ledgerful index`.
fn indexed_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).expect("repo dir");
    setup_git_repo(&dir);
    write(&dir.join("src/a.rs"), "pub fn caller() { helper(); }\n");
    write(&dir.join("src/b.rs"), "pub fn helper() {}\n");
    git_add_and_commit(&dir, "init");

    let (_out, err, code) = run_cli_isolated(&dir, &["init"]);
    assert_eq!(code, 0, "ledgerful init failed: {err}");
    let (_out, err, code) = run_cli_isolated(&dir, &["index", "--full"]);
    assert_eq!(code, 0, "ledgerful index --full failed: {err}");
    (tmp, dir)
}

fn json(stdout: &str) -> Value {
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("expected JSON on stdout, got {e}: {stdout}");
    })
}

fn create(dir: &Path, name: &str) -> Value {
    let (out, err, code) = run_cli_isolated(dir, &["baseline", "create", name, "--json"]);
    assert_eq!(code, 0, "create {name} failed: {err}");
    assert!(err.trim().is_empty(), "create wrote stderr: {err}");
    json(&out)
}

#[test]
#[allow(non_snake_case)]
fn baseline_create_list_show_compare_unchanged__slow() {
    let (_tmp, dir) = indexed_repo();

    let created = create(&dir, "base1");
    assert_eq!(created["schemaVersion"], 1);
    assert_eq!(created["kind"], "analysisBaselineCreate");
    assert_eq!(created["edgeCount"], 1);
    assert_eq!(
        created["identity"]["inputCompleteness"]["state"],
        "COMPLETE"
    );

    let (out, err, code) = run_cli_isolated(&dir, &["baseline", "list", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.trim().is_empty());
    let listed = json(&out);
    assert_eq!(listed["kind"], "analysisBaselineList");
    assert_eq!(listed["baselines"][0]["name"], "base1");
    assert_eq!(listed["baselines"][0]["edgeCount"], 1);

    let (out, err, code) = run_cli_isolated(&dir, &["baseline", "show", "base1", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.trim().is_empty());
    let shown = json(&out);
    assert_eq!(shown["kind"], "analysisBaselineShow");
    assert_eq!(shown["digestVerified"], true);
    assert_eq!(shown["edges"][0]["srcPath"], "src/a.rs");
    assert_eq!(shown["edges"][0]["dstPath"], "src/b.rs");
    assert_eq!(shown["edges"][0]["weight"], 1);

    let (out, err, code) = run_cli_isolated(&dir, &["baseline", "compare", "base1", "--json"]);
    assert_eq!(
        code, 0,
        "compare without --fail-on-regression exits 0: {err}"
    );
    assert!(err.trim().is_empty());
    let compared = json(&out);
    assert_eq!(compared["kind"], "analysisBaselineCompare");
    assert_eq!(compared["status"], "comparable");
    assert_eq!(compared["regression"], "none");
    assert_eq!(compared["counts"]["unchanged"], 1);
    assert!(compared["items"].as_array().unwrap().is_empty());
}

#[test]
#[allow(non_snake_case)]
fn baseline_compare_added_edge_is_present__slow() {
    let (_tmp, dir) = indexed_repo();
    create(&dir, "base1");

    write(&dir.join("src/c.rs"), "pub fn caller2() { helper(); }\n");
    git_add_and_commit(&dir, "add c");
    let (_out, err, code) = run_cli_isolated(&dir, &["index"]);
    assert_eq!(code, 0, "{err}");

    let (out, _err, code) = run_cli_isolated(&dir, &["baseline", "compare", "base1", "--json"]);
    assert_eq!(code, 0);
    let compared = json(&out);
    assert_eq!(compared["regression"], "present");
    assert_eq!(compared["counts"]["added"], 1);
    let item = compared["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["class"] == "added")
        .expect("added item");
    assert_eq!(item["edgeKey"]["srcPath"], "src/c.rs");
    assert_eq!(item["edgeKey"]["dstPath"], "src/b.rs");

    let (out, _err, code) = run_cli_isolated(
        &dir,
        &[
            "baseline",
            "compare",
            "base1",
            "--json",
            "--fail-on-regression",
        ],
    );
    assert_eq!(code, 1, "present must fail with the flag");
    assert_eq!(json(&out)["regression"], "present", "JSON still on stdout");
}

#[test]
#[allow(non_snake_case)]
fn baseline_compare_worsened_weight_is_present__slow() {
    let (_tmp, dir) = indexed_repo();
    create(&dir, "base1");

    write(
        &dir.join("src/a.rs"),
        "pub fn caller() { helper(); }\npub fn caller3() { helper(); }\n",
    );
    git_add_and_commit(&dir, "second caller");
    let (_out, err, code) = run_cli_isolated(&dir, &["index"]);
    assert_eq!(code, 0, "{err}");

    let (out, _err, code) = run_cli_isolated(&dir, &["baseline", "compare", "base1", "--json"]);
    assert_eq!(code, 0);
    let compared = json(&out);
    assert_eq!(compared["regression"], "present");
    assert_eq!(compared["counts"]["worsened"], 1);
    assert_eq!(compared["counts"]["added"], 0);
    let item = compared["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["class"] == "worsened")
        .expect("worsened item");
    assert_eq!(item["baselineWeight"], 1);
    assert_eq!(item["currentWeight"], 2);

    let (_out, _err, code) = run_cli_isolated(
        &dir,
        &[
            "baseline",
            "compare",
            "base1",
            "--json",
            "--fail-on-regression",
        ],
    );
    assert_eq!(code, 1);
}

#[test]
#[allow(non_snake_case)]
fn baseline_compare_removed_edge_is_none_when_unresolved_not_grown__slow() {
    let (_tmp, dir) = indexed_repo();
    create(&dir, "base1");

    write(&dir.join("src/a.rs"), "pub fn caller() {}\n");
    git_add_and_commit(&dir, "remove call");
    let (_out, err, code) = run_cli_isolated(&dir, &["index"]);
    assert_eq!(code, 0, "{err}");

    let (out, _err, code) = run_cli_isolated(&dir, &["baseline", "compare", "base1", "--json"]);
    assert_eq!(code, 0);
    let compared = json(&out);
    assert_eq!(compared["status"], "comparable");
    assert_eq!(compared["counts"]["removed"], 1);
    assert_eq!(compared["regression"], "none");
    assert!(
        compared["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "RESOLVED_EDGES_REMOVED"),
        "a removal with flat coverage must carry the note: {compared}"
    );
}

#[test]
#[allow(non_snake_case)]
fn baseline_compare_unresolved_growth_is_unknown__slow() {
    let (_tmp, dir) = indexed_repo();
    create(&dir, "base1");

    write(
        &dir.join("src/a.rs"),
        "pub fn caller() { helper(); }\npub fn broken() { missing_thing(); }\n",
    );
    git_add_and_commit(&dir, "add unresolved call");
    let (_out, err, code) = run_cli_isolated(&dir, &["index"]);
    assert_eq!(code, 0, "{err}");

    let (out, _err, code) = run_cli_isolated(&dir, &["baseline", "compare", "base1", "--json"]);
    assert_eq!(code, 0);
    let compared = json(&out);
    assert_eq!(compared["regression"], "unknown");
    assert_eq!(compared["regressionReasons"][0], "UNRESOLVED_GREW");

    let (_out, _err, code) = run_cli_isolated(
        &dir,
        &[
            "baseline",
            "compare",
            "base1",
            "--json",
            "--fail-on-regression",
        ],
    );
    assert_eq!(code, 1, "unknown must fail with the flag");
}

#[test]
#[allow(non_snake_case)]
fn baseline_compare_uncommitted_edit_without_reindex_is_incomparable__slow() {
    let (_tmp, dir) = indexed_repo();
    create(&dir, "base1");

    write(&dir.join("src/b.rs"), "pub fn helper() { /* edited */ }\n");

    let (out, _err, code) = run_cli_isolated(&dir, &["baseline", "compare", "base1", "--json"]);
    assert_eq!(code, 0);
    let compared = json(&out);
    assert_eq!(compared["status"], "incomparable");
    assert_eq!(compared["regression"], "unknown");
    assert!(
        compared["incomparableReasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "CURRENT_INCOMPLETE")
    );
    assert!(
        compared["currentCompleteness"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "CONTENT_DRIFT")
    );
    assert!(compared.get("counts").is_none());
    assert!(compared.get("items").is_none());

    let (_out, _err, code) = run_cli_isolated(
        &dir,
        &[
            "baseline",
            "compare",
            "base1",
            "--json",
            "--fail-on-regression",
        ],
    );
    assert_eq!(code, 1);
}

#[test]
#[allow(non_snake_case)]
fn baseline_create_same_name_twice_is_refused__slow() {
    let (_tmp, dir) = indexed_repo();
    let first = create(&dir, "base1");

    let (_out, err, code) = run_cli_isolated(&dir, &["baseline", "create", "base1", "--json"]);
    assert_eq!(code, 1);
    assert!(err.contains("BASELINE_EXISTS"), "stderr: {err}");

    let (out, _err, code) = run_cli_isolated(&dir, &["baseline", "show", "base1", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        json(&out)["payloadDigest"],
        first["payloadDigest"],
        "the stored row must be byte-identical"
    );
}

#[test]
#[allow(non_snake_case)]
fn baseline_compare_unknown_name_exits_nonzero__slow() {
    let (_tmp, dir) = indexed_repo();
    create(&dir, "base1");

    let (out, err, code) = run_cli_isolated(&dir, &["baseline", "compare", "nosuchname", "--json"]);
    assert_eq!(code, 1);
    assert!(out.trim().is_empty(), "stdout must stay empty: {out}");
    assert!(err.contains("BASELINE_NOT_FOUND"), "stderr: {err}");
}

#[test]
#[allow(non_snake_case)]
fn baseline_create_on_incomplete_index_is_refused__slow() {
    let (_tmp, dir) = indexed_repo();

    write(
        &dir.join("src/a.rs"),
        "pub fn caller() { helper(); }\n// drifted\n",
    );

    let (_out, err, code) = run_cli_isolated(&dir, &["baseline", "create", "drifted", "--json"]);
    assert_eq!(code, 1);
    assert!(err.contains("CONTENT_DRIFT"), "stderr: {err}");

    let (out, _err, code) = run_cli_isolated(&dir, &["baseline", "list", "--json"]);
    assert_eq!(code, 0);
    assert!(
        json(&out)["baselines"].as_array().unwrap().is_empty(),
        "a refused create must write no row"
    );
}

#[test]
fn baseline_create_invalid_name_exits_nonzero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).expect("repo dir");
    setup_git_repo(&dir);

    let (_out, err, code) = run_cli_isolated(&dir, &["baseline", "create", "Bad_Name", "--json"]);
    assert_eq!(code, 1);
    assert!(err.contains("BASELINE_NAME_INVALID"), "stderr: {err}");
    assert!(
        !dir.join(".ledgerful")
            .join("state")
            .join("ledger.db")
            .exists(),
        "an invalid name must be refused before any storage access"
    );
}

#[test]
fn baseline_create_without_index_database_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).expect("repo dir");
    setup_git_repo(&dir);
    write(&dir.join("src/a.rs"), "pub fn a() {}\n");
    git_add_and_commit(&dir, "init");

    let (_out, err, code) = run_cli_isolated(&dir, &["baseline", "create", "base1", "--json"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("ledger.db"),
        "the missing file must be named: {err}"
    );
    assert!(err.contains("INDEX_EMPTY"), "stderr: {err}");
    assert!(
        !dir.join(".ledgerful")
            .join("state")
            .join("ledger.db")
            .exists(),
        "create must not bootstrap a database"
    );
}

#[test]
#[allow(non_snake_case)]
fn baseline_higher_sqlite_version_refused_for_every_subcommand__slow() {
    let (_tmp, dir) = indexed_repo();
    let db = rusqlite::Connection::open(dir.join(".ledgerful/state/ledger.db")).unwrap();
    let supported_schema = i64::try_from(get_migrations_count()).unwrap();
    let future_schema = supported_schema + 1;
    db.execute_batch("DROP TABLE analysis_baselines").unwrap();
    db.pragma_update(None, "user_version", future_schema)
        .unwrap();
    for args in [
        &["baseline", "create", "test", "--json"][..],
        &["baseline", "list", "--json"],
        &["baseline", "show", "test", "--json"],
        &["baseline", "compare", "test", "--json"],
    ] {
        let (out, err, code) = run_cli_isolated(&dir, args);
        assert_eq!(code, 1, "{args:?}: {err}");
        assert!(out.is_empty(), "{args:?}: {out}");
        assert!(err.contains("BASELINE_SCHEMA_TOO_NEW"), "{args:?}: {err}");
        assert!(
            err.contains(&future_schema.to_string()) && err.contains(&supported_schema.to_string()),
            "{args:?}: {err}"
        );
    }
    let schema: i64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(schema, future_schema);
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'analysis_baselines%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
#[allow(non_snake_case)]
fn baseline_populated_bad_timestamp_refuses_with_index_evidence_message__slow() {
    let (_tmp, dir) = indexed_repo();
    let db = rusqlite::Connection::open(dir.join(".ledgerful/state/ledger.db")).unwrap();
    db.execute(
        "UPDATE index_metadata SET value = 'not a timestamp' WHERE key = 'last_indexed_at'",
        [],
    )
    .unwrap();
    let (out, err, code) = run_cli_isolated(&dir, &["baseline", "create", "test", "--json"]);
    assert_eq!(code, 1, "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("INDEX_INDETERMINATE"), "{err}");
    assert!(err.contains("missing or invalid index evidence"), "{err}");
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM analysis_baselines", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
#[allow(non_snake_case)]
fn baseline_malformed_nested_ignore_blocks_create_and_compare__slow() {
    let (_tmp, dir) = indexed_repo();
    create(&dir, "base1");
    write(&dir.join("src/.gitignore"), "invalid\\\n");

    let (out, err, code) = run_cli_isolated(&dir, &["baseline", "compare", "base1", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.is_empty(), "{err}");
    let compared = json(&out);
    assert_eq!(compared["status"], "incomparable");
    assert_eq!(compared["regression"], "unknown");
    assert_eq!(
        compared["incomparableReasons"],
        serde_json::json!(["CURRENT_INCOMPLETE"])
    );
    assert_eq!(
        compared["currentCompleteness"]["reasons"],
        serde_json::json!(["SCAN_INCOMPLETE"])
    );
    assert!(compared.get("items").is_none());
    assert!(compared.get("counts").is_none());

    let (out, err, code) = run_cli_isolated(&dir, &["baseline", "create", "invalid", "--json"]);
    assert_eq!(code, 1, "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("SCAN_INCOMPLETE"), "{err}");
    let (out, err, code) = run_cli_isolated(&dir, &["baseline", "list", "--json"]);
    assert_eq!(code, 0, "{err}");
    let listed = json(&out);
    assert_eq!(listed["baselines"].as_array().unwrap().len(), 1);
    assert_eq!(listed["baselines"][0]["name"], "base1");
}
