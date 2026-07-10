use crate::common::{DirGuard, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::verify::execute_verify;
use ledgerful::verify::plan::VerifyScope;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_verify_command_pass() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    let cmd = "echo hello";
    let result = execute_verify(
        Some(cmd.into()),
        None,
        5,
        false,
        false,
        None,
        false,
        false,
        VerifyScope::Full,
    );
    assert!(result.is_ok());
}

#[test]
fn test_verify_command_fail() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    let cmd = "exit 1";
    let result = execute_verify(
        Some(cmd.into()),
        None,
        5,
        false,
        false,
        None,
        false,
        false,
        VerifyScope::Full,
    );
    assert!(result.is_err());
}

#[test]
fn test_verify_command_timeout() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    let cmd = if cfg!(target_os = "windows") {
        "ping -n 10 127.0.0.1 >nul"
    } else {
        "sleep 10"
    };
    let result = execute_verify(
        Some(cmd.into()),
        None,
        1,
        false,
        false,
        None,
        false,
        false,
        VerifyScope::Full,
    );
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.err().unwrap());
    assert!(err_msg.contains("Timed out"));
}

#[test]
fn test_verify_command_not_found() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    let result = execute_verify(
        Some("nonexistent_command_9999".into()),
        None,
        5,
        false,
        false,
        None,
        false,
        false,
        VerifyScope::Full,
    );
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.err().unwrap());
    assert!(err_msg.contains("Command not found"));
}

// CR5: --dry-run flag should always succeed without executing any command.
#[test]
fn test_verify_dry_run_does_not_execute() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    let result = execute_verify(
        Some("nonexistent_command_that_would_fail_if_run".into()),
        None,
        5,
        false,
        false,
        None,
        false,
        true, // dry_run = true
        VerifyScope::Full,
    );
    assert!(
        result.is_ok(),
        "dry-run should succeed even with a bad command: {:?}",
        result.err()
    );
}

// CR5: --health flag should pass for a known executable.
#[test]
fn test_verify_health_check_known_executable() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    let result = execute_verify(
        Some("cargo --version".into()),
        None,
        10,
        false,
        false,
        None,
        true, // health = true
        false,
        VerifyScope::Full,
    );
    assert!(
        result.is_ok(),
        "health check for 'cargo' should pass: {:?}",
        result.err()
    );
}

// CR5: --health flag should fail for a missing executable.
#[test]
fn test_verify_health_check_missing_executable() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    // Health mode checks config steps and auto-detected tools. We test that
    // health mode completes without panicking/hanging on a normal dev machine.
    let result = execute_verify(
        None,
        None,
        5,
        false,
        false,
        None,
        true, // health = true
        false,
        VerifyScope::Full,
    );
    // On a dev machine with cargo available, health check should succeed.
    assert!(
        result.is_ok(),
        "health check should succeed on dev machine: {:?}",
        result.err()
    );
}

// CR4 regression: env-var prefix commands must correctly identify the real executable.
#[test]
fn test_verify_health_check_env_prefix_command() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    // Health check passes None manual command so it uses auto-detection.
    // The key test is that it doesn't crash or hang.
    let result = execute_verify(
        None,
        None,
        10,
        false,
        false,
        None,
        true, // health = true
        false,
        VerifyScope::Full,
    );
    assert!(
        result.is_ok(),
        "health check on dev machine should not error: {:?}",
        result.err()
    );
}

// CG-F35 (requirement #1, #6): `verify` must warn -- visibly, on the
// terminal, not just inside a JSON report nobody reads -- when the cached
// impact packet driving its predictions/plan-ordering is stale relative to
// current HEAD. Spawns the real binary (rather than calling `execute_verify`
// in-process) specifically so this asserts the warning reaches actual
// process output via `VerificationReporter::report`, the same gap this
// track closed.
//
// CG-F35 review fix: there are three plan-building paths, not two --
// manual-command (`command_str` is `Some`), config-defined
// (`[[verify.steps]]` present), and predicted (`OutcomePredictor::predict`).
// Only the predicted path actually consults `ctx.packet`, so this test must
// exercise *that* path specifically: no explicit command string, and no
// `[[verify.steps]]` config override (an earlier version of this test added
// one, which made it exercise the config-defined path instead -- see
// `test_verify_config_plan_does_not_warn_on_stale_cached_impact_packet`
// below for why that path must NOT warn). `cargo` has no `Cargo.toml` in
// this throwaway tmpdir, so the default `cargo test`/`cargo nextest run`
// step fails near-instantly rather than hanging; the warning is emitted by
// `VerificationReporter::report` before that step result is even relevant,
// so the test only asserts on stderr content, not overall exit status.
#[test]
#[allow(non_snake_case)]
fn test_verify_warns_on_stale_cached_impact_packet__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    std::fs::write(root.join("a.txt"), "v1").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(root)
        .output()
        .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Record a packet via `scan --impact`, then advance HEAD past it so the
    // cached packet is stale by the time `verify` reads it.
    std::fs::write(root.join("a.txt"), "v2").unwrap();
    let scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "advance head past the cached packet"])
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(ledgerful_bin)
        .arg("verify")
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("stale"),
        "expected a staleness warning on stderr from VerificationReporter, got stderr: {stderr}"
    );
}

// CG-F35 review fix: a config-defined plan (`[[verify.steps]]` present) takes
// priority over `OutcomePredictor::predict` in `execute_verify`'s
// plan-building match, and -- like the manual-command path --
// `build_plan_from_config` never consults `ctx.packet` at all. So the
// stale-cache warning must NOT fire when a config-defined plan is used, even
// though the cached packet is genuinely stale; warning there would falsely
// imply a prediction was made from stale data when none happened. Mirrors
// `test_verify_manual_command_does_not_warn_on_stale_cached_impact_packet`
// below, but for the config-plan case instead of the manual-command case.
#[test]
fn test_verify_config_plan_does_not_warn_on_stale_cached_impact_packet() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    std::fs::write(root.join("a.txt"), "v1").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(root)
        .output()
        .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Configure a fast, deterministic `[[verify.steps]]` override so
    // `build_plan_from_config` drives the plan instead of
    // `OutcomePredictor::predict`.
    let config_path = root.join(".ledgerful").join("config.toml");
    let config_contents = std::fs::read_to_string(&config_path).unwrap();
    let mut config_doc = config_contents.parse::<toml_edit::DocumentMut>().unwrap();
    if config_doc.get("verify").is_none() {
        config_doc["verify"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    config_doc["verify"]["mode"] = toml_edit::value("explicit");
    config_doc["verify"]["steps"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    let steps = config_doc["verify"]["steps"]
        .as_array_of_tables_mut()
        .unwrap();
    let mut step = toml_edit::Table::new();
    step["description"] = toml_edit::value("fast no-op");
    step["command"] = toml_edit::value("echo ok");
    steps.push(step);
    let rendered_config = config_doc.to_string();
    let parsed_config: ledgerful::config::model::Config = toml::from_str(&rendered_config).unwrap();
    assert_eq!(
        parsed_config.verify.steps.len(),
        1,
        "fixture must contain one verification step:\n{rendered_config}"
    );
    std::fs::write(&config_path, rendered_config).unwrap();

    // Record a packet via `scan --impact`, then advance HEAD past it so the
    // cached packet is stale by the time `verify` reads it.
    std::fs::write(root.join("a.txt"), "v2").unwrap();
    let scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "advance head past the cached packet"])
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(ledgerful_bin)
        .arg("verify")
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.to_lowercase().contains("stale"),
        "config-defined plan path must not claim predictions are based on stale data \
         (no prediction happens in that path), got stderr: {stderr}"
    );
}

// CG-F35 review fix: the manual-command path (`verify "<command>"`) must NOT
// emit the stale-cache warning, because that branch never builds a plan from
// `ctx.packet` -- it just runs the literal command. Warning there would
// falsely imply a prediction was made from stale data.
#[test]
fn test_verify_manual_command_does_not_warn_on_stale_cached_impact_packet() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    std::fs::write(root.join("a.txt"), "v1").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(root)
        .output()
        .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Record a packet via `scan --impact`, then advance HEAD past it so the
    // cached packet is stale by the time `verify` reads it.
    std::fs::write(root.join("a.txt"), "v2").unwrap();
    let scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "advance head past the cached packet"])
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(ledgerful_bin)
        .args(["verify", "echo hello"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.to_lowercase().contains("stale"),
        "manual command path must not claim predictions are based on stale data \
         (no prediction happens in that path), got stderr: {stderr}"
    );
}

// CR8: Unit tests for the Cozo Datalog string escaping helper.
mod escape_cozo_string_tests {
    use ledgerful::commands::ask::escape_cozo_string;

    #[test]
    fn test_plain_symbol_unchanged() {
        assert_eq!(escape_cozo_string("foo_bar"), "foo_bar");
    }

    #[test]
    fn test_single_quote_doubled() {
        assert_eq!(escape_cozo_string("foo'bar"), "foo''bar");
    }

    #[test]
    fn test_backslash_escaped() {
        assert_eq!(escape_cozo_string("foo\\bar"), "foo\\\\bar");
    }

    #[test]
    fn test_both_special_chars() {
        assert_eq!(escape_cozo_string("it's a\\test"), "it''s a\\\\test");
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(escape_cozo_string(""), "");
    }
}

#[test]
fn test_verify_fails_with_unresolvable_tx_id() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    let output = std::process::Command::new(ledgerful_bin)
        .args(["verify", "--tx-id", "test_tx_id", "echo hello"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "verify command should have failed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to resolve tx-id 'test_tx_id'"),
        "Missing diagnostic in stderr: {}",
        stderr
    );
}

#[test]
fn test_verify_persists_with_resolved_tx_id() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Start a transaction
    std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "my-feature",
            "--category",
            "FEATURE",
            "--message",
            "test tx",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    // Get the pending tx id
    let output = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let status_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tx_id = status_json["pendingTxIds"][0].as_str().unwrap().to_string();

    let verify_output = std::process::Command::new(ledgerful_bin)
        .args(["verify", "--tx-id", &tx_id, "echo hello"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(
        verify_output.status.success(),
        "verify command failed: {}",
        String::from_utf8_lossy(&verify_output.stderr)
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM verification_runs WHERE tx_id = ?1",
            rusqlite::params![tx_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "Expected exactly 1 verification run with tx_id = '{}'",
        tx_id
    );
}

#[test]
fn test_verify_explicit_tx_id_must_be_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(root.as_std_path());

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Start a transaction
    let start_out = std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "my-feature-3",
            "--category",
            "FEATURE",
            "--message",
            "test pending",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        start_out.status.success(),
        "Start failed: {}",
        String::from_utf8_lossy(&start_out.stderr)
    );

    // Get the pending tx id
    let output = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let status_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tx_id = status_json["pendingTxIds"][0].as_str().unwrap().to_string();

    // Commit the transaction
    std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "commit",
            &tx_id,
            "--summary",
            "done",
            "--reason",
            "reason",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    // Try verifying with the COMMITTED tx-id
    let verify_output = std::process::Command::new(ledgerful_bin)
        .args(["verify", "echo hello", "--tx-id", &tx_id])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(
        !verify_output.status.success(),
        "Verify should fail when tx-id is not PENDING"
    );
    let err = String::from_utf8_lossy(&verify_output.stderr);
    assert!(err.contains("must be PENDING"));
}

#[test]
fn test_verify_tx_id_deferred_on_dry_run_or_health() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(root.as_std_path());

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Dry run with an invalid tx-id should succeed because tx-id resolution is deferred
    let verify_dry = std::process::Command::new(ledgerful_bin)
        .args([
            "verify",
            "echo hello",
            "--tx-id",
            "invalid-id-123",
            "--dry-run",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        verify_dry.status.success(),
        "Verify --dry-run should ignore invalid tx-id"
    );

    // Health run with an invalid tx-id should succeed
    let verify_health = std::process::Command::new(ledgerful_bin)
        .args(["verify", "--tx-id", "invalid-id-123", "--health"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        verify_health.status.success(),
        "Verify --health should ignore invalid tx-id"
    );
}

#[test]
fn test_verify_commit_editmsg_without_index_lock_does_not_autobind() {
    use std::fs;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    std::fs::write(root.join("dummy.txt"), "hello").unwrap();
    crate::common::git_add_and_commit(root, "initial commit");

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "my-feature-stale",
            "--category",
            "FEATURE",
            "--message",
            "test stale bind",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    let output = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let status_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tx_id = status_json["pendingTxIds"][0].as_str().unwrap().to_string();

    // Write the sidecar
    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");

    let editmsg_content = "feat: stale commit\n\nThis is a stale commit message.";
    let cleaned = ledgerful::util::text::clean_commit_msg(editmsg_content);
    let edit_hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(cleaned.as_bytes());
        hex::encode(hasher.finalize())
    };

    let sidecar_json = serde_json::json!({
        "tx_id": tx_id,
        "commit_msg_hash": edit_hash,
        "summary": "stale commit",
        "reason": "dummy reason"
    });
    fs::write(sidecar_path, serde_json::to_string(&sidecar_json).unwrap()).unwrap();

    // Write the COMMIT_EDITMSG
    let editmsg_path = root.join(".git").join("COMMIT_EDITMSG");
    fs::write(&editmsg_path, editmsg_content).unwrap();

    // DELIBERATELY DO NOT CREATE .git/index.lock

    // Run verify WITH the sidecar and COMMIT_EDITMSG present
    let verify_output = std::process::Command::new(ledgerful_bin)
        .args(["verify", "echo hello stale"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(verify_output.status.success());

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM verification_runs WHERE tx_id = ?1",
            rusqlite::params![tx_id],
            |row| row.get(0),
        )
        .unwrap();
    // Because index.lock does not exist, it should NOT auto-bind
    assert_eq!(count, 0, "Expected NO auto-bind without index.lock");
}

#[test]
fn test_verify_commit_editmsg_with_index_lock_autobinds() {
    use std::fs;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    std::fs::write(root.join("dummy.txt"), "hello").unwrap();
    crate::common::git_add_and_commit(root, "initial commit");

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "my-feature-fresh",
            "--category",
            "FEATURE",
            "--message",
            "test fresh bind",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    let output = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let status_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tx_id = status_json["pendingTxIds"][0].as_str().unwrap().to_string();

    // Write the sidecar
    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");

    let editmsg_content = "feat: fresh commit\n\nThis is a fresh commit message.";
    let cleaned = ledgerful::util::text::clean_commit_msg(editmsg_content);
    let edit_hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(cleaned.as_bytes());
        hex::encode(hasher.finalize())
    };

    let sidecar_json = serde_json::json!({
        "tx_id": tx_id,
        "commit_msg_hash": edit_hash,
        "summary": "fresh commit",
        "reason": "dummy reason"
    });
    fs::write(sidecar_path, serde_json::to_string(&sidecar_json).unwrap()).unwrap();

    // Write the COMMIT_EDITMSG
    let editmsg_path = root.join(".git").join("COMMIT_EDITMSG");
    fs::write(&editmsg_path, editmsg_content).unwrap();

    // Create the .git/index.lock dummy file
    let index_lock_path = root.join(".git").join("index.lock");
    fs::write(&index_lock_path, "").unwrap();

    // Run verify WITH the sidecar, COMMIT_EDITMSG, and index.lock present
    let verify_output = std::process::Command::new(ledgerful_bin)
        .args(["verify", "echo hello fresh"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(verify_output.status.success());

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM verification_runs WHERE tx_id = ?1",
            rusqlite::params![tx_id],
            |row| row.get(0),
        )
        .unwrap();
    // Because index.lock exists, it SHOULD auto-bind
    assert_eq!(count, 1, "Expected auto-bind with index.lock present");
}

/// Negative test for the Codex Round 9 MEDIUM finding.
///
/// The verify auto-bind freshness check must ONLY trigger when BOTH:
///   1. `COMMIT_EDITMSG` exists and its hash matches the sidecar, AND
///   2. `.git/index.lock` exists.
///
/// A sidecar whose `commit_msg_hash` matches HEAD (the last committed message)
/// but where `COMMIT_EDITMSG` is absent must NOT auto-bind, even if
/// `index.lock` is present — because `index.lock` can be created by unrelated
/// git activity (e.g., a concurrent `git add` in another terminal) and HEAD
/// matching the sidecar is the signature of a stale "post-commit failed" sidecar.
#[test]
fn test_verify_head_match_with_index_lock_but_no_editmsg_does_not_autobind() {
    use std::fs;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    // Make an initial commit so HEAD exists
    std::fs::write(root.join("dummy.txt"), "hello").unwrap();
    crate::common::git_add_and_commit(root, "initial commit");

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "my-stale-head-feature",
            "--category",
            "FEATURE",
            "--message",
            "test head-only bind is rejected",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    let output = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let status_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tx_id = status_json["pendingTxIds"][0].as_str().unwrap().to_string();

    // Hash the HEAD commit message to simulate a "HEAD-matching" sidecar
    let head_output = std::process::Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_msg = String::from_utf8_lossy(&head_output.stdout).to_string();
    let cleaned = ledgerful::util::text::clean_commit_msg(&head_msg);
    let head_hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(cleaned.as_bytes());
        hex::encode(hasher.finalize())
    };

    // Write sidecar whose commit_msg_hash matches HEAD
    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    let sidecar_json = serde_json::json!({
        "tx_id": tx_id,
        "commit_msg_hash": head_hash,
        "summary": "head-matching sidecar",
        "reason": "dummy reason"
    });
    fs::write(&sidecar_path, serde_json::to_string(&sidecar_json).unwrap()).unwrap();

    // Ensure COMMIT_EDITMSG does NOT exist (simulate absence of active in-flight commit)
    let editmsg_path = root.join(".git").join("COMMIT_EDITMSG");
    if editmsg_path.exists() {
        fs::remove_file(&editmsg_path).unwrap();
    }

    // Create .git/index.lock (e.g., from a concurrent `git add` in another terminal)
    let index_lock_path = root.join(".git").join("index.lock");
    fs::write(&index_lock_path, "").unwrap();

    // Run verify — should succeed but must NOT auto-bind
    let verify_output = std::process::Command::new(ledgerful_bin)
        .args(["verify", "echo hello head-only"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(verify_output.status.success());

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM verification_runs WHERE tx_id = ?1",
            rusqlite::params![tx_id],
            |row| row.get(0),
        )
        .unwrap();
    // HEAD-match + index.lock without COMMIT_EDITMSG must NOT auto-bind.
    // If a HEAD-match path exists in the auto-bind logic this assertion will fail with count == 1.
    assert_eq!(
        count, 0,
        "Auto-bind must NOT trigger from HEAD-match + index.lock when COMMIT_EDITMSG is absent"
    );
}
