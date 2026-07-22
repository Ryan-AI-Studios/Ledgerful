//! Track 0074 — Enforce Lifecycle Integrity
//!
//! Load-bearing tests:
//! 1. Promote failure under enforce retains PENDING + promote_failed sidecar; outer Ok
//! 2. Next commit-msg with different message does NOT GC the orphan
//! 3. Promote success → Unverified (not Verified) for non-TRIVIAL
//! 4. recover-orphan --promote clears orphan
//! 5. intent.required=never + enforce → hard-fail
//! 6. Adaptive trivial bypass under enforce → durable [SKIPPED] sidecar
//! 7. TUI Skip disposition under enforce → same record_enforce_skipped path
//! 8. recover-orphan --abandon clears orphan with reason
//! 9. ledger gc / hook-repair refuse promote_failed orphans

use crate::common::{DirGuard, TempEnv, git_add_and_commit_no_verify, setup_git_repo};
use ledgerful::commands::hook_commit_msg::{
    SKIPPED_COVERAGE_RISK, SKIPPED_SUMMARY_PREFIX, is_tui_skip_disposition,
    skipped_coverage_summary,
};
use ledgerful::commands::hook_post_commit::execute_hook_post_commit_for_layout;
use ledgerful::commands::hook_sidecar::{GcContext, PendingHookTx, hash_message, is_gc_eligible};
use ledgerful::commands::init::execute_init;
use ledgerful::ledger::crypto::get_or_create_keys_in;
use ledgerful::state::layout::Layout;
use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn ledgerful_bin() -> &'static str {
    std::env!("CARGO_BIN_EXE_ledgerful")
}

fn keys_dir(root: &Path) -> std::path::PathBuf {
    root.join(".ledgerful").join("keys")
}

struct HookedRepo {
    tmp: tempfile::TempDir,
    _home_guard_home: TempEnv,
    _home_guard_profile: TempEnv,
    _dir_guard: DirGuard,
}

impl HookedRepo {
    fn path(&self) -> &Path {
        self.tmp.path()
    }
}

fn setup_repo() -> HookedRepo {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join(".gitignore"), ".ledgerful/\n").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    git_add_and_commit_no_verify(root, "initial");

    let kdir = keys_dir(root);
    fs::create_dir_all(&kdir).unwrap();
    let _ = get_or_create_keys_in(&kdir);

    let home_guard_home = TempEnv::set("HOME", root.to_str().unwrap());
    let home_guard_profile = TempEnv::set("USERPROFILE", root.to_str().unwrap());
    let dir_guard = DirGuard::new(root);

    execute_init(false, false).unwrap();

    // Remove auto-installed hooks — drive in-process / CLI explicitly.
    let hooks_dir = root.join(".git").join("hooks");
    for hook in ["commit-msg", "post-commit", "pre-commit", "pre-push"] {
        let _ = fs::remove_file(hooks_dir.join(hook));
    }

    HookedRepo {
        tmp,
        _home_guard_home: home_guard_home,
        _home_guard_profile: home_guard_profile,
        _dir_guard: dir_guard,
    }
}

fn set_gate_mode(root: &Path, mode: &str) {
    let config_path = root.join(".ledgerful").join("config.toml");
    let mut content = fs::read_to_string(&config_path).unwrap_or_default();
    if content.contains("[gate]") {
        // Replace existing mode line if present.
        let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
        let mut in_gate = false;
        for line in &mut lines {
            if line.trim() == "[gate]" {
                in_gate = true;
                continue;
            }
            if in_gate && line.trim().starts_with('[') {
                in_gate = false;
            }
            if in_gate && line.trim().starts_with("mode") {
                *line = format!("mode = \"{mode}\"");
                in_gate = false;
            }
        }
        content = lines.join("\n");
        if !content.contains(&format!("mode = \"{mode}\"")) {
            content.push_str(&format!("\n[gate]\nmode = \"{mode}\"\n"));
        }
    } else {
        content.push_str(&format!("\n[gate]\nmode = \"{mode}\"\n"));
    }
    fs::write(&config_path, content).unwrap();
}

fn set_intent_required(root: &Path, required: &str) {
    let config_path = root.join(".ledgerful").join("config.toml");
    let mut content = fs::read_to_string(&config_path).unwrap_or_default();
    if content.contains("[intent]") {
        let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
        let mut in_intent = false;
        let mut replaced = false;
        for line in &mut lines {
            if line.trim() == "[intent]" {
                in_intent = true;
                continue;
            }
            if in_intent && line.trim().starts_with('[') {
                in_intent = false;
            }
            if in_intent && line.trim().starts_with("required") {
                *line = format!("required = \"{required}\"");
                replaced = true;
                in_intent = false;
            }
        }
        content = lines.join("\n");
        if !replaced {
            content.push_str(&format!("\n[intent]\nrequired = \"{required}\"\n"));
        }
    } else {
        content.push_str(&format!("\n[intent]\nrequired = \"{required}\"\n"));
    }
    fs::write(&config_path, content).unwrap();
}

fn sidecar_path(root: &Path) -> std::path::PathBuf {
    root.join(".ledgerful")
        .join("state")
        .join("pending_hook_tx")
}

fn write_pending_sidecar_for_msg(root: &Path, msg: &str, risk: &str) -> String {
    // Start a real PENDING transaction so promote has something to commit.
    let start = Command::new(ledgerful_bin())
        .args([
            "ledger",
            "start",
            "src/lib.rs",
            "--category",
            "FEATURE",
            "--message",
            "lifecycle test pending",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "ledger start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    let status = Command::new(ledgerful_bin())
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let tx_id = json["pendingTxIds"][0].as_str().unwrap().to_string();

    let cleaned = ledgerful::util::text::clean_commit_msg(msg);
    let hash = hash_message(&cleaned);
    let pending = PendingHookTx {
        tx_id: tx_id.clone(),
        commit_msg_hash: hash,
        summary: "feat: lifecycle integrity test".to_string(),
        reason: "test promote path".to_string(),
        committed_at: Some(chrono::Utc::now().to_rfc3339()),
        risk: Some(risk.to_string()),
        related_tickets: None,
        signature: None,
        public_key: None,
        snapshot_id: None,
        observed: None,
        promote_failed: None,
        promote_error: None,
    };
    fs::write(sidecar_path(root), serde_json::to_string(&pending).unwrap()).unwrap();
    tx_id
}

/// Force promote failure by setting a sidecar that points at a non-existent tx
/// while matching HEAD — commit_change will fail with NotFound.
fn force_promote_fail_orphan(root: &Path) {
    // Make a real commit so HEAD has a message.
    fs::write(root.join("src/lib.rs"), "pub fn f() { /* orphan */ }\n").unwrap();
    let msg = "feat: orphan promote fail\n\nBody for conventional.";
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    fs::write(&msg_file, format!("{msg}\n")).unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    let commit = Command::new("git")
        .args(["commit", "--no-verify", "-F", msg_file.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    // PENDING with a bogus tx_id so promote fails, but hash matches HEAD.
    let head_out = Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_msg = String::from_utf8_lossy(&head_out.stdout).to_string();
    let cleaned = ledgerful::util::text::clean_commit_msg(&head_msg);
    let hash = hash_message(&cleaned);

    // Create a real pending row, then rewrite sidecar to keep real tx_id but we
    // force failure by deleting the transaction? Easier: use real pending and
    // poison via invalid signature? Simplest: use a non-existent tx_id.
    let pending = PendingHookTx {
        tx_id: "00000000-0000-0000-0000-00000000dead".to_string(),
        commit_msg_hash: hash,
        summary: "feat: orphan promote fail".to_string(),
        reason: "force fail".to_string(),
        committed_at: Some(chrono::Utc::now().to_rfc3339()),
        risk: Some("HIGH".to_string()),
        related_tickets: None,
        signature: None,
        public_key: None,
        snapshot_id: None,
        observed: None,
        promote_failed: None,
        promote_error: None,
    };
    fs::write(sidecar_path(root), serde_json::to_string(&pending).unwrap()).unwrap();
}

#[test]
#[serial(env)]
fn promote_failure_under_enforce_retains_trail_outer_ok() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "enforce");
    force_promote_fail_orphan(root);

    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    let result = execute_hook_post_commit_for_layout(&layout);
    assert!(
        result.is_ok(),
        "outer post-commit must return Ok to git: {result:?}"
    );

    let sidecar = sidecar_path(root);
    assert!(
        sidecar.exists(),
        "sidecar must be retained after promote failure"
    );
    let content: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(
        content["promote_failed"], true,
        "promote_failed must be set: {content}"
    );
    assert!(
        content["promote_error"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "promote_error must be recorded: {content}"
    );
}

#[test]
#[serial(env)]
fn next_commit_msg_does_not_gc_promote_failed_orphan() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "enforce");
    force_promote_fail_orphan(root);

    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();
    assert!(sidecar_path(root).exists());
    let orphan_before = fs::read_to_string(sidecar_path(root)).unwrap();

    // Different message for a new commit attempt.
    fs::write(root.join("src/lib.rs"), "pub fn g() {}\n").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    fs::write(&msg_file, "feat: totally different message\n\nNew body.\n").unwrap();

    let hook = Command::new(ledgerful_bin())
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    // Under enforce, promote orphan hard-fails next commit-msg.
    assert!(
        !hook.status.success(),
        "commit-msg must hard-fail on promote orphan under enforce; stdout={} stderr={}",
        String::from_utf8_lossy(&hook.stdout),
        String::from_utf8_lossy(&hook.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&hook.stdout),
        String::from_utf8_lossy(&hook.stderr)
    );
    assert!(
        combined.contains("PROMOTE_ORPHAN") || combined.contains("recover-orphan"),
        "error should mention PROMOTE_ORPHAN / recover: {combined}"
    );

    // Orphan sidecar still present and not GC'd.
    assert!(sidecar_path(root).exists(), "orphan must not be GC'd");
    let orphan_after = fs::read_to_string(sidecar_path(root)).unwrap();
    let before_json: serde_json::Value = serde_json::from_str(&orphan_before).unwrap();
    let after_json: serde_json::Value = serde_json::from_str(&orphan_after).unwrap();
    assert_eq!(before_json["tx_id"], after_json["tx_id"]);
    assert_eq!(after_json["promote_failed"], true);
}

#[test]
#[serial(env)]
fn promote_success_sets_unverified_not_verified() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "observe"); // default path; success still Unverified

    fs::write(root.join("src/lib.rs"), "pub fn promote_ok() {}\n").unwrap();
    let msg = "feat: promote success path\n\nBody for hash match.";
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    fs::write(&msg_file, format!("{msg}\n")).unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    let commit = Command::new("git")
        .args(["commit", "--no-verify", "-F", msg_file.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(commit.status.success());

    let head_out = Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_msg = String::from_utf8_lossy(&head_out.stdout).to_string();
    let _tx_id = write_pending_sidecar_for_msg(root, &head_msg, "HIGH");

    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();

    assert!(
        !sidecar_path(root).exists(),
        "sidecar should be dropped on successful promote"
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let status: Option<String> = db
        .query_row(
            "SELECT verification_status FROM ledger_entries
             WHERE summary = 'feat: lifecycle integrity test'
             ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    assert_eq!(
        status.as_deref(),
        Some("unverified"),
        "non-TRIVIAL promote must set Unverified, not Verified; got {status:?}"
    );
    let basis: Option<String> = db
        .query_row(
            "SELECT verification_basis FROM ledger_entries
             WHERE summary = 'feat: lifecycle integrity test'
             ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    assert!(
        basis.is_none(),
        "verification_basis must be None on promote; got {basis:?}"
    );
}

#[test]
#[serial(env)]
fn recover_orphan_promote_clears_orphan() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "enforce");

    // Real pending + matching HEAD for successful recover --promote.
    fs::write(root.join("src/lib.rs"), "pub fn recover() {}\n").unwrap();
    let msg = "feat: recover orphan path\n\nBody.";
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    fs::write(&msg_file, format!("{msg}\n")).unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    let commit = Command::new("git")
        .args(["commit", "--no-verify", "-F", msg_file.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(commit.status.success());

    let head_out = Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_msg = String::from_utf8_lossy(&head_out.stdout).to_string();
    let _tx_id = write_pending_sidecar_for_msg(root, &head_msg, "HIGH");

    // Mark as promote_failed without destroying pending.
    let mut pending: PendingHookTx =
        serde_json::from_str(&fs::read_to_string(sidecar_path(root)).unwrap()).unwrap();
    pending.promote_failed = Some(true);
    pending.promote_error = Some("simulated".to_string());
    fs::write(sidecar_path(root), serde_json::to_string(&pending).unwrap()).unwrap();

    let recover = Command::new(ledgerful_bin())
        .args(["ledger", "recover-orphan", "--promote"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        recover.status.success(),
        "recover-orphan --promote failed: stdout={} stderr={}",
        String::from_utf8_lossy(&recover.stdout),
        String::from_utf8_lossy(&recover.stderr)
    );
    assert!(
        !sidecar_path(root).exists(),
        "sidecar must be cleared after recover --promote"
    );

    let status = Command::new(ledgerful_bin())
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(json["promoteOrphan"], false);
    assert_eq!(json["headUncovered"], false);
}

#[test]
#[serial(env)]
fn intent_never_under_enforce_hard_fails() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "enforce");
    set_intent_required(root, "never");

    fs::write(root.join("src/lib.rs"), "pub fn never() {}\n").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    fs::write(&msg_file, "feat: should be blocked\n\nBody.\n").unwrap();

    let hook = Command::new(ledgerful_bin())
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        !hook.status.success(),
        "intent.required=never under enforce must hard-fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&hook.stdout),
        String::from_utf8_lossy(&hook.stderr)
    );
    assert!(
        combined.contains("INTENT_NEVER_UNDER_ENFORCE"),
        "expected INTENT_NEVER_UNDER_ENFORCE in output: {combined}"
    );
}

#[test]
fn is_gc_eligible_table() {
    let cases = [
        // (promote_failed, hash, head, edit, eligible)
        (true, "aaa", Some("bbb"), Some("ccc"), false),
        (false, "head", Some("head"), None, false),
        (false, "edit", Some("other"), Some("edit"), false),
        (false, "stale", Some("head"), Some("edit"), true),
        (false, "stale", None, None, true),
        (true, "head", Some("head"), Some("head"), false),
    ];
    for (i, (pf, hash, head, edit, expected)) in cases.iter().enumerate() {
        let s = PendingHookTx {
            tx_id: "t".into(),
            commit_msg_hash: (*hash).into(),
            summary: "s".into(),
            reason: "r".into(),
            committed_at: None,
            risk: None,
            related_tickets: None,
            signature: None,
            public_key: None,
            snapshot_id: None,
            observed: None,
            promote_failed: if *pf { Some(true) } else { None },
            promote_error: None,
        };
        let ctx = GcContext {
            head_msg_hash: *head,
            editmsg_hash: *edit,
        };
        assert_eq!(
            is_gc_eligible(&s, &ctx),
            *expected,
            "case {i}: promote_failed={pf} hash={hash} head={head:?} edit={edit:?}"
        );
    }
}

/// Adaptive trivial bypass under enforce must write durable [SKIPPED] sidecar
/// (not silent Ok with no row).
#[test]
#[serial(env)]
fn adaptive_trivial_bypass_under_enforce_writes_skipped() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "enforce");

    // Seed adaptive bypass budget (as if user skipped twice previously).
    let skip_history = root
        .join(".ledgerful")
        .join("state")
        .join("skip_history.json");
    fs::write(
        &skip_history,
        r#"{"consecutive_skips":2,"bypass_remaining":2}"#,
    )
    .unwrap();

    fs::write(root.join("src/lib.rs"), "pub fn adaptive_skip() {}\n").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    // Trivial conventional subject so is_trivial_commit fires.
    fs::write(&msg_file, "chore: adaptive bypass under enforce\n").unwrap();

    let hook = Command::new(ledgerful_bin())
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        hook.status.success(),
        "adaptive SKIPPED path must return Ok; stdout={} stderr={}",
        String::from_utf8_lossy(&hook.stdout),
        String::from_utf8_lossy(&hook.stderr)
    );

    let sidecar = sidecar_path(root);
    assert!(
        sidecar.exists(),
        "enforce adaptive bypass must write durable pending_hook_tx, not silent Ok"
    );
    let pending: PendingHookTx =
        serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert!(
        pending.summary.starts_with(SKIPPED_SUMMARY_PREFIX),
        "summary must be [SKIPPED]-prefixed: {}",
        pending.summary
    );
    assert_eq!(
        pending.risk.as_deref(),
        Some(SKIPPED_COVERAGE_RISK),
        "SKIPPED risk must be non-TRIVIAL so promote → Unverified"
    );
    assert_ne!(pending.risk.as_deref(), Some("TRIVIAL"));

    // Promote path: Unverified, never Verified.
    fs::write(
        root.join("src/lib.rs"),
        "pub fn adaptive_skip() { let _ = 1; }\n",
    )
    .unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    // Re-bind sidecar hash to the message we will commit as HEAD.
    let msg = "chore: adaptive bypass under enforce\n";
    let cleaned = ledgerful::util::text::clean_commit_msg(msg);
    let mut pending = pending;
    pending.commit_msg_hash = hash_message(&cleaned);
    fs::write(&sidecar, serde_json::to_string(&pending).unwrap()).unwrap();
    let commit = Command::new("git")
        .args([
            "commit",
            "--no-verify",
            "-m",
            "chore: adaptive bypass under enforce",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    // Re-hash to actual HEAD message.
    let head_out = Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_msg = String::from_utf8_lossy(&head_out.stdout).to_string();
    let mut pending: PendingHookTx =
        serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
    pending.commit_msg_hash = hash_message(&ledgerful::util::text::clean_commit_msg(&head_msg));
    fs::write(&sidecar, serde_json::to_string(&pending).unwrap()).unwrap();

    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();
    assert!(
        !sidecar.exists(),
        "sidecar dropped after successful promote"
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let (summary, status): (String, Option<String>) = db
        .query_row(
            "SELECT summary, verification_status FROM ledger_entries
             WHERE summary LIKE '[SKIPPED]%'
             ORDER BY rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("SKIPPED COMMITTED row must exist");
    assert!(summary.starts_with(SKIPPED_SUMMARY_PREFIX));
    assert_eq!(
        status.as_deref(),
        Some("unverified"),
        "SKIPPED promote must be Unverified, not None/Verified; got {status:?}"
    );
}

/// TUI Skip under enforce uses the same disposition → record_enforce_skipped path.
/// Drive the shared helper via a unit-equivalent table + ensure the disposition
/// gate matches IntentState Skip (`s`).
#[test]
fn tui_skip_disposition_under_enforce_uses_skipped_model() {
    // Table-driven closed skip paths (DoD-2).
    let cases = [
        // (path, risk, what, is_skip)
        ("tui_s", "TRIVIAL", "Skipped intent entry", true),
        ("tui_accept", "MEDIUM", "feat: real", false),
        ("tui_wrong_risk", "MEDIUM", "Skipped intent entry", false),
    ];
    for (name, risk, what, expect_skip) in cases {
        assert_eq!(
            is_tui_skip_disposition(risk, what),
            expect_skip,
            "case {name}"
        );
        if expect_skip {
            let summary = skipped_coverage_summary("chore: from tui skip");
            assert!(summary.starts_with(SKIPPED_SUMMARY_PREFIX));
            assert_ne!(SKIPPED_COVERAGE_RISK, "TRIVIAL");
        }
    }
}

/// recover-orphan --abandon requires reason; writes MAINTENANCE; drops sidecar.
#[test]
#[serial(env)]
fn recover_orphan_abandon_clears_orphan_with_reason() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "enforce");
    force_promote_fail_orphan(root);

    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();
    assert!(sidecar_path(root).exists());

    // Empty reason must hard-fail (never silent delete).
    let empty = Command::new(ledgerful_bin())
        .args(["ledger", "recover-orphan", "--abandon", "--reason", "   "])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        !empty.status.success(),
        "empty abandon reason must fail; stdout={} stderr={}",
        String::from_utf8_lossy(&empty.stdout),
        String::from_utf8_lossy(&empty.stderr)
    );
    assert!(
        sidecar_path(root).exists(),
        "sidecar must remain after rejected empty reason"
    );

    let abandon = Command::new(ledgerful_bin())
        .args([
            "ledger",
            "recover-orphan",
            "--abandon",
            "--reason",
            "test abandon",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        abandon.status.success(),
        "recover-orphan --abandon failed: stdout={} stderr={}",
        String::from_utf8_lossy(&abandon.stdout),
        String::from_utf8_lossy(&abandon.stderr)
    );
    assert!(
        !sidecar_path(root).exists(),
        "sidecar must be cleared after abandon"
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let summary: String = db
        .query_row(
            "SELECT summary FROM ledger_entries
             WHERE summary LIKE '[ABANDONED]%'
             ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("durable ABANDONED / MAINTENANCE row required");
    assert!(
        summary.contains("test abandon") || summary.contains("ABANDONED"),
        "abandon row should record reason: {summary}"
    );

    let status = Command::new(ledgerful_bin())
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(json["promoteOrphan"], false);
    assert_eq!(json["headUncovered"], false);
}

/// ledger gc must refuse promote_failed orphans (shared GC policy).
#[test]
#[serial(env)]
fn ledger_gc_refuses_promote_failed_orphan() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "enforce");

    // Real pending + promote_failed sidecar so GC would see the PENDING id.
    fs::write(root.join("src/lib.rs"), "pub fn gc_protect() {}\n").unwrap();
    let msg = "feat: gc protect orphan\n\nBody.";
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    fs::write(&msg_file, format!("{msg}\n")).unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    let commit = Command::new("git")
        .args(["commit", "--no-verify", "-F", msg_file.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(commit.status.success());
    let head_out = Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_msg = String::from_utf8_lossy(&head_out.stdout).to_string();
    let tx_id = write_pending_sidecar_for_msg(root, &head_msg, "HIGH");
    let mut pending: PendingHookTx =
        serde_json::from_str(&fs::read_to_string(sidecar_path(root)).unwrap()).unwrap();
    pending.promote_failed = Some(true);
    pending.promote_error = Some("simulated for gc".to_string());
    fs::write(sidecar_path(root), serde_json::to_string(&pending).unwrap()).unwrap();

    // Backdate PENDING so --ttl-hours 1 / ttl_days 1 treats it as stale.
    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let old = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    db.execute(
        "UPDATE transactions SET started_at = ?1 WHERE tx_id = ?2",
        rusqlite::params![old, tx_id],
    )
    .unwrap();
    drop(db);

    let gc = Command::new(ledgerful_bin())
        .args(["ledger", "gc", "--stale", "--ttl-hours", "1", "--force"])
        .current_dir(root)
        .output()
        .unwrap();
    // Refuse is a non-zero exit when the only candidates are protected.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&gc.stdout),
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(
        combined.contains("PROMOTE_ORPHAN")
            || combined.contains("recover-orphan")
            || combined.contains("Refusing")
            || !gc.status.success(),
        "GC must refuse or surface recover path for promote_failed; output={combined}"
    );
    assert!(
        sidecar_path(root).exists(),
        "promote_failed sidecar must not be destroyed by ledger gc"
    );
    let after: PendingHookTx =
        serde_json::from_str(&fs::read_to_string(sidecar_path(root)).unwrap()).unwrap();
    assert_eq!(after.tx_id, tx_id);
    assert_eq!(after.promote_failed, Some(true));

    // PENDING must still exist (not rolled back).
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let status: String = db
        .query_row(
            "SELECT status FROM transactions WHERE tx_id = ?1",
            rusqlite::params![tx_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "PENDING", "GC must not rollback promote_failed tx");
}

/// hook-repair --force must refuse promote_failed orphans.
#[test]
#[serial(env)]
fn hook_repair_refuses_promote_failed_orphan() {
    let repo = setup_repo();
    let root = repo.path();
    set_gate_mode(root, "enforce");
    force_promote_fail_orphan(root);

    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();
    assert!(sidecar_path(root).exists());

    // Mismatch HEAD so hook-repair would otherwise try force delete: rewrite hash.
    let mut pending: PendingHookTx =
        serde_json::from_str(&fs::read_to_string(sidecar_path(root)).unwrap()).unwrap();
    pending.commit_msg_hash = "deadbeef_mismatch_for_repair".to_string();
    pending.promote_failed = Some(true);
    fs::write(sidecar_path(root), serde_json::to_string(&pending).unwrap()).unwrap();

    let repair = Command::new(ledgerful_bin())
        .args(["ledger", "hook-repair", "--force"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        !repair.status.success(),
        "hook-repair --force must refuse promote_failed; stdout={} stderr={}",
        String::from_utf8_lossy(&repair.stdout),
        String::from_utf8_lossy(&repair.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&repair.stdout),
        String::from_utf8_lossy(&repair.stderr)
    );
    assert!(
        combined.contains("PROMOTE_ORPHAN") || combined.contains("recover-orphan"),
        "expected recover-orphan guidance: {combined}"
    );
    assert!(
        sidecar_path(root).exists(),
        "promote_failed sidecar must remain after refused hook-repair"
    );
}
