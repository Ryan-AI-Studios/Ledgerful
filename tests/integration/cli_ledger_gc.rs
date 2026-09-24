#![allow(non_snake_case)]

use camino::Utf8Path;
use std::fs;
use tempfile::tempdir;

use crate::common::{run_cli, setup_git_repo};

fn init_repo(tmp: &tempfile::TempDir) -> camino::Utf8PathBuf {
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
    setup_git_repo(tmp.path());
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .expect("state dir");
    let (stdout, stderr, code) = run_cli(tmp.path(), &["init", "--force"]);
    assert_eq!(
        code, 0,
        "init must succeed; stdout={stdout}; stderr={stderr}"
    );
    root.to_path_buf()
}

fn start_pending(tmp: &tempfile::TempDir) -> String {
    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "start",
            "gc-preview",
            "--category",
            "CHORE",
            "--message",
            "hermetic gc fixture",
        ],
    );
    assert_eq!(
        code, 0,
        "ledger start must succeed; stdout={stdout}; stderr={stderr}"
    );
    pending_tx_id(tmp)
}

fn pending_tx_id(tmp: &tempfile::TempDir) -> String {
    let db = Utf8Path::from_path(tmp.path())
        .expect("utf8")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    let conn = rusqlite::Connection::open(db.as_std_path()).expect("open ledger.db");
    conn.query_row(
        "SELECT tx_id FROM transactions WHERE status = 'PENDING' LIMIT 1",
        [],
        |row| row.get(0),
    )
    .expect("one PENDING")
}

fn backdate_pending(tmp: &tempfile::TempDir, tx_id: &str) {
    let db = Utf8Path::from_path(tmp.path())
        .expect("utf8")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    let conn = rusqlite::Connection::open(db.as_std_path()).expect("open ledger.db");
    let old = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    conn.execute(
        "UPDATE transactions SET started_at = ?1 WHERE tx_id = ?2",
        rusqlite::params![old, tx_id],
    )
    .expect("backdate");
}

fn write_promote_failed_sidecar(root: &Utf8Path, tx_id: &str) {
    let sidecar = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    let body = serde_json::json!({
        "tx_id": tx_id,
        "commit_msg_hash": "not-a-head-hash",
        "summary": "gc protect",
        "reason": "fixture",
        "promote_failed": true,
        "promote_error": "simulated for gc preview"
    });
    fs::write(sidecar.as_std_path(), body.to_string()).expect("sidecar");
}

fn tx_status(tmp: &tempfile::TempDir, tx_id: &str) -> String {
    let db = Utf8Path::from_path(tmp.path())
        .expect("utf8")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    let conn = rusqlite::Connection::open(db.as_std_path()).expect("open ledger.db");
    conn.query_row(
        "SELECT status FROM transactions WHERE tx_id = ?1",
        rusqlite::params![tx_id],
        |row| row.get(0),
    )
    .expect("status")
}

fn rollback_row_count(tmp: &tempfile::TempDir, tx_id: &str) -> i64 {
    let db = Utf8Path::from_path(tmp.path())
        .expect("utf8")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    let conn = rusqlite::Connection::open(db.as_std_path()).expect("open ledger.db");
    conn.query_row(
        "SELECT COUNT(*) FROM ledger_entries WHERE tx_id = ?1 AND entry_type = 'ROLLBACK'",
        rusqlite::params![tx_id],
        |row| row.get(0),
    )
    .expect("rollback count")
}

#[test]
fn ledger_gc__dry_run_no_selector__zero_plan() {
    let tmp = tempdir().expect("tempdir");
    let _root = init_repo(&tmp);
    let (stdout, stderr, code) = run_cli(tmp.path(), &["ledger", "gc", "--dry-run"]);
    assert_eq!(code, 0, "stdout={stdout}; stderr={stderr}");
    assert!(
        stdout
            .contains("No selector specified: pass --stale and/or --orphans. Nothing was scanned."),
        "{stdout}"
    );
    assert!(
        stdout.contains("Dry-run completed. No transactions were modified."),
        "{stdout}"
    );
    assert!(!stdout.contains("Usage:"), "{stdout}");
    assert!(!stdout.contains("Protected"), "{stdout}");
    assert!(!stdout.contains("Stale PENDING"), "{stdout}");
}

#[test]
fn ledger_gc__combined_empty_dry_run__prints_both_classes() {
    let tmp = tempdir().expect("tempdir");
    let _root = init_repo(&tmp);
    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "gc",
            "--stale",
            "--orphans",
            "--dry-run",
            "--ttl-hours",
            "1",
        ],
    );
    assert_eq!(code, 0, "stdout={stdout}; stderr={stderr}");
    assert!(
        stdout.contains("Stale PENDING (older than 1 hours): 0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Orphans (TTL PENDING scan; same selector as --stale): 0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Protected (recover-orphan; not candidates): 0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Dry-run completed. No transactions were modified."),
        "{stdout}"
    );
    assert!(!stdout.contains("no corresponding git commit"), "{stdout}");
}

#[test]
fn ledger_gc__stale_only_backdated__omits_orphans_section() {
    let tmp = tempdir().expect("tempdir");
    let _root = init_repo(&tmp);
    let tx_id = start_pending(&tmp);
    backdate_pending(&tmp, &tx_id);
    let before = tx_status(&tmp, &tx_id);
    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["ledger", "gc", "--stale", "--dry-run", "--ttl-hours", "1"],
    );
    assert_eq!(code, 0, "stdout={stdout}; stderr={stderr}");
    assert!(
        stdout.contains("Stale PENDING (older than 1 hours): 1"),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("  {tx_id}")), "{stdout}");
    assert!(
        !stdout.contains("Orphans (TTL PENDING scan"),
        "stale-only must omit orphans: {stdout}"
    );
    assert_eq!(tx_status(&tmp, &tx_id), before, "dry-run must not write");
}

#[test]
fn ledger_gc__both_flags_same_candidate__lists_both_classes() {
    let tmp = tempdir().expect("tempdir");
    let _root = init_repo(&tmp);
    let tx_id = start_pending(&tmp);
    backdate_pending(&tmp, &tx_id);
    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "gc",
            "--stale",
            "--orphans",
            "--dry-run",
            "--ttl-hours",
            "1",
        ],
    );
    assert_eq!(code, 0, "stdout={stdout}; stderr={stderr}");
    assert!(
        stdout.contains("Stale PENDING (older than 1 hours): 1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Orphans (TTL PENDING scan; same selector as --stale): 1"),
        "{stdout}"
    );
    assert_eq!(
        stdout.matches(&tx_id).count(),
        2,
        "same id in both class sections: {stdout}"
    );
    assert_eq!(tx_status(&tmp, &tx_id), "PENDING");
}

#[test]
fn ledger_gc__protected_only_dry_run__exit_zero() {
    let tmp = tempdir().expect("tempdir");
    let root = init_repo(&tmp);
    let tx_id = start_pending(&tmp);
    backdate_pending(&tmp, &tx_id);
    write_promote_failed_sidecar(&root, &tx_id);
    let sidecar = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    let before_sidecar = fs::read_to_string(sidecar.as_std_path()).expect("sidecar");
    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "gc",
            "--stale",
            "--orphans",
            "--dry-run",
            "--ttl-hours",
            "1",
        ],
    );
    assert_eq!(code, 0, "stdout={stdout}; stderr={stderr}");
    assert!(
        stdout.contains("Stale PENDING (older than 1 hours): 0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Orphans (TTL PENDING scan; same selector as --stale): 0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Protected (recover-orphan; not candidates): 1"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("  {tx_id} (promote_failed)")),
        "{stdout}"
    );
    assert!(
        stdout.contains("Dry-run completed. No transactions were modified."),
        "{stdout}"
    );
    assert!(
        stderr.contains("PROMOTE_ORPHAN") || stderr.contains("Would refuse"),
        "stderr={stderr}"
    );
    assert_eq!(tx_status(&tmp, &tx_id), "PENDING");
    assert_eq!(
        fs::read_to_string(sidecar.as_std_path()).expect("sidecar after"),
        before_sidecar
    );
}

#[test]
fn ledger_gc__combined_force__rolls_back_once() {
    let tmp = tempdir().expect("tempdir");
    let _root = init_repo(&tmp);
    let tx_id = start_pending(&tmp);
    backdate_pending(&tmp, &tx_id);
    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "gc",
            "--stale",
            "--orphans",
            "--ttl-hours",
            "1",
            "--force",
        ],
    );
    assert_eq!(code, 0, "stdout={stdout}; stderr={stderr}");
    assert!(
        stdout.contains("Successfully cleaned up 1 transaction(s)."),
        "{stdout}"
    );
    assert!(!stdout.contains("Failed to clean up"), "{stdout}");
    assert_eq!(tx_status(&tmp, &tx_id), "ROLLED_BACK");
    assert_eq!(rollback_row_count(&tmp, &tx_id), 1);
}

#[test]
fn ledger_gc__help__drops_git_commit_claim() {
    let tmp = tempdir().expect("tempdir");
    let _root = init_repo(&tmp);
    let (stdout, stderr, code) = run_cli(tmp.path(), &["ledger", "gc", "--help"]);
    assert_eq!(code, 0, "stdout={stdout}; stderr={stderr}");
    assert!(!stdout.contains("no corresponding git commit"), "{stdout}");
    assert!(
        stdout.contains("used with --stale and --orphans"),
        "{stdout}"
    );
    assert!(stdout.contains("same selector as --stale"), "{stdout}");
}
