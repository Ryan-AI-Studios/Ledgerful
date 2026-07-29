//! Track 0100 — CLI output scannability fixtures.
//! Summary-first verify/doctor, honest dead-code labels, search truncation,
//! config help examples.

use crate::common::{git_add_and_commit, run_cli, setup_git_repo};
use std::fs;
use tempfile::tempdir;

/// Doctor leads with an aggregate status line (0100 DoD-5).
#[test]
fn doctor_first_meaningful_line_is_aggregate() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let (stdout, _stderr, code) = run_cli(root, &["doctor"]);
    // Soft failures (e.g. not-configured embedding) still exit 0.
    assert_eq!(
        code, 0,
        "doctor without CRITICAL should exit 0; stderr={_stderr}"
    );

    let first = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    assert!(
        first.contains("Doctor:"),
        "first non-empty line must be aggregate status, got: {first:?}\nfull:\n{stdout}"
    );
    assert!(
        stdout.contains("Optional Accelerators"),
        "optional accelerators section missing:\n{stdout}"
    );
}

/// Dead-code human output carries honest-ceiling footer + empty-state wording.
#[test]
fn dead_code_honesty_footer_and_empty_state() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let (init_out, init_err, init_code) = run_cli(root, &["init", "--force"]);
    assert_eq!(
        init_code, 0,
        "init failed: stdout={init_out} stderr={init_err}"
    );

    // High threshold → empty findings path on a tiny repo without index symbols.
    let (stdout, stderr, code) = run_cli(root, &["dead-code", "--threshold", "0.99"]);
    assert_eq!(code, 0, "dead-code failed: stderr={stderr}");
    assert!(
        stdout.contains("Heuristic evidence") || stdout.contains("not proof of dead code"),
        "honesty footer missing:\n{stdout}"
    );
    // Either empty state or table+footer; empty path must not use old dishonest copy.
    assert!(
        !stdout.contains("No dead code found above threshold"),
        "old empty-state wording must not appear:\n{stdout}"
    );
    if !stdout.contains('┌') && !stdout.contains("Symbol") {
        assert!(
            stdout.contains("heuristic analysis") || stdout.contains("No findings above threshold"),
            "empty path should use heuristic empty-state:\n{stdout}"
        );
    }
}

/// Search human truncation affordance when more hits than --limit (0100 DoD-7).
#[test]
fn search_limit_shows_and_more_results() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    // Distinct files sharing a common token so BM25/hybrid returns many hits.
    for i in 0..6 {
        fs::write(
            root.join(format!("hit_{i}.rs")),
            format!("// scannability_marker_token unique_{i}\npub fn f_{i}() {{}}"),
        )
        .unwrap();
    }
    git_add_and_commit(root, "hits");

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "search",
            "scannability_marker_token",
            "--index",
            "--limit",
            "2",
        ],
    );
    assert_eq!(code, 0, "search failed: stderr={stderr}");
    assert!(
        stdout.contains("and more results") && stdout.contains("--limit"),
        "expected truncation affordance; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Must not claim an exact remaining count like "3 more".
    for line in stdout.lines() {
        if line.contains("and more results") {
            assert!(
                !line.contains(" more results (")
                    || !line
                        .split_whitespace()
                        .any(|w| w.chars().all(|c| c.is_ascii_digit())),
                "must not claim exact K more; line={line}"
            );
        }
    }
}

/// Config help lists real subcommands only (no `show`) — DoD-8.
#[test]
fn config_help_includes_real_subcommand_examples() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(root, &["config", "--help"]);
    let combined = format!("{stdout}{stderr}");
    assert!(
        code == 0 || combined.contains("Usage"),
        "config --help failed: code={code} out={combined}"
    );
    assert!(
        combined.contains("config view") || combined.contains("ledgerful config view"),
        "examples must include config view:\n{combined}"
    );
    assert!(
        combined.contains("config set") || combined.contains("coverage.enabled"),
        "examples must include config set key=value:\n{combined}"
    );
    assert!(
        !combined.contains("config show"),
        "must not advertise non-existent `show` subcommand:\n{combined}"
    );
}

/// verify --signatures on an init'd empty ledger: default must not emit per-entry
/// `[VALID] TX` lines. Filter-level behaviour is covered in unit tests.
#[test]
fn verify_signatures_default_hides_per_entry_valid() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let (init_out, init_err, init_code) = run_cli(root, &["init", "--force"]);
    assert_eq!(
        init_code, 0,
        "init failed: stdout={init_out} stderr={init_err}"
    );

    let (stdout, stderr, code) = run_cli(root, &["verify", "--signatures"]);
    assert!(
        code == 0 || code == 1 || code == 3,
        "unexpected exit {code}; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout
            .lines()
            .any(|l| l.contains("[VALID]") && l.contains("TX ")),
        "default must not print per-entry [VALID] lines; stdout:\n{stdout}"
    );

    // Verbose still runs cleanly (restores DEBUG filter for per-entry when present).
    let (v_stdout, v_stderr, v_code) = run_cli(root, &["verify", "--signatures", "--verbose"]);
    assert!(
        v_code == 0 || v_code == 1 || v_code == 3,
        "verbose signatures unexpected exit {v_code}; {v_stdout}{v_stderr}"
    );
}

/// Quiet signatures path stays wired; hard failures use raw stderr (unit-tested
/// on SigEntryStream). Empty ledger must not panic under --quiet.
#[test]
fn verify_signatures_quiet_wiring() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");
    let (init_out, init_err, init_code) = run_cli(root, &["init", "--force"]);
    assert_eq!(init_code, 0, "init: {init_out}{init_err}");

    let (stdout, stderr, code) = run_cli(root, &["verify", "--signatures", "--quiet"]);
    assert!(
        code == 0 || code == 1 || code == 3,
        "unexpected exit {code}; stdout={stdout} stderr={stderr}"
    );
}
