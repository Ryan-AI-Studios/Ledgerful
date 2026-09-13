//! 0329: human chrome follows LEDGERFUL_TABLE_STYLE on the child CLI.

use crate::common::{run_cli_env, setup_git_repo};
use std::fs;
use tempfile::tempdir;

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn init_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    let (stdout, stderr, code) = run_cli_env(
        root,
        &["init"],
        &[("LEDGERFUL_NON_INTERACTIVE", "1"), ("NO_COLOR", "1")],
    );
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");
    tmp
}

#[test]
fn doctor_human_ascii_ok_mark_via_child_env() {
    let tmp = init_repo();
    let (stdout, stderr, code) = run_cli_env(
        tmp.path(),
        &["doctor"],
        &[
            ("LEDGERFUL_NON_INTERACTIVE", "1"),
            ("NO_COLOR", "1"),
            ("LEDGERFUL_TABLE_STYLE", "ascii"),
        ],
    );
    assert_eq!(code, 0, "doctor ascii; stderr={stderr}");
    assert!(
        stdout.contains("OK Doctor:") || stdout.contains("FAIL Doctor:"),
        "ascii doctor mark: {stdout}"
    );
    assert!(
        !stdout.contains('✓'),
        "ascii must not emit U+2713: {stdout}"
    );
    assert!(
        !stdout.contains('✗'),
        "ascii must not emit U+2717: {stdout}"
    );
    assert!(
        stdout.contains("== Optional Accelerators"),
        "ascii section rule: {stdout}"
    );
    assert!(
        !stdout.contains('─'),
        "ascii must not emit U+2500: {stdout}"
    );
}

#[test]
fn doctor_human_utf8_checkmark_via_child_env() {
    let tmp = init_repo();
    let (stdout, stderr, code) = run_cli_env(
        tmp.path(),
        &["doctor"],
        &[
            ("LEDGERFUL_NON_INTERACTIVE", "1"),
            ("NO_COLOR", "1"),
            ("LEDGERFUL_TABLE_STYLE", "utf8"),
        ],
    );
    assert_eq!(code, 0, "doctor utf8; stderr={stderr}");
    assert!(
        stdout.contains('✓') || stdout.contains('✗'),
        "utf8 doctor mark: {stdout}"
    );
    assert!(
        !stdout.contains("OK Doctor:"),
        "utf8 must keep checkmark: {stdout}"
    );
}

#[test]
fn config_schema_ascii_lock_cell_is_y() {
    let tmp = init_repo();
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env.example");
    if src.exists() {
        fs::copy(&src, tmp.path().join(".env.example")).unwrap();
        let (iout, ierr, icode) = run_cli_env(
            tmp.path(),
            &["index", "--incremental"],
            &[("LEDGERFUL_NON_INTERACTIVE", "1"), ("NO_COLOR", "1")],
        );
        assert_eq!(icode, 0, "index; stdout={iout} stderr={ierr}");
    }
    let (stdout, stderr, code) = run_cli_env(
        tmp.path(),
        &["config", "schema"],
        &[
            ("LEDGERFUL_NON_INTERACTIVE", "1"),
            ("NO_COLOR", "1"),
            ("LEDGERFUL_TABLE_STYLE", "ascii"),
        ],
    );
    assert_eq!(code, 0, "config schema ascii; stderr={stderr}");
    assert!(
        !stdout.contains('🔒'),
        "ascii schema must not emit lock emoji: {stdout}"
    );
}

#[test]
fn adr_and_schema_json_dumps_hash_equal_twice() {
    let tmp = init_repo();
    let extra = [("LEDGERFUL_NON_INTERACTIVE", "1"), ("NO_COLOR", "1")];
    let (a1, e1, c1) = run_cli_env(tmp.path(), &["ledger", "adr", "list", "--json"], &extra);
    assert_eq!(c1, 0, "adr json 1; stderr={e1}");
    let (a2, e2, c2) = run_cli_env(tmp.path(), &["ledger", "adr", "list", "--json"], &extra);
    assert_eq!(c2, 0, "adr json 2; stderr={e2}");
    assert_eq!(
        sha256_hex(&a1),
        sha256_hex(&a2),
        "adr json must be byte-stable"
    );
    let v: serde_json::Value = serde_json::from_str(a1.trim()).expect("adr json");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "ledgerAdr");

    let (s1, se1, sc1) = run_cli_env(tmp.path(), &["config", "schema", "--json"], &extra);
    assert_eq!(sc1, 0, "schema json 1; stderr={se1}");
    let (s2, se2, sc2) = run_cli_env(tmp.path(), &["config", "schema", "--json"], &extra);
    assert_eq!(sc2, 0, "schema json 2; stderr={se2}");
    assert_eq!(
        sha256_hex(&s1),
        sha256_hex(&s2),
        "schema json must be byte-stable"
    );
}
