//! Production-path CLI coverage for ledger search rollback omit/rank (0213 P2).
//!
//! Spawns `env!("CARGO_BIN_EXE_ledgerful")` so human/JSON output cannot be
//! stubbed by serializing a `Vec<String>` or calling format helpers alone.

use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

use crate::common::setup_git_repo;
use crate::ledger_search::{dummy_entry, insert_dummy_tx};
use ledgerful::ledger::db::LedgerDb;
use ledgerful::ledger::types::EntryType;
use ledgerful::state::storage::StorageManager;

const SHARED_TOKEN: &str = "zxq0213sharedtok";
const RBONLY_TOKEN: &str = "zxq0213rbonlytok";
const TX_IMPL: &str = "impl0213aaaaaaaa";
const TX_RB_SHARED: &str = "rbsh0213bbbbbbbb";
const TX_RB_ONLY: &str = "rbon0213cccccccc";
const IMPL_SUMMARY: &str = "Committed implementation CLI pin";
const RB_SUMMARY: &str = "RB";

fn run_bin(root: &Path, args: &[&str]) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("failed to spawn ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn setup_seeded_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    let (init_out, init_err, init_code) = run_bin(root, &["init"]);
    assert_eq!(
        init_code, 0,
        "ledgerful init failed: stderr={init_err} stdout={init_out}"
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    assert!(
        db_path.exists(),
        "expected Layout SoT .ledgerful/state/ledger.db, missing {}",
        db_path.display()
    );

    let storage = StorageManager::init(&db_path).unwrap();
    {
        let db = LedgerDb::new(storage.get_connection());
        insert_dummy_tx(&db, TX_IMPL);
        insert_dummy_tx(&db, TX_RB_SHARED);
        insert_dummy_tx(&db, TX_RB_ONLY);

        db.insert_ledger_entry(&dummy_entry(
            1,
            TX_IMPL,
            EntryType::Implementation,
            SHARED_TOKEN,
            IMPL_SUMMARY,
            "Longer reason so the implementation FTS document is not empty",
        ))
        .unwrap();
        db.insert_ledger_entry(&dummy_entry(
            2,
            TX_RB_SHARED,
            EntryType::Rollback,
            SHARED_TOKEN,
            RB_SUMMARY,
            "x",
        ))
        .unwrap();
        db.insert_ledger_entry(&dummy_entry(
            3,
            TX_RB_ONLY,
            EntryType::Rollback,
            RBONLY_TOKEN,
            RB_SUMMARY,
            "x",
        ))
        .unwrap();
    }
    storage.shutdown().unwrap();
    tmp
}

fn assert_success(args: &[&str], code: i32, stdout: &str, stderr: &str) {
    assert_eq!(
        code, 0,
        "ledgerful {args:?} failed (code {code}): stderr={stderr} stdout={stdout}"
    );
}

#[test]
fn cli_ledger_search_human_omits_rollback_and_prints_honesty() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let args = ["ledger", "search", SHARED_TOKEN];
    let (stdout, stderr, code) = run_bin(root, &args);
    assert_success(&args, code, &stdout, &stderr);

    assert!(
        stdout.contains("impl0213") || stdout.contains(IMPL_SUMMARY),
        "human search must show IMPLEMENTATION identity, got: {stdout}"
    );
    assert!(
        stdout.contains("rolled-back matches omitted"),
        "human search must print omitted honesty, got: {stdout}"
    );
    assert!(
        stdout.contains("--include-rollback"),
        "human search must mention --include-rollback, got: {stdout}"
    );
    assert!(
        !stdout.contains("No ledger entries found matching"),
        "shared-token search is not a miss, got: {stdout}"
    );
    assert!(
        !stdout.contains("rbsh0213"),
        "default human table must not list the rollback tx prefix, got: {stdout}"
    );
    assert!(
        !stdout.contains(RB_SUMMARY),
        "default human table must not list rollback summary {RB_SUMMARY:?}, got: {stdout}"
    );
}

#[test]
fn cli_ledger_search_json_is_bare_array_without_omitted_footer() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let args = ["ledger", "search", SHARED_TOKEN, "--json"];
    let (stdout, stderr, code) = run_bin(root, &args);
    assert_success(&args, code, &stdout, &stderr);

    let trimmed = stdout.trim();
    assert!(
        trimmed.starts_with('['),
        "JSON must be a bare array, got: {stdout}"
    );
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .expect("JSON stdout must parse as a single value (no footer)");
    let arr = value
        .as_array()
        .unwrap_or_else(|| panic!("expected JSON array, got: {stdout}"));
    assert!(
        !stdout.contains("rolled-back matches omitted"),
        "JSON must not print omitted honesty, got: {stdout}"
    );
    assert!(
        !stdout.contains("--include-rollback"),
        "JSON must not print --include-rollback footer, got: {stdout}"
    );
    assert_eq!(
        arr.len(),
        1,
        "default JSON must omit ROLLBACK, got: {stdout}"
    );
    for item in arr {
        assert_eq!(
            item["entry_type"], "IMPLEMENTATION",
            "default JSON must only include IMPLEMENTATION, got: {stdout}"
        );
    }
}

#[test]
fn cli_ledger_search_rollback_only_human_is_not_a_bare_miss() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let args = ["ledger", "search", RBONLY_TOKEN];
    let (stdout, stderr, code) = run_bin(root, &args);
    assert_success(&args, code, &stdout, &stderr);

    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("No ledger entries found matching"),
        "rollback-only human still reports a miss, got: {combined}"
    );
    assert!(
        combined.contains("rolled-back matches omitted"),
        "rollback-only human must not be a bare miss; expected omitted line, got: {combined}"
    );
}

#[test]
fn cli_ledger_search_include_rollback_json_ranks_non_rollback_first() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let args = [
        "ledger",
        "search",
        SHARED_TOKEN,
        "--include-rollback",
        "--json",
    ];
    let (stdout, stderr, code) = run_bin(root, &args);
    assert_success(&args, code, &stdout, &stderr);

    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("include-rollback JSON must parse");
    let arr = value
        .as_array()
        .unwrap_or_else(|| panic!("expected JSON array, got: {stdout}"));
    assert!(
        arr.iter().any(|e| e["entry_type"] == "ROLLBACK"),
        "opt-in JSON must include ROLLBACK, got: {stdout}"
    );
    assert!(
        !arr.is_empty(),
        "include-rollback JSON must not be empty, got: {stdout}"
    );
    assert_ne!(
        arr[0]["entry_type"], "ROLLBACK",
        "first element must not be ROLLBACK, got: {stdout}"
    );
}

#[test]
fn cli_ledger_history_alias_include_rollback_parses() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let args = [
        "ledger",
        "history",
        SHARED_TOKEN,
        "--include-rollback",
        "--json",
    ];
    let (stdout, stderr, code) = run_bin(root, &args);
    assert_success(&args, code, &stdout, &stderr);

    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("history alias JSON must parse");
    assert!(
        value.is_array(),
        "history alias must emit a JSON array, got: {stdout}"
    );
}
