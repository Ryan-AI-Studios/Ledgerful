//! Integration tests for `ledgerful policy check` (track 0049).
//!
//! Covers: JSON schema, each built-in rule, bypass-proof base-branch policy,
//! local no_pending_tx (sidecar), observe exit 0 / enforce exit 1, signing basis.

use ledgerful::commands::init::execute_init;
use ledgerful::commands::policy_check::{
    POLICY_CHECK_SCHEMA_VERSION, PolicyCheckReport, evaluate_policy_check, execute_policy_check,
    parse_policy_toml,
};
use ledgerful::config::model::Config;
use ledgerful::ledger::crypto::sign_ledger_entry_in;
use ledgerful::ledger::transaction::TransactionManager;
use ledgerful::ledger::types::{
    Category, ChangeType, CommitRequest, EntryType, TransactionRequest,
};
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, git_add_and_commit, non_interactive, setup_git_repo};

fn git_cmd(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_policy(root: &Path, body: &str) {
    let dir = root.join(".ledgerful");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("policy.toml"), body).unwrap();
}

/// Force-add policy.toml (under gitignored `.ledgerful/`) and commit.
fn commit_policy(root: &Path, msg: &str) {
    git_cmd(root, &["add", "-f", ".ledgerful/policy.toml"]);
    // Stage other non-ignored changes too if present.
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output();
    git_cmd(root, &["commit", "-m", msg, "--no-verify"]);
}

fn seed_passing_verification(root: &Path) {
    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    storage
        .save_verification_run(&chrono::Utc::now().to_rfc3339(), Some("[]"), true, None)
        .unwrap();
}

fn commit_signed_entry(root: &Path, entity: &str, summary: &str) {
    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut config = Config::default();
    config.gate.mode = "enforce".to_string();
    config.intent.require_signing = false;
    let mut tx_mgr = TransactionManager::new(&mut storage, root.to_path_buf(), config);
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: entity.to_string(),
            ..Default::default()
        })
        .unwrap();
    tx_mgr
        .commit_change(
            tx_id,
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: summary.to_string(),
                reason: "policy_check test".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
}

fn commit_adr_entry(root: &Path) {
    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut config = Config::default();
    config.gate.mode = "enforce".to_string();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.to_path_buf(), config);
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "docs/adr/0001.md".to_string(),
            ..Default::default()
        })
        .unwrap();
    tx_mgr
        .commit_change(
            tx_id,
            CommitRequest {
                change_type: ChangeType::Create,
                summary: "ADR: accept policy gates".to_string(),
                reason: "high-risk change requires ADR".to_string(),
                entry_type: Some(EntryType::Architecture),
                is_breaking: true,
                ..Default::default()
            },
            false,
        )
        .unwrap();
}

/// Minimal green workspace for policy check: init + verification pass + no pending.
fn setup_green_repo(root: &Path) {
    setup_git_repo(root);
    fs::write(root.join("README.md"), "base\n").unwrap();
    git_add_and_commit(root, "initial");

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    seed_passing_verification(root);
}

// ---------------------------------------------------------------------------
// JSON schema
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn policy_check_json_schema_version_and_fields() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    // Relax risk/ADR/signed rules so a fresh init is clean enough.
    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = true
verification_must_pass = true
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(None, None, None).unwrap();

    assert_eq!(report.schema_version, POLICY_CHECK_SCHEMA_VERSION);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert!(json["violations"].is_array());
    assert!(json.get("passed").is_some());
    assert!(json.get("mode").is_some());
    assert!(json.get("policySource").is_some());
    // camelCase keys only (no snake_case).
    assert!(json.get("schema_version").is_none());
    assert!(json.get("policy_source").is_none());
}

// ---------------------------------------------------------------------------
// Observe vs enforce exit behaviour
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn observe_mode_never_exits_nonzero() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    // Force a violation: no verification would fail, but we seed one that fails.
    // Clearer: leave verification_must_pass on and remove runs by writing a
    // policy that requires signed entries on an unsigned ledger, or create pending.
    write_policy(
        root,
        r#"
preset = "observe"
[rules]
require_signed_entries = true
no_pending_tx = true
verification_must_pass = true
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );

    // Create a pending tx so there is a definite violation.
    {
        let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
        let db_path = layout.state_subdir().join("ledger.db");
        let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
        let config = Config::default();
        let mut tx_mgr = TransactionManager::new(&mut storage, root.to_path_buf(), config);
        let _ = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: "src/pending.rs".to_string(),
                ..Default::default()
            })
            .unwrap();
    }

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);

    let report = evaluate_policy_check(None, None, None).unwrap();
    assert_eq!(report.mode, "observe");
    assert!(!report.passed, "should have violations");
    assert!(
        report.violations.iter().all(|v| v.severity == "warn"),
        "observe marks severity warn: {:?}",
        report.violations
    );

    // execute must return Ok even with violations in observe.
    let result = execute_policy_check(None, None, None, Some("json".into()));
    assert!(
        result.is_ok(),
        "observe must exit 0 (Ok): {:?}",
        result.err()
    );
}

#[test]
#[serial(env, cwd)]
fn enforce_mode_exits_nonzero_on_violation() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = true
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );

    {
        let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
        let db_path = layout.state_subdir().join("ledger.db");
        let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
        let config = Config::default();
        let mut tx_mgr = TransactionManager::new(&mut storage, root.to_path_buf(), config);
        let _ = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: "src/pending.rs".to_string(),
                ..Default::default()
            })
            .unwrap();
    }

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);

    let report = evaluate_policy_check(None, None, None).unwrap();
    assert_eq!(report.mode, "enforce");
    assert!(!report.passed);
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule_id == "no_pending_tx")
    );
    assert!(report.violations.iter().all(|v| v.severity == "error"));

    let result = execute_policy_check(None, None, None, Some("json".into()));
    assert!(result.is_err(), "enforce must exit nonzero on violation");
}

// ---------------------------------------------------------------------------
// no_pending_tx local (DoD-1c) — sidecar + --pr contrast
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn no_pending_tx_fails_locally_on_sidecar() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    // Need a committed base for --pr range, plus a second commit so HEAD~1...HEAD works.
    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = true
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );
    commit_policy(root, "base policy for sidecar contrast");
    fs::write(root.join("extra.txt"), "pr change\n").unwrap();
    git_add_and_commit(root, "extra commit for pr range");

    // Write pending_hook_tx sidecar (local-only signal).
    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    let sidecar = layout.state_subdir().join("pending_hook_tx");
    fs::write(
        sidecar.as_std_path(),
        r#"{"tx_id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","commit_msg_hash":"deadbeef","summary":"x","reason":"y"}"#,
    )
    .unwrap();

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);

    // Local mode → must flag sidecar.
    let local = evaluate_policy_check(None, None, None).unwrap();
    assert!(
        local
            .violations
            .iter()
            .any(|v| v.rule_id == "no_pending_tx" && v.file.contains("pending_hook_tx")),
        "local mode must flag sidecar: {:?}",
        local.violations
    );

    // --pr mode → committed range only; NO sidecar-based no_pending_tx violation.
    let pr = evaluate_policy_check(Some("HEAD~1...HEAD"), None, None).unwrap();
    assert!(
        !pr.violations
            .iter()
            .any(|v| v.rule_id == "no_pending_tx" && v.file.contains("pending_hook_tx")),
        "--pr must not flag pending_hook_tx sidecar: {:?}",
        pr.violations
    );
}

// ---------------------------------------------------------------------------
// verification_must_pass
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn verification_must_pass_fails_when_no_runs() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "base\n").unwrap();
    git_add_and_commit(root, "initial");

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = false
verification_must_pass = true
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );

    let report = evaluate_policy_check(None, None, None).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule_id == "verification_must_pass"
                && v.message.contains("no verification run")),
        "{:?}",
        report.violations
    );
}

#[test]
#[serial(env, cwd)]
fn verification_must_pass_fails_when_last_run_failed() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    // Overwrite with a failing run.
    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    storage
        .save_verification_run(&chrono::Utc::now().to_rfc3339(), Some("[]"), false, None)
        .unwrap();

    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = false
verification_must_pass = true
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(None, None, None).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule_id == "verification_must_pass"
                && v.message.contains("overall_pass is false")),
        "{:?}",
        report.violations
    );
}

// ---------------------------------------------------------------------------
// require_signed_entries
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn require_signed_entries_flags_unsigned() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    // Commit an entry without forcing signatures (require_signing=false default).
    commit_signed_entry(root, "src/foo.rs", "unsigned-ish entry");

    // Force-clear signature columns to guarantee missing signatures.
    {
        let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).unwrap();
        storage
            .get_connection()
            .execute(
                "UPDATE ledger_entries SET signature = NULL, public_key = NULL",
                [],
            )
            .unwrap();
    }

    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = true
no_pending_tx = false
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(None, None, None).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule_id == "require_signed_entries"),
        "{:?}",
        report.violations
    );
}

// ---------------------------------------------------------------------------
// fail_on / max_risk_without_adr
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn fail_on_high_triggers_on_sensitive_path() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    // Touch a sensitive path so scan risk becomes High.
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    git_add_and_commit(root, "add cargo");

    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = false
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "high"
"#,
    );

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(Some("HEAD~1...HEAD"), None, None).unwrap();
    assert!(
        report.violations.iter().any(|v| v.rule_id == "fail_on"),
        "expected fail_on for high-risk PR: {:?}",
        report.violations
    );
}

#[test]
#[serial(env, cwd)]
fn max_risk_without_adr_requires_adr_on_high_risk() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    git_add_and_commit(root, "add cargo");

    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = false
verification_must_pass = false
max_risk_without_adr = "high"
fail_on = "off"
"#,
    );

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(Some("HEAD~1...HEAD"), None, None).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule_id == "max_risk_without_adr"),
        "high risk without ADR must violate: {:?}",
        report.violations
    );

    // Adding an ADR clears the violation.
    commit_adr_entry(root);
    let report2 = evaluate_policy_check(Some("HEAD~1...HEAD"), None, None).unwrap();
    assert!(
        !report2
            .violations
            .iter()
            .any(|v| v.rule_id == "max_risk_without_adr"),
        "ADR should satisfy max_risk_without_adr: {:?}",
        report2.violations
    );
}

#[test]
#[serial(env, cwd)]
fn cli_fail_on_overrides_config() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    git_add_and_commit(root, "add cargo");

    // Config has fail_on=off, CLI overrides to high.
    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = false
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(Some("HEAD~1...HEAD"), Some("high"), None).unwrap();
    assert!(
        report.violations.iter().any(|v| v.rule_id == "fail_on"),
        "CLI --fail-on high must override config: {:?}",
        report.violations
    );
}

// ---------------------------------------------------------------------------
// DoD-1b: bypass-proof base-branch policy
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn base_branch_policy_not_bypassed_by_pr_head_edit() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "base\n").unwrap();

    // Base commit with enforce policy requiring no_pending_tx + verification.
    let _ni = non_interactive();
    {
        let _guard = DirGuard::new(root);
        execute_init(false, false).unwrap();
    }
    seed_passing_verification(root);

    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = true
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );
    // policy.toml lives under gitignored .ledgerful/ — force-add for base branch.
    commit_policy(root, "base with enforce policy");

    // Remember base ref name (main or master depending on git config).
    let base_ref = {
        let out = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // Create a branch and weaken the policy in the PR head.
    git_cmd(root, &["checkout", "-b", "pr-branch"]);
    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = false
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );
    // Also create a pending tx that base policy must catch.
    {
        let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
        let db_path = layout.state_subdir().join("ledger.db");
        let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
        let config = Config::default();
        let mut tx_mgr = TransactionManager::new(&mut storage, root.to_path_buf(), config);
        let _ = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: "src/bypass.rs".to_string(),
                ..Default::default()
            })
            .unwrap();
    }
    fs::write(root.join("pr.txt"), "pr change\n").unwrap();
    // Force-add the weakened policy so git has a PR-head copy (must still be ignored).
    git_cmd(root, &["add", "-f", ".ledgerful/policy.toml"]);
    git_cmd(root, &["add", "pr.txt"]);
    git_cmd(root, &["commit", "-m", "pr weakens policy", "--no-verify"]);

    let _guard = DirGuard::new(root);

    // Without --policy, --pr must load base-branch policy (no_pending_tx=true).
    let range = format!("{}...HEAD", base_ref);
    let report = evaluate_policy_check(Some(&range), None, None).unwrap();

    assert_eq!(
        report.policy_source, "base-branch",
        "PR mode without --policy must use base-branch source"
    );
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule_id == "no_pending_tx"),
        "base policy still enforces no_pending_tx despite PR disabling it: {:?}",
        report.violations
    );

    // Explicit trusted --policy that disables the rule should use trusted-path
    // and pass that rule (proves --policy is the intentional override path).
    let trusted = root.join("trusted-policy.toml");
    fs::write(
        &trusted,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = false
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    )
    .unwrap();
    let report2 =
        evaluate_policy_check(Some("HEAD~1...HEAD"), None, Some(trusted.as_path())).unwrap();
    assert_eq!(report2.policy_source, "trusted-path");
    assert!(
        !report2
            .violations
            .iter()
            .any(|v| v.rule_id == "no_pending_tx"),
        "trusted path may disable the rule: {:?}",
        report2.violations
    );
}

// ---------------------------------------------------------------------------
// Synthesized defaults from gate.mode (DoD-3 / 0050 subsumption)
// ---------------------------------------------------------------------------

fn set_gate_mode(root: &Path, mode: &str) {
    let config_path = root.join(".ledgerful").join("config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap_or_default();
    if !cfg.contains("[gate]") {
        cfg.push_str(&format!("\n[gate]\nmode = \"{mode}\"\n"));
    } else if cfg.contains("mode = \"observe\"") || cfg.contains("mode = \"enforce\"") {
        cfg = cfg.replace("mode = \"observe\"", &format!("mode = \"{mode}\""));
        cfg = cfg.replace("mode = \"enforce\"", &format!("mode = \"{mode}\""));
    } else {
        cfg.push_str(&format!("mode = \"{mode}\"\n"));
    }
    fs::write(&config_path, cfg).unwrap();
}

fn force_pending_violation(root: &Path) {
    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let config = Config::default();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.to_path_buf(), config);
    let _ = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/pending.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
}

#[test]
#[serial(env, cwd)]
fn missing_policy_file_synthesizes_from_gate_mode() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);
    // Ensure no policy.toml
    let policy = root.join(".ledgerful").join("policy.toml");
    let _ = fs::remove_file(&policy);

    set_gate_mode(root, "enforce");

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(None, Some("off"), None).unwrap();
    // Synthesized (no file loaded) → policySource=synthesized; mode from gate.mode.
    assert_eq!(report.mode, "enforce");
    assert_eq!(report.policy_source, "synthesized");
}

/// DoD-3: no policy.toml + gate.mode=observe → synthesize observe; never exit nonzero.
#[test]
#[serial(env, cwd)]
fn no_policy_gate_observe_never_exits_nonzero_on_violations() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);
    let _ = fs::remove_file(root.join(".ledgerful").join("policy.toml"));
    set_gate_mode(root, "observe");
    force_pending_violation(root);

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);

    let report = evaluate_policy_check(None, Some("off"), None).unwrap();
    assert_eq!(report.mode, "observe");
    assert_eq!(report.policy_source, "synthesized");
    assert!(
        !report.passed,
        "pending tx must produce violations: {:?}",
        report.violations
    );
    assert!(
        report.violations.iter().all(|v| v.severity == "warn"),
        "observe severities must be warn: {:?}",
        report.violations
    );

    let result = execute_policy_check(None, Some("off".into()), None, Some("json".into()));
    assert!(
        result.is_ok(),
        "observe synthesize must never exit nonzero: {:?}",
        result.err()
    );
}

/// DoD-3: no policy.toml + gate.mode=enforce → synthesize enforce; exit nonzero on violations.
#[test]
#[serial(env, cwd)]
fn no_policy_gate_enforce_exits_nonzero_on_violations() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);
    let _ = fs::remove_file(root.join(".ledgerful").join("policy.toml"));
    set_gate_mode(root, "enforce");
    force_pending_violation(root);

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);

    let report = evaluate_policy_check(None, Some("off"), None).unwrap();
    assert_eq!(report.mode, "enforce");
    assert_eq!(report.policy_source, "synthesized");
    assert!(!report.passed);
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule_id == "no_pending_tx"),
        "{:?}",
        report.violations
    );
    assert!(report.violations.iter().all(|v| v.severity == "error"));

    let result = execute_policy_check(None, Some("off".into()), None, Some("json".into()));
    assert!(
        result.is_err(),
        "enforce synthesize must exit nonzero on violations"
    );
}

// ---------------------------------------------------------------------------
// R1-P1: --pr CI-safe defaults when preset omitted / policy missing
// ---------------------------------------------------------------------------

/// Base policy with rules but no `preset`; working-tree gate.mode=observe;
/// `--pr` with a forced violation must use enforce and exit nonzero.
#[test]
#[serial(env, cwd)]
fn pr_mode_defaults_to_enforce_when_preset_omitted() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "base\n").unwrap();

    let _ni = non_interactive();
    {
        let _guard = DirGuard::new(root);
        execute_init(false, false).unwrap();
    }
    seed_passing_verification(root);
    // Working-tree gate.mode is observe — must NOT leak into --pr preset default.
    set_gate_mode(root, "observe");

    // Base policy: rules present, preset intentionally omitted.
    write_policy(
        root,
        r#"
[rules]
require_signed_entries = false
no_pending_tx = true
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );
    commit_policy(root, "base policy without preset");

    let base_ref = {
        let out = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // Create a violation (pending tx) and a PR-range commit.
    force_pending_violation(root);
    fs::write(root.join("pr.txt"), "change\n").unwrap();
    git_add_and_commit(root, "pr change");

    let _guard = DirGuard::new(root);
    let range = format!("{base_ref}...HEAD");
    let report = evaluate_policy_check(Some(&range), None, None).unwrap();

    assert_eq!(
        report.policy_source, "base-branch",
        "base policy.toml was loaded via git show"
    );
    assert_eq!(
        report.mode, "enforce",
        "--pr with omitted preset must default to enforce, not working-tree gate.mode=observe"
    );
    assert!(
        !report.passed
            && report
                .violations
                .iter()
                .any(|v| v.rule_id == "no_pending_tx"),
        "expected forced no_pending_tx violation: {:?}",
        report.violations
    );
    assert!(report.violations.iter().all(|v| v.severity == "error"));

    let result = execute_policy_check(Some(range), None, None, Some("json".into()));
    assert!(
        result.is_err(),
        "--pr omitted-preset must exit nonzero on violations (CI-safe enforce): {:?}",
        result.err()
    );
}

/// Missing base policy.toml under `--pr` synthesizes enforce, not gate.mode.
#[test]
#[serial(env, cwd)]
fn pr_mode_missing_base_policy_synthesizes_enforce() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);
    // No policy.toml anywhere.
    let _ = fs::remove_file(root.join(".ledgerful").join("policy.toml"));
    set_gate_mode(root, "observe");

    // Commit a second change so HEAD~1...HEAD is a valid range; base has no policy.
    fs::write(root.join("pr.txt"), "change\n").unwrap();
    git_add_and_commit(root, "pr change");
    force_pending_violation(root);

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);

    let report = evaluate_policy_check(Some("HEAD~1...HEAD"), Some("off"), None).unwrap();
    assert_eq!(report.policy_source, "synthesized");
    assert_eq!(
        report.mode, "enforce",
        "--pr synthesize must be enforce even when gate.mode=observe"
    );
    assert!(
        !report.passed
            && report
                .violations
                .iter()
                .any(|v| v.rule_id == "no_pending_tx"),
        "{:?}",
        report.violations
    );

    let result = execute_policy_check(
        Some("HEAD~1...HEAD".into()),
        Some("off".into()),
        None,
        Some("json".into()),
    );
    assert!(
        result.is_err(),
        "synthesized --pr enforce must exit nonzero: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// Trusted path
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn trusted_policy_path_source() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    let trusted = root.join("org-policy.toml");
    fs::write(
        &trusted,
        r#"
preset = "observe"
[rules]
require_signed_entries = false
no_pending_tx = false
verification_must_pass = false
max_risk_without_adr = "off"
fail_on = "off"
"#,
    )
    .unwrap();

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(None, None, Some(trusted.as_path())).unwrap();
    assert_eq!(report.policy_source, "trusted-path");
    assert_eq!(report.mode, "observe");
    assert!(report.passed);
}

// ---------------------------------------------------------------------------
// Signing basis intact (DoD-5 integration-level)
// ---------------------------------------------------------------------------

#[test]
fn signing_basis_intact_five_fields_only() {
    // Document and assert the signing basis used by crypto (must not include policy/mode).
    let tmp = tempdir().unwrap();
    let keys = tmp.path().join("keys");
    fs::create_dir_all(&keys).unwrap();

    let (sig, pk) = sign_ledger_entry_in(
        &keys,
        "tx1",
        "FEATURE",
        "summary",
        "reason",
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    assert!(ledgerful::ledger::crypto::verify_signature(
        "tx1",
        "FEATURE",
        "summary",
        "reason",
        "2026-01-01T00:00:00Z",
        sig.as_ref().unwrap(),
        pk.as_ref().unwrap(),
    ));
    // Policy config parse must not touch crypto.
    let _ = parse_policy_toml(
        r#"
preset = "enforce"
[rules]
fail_on = "high"
"#,
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Deterministic violation ordering
// ---------------------------------------------------------------------------

#[test]
#[serial(env, cwd)]
fn violations_sorted_by_rule_id_file_message() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_green_repo(root);

    // Multiple violations: pending + failed verify.
    {
        let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
        let db_path = layout.state_subdir().join("ledger.db");
        let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
        storage
            .save_verification_run(&chrono::Utc::now().to_rfc3339(), Some("[]"), false, None)
            .unwrap();
        let config = Config::default();
        let mut tx_mgr = TransactionManager::new(&mut storage, root.to_path_buf(), config);
        let _ = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: "a.rs".to_string(),
                ..Default::default()
            })
            .unwrap();
    }

    write_policy(
        root,
        r#"
preset = "enforce"
[rules]
require_signed_entries = false
no_pending_tx = true
verification_must_pass = true
max_risk_without_adr = "off"
fail_on = "off"
"#,
    );

    let _ni = non_interactive();
    let _guard = DirGuard::new(root);
    let report = evaluate_policy_check(None, None, None).unwrap();
    assert!(report.violations.len() >= 2);
    let mut sorted = report.violations.clone();
    sorted.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.message.cmp(&b.message))
    });
    assert_eq!(report.violations, sorted);
}

// ---------------------------------------------------------------------------
// Round-trip serde of PolicyCheckReport
// ---------------------------------------------------------------------------

#[test]
fn policy_check_report_roundtrip() {
    let report = PolicyCheckReport {
        schema_version: 1,
        violations: vec![],
        passed: true,
        mode: "observe".into(),
        policy_source: "local".into(),
        notes: vec![],
    };
    let json = serde_json::to_string(&report).unwrap();
    let back: PolicyCheckReport = serde_json::from_str(&json).unwrap();
    assert_eq!(report, back);

    // Non-empty notes round-trip too.
    let with_notes = PolicyCheckReport {
        notes: vec!["partial evaluation note".into()],
        ..report
    };
    let json2 = serde_json::to_string(&with_notes).unwrap();
    let back2: PolicyCheckReport = serde_json::from_str(&json2).unwrap();
    assert_eq!(with_notes, back2);
    assert!(json2.contains("\"notes\""));
}
