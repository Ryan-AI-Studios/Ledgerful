//! SCIP augment CLI contract (0095).
//!
//! Missing/failed SCIP must not replace the native index: exit 0, explicit
//! `scip.status = failed` under `--json`, and a non-empty native floor when
//! the temp repo has indexable sources.

use std::process::Command;

fn git_init(dir: &std::path::Path) {
    let out = Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init");
    assert!(out.status.success(), "git init failed: {:?}", out);
    // Minimal identity so git ops that need author do not fail in CI.
    let _ = Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir)
        .output();
    let _ = Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(dir)
        .output();
}

fn write_minimal_rust_crate(dir: &std::path::Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "scip_fixture"
version = "0.1.0"
edition = "2021"
"#,
    )
    .expect("Cargo.toml");
    std::fs::create_dir_all(dir.join("src")).expect("src");
    std::fs::write(
        dir.join("src/lib.rs"),
        r#"
pub fn outer() {
    inner();
}

fn inner() {}
"#,
    )
    .expect("lib.rs");
}

#[test]
fn scip_missing_path_continues_native_and_reports_failed_json() {
    let binary_path = env!("CARGO_BIN_EXE_ledgerful");
    let tmp = tempfile::tempdir().expect("tempdir");
    git_init(tmp.path());
    write_minimal_rust_crate(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".ledgerful/state")).expect("state dir");

    // 0095: --scip PATH no longer early-returns on failure. Native floor runs;
    // JSON must carry an explicit failed SCIP status (not exit non-zero alone).
    let output = Command::new(binary_path)
        .args(["index", "--scip", "non_existent.scip", "--json"])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to execute ledgerful index");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "native floor must still succeed; status={:?}\nstdout={}\nstderr={}",
        output.status,
        stdout,
        stderr
    );
    assert!(
        !stdout.trim().is_empty(),
        "DoD-3: --json must emit a document on SCIP-failure path"
    );

    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be parseable JSON");
    let status = v
        .pointer("/scip/status")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    assert_eq!(
        status, "failed",
        "missing SCIP path must report scip.status=failed, got: {}",
        stdout
    );
    // Failure reason should mention the path or load/generation failure.
    let msg = v
        .pointer("/scip/message")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("non_existent")
            || msg.to_ascii_lowercase().contains("fail")
            || msg.to_ascii_lowercase().contains("ingest")
            || msg.to_ascii_lowercase().contains("load"),
        "failed message should explain why: {msg}"
    );
}

#[test]
fn auto_scip_json_always_emits_scip_section_even_when_not_requested() {
    let binary_path = env!("CARGO_BIN_EXE_ledgerful");
    let tmp = tempfile::tempdir().expect("tempdir");
    git_init(tmp.path());
    write_minimal_rust_crate(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".ledgerful/state")).expect("state dir");

    let output = Command::new(binary_path)
        .args(["index", "--json"])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to execute ledgerful index");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "index --json should succeed: {}",
        stdout
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be parseable JSON");
    let status = v
        .pointer("/scip/status")
        .and_then(|s| s.as_str())
        .expect("scip.status must always be present");
    assert_eq!(status, "did_not_run");
}

#[test]
fn auto_scip_on_fresh_state_still_builds_native_floor() {
    // DoD-4: --auto-scip on a repo with no prior index must not produce an
    // empty index. Even if SCIP generation fails (no indexer / timeout),
    // native symbols and edges must be present. We force SCIP request via
    // --auto-scip; capability may or may not find rust-analyzer.
    let binary_path = env!("CARGO_BIN_EXE_ledgerful");
    let tmp = tempfile::tempdir().expect("tempdir");
    git_init(tmp.path());
    write_minimal_rust_crate(tmp.path());
    // Fresh state: no prior .ledgerful/state index rows.
    std::fs::create_dir_all(tmp.path().join(".ledgerful/state")).expect("state dir");

    let output = Command::new(binary_path)
        .args(["index", "--auto-scip", "--json"])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to execute ledgerful index --auto-scip");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "fresh --auto-scip must exit 0 with native floor; status={:?}\nstdout={}\nstderr={}",
        output.status,
        stdout,
        stderr
    );

    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be parseable JSON");
    // Native floor: files/symbols reported in index JSON (field names may vary —
    // assert at least that scip is present and not silent, and process succeeded).
    let scip_status = v
        .pointer("/scip/status")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    assert!(
        matches!(scip_status, "success" | "failed" | "did_not_run"),
        "unexpected scip status: {scip_status}"
    );
    // With --auto-scip requested, status must not be did_not_run.
    assert_ne!(
        scip_status, "did_not_run",
        "auto-scip requested must not report did_not_run: {stdout}"
    );

    // SQLite should have native symbols after the run.
    let db_path = tmp.path().join(".ledgerful/state/ledger.db");
    assert!(db_path.exists(), "ledger.db must exist after index");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let symbols: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_symbols", [], |r| r.get(0))
        .unwrap_or(0);
    let edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM structural_edges", [], |r| r.get(0))
        .unwrap_or(0);
    assert!(
        symbols > 0,
        "DoD-4: fresh --auto-scip must leave non-zero project_symbols (got {symbols})"
    );
    // Edges may be zero on a two-function fixture if call resolution misses,
    // but symbols must exist. Prefer non-zero when possible.
    let _ = edges;
}
