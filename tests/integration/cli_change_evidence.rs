//! Command-level verification capture coverage in an isolated Git checkout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::{TempDir, tempdir};

use crate::common::{isolated_command, nested_repo_root, synthetic_home};

struct Checkout {
    _temp: TempDir,
    #[cfg(windows)]
    fixture: PathBuf,
    root: PathBuf,
    home: PathBuf,
}

impl Checkout {
    fn new() -> Self {
        let temp = tempdir().expect("capture tempdir");
        #[cfg(windows)]
        let fixture = dunce::canonicalize(temp.path()).expect("canonicalize private fixture");
        #[cfg(windows)]
        let fixture_path: &Path = fixture.as_path();
        #[cfg(not(windows))]
        let fixture_path: &Path = temp.path();
        let root = nested_repo_root(fixture_path);
        let home = synthetic_home(fixture_path);
        let checkout = Self {
            _temp: temp,
            #[cfg(windows)]
            fixture,
            root,
            home,
        };
        checkout.git(&["init", "-b", "main", "--object-format=sha1", "--template="]);
        checkout.git(&["config", "core.autocrlf", "false"]);
        checkout.git(&["config", "core.filemode", "false"]);
        fs::write(checkout.root.join(".gitignore"), ".ledgerful/\n").unwrap();
        fs::write(checkout.root.join("fixture.txt"), "initial\n").unwrap();
        checkout.git(&["add", ".gitignore", "fixture.txt"]);
        checkout.git(&["commit", "-m", "capture fixture"]);
        checkout
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = isolated_command(binary, &self.home);
        command
            .current_dir(&self.root)
            .env("LEDGERFUL_CONFIG_HOME", self.home.join("config"))
            .env("LEDGERFUL_NON_INTERACTIVE", "1")
            .env("LEDGERFUL_NO_NETWORK", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "Capture Fixture")
            .env("GIT_AUTHOR_EMAIL", "capture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Capture Fixture")
            .env("GIT_COMMITTER_EMAIL", "capture@example.invalid")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CONFIG")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS");
        command
    }

    fn git(&self, args: &[&str]) -> Output {
        successful(self.command("git").args(args))
    }

    fn cli(&self, args: &[&str]) -> Command {
        let mut command = self.command(env!("CARGO_BIN_EXE_ledgerful"));
        command.args(args);
        command
    }
}

fn successful(command: &mut Command) -> Output {
    let output = command.output().expect("run fixture command");
    assert!(
        output.status.success(),
        "{command:?} exited {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn verify_json(command: &mut Command) -> Value {
    let output = successful(command);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "verify did not emit one JSON object ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn assert_no_database_companions(database: &Path) {
    for filename in ["ledger.db-wal", "ledger.db-shm", "ledger.db-journal"] {
        assert!(!database.with_file_name(filename).exists(), "{filename}");
    }
}

// Windows ordinary local storage is the current positive platform minimum
// for both executed and plan-only capture.
#[cfg(windows)]
#[test]
fn capture_records_exact_executed_result_and_separate_fast_preview() {
    let checkout = Checkout::new();
    successful(&mut checkout.cli(&["init"]));

    let preview = verify_json(&mut checkout.cli(&[
        "verify",
        "--scope",
        "fast",
        "--dry-run",
        "--capture-change-evidence",
        "--json",
    ]));
    assert_eq!(preview["executed"], false, "{preview}");
    let planned = &preview["changeEvidenceCapture"];
    assert_eq!(planned["status"], "recorded", "{preview}");
    assert_eq!(planned["attemptKind"], "dry-run", "{preview}");
    assert!(planned.get("runId").is_none(), "{planned}");
    let planned_id = planned["attemptId"].as_str().unwrap();

    let report = verify_json(&mut checkout.cli(&[
        "verify",
        "--scope",
        "full",
        "--capture-change-evidence",
        "--json",
        "git --version",
    ]));
    assert_eq!(report["ok"], true, "{report}");
    let executed = &report["changeEvidenceCapture"];
    assert_eq!(executed["status"], "recorded", "{report}");
    assert_eq!(executed["attemptKind"], "executed", "{report}");
    let executed_id = executed["attemptId"].as_str().unwrap();
    assert_ne!(planned_id, executed_id);
    let run_id = executed["runId"].as_i64().unwrap();
    let legacy =
        verify_json(&mut checkout.cli(&["verify", "--scope", "full", "--json", "git --version"]));
    assert_eq!(legacy["ok"], true, "{legacy}");
    assert!(legacy.get("changeEvidenceCapture").is_none(), "{legacy}");
    assert_eq!(legacy["steps"][0]["command"], report["steps"][0]["command"]);
    assert_eq!(
        legacy["steps"][0]["exitCode"],
        report["steps"][0]["exitCode"]
    );

    let head = checkout.git(&["rev-parse", "HEAD"]);
    let head = std::str::from_utf8(&head.stdout).unwrap().trim();
    let database = checkout.root.join(".ledgerful/state/ledger.db");
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let plan: (String, Option<i64>, i64) = connection
        .query_row(
            "SELECT head, run_id, sample_count FROM change_evidence_attempts WHERE attempt_id = ?1",
            [planned_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(plan, (head.to_string(), None, 0));
    let execution: (String, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT a.head, a.run_id, a.sample_count, s.position, r.exit_code
             FROM change_evidence_attempts a
             JOIN change_evidence_samples s
               ON s.repo_scope = a.repo_scope AND s.attempt_id = a.attempt_id
             JOIN verification_results r ON r.id = s.result_id AND r.run_id = a.run_id
             WHERE a.attempt_id = ?1",
            [executed_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(execution, (head.to_string(), run_id, 1, 0, 0));
    let counts: (i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM change_evidence_attempts),
                    (SELECT COUNT(*) FROM verification_runs)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (2, 2));
}

#[test]
fn dirty_preview_with_state_override_does_not_touch_either_store() {
    let checkout = Checkout::new();
    fs::write(checkout.root.join("fixture.txt"), "dirty\n").unwrap();
    let foreign = checkout.home.join("override");
    fs::create_dir_all(foreign.join("state")).unwrap();
    let sentinel = foreign.join("state/ledger.db");
    fs::write(&sentinel, b"foreign fixture sentinel").unwrap();

    let preview = verify_json(
        checkout
            .cli(&[
                "verify",
                "--scope",
                "full",
                "--dry-run",
                "--capture-change-evidence",
                "--json",
                "git --version",
            ])
            .env("LEDGERFUL_STATE_DIR", &foreign),
    );

    let capture = &preview["changeEvidenceCapture"];
    assert_eq!(preview["executed"], false, "{preview}");
    assert_eq!(capture["status"], "unavailable", "{preview}");
    assert!(
        matches!(
            capture["reasonCode"].as_str(),
            Some("scope-unavailable" | "platform-unavailable")
        ),
        "{preview}",
    );
    assert!(capture.get("attemptId").is_none(), "{capture}");
    assert!(capture.get("runId").is_none(), "{capture}");
    assert_eq!(fs::read(&sentinel).unwrap(), b"foreign fixture sentinel");
    assert_no_database_companions(&sentinel);
    assert!(!checkout.root.join(".ledgerful").exists());
}

#[cfg(windows)]
#[test]
fn preview_refuses_linked_checkout_and_copied_database() {
    let checkout = Checkout::new();
    successful(&mut checkout.cli(&["init"]));
    let preview_args = [
        "verify",
        "--scope",
        "full",
        "--dry-run",
        "--capture-change-evidence",
        "--json",
        "git --version",
    ];
    let source = verify_json(&mut checkout.cli(&preview_args));
    assert_eq!(
        source["changeEvidenceCapture"]["status"], "recorded",
        "{source}"
    );
    let source_db = checkout.root.join(".ledgerful/state/ledger.db");
    let source_bytes = fs::read(&source_db).unwrap();
    assert_no_database_companions(&source_db);

    let sibling = checkout.fixture.join("linked");
    checkout.git(&["worktree", "add", "--detach", sibling.to_str().unwrap()]);
    let linked = verify_json(checkout.cli(&preview_args).current_dir(&sibling));
    let capture = &linked["changeEvidenceCapture"];
    assert_eq!(capture["status"], "unavailable", "{linked}");
    assert_eq!(capture["reasonCode"], "scope-unavailable", "{linked}");
    assert!(capture.get("attemptId").is_none(), "{linked}");
    assert!(!sibling.join(".ledgerful").exists());
    assert_eq!(fs::read(&source_db).unwrap(), source_bytes);
    assert_no_database_companions(&source_db);

    let independent = checkout.fixture.join("independent");
    checkout.git(&[
        "clone",
        "--quiet",
        "--no-local",
        checkout.root.to_str().unwrap(),
        independent.to_str().unwrap(),
    ]);
    let copied = independent.join(".ledgerful/state/ledger.db");
    fs::create_dir_all(copied.parent().unwrap()).unwrap();
    fs::copy(&source_db, &copied).unwrap();
    let result = verify_json(checkout.cli(&preview_args).current_dir(&independent));
    let capture = &result["changeEvidenceCapture"];
    assert_eq!(capture["status"], "unavailable", "{result}");
    assert_eq!(capture["reasonCode"], "scope-unavailable", "{result}");
    assert!(capture.get("attemptId").is_none(), "{result}");
    assert_eq!(fs::read(&copied).unwrap(), source_bytes);
    assert_no_database_companions(&copied);
}
