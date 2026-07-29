//! Track 0100 — CLI output scannability fixtures.
//! Summary-first verify/doctor, honest dead-code labels, search truncation,
//! config help examples.

use crate::common::{
    DirGuard, TempEnv, git_add_and_commit, non_interactive, run_cli, setup_git_repo,
};
use ledgerful::commands::init::execute_init;
use ledgerful::config::model::Config;
use ledgerful::ledger::{
    Category, ChangeType, CommitRequest, TransactionManager, TransactionRequest,
};
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::fs;
use std::path::PathBuf;
use tempfile::{TempDir, tempdir};

/// Hermetic repo with production-signed ledger entries for `verify --signatures`.
struct SignedLedgerFixture {
    #[allow(dead_code)]
    dir: TempDir,
    root: PathBuf,
    db_path: PathBuf,
    #[allow(dead_code)]
    _cwd: DirGuard,
    #[allow(dead_code)]
    _home: TempEnv,
    #[allow(dead_code)]
    _profile: TempEnv,
}

/// Create a hermetic git repo, `init`, and commit `extra_signed` production-signed
/// ledger entries (in addition to the init gate-mode row). Total LOCAL signed
/// rows is therefore ≥ `extra_signed` (typically ≥3 when `extra_signed >= 3`).
fn setup_signed_ledger_fixture(extra_signed: usize) -> SignedLedgerFixture {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    setup_git_repo(&root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(&root, "initial");

    // Keep keys + state inside the temp dir (never touch real home).
    let home = TempEnv::set("HOME", root.to_str().unwrap());
    let profile = TempEnv::set("USERPROFILE", root.to_str().unwrap());
    let cwd = DirGuard::new(&root);

    execute_init(false, false).unwrap();

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    fs::create_dir_all(root.join("src")).unwrap();

    {
        let mut storage = StorageManager::init(&db_path).unwrap();
        let mut tx_mgr = TransactionManager::new(&mut storage, root.clone(), Config::default());
        for i in 0..extra_signed {
            let entity = format!("src/scannability_{i}.rs");
            fs::write(root.join(&entity), format!("// scannability fixture {i}\n")).unwrap();
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: entity.clone(),
                    planned_action: Some(format!("scannability signed entry {i}")),
                    ..Default::default()
                })
                .expect("start_change should succeed");
            tx_mgr
                .commit_change(
                    tx_id,
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: format!("scannability signed entry {i}"),
                        reason: "0100 golden fixture".to_string(),
                        committed_at: Some(format!("2026-07-28T12:00:{i:02}Z")),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .expect("commit_change should sign and succeed");
        }
    }

    SignedLedgerFixture {
        dir,
        root,
        db_path,
        _cwd: cwd,
        _home: home,
        _profile: profile,
    }
}

fn has_per_entry_valid_tx_line(text: &str) -> bool {
    // Per-entry lines look like `  [VALID (unknown key)] TX <id>`; status may be
    // ANSI-coloured so `[` and `VALID` are not always adjacent.
    text.lines().any(|l| {
        let has_status = l.contains("VALID (unknown key)")
            || l.contains("VALID (trusted)")
            || l.contains("[VALID");
        has_status && l.contains("TX ")
    })
}

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

/// DoD-9 / F-001: multi-entry signed ledger — default is summary-first (no
/// per-entry `[VALID] TX` lines); aggregate summary is present.
#[test]
#[serial(cwd, env)]
fn verify_signatures_default_hides_per_entry_valid() {
    let _ni = non_interactive();
    let fixture = setup_signed_ledger_fixture(3);
    let root = fixture.root.as_path();

    let (stdout, stderr, code) = run_cli(root, &["verify", "--signatures"]);
    assert!(
        code == 0 || code == 1 || code == 3,
        "unexpected exit {code}; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Signature verification summary")
            || stderr.contains("Signature verification summary"),
        "default must print aggregate summary; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !has_per_entry_valid_tx_line(&stdout),
        "default must not print per-entry [VALID] TX lines; stdout:\n{stdout}"
    );
    assert!(
        !has_per_entry_valid_tx_line(&stderr),
        "default must not print per-entry [VALID] TX on stderr; stderr:\n{stderr}"
    );
}

/// DoD-9 / F-001: `--verbose` restores per-entry VALID lines when entries exist.
#[test]
#[serial(cwd, env)]
fn verify_signatures_verbose_shows_per_entry_valid() {
    let _ni = non_interactive();
    let fixture = setup_signed_ledger_fixture(3);
    let root = fixture.root.as_path();

    let (stdout, stderr, code) = run_cli(root, &["verify", "--signatures", "--verbose"]);
    assert!(
        code == 0 || code == 1 || code == 3,
        "verbose signatures unexpected exit {code}; stdout={stdout} stderr={stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("Signature verification summary"),
        "verbose must still show aggregate; out:\n{combined}"
    );
    // Per-entry status: VALID (trusted) or VALID (unknown key) with TX id.
    assert!(
        has_per_entry_valid_tx_line(&combined)
            || (combined.contains("[VALID") && combined.contains("TX ")),
        "verbose must restore per-entry VALID lines when entries exist; out:\n{combined}"
    );
}

/// Quiet path: aggregate still visible; no per-entry VALID; does not panic.
#[test]
#[serial(cwd, env)]
fn verify_signatures_quiet_wiring() {
    let _ni = non_interactive();
    let fixture = setup_signed_ledger_fixture(3);
    let root = fixture.root.as_path();

    let (stdout, stderr, code) = run_cli(root, &["verify", "--signatures", "--quiet"]);
    assert!(
        code == 0 || code == 1 || code == 3,
        "unexpected exit {code}; stdout={stdout} stderr={stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("Signature verification summary"),
        "quiet must still show aggregate summary; out:\n{combined}"
    );
    assert!(
        !has_per_entry_valid_tx_line(&stdout) && !has_per_entry_valid_tx_line(&stderr),
        "quiet must not print per-entry [VALID] TX lines; out:\n{combined}"
    );
}

/// F-004: INVALID hard failures stay on raw stderr at default / quiet / verbose.
#[test]
#[serial(cwd, env)]
fn verify_signatures_invalid_visible_on_stderr_all_levels() {
    let _ni = non_interactive();
    let fixture = setup_signed_ledger_fixture(3);
    let root = fixture.root.as_path();

    // Corrupt one signature so classification is INVALID (cheap SQL write).
    {
        let storage = StorageManager::init(&fixture.db_path).unwrap();
        let conn = storage.get_connection();
        let updated = conn
            .execute(
                "UPDATE ledger_entries SET signature = '00' WHERE rowid = (
                    SELECT rowid FROM ledger_entries
                    WHERE signature IS NOT NULL AND signature != ''
                    ORDER BY committed_at DESC LIMIT 1
                )",
                [],
            )
            .expect("corrupt signature update");
        assert!(
            updated >= 1,
            "expected to corrupt at least one signed entry"
        );
    }

    for args in [
        ["verify", "--signatures"].as_slice(),
        ["verify", "--signatures", "--quiet"].as_slice(),
        ["verify", "--signatures", "--verbose"].as_slice(),
    ] {
        let (stdout, stderr, code) = run_cli(root, args);
        assert_eq!(
            code, 1,
            "INVALID should exit 1 for args={args:?}; stdout={stdout} stderr={stderr}"
        );
        assert!(
            stderr.contains("INVALID"),
            "INVALID must appear on stderr at all verbosity levels; args={args:?}\nstderr:\n{stderr}\nstdout:\n{stdout}"
        );
        // Hard failures must not be filter-gated onto stdout-only paths.
        let _ = stdout;
    }
}

/// Empty-ledger smoke (init only, no extra commits): still no per-entry VALID.
#[test]
fn verify_signatures_empty_ledger_no_per_entry_valid() {
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
        !has_per_entry_valid_tx_line(&stdout),
        "empty/default must not print per-entry [VALID] TX; stdout:\n{stdout}"
    );
}
