use ledgerful::commands::hook_commit_msg::{
    LedgerRefStatus, ProvenanceSotClass, canonical_entity, classify_provenance_sot,
    extract_ledger_tx_ref, extract_trailers, is_trivial_commit, parse_category_from_message,
};
use ledgerful::ledger::Category;

#[test]
fn test_category_inference_covers_all_prefixes() {
    assert_eq!(
        parse_category_from_message("feat: add something"),
        Category::Feature
    );
    assert_eq!(
        parse_category_from_message("fix: fix bug"),
        Category::Bugfix
    );
    assert_eq!(
        parse_category_from_message("bug: fix another bug"),
        Category::Bugfix
    );
    assert_eq!(
        parse_category_from_message("docs: update readme"),
        Category::Docs
    );
    assert_eq!(
        parse_category_from_message("refactor: clean up"),
        Category::Refactor
    );
    assert_eq!(
        parse_category_from_message("perf: make it faster"),
        Category::Refactor
    );
    assert_eq!(
        parse_category_from_message("chore: cleanup"),
        Category::Chore
    );
    assert_eq!(
        parse_category_from_message("ci: update workflow"),
        Category::Infra
    );
    assert_eq!(
        parse_category_from_message("infra: update server"),
        Category::Infra
    );
    assert_eq!(
        parse_category_from_message("build: compile fix"),
        Category::Infra
    );
    assert_eq!(
        parse_category_from_message("style: format code"),
        Category::Tooling
    );
    assert_eq!(
        parse_category_from_message("revert: undo last"),
        Category::Bugfix
    );
    assert_eq!(
        parse_category_from_message("security: fix vulnerability"),
        Category::Security
    );
    assert_eq!(
        parse_category_from_message("breaking: major change"),
        Category::Architecture
    );
    assert_eq!(
        parse_category_from_message("random: no prefix"),
        Category::Chore
    );
}

#[test]
fn test_multi_file_entity_canonical_path() {
    let files = vec![
        "src/ledger/mod.rs".to_string(),
        "src/ledger/types.rs".to_string(),
    ];
    assert_eq!(canonical_entity(&files), "src/ledger");

    let files = vec![
        "src/ledger/mod.rs".to_string(),
        "src/commands/mod.rs".to_string(),
    ];
    assert_eq!(canonical_entity(&files), "src");

    let files = vec![
        "src/ledger/mod.rs".to_string(),
        "docs/README.md".to_string(),
    ];
    assert_eq!(canonical_entity(&files), "src/ledger/mod.rs (+1 more)");
}

#[test]
fn test_trivial_bypass_skips_tui() {
    assert!(is_trivial_commit("chore: cleanup"));
    assert!(is_trivial_commit("docs: update"));
    assert!(is_trivial_commit("style: format"));
    assert!(is_trivial_commit("test: add tests"));
    assert!(!is_trivial_commit("feat: new feature"));
}

#[test]
fn test_trailer_preservation() {
    let msg = "feat: add feature\n\nThis adds a new feature.\n\nSigned-off-by: Alice <alice@example.com>\nCo-authored-by: Bob <bob@example.com>";
    let trailers = extract_trailers(msg);
    assert!(trailers.contains("Signed-off-by: Alice <alice@example.com>"));
    assert!(trailers.contains("Co-authored-by: Bob <bob@example.com>"));

    let msg_no_trailers = "feat: add feature\n\nThis adds a new feature.";
    assert_eq!(extract_trailers(msg_no_trailers), "");
}

#[test]
fn test_non_interactive_bypasses_tui() {
    // This would test the environment variable check in execute_hook_commit_msg
    // but requires full command execution setup.
}

#[test]
fn test_stale_sidecar_is_rolled_back_on_next_commit_attempt() {
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

    // Start a transaction
    std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "my-abandoned-feature",
            "--category",
            "FEATURE",
            "--message",
            "test stale rollback",
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

    // Write a sidecar with a hash that doesn't match HEAD
    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    let sidecar_json = serde_json::json!({
        "tx_id": tx_id,
        "commit_msg_hash": "deadbeef12345678", // Definitely won't match
        "summary": "abandoned commit",
        "reason": "abandoned reason"
    });
    std::fs::write(&sidecar_path, serde_json::to_string(&sidecar_json).unwrap()).unwrap();

    // Now simulate a new commit attempt by running the commit-msg hook
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    std::fs::write(
        &msg_file,
        "feat: new fresh commit\n\nThis is a brand new commit.",
    )
    .unwrap();
    std::fs::write(root.join("dummy2.txt"), "hello").unwrap();
    std::process::Command::new("git")
        .args(["add", "dummy2.txt"])
        .current_dir(root)
        .output()
        .unwrap();

    let hook_output = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    // Hook should succeed, not fail! It cleans up the sidecar and processes the new commit.
    assert!(
        hook_output.status.success(),
        "Hook should not fail on a stale sidecar: {}",
        String::from_utf8_lossy(&hook_output.stderr)
    );

    // Verify sidecar was replaced with a new one
    assert!(
        sidecar_path.exists(),
        "Sidecar should have been recreated for the new commit attempt. Hook output:\nSTDOUT:\n{}\nSTDERR:\n{}",
        String::from_utf8_lossy(&hook_output.stdout),
        String::from_utf8_lossy(&hook_output.stderr)
    );
    let new_sidecar_content = std::fs::read_to_string(&sidecar_path).unwrap();
    let new_sidecar_json: serde_json::Value = serde_json::from_str(&new_sidecar_content).unwrap();
    let new_tx_id = new_sidecar_json["tx_id"].as_str().unwrap();
    assert_ne!(
        new_tx_id, tx_id,
        "Sidecar tx_id should be different after recreating"
    );

    // Verify the original transaction was rolled back
    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let status: String = db
        .query_row(
            "SELECT status FROM transactions WHERE tx_id = ?1",
            rusqlite::params![tx_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "ROLLED_BACK",
        "Stale transaction should be rolled back"
    );
}

#[test]
fn test_real_shell_git_commit_amend_success() {
    // This test exercises the `matches_editmsg` early-return in the commit-msg hook.
    //
    // Scenario: the user makes an initial commit (commit-msg hook runs, TX1 PENDING sidecar
    // written), but the post-commit hook does NOT run yet. The user then amends the commit
    // (commit-msg runs again, sees the existing sidecar, `matches_editmsg` == true, returns
    // early without creating TX2). Finally, post-commit runs once and promotes TX1 to COMMITTED.
    //
    // Without the `return Ok(())` on `matches_editmsg`, the amend would call `start_change`
    // again and produce an orphaned second pending transaction.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Setup git user just in case
    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(root)
        .output()
        .unwrap();

    // Install ONLY the commit-msg hook for the initial commit.
    // Remove any post-commit hook that `ledgerful init` may have installed so TX1 stays PENDING.
    std::fs::write(
        root.join(".git").join("hooks").join("commit-msg"),
        format!(
            "#!/bin/sh\n\"{}\" internal hook-commit-msg \"$1\"\n",
            ledgerful_bin.replace("\\", "/")
        ),
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::PermissionsExt::set_mode(
        &mut std::fs::File::open(root.join(".git").join("hooks").join("commit-msg"))
            .unwrap()
            .metadata()
            .unwrap()
            .permissions(),
        0o755,
    );

    // Remove post-commit so the sidecar stays PENDING after the initial commit.
    // Remove pre-commit so it does not block the amend (it would reject the amend because TX1 is pending).
    // We install post-commit later to run it once after the amend.
    for hook_name in &["post-commit", "pre-commit"] {
        let hook_path = root.join(".git").join("hooks").join(hook_name);
        if hook_path.exists() {
            std::fs::remove_file(&hook_path).unwrap();
        }
    }

    // Initial commit — post-commit is NOT installed here, so TX1 stays PENDING
    std::fs::write(root.join("dummy.txt"), "hello").unwrap();
    std::process::Command::new("git")
        .args(["add", "dummy.txt"])
        .current_dir(root)
        .output()
        .unwrap();

    let commit_output = std::process::Command::new("git")
        .args(["commit", "-m", "feat: initial commit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(
        commit_output.status.success(),
        "Initial commit failed: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );

    // Verify sidecar was created (TX1 is PENDING because post-commit wasn't installed)
    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    assert!(
        sidecar_path.exists(),
        "Sidecar should exist after initial commit (post-commit was skipped)"
    );
    let sidecar_before = std::fs::read_to_string(&sidecar_path).unwrap();
    let sidecar_json_before: serde_json::Value = serde_json::from_str(&sidecar_before).unwrap();
    let tx1_id = sidecar_json_before["tx_id"].as_str().unwrap().to_string();

    // Amend commit (same message, adding a file change)
    // The commit-msg hook will see the existing sidecar and matches_editmsg will be true,
    // triggering the early return without creating a second pending transaction.
    std::fs::write(root.join("dummy.txt"), "hello world").unwrap();
    std::process::Command::new("git")
        .args(["add", "dummy.txt"])
        .current_dir(root)
        .output()
        .unwrap();

    let amend_output = std::process::Command::new("git")
        .args(["commit", "--amend", "--no-edit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(
        amend_output.status.success(),
        "Amend failed. STDOUT: {}\nSTDERR: {}",
        String::from_utf8_lossy(&amend_output.stdout),
        String::from_utf8_lossy(&amend_output.stderr)
    );

    // The sidecar must still be present and contain the SAME tx_id (TX1 was reused, not replaced)
    assert!(
        sidecar_path.exists(),
        "Sidecar should still exist after amend (post-commit hasn't run yet)"
    );
    let sidecar_after = std::fs::read_to_string(&sidecar_path).unwrap();
    let sidecar_json_after: serde_json::Value = serde_json::from_str(&sidecar_after).unwrap();
    let tx_id_after = sidecar_json_after["tx_id"].as_str().unwrap().to_string();
    assert_eq!(
        tx1_id, tx_id_after,
        "Amend must reuse the existing sidecar tx_id, not create a new one"
    );

    // Now install and run the post-commit hook to promote TX1 to COMMITTED
    std::fs::write(
        root.join(".git").join("hooks").join("post-commit"),
        format!(
            "#!/bin/sh\n\"{}\" internal hook-post-commit\n",
            ledgerful_bin.replace("\\", "/")
        ),
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::PermissionsExt::set_mode(
        &mut std::fs::File::open(root.join(".git").join("hooks").join("post-commit"))
            .unwrap()
            .metadata()
            .unwrap()
            .permissions(),
        0o755,
    );

    let post_commit_output = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-post-commit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();

    assert!(
        post_commit_output.status.success(),
        "Post-commit failed: {}",
        String::from_utf8_lossy(&post_commit_output.stderr)
    );

    // Sidecar should now be cleaned up
    assert!(
        !sidecar_path.exists(),
        "Sidecar should be cleaned up after post-commit runs"
    );

    // Verify the DB state: exactly one transaction (TX1) from the commit+amend
    // lifecycle was ever created and it is now COMMITTED. `ledgerful init` seeds
    // an additional mode transaction, so exclude that from counts.
    // If the early-return on `matches_editmsg` is missing, the amend would call
    // `start_change` again, making total_count == 2.
    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();

    let committed_count: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE status = 'COMMITTED' AND entity != 'ledgerful/gate-mode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        committed_count, 1,
        "There should be exactly one committed transaction from the commit+amend lifecycle"
    );

    let pending_count: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE status = 'PENDING' AND entity != 'ledgerful/gate-mode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        pending_count, 0,
        "No extra pending transactions should be leaked by an amend"
    );

    // The key assertion: exactly one transaction was ever created across the initial commit + amend
    // lifecycle. Without the `matches_editmsg` early return this would be 2.
    let total_count: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE entity != 'ledgerful/gate-mode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        total_count, 1,
        "Exactly one transaction should have been created across the initial commit + amend lifecycle (got {total_count})"
    );
}

// ---------------------------------------------------------------------------
// 0122 — Provenance SoT
// ---------------------------------------------------------------------------

/// Pure extract re-export surface (integration crate can call lib helpers).
#[test]
fn extract_ledger_tx_ref_reporting_fixture_none() {
    assert_eq!(
        extract_ledger_tx_ref("Ledger: 2 pending, 0 unaudited drift."),
        None
    );
    assert_eq!(extract_ledger_tx_ref("Ledger: 3 unaudited…"), None);
}

#[test]
fn classify_provenance_sot_pure_surface() {
    let id = "d7f2e5e8-59b5-42fd-bcf3-d4ee99c507bf";
    assert!(matches!(
        classify_provenance_sot(Some(id), &[], Some(LedgerRefStatus::Committed)),
        ProvenanceSotClass::AlreadyCommitted { .. }
    ));
    assert!(matches!(
        classify_provenance_sot(None, &[id.to_string()], None),
        ProvenanceSotClass::LinkPending { .. }
    ));
    assert_eq!(
        classify_provenance_sot(None, &["a".into(), "b".into()], None),
        ProvenanceSotClass::AmbiguousMulti
    );
    assert_eq!(
        classify_provenance_sot(None, &[], None),
        ProvenanceSotClass::Fallback
    );
}

/// AlreadyCommitted: agent ledger start+commit; conventional msg + `Ledger: {tx}`;
/// hook must not open a second pending / new sidecar TX id.
#[test]
fn provenance_sot_already_committed_skips_intent_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    let init = std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let start = std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "agent-entity-0122",
            "--category",
            "FEATURE",
            "--message",
            "agent intentional summary",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "ledger start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    let status_out = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let status_json: serde_json::Value = serde_json::from_slice(&status_out.stdout).unwrap();
    let tx_id = status_json["pendingTxIds"][0]
        .as_str()
        .expect("pending tx id")
        .to_string();

    let commit = std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "commit",
            &tx_id,
            "--summary",
            "agent committed summary",
            "--reason",
            "agent intentional reason",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "ledger commit failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    // Stage a file for the git commit-msg hook path.
    std::fs::write(root.join("feature.txt"), "content").unwrap();
    std::process::Command::new("git")
        .args(["add", "feature.txt"])
        .current_dir(root)
        .output()
        .unwrap();

    // Well-formed conventional subject+body AND Ledger line (AI3 regression class).
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    let msg = format!(
        "feat: ship agent change\n\nBody with enough intent for conventional bypass.\n\nLedger: {tx_id}"
    );
    std::fs::write(&msg_file, &msg).unwrap();

    let hook_output = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env("RUST_LOG", "info")
        .output()
        .unwrap();

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&hook_output.stdout),
        String::from_utf8_lossy(&hook_output.stderr)
    );
    assert!(
        hook_output.status.success(),
        "hook-commit-msg failed: {combined}"
    );
    assert!(
        combined.contains("Provenance SoT") && combined.contains("already committed"),
        "expected Provenance SoT already-committed line, got:\n{combined}"
    );
    assert!(
        !combined.contains("Drafting change intent via local LLM"),
        "must not draft LLM intent on AlreadyCommitted:\n{combined}"
    );

    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    assert!(
        !sidecar_path.exists(),
        "AlreadyCommitted must not write a new pending_hook_tx sidecar"
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let pending_count: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE status = 'PENDING' AND entity != 'ledgerful/gate-mode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        pending_count, 0,
        "AlreadyCommitted must not open a second pending TX"
    );

    let committed_agent: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE tx_id = ?1 AND status = 'COMMITTED'",
            rusqlite::params![tx_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(committed_agent, 1, "agent TX must remain committed");
}

/// LinkPending: one open PENDING globally; hook without Ledger line links that id.
#[test]
fn provenance_sot_link_pending_single_open() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    let init = std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let start = std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "agent-slug-entity",
            "--category",
            "FEATURE",
            "--message",
            "open agent pending",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "ledger start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    let status_out = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let status_json: serde_json::Value = serde_json::from_slice(&status_out.stdout).unwrap();
    let pending_tx = status_json["pendingTxIds"][0]
        .as_str()
        .expect("pending tx")
        .to_string();

    std::fs::write(root.join("work.txt"), "w").unwrap();
    std::process::Command::new("git")
        .args(["add", "work.txt"])
        .current_dir(root)
        .output()
        .unwrap();

    // No Ledger: line — single global PENDING should LinkPending.
    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    std::fs::write(
        &msg_file,
        "feat: continue agent work\n\nBody for conventional well-formed path.",
    )
    .unwrap();

    let hook_output = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env("RUST_LOG", "info")
        .output()
        .unwrap();

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&hook_output.stdout),
        String::from_utf8_lossy(&hook_output.stderr)
    );
    assert!(
        hook_output.status.success(),
        "hook-commit-msg failed: {combined}"
    );
    assert!(
        combined.contains("Provenance SoT") && combined.contains("linking open ledger TX"),
        "expected LinkPending SoT line, got:\n{combined}"
    );
    assert!(
        !combined.contains("Drafting change intent via local LLM"),
        "must not draft LLM on LinkPending:\n{combined}"
    );

    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    assert!(sidecar_path.exists(), "LinkPending must write sidecar");
    let sidecar: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    assert_eq!(
        sidecar["tx_id"].as_str().unwrap(),
        pending_tx,
        "sidecar must link the single open pending TX"
    );
    // Sidecar risk/category basis must match pending FEATURE TX (not re-parsed CHORE).
    assert_ne!(
        sidecar["risk"].as_str().unwrap_or(""),
        "TRIVIAL",
        "LinkPending must sign with pending TX category risk, not re-parsed message category"
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let pending_count: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE status = 'PENDING' AND entity != 'ledgerful/gate-mode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        pending_count, 1,
        "LinkPending must not start_change a second pending"
    );

    // Finalize git commit (no hooks) then promote — signature must verify against TX category.
    let empty_hooks = root.join("empty-hooks");
    std::fs::create_dir_all(&empty_hooks).unwrap();
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            &format!("core.hooksPath={}", empty_hooks.display()),
            "commit",
            "-F",
            msg_file.to_str().unwrap(),
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "git commit (no hooks) failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    let promote = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-post-commit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        promote.status.success(),
        "post-commit promote failed: stdout={} stderr={}",
        String::from_utf8_lossy(&promote.stdout),
        String::from_utf8_lossy(&promote.stderr)
    );

    let status: String = db
        .query_row(
            "SELECT status FROM transactions WHERE tx_id = ?1",
            rusqlite::params![&pending_tx],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "COMMITTED",
        "LinkPending signature must verify so post-commit can promote agent TX"
    );
    assert!(
        !sidecar_path.exists(),
        "sidecar should be removed after successful promote"
    );
}

/// 0287: LinkPending sidecar + promoted row store the entity slug ticket id, not staged files.
#[test]
fn link_pending_related_tickets_are_ticket_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    let init = std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let start = std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "0287-RelatedTicketsTicketIds",
            "--category",
            "BUGFIX",
            "--message",
            "related_tickets store ticket ids not staged files",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "ledger start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    let status_out = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let status_json: serde_json::Value = serde_json::from_slice(&status_out.stdout).unwrap();
    let pending_tx = status_json["pendingTxIds"][0]
        .as_str()
        .expect("pending tx")
        .to_string();

    std::fs::write(root.join("CHANGELOG.md"), "# changelog\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "CHANGELOG.md"])
        .current_dir(root)
        .output()
        .unwrap();

    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    std::fs::write(
        &msg_file,
        "fix: store ticket ids in related_tickets\n\nBody for conventional well-formed path.",
    )
    .unwrap();

    let hook_output = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        hook_output.status.success(),
        "hook-commit-msg failed: stdout={} stderr={}",
        String::from_utf8_lossy(&hook_output.stdout),
        String::from_utf8_lossy(&hook_output.stderr)
    );

    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    let sidecar: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    let sidecar_tickets = sidecar["related_tickets"].as_str().unwrap_or("");
    assert_eq!(sidecar_tickets, "0287");
    assert!(!sidecar_tickets.contains("CHANGELOG"));
    assert!(!sidecar_tickets.contains('/'));
    assert!(!sidecar_tickets.contains('\\'));

    let empty_hooks = root.join("empty-hooks");
    std::fs::create_dir_all(&empty_hooks).unwrap();
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            &format!("core.hooksPath={}", empty_hooks.display()),
            "commit",
            "-F",
            msg_file.to_str().unwrap(),
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "git commit (no hooks) failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    let promote = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-post-commit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        promote.status.success(),
        "post-commit promote failed: stdout={} stderr={}",
        String::from_utf8_lossy(&promote.stdout),
        String::from_utf8_lossy(&promote.stderr)
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let stored: Option<String> = db
        .query_row(
            "SELECT related_tickets FROM ledger_entries WHERE tx_id = ?1",
            rusqlite::params![&pending_tx],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored.as_deref(), Some("0287"));
}

/// HookFallback regression: zero pending, no Ledger line, conventional still creates pending_hook_tx.
#[test]
fn provenance_sot_hook_fallback_creates_pending_when_no_sot() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");

    let init = std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    std::fs::write(root.join("fallback.txt"), "f").unwrap();
    std::process::Command::new("git")
        .args(["add", "fallback.txt"])
        .current_dir(root)
        .output()
        .unwrap();

    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    std::fs::write(
        &msg_file,
        "feat: human only commit\n\nBody ensures well-formed conventional bypass.",
    )
    .unwrap();

    let hook_output = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env("RUST_LOG", "info")
        .output()
        .unwrap();

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&hook_output.stdout),
        String::from_utf8_lossy(&hook_output.stderr)
    );
    assert!(
        hook_output.status.success(),
        "hook-commit-msg failed: {combined}"
    );
    assert!(
        !combined.contains("Provenance SoT"),
        "HookFallback must not emit Provenance SoT line:\n{combined}"
    );

    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    assert!(
        sidecar_path.exists(),
        "HookFallback conventional path must still write pending_hook_tx"
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let pending_count: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE status = 'PENDING' AND entity != 'ledgerful/gate-mode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        pending_count, 1,
        "HookFallback must create exactly one pending TX"
    );
}

/// LinkPending sidecar why must not be a trailer-only Co-authored-by body.
#[test]
fn link_pending_sidecar_reason_is_not_trailer_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");
    let init = std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let start = std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "0319-fixture-track",
            "--category",
            "BUGFIX",
            "--message",
            "open pending",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "ledger start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    std::fs::write(root.join("work.txt"), "w").unwrap();
    std::process::Command::new("git")
        .args(["add", "work.txt"])
        .current_dir(root)
        .output()
        .unwrap();

    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    let original = "feat: bind review evidence\n\nCo-authored-by: Cursor <cursoragent@cursor.com>";
    std::fs::write(&msg_file, original).unwrap();

    let hook_output = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        hook_output.status.success(),
        "hook-commit-msg failed: stdout={} stderr={}",
        String::from_utf8_lossy(&hook_output.stdout),
        String::from_utf8_lossy(&hook_output.stderr)
    );

    let rewritten = std::fs::read_to_string(&msg_file).unwrap();
    let subject_count = rewritten.matches("feat: bind review evidence").count();
    assert_eq!(
        subject_count, 1,
        "conventional bypass must not duplicate the subject in COMMIT_EDITMSG: {rewritten}"
    );
    assert!(
        rewritten.contains("Co-authored-by: Cursor"),
        "trailers must stay on the git message: {rewritten}"
    );

    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    let sidecar: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    let why = sidecar["reason"].as_str().unwrap_or("");
    assert!(
        !why.contains("Co-authored-by"),
        "sidecar reason must not be trailer-only: {why}"
    );
    assert_eq!(why, "feat: bind review evidence");
}

struct ManualHookEntry {
    tx_id: String,
    summary: String,
    reason: String,
    entry_type: String,
    verification_status: Option<String>,
    signature: Option<String>,
    public_key: Option<String>,
}

/// Manual `ledger commit`, then a git commit with no ledger line and zero PENDING,
/// keeps the CLI row and a distinct HOOK row. Same summary is not a merge.
#[test]
fn manual_commit_then_hook_without_trailer_keeps_two_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");
    let subject_a = "feat: record one edit";
    let body_a = "Body records the same edit.";
    let subject_b = "feat: record a different edit";
    let body_b = "Body records a different edit.";

    let init = std::process::Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let config = std::fs::read_to_string(root.join(".ledgerful").join("config.toml")).unwrap();
    assert!(
        config.contains("mode = \"observe\""),
        "oracle stays on default observe gate mode"
    );
    assert!(
        !config.contains("mode = \"enforce\""),
        "oracle must not set enforce"
    );

    let start = std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "alpha.rs",
            "--category",
            "FEATURE",
            "--message",
            "open the CLI row",
        ])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "ledger start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    let status_out = std::process::Command::new(ledgerful_bin)
        .args(["ledger", "status", "--json"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    let status_json: serde_json::Value = serde_json::from_slice(&status_out.stdout).unwrap();
    let cli_tx = status_json["pendingTxIds"][0]
        .as_str()
        .expect("pending tx id")
        .to_string();

    let commit = std::process::Command::new(ledgerful_bin)
        .args([
            "ledger",
            "commit",
            &cli_tx,
            "--summary",
            subject_a,
            "--reason",
            "manual ledger commit",
        ])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "ledger commit failed: stdout={} stderr={}",
        String::from_utf8_lossy(&commit.stdout),
        String::from_utf8_lossy(&commit.stderr)
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let (sig_before, key_before, summary_before, reason_before, status_before) = {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        let row: (
            Option<String>,
            Option<String>,
            String,
            String,
            Option<String>,
        ) = db
            .query_row(
                "SELECT signature, public_key, summary, reason, verification_status \
                 FROM ledger_entries WHERE tx_id = ?1 AND entry_type = 'IMPLEMENTATION'",
                rusqlite::params![&cli_tx],
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
        row
    };
    assert!(
        sig_before.as_ref().is_some_and(|s| !s.is_empty()),
        "CLI signature must be non-null and non-empty before the hook"
    );
    assert!(
        key_before.as_ref().is_some_and(|s| !s.is_empty()),
        "CLI public key must be non-null and non-empty before the hook"
    );
    assert_eq!(summary_before, subject_a);
    assert_eq!(reason_before, "manual ledger commit");
    assert!(
        status_before.is_none(),
        "CLI verification_status must be SQL NULL, got {status_before:?}"
    );

    std::fs::write(root.join("alpha.rs"), "fn alpha() {}\n").unwrap();
    let add_a = std::process::Command::new("git")
        .args(["add", "alpha.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(add_a.status.success(), "git add alpha.rs failed");

    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    let msg_a = format!("{subject_a}\n\n{body_a}\n");
    assert!(!msg_a.contains("Ledger:"));
    assert!(!msg_a.contains("Ledger-Tx:"));
    std::fs::write(&msg_file, &msg_a).unwrap();

    let hook_a = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        hook_a.status.success(),
        "hook-commit-msg failed: stdout={} stderr={}",
        String::from_utf8_lossy(&hook_a.stdout),
        String::from_utf8_lossy(&hook_a.stderr)
    );

    let empty_hooks = root.join("empty-hooks");
    std::fs::create_dir_all(&empty_hooks).unwrap();
    let git_commit_a = std::process::Command::new("git")
        .args([
            "-c",
            &format!("core.hooksPath={}", empty_hooks.display()),
            "commit",
            "-F",
            msg_file.to_str().unwrap(),
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        git_commit_a.status.success(),
        "git commit (no hooks) failed: {}",
        String::from_utf8_lossy(&git_commit_a.stderr)
    );

    let promote_a = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-post-commit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        promote_a.status.success(),
        "post-commit failed: stdout={} stderr={}",
        String::from_utf8_lossy(&promote_a.stdout),
        String::from_utf8_lossy(&promote_a.stderr)
    );

    let count_after_first = {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        let count: i32 = db
            .query_row(
                "SELECT COUNT(*) FROM transactions WHERE entity != 'ledgerful/gate-mode'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        count
    };

    let promote_a_again = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-post-commit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        promote_a_again.status.success(),
        "second post-commit failed: stdout={} stderr={}",
        String::from_utf8_lossy(&promote_a_again.stdout),
        String::from_utf8_lossy(&promote_a_again.stderr)
    );

    let db = rusqlite::Connection::open(&db_path).unwrap();
    let count_after_second: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE entity != 'ledgerful/gate-mode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count_after_first, 2, "first promote must leave two rows");
    assert_eq!(
        count_after_second, count_after_first,
        "second post-commit must not add a transaction"
    );

    let mut tx_stmt = db
        .prepare(
            "SELECT tx_id, source, status, entity FROM transactions \
             WHERE entity != 'ledgerful/gate-mode' ORDER BY tx_id",
        )
        .unwrap();
    let tx_rows: Vec<(String, String, String, String)> = tx_stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(tx_rows.len(), 2, "expected CLI and HOOK transactions");
    assert!(
        tx_rows.iter().all(|row| row.2 == "COMMITTED"),
        "both rows must be COMMITTED: {tx_rows:?}"
    );
    assert_ne!(tx_rows[0].0, tx_rows[1].0, "tx ids must be distinct");
    let cli = tx_rows
        .iter()
        .find(|row| row.1 == "CLI")
        .expect("CLI source row");
    let hook = tx_rows
        .iter()
        .find(|row| row.1 == "HOOK")
        .expect("HOOK source row");
    assert_eq!(cli.0, cli_tx);
    assert_eq!(hook.3, "alpha.rs");
    let hook_tx = hook.0.clone();

    let mut entry_stmt = db
        .prepare(
            "SELECT tx_id, summary, reason, entry_type, verification_status, signature, public_key \
             FROM ledger_entries WHERE tx_id = ?1 OR tx_id = ?2 ORDER BY tx_id",
        )
        .unwrap();
    let entries: Vec<ManualHookEntry> = entry_stmt
        .query_map(rusqlite::params![&cli_tx, &hook_tx], |row| {
            Ok(ManualHookEntry {
                tx_id: row.get(0)?,
                summary: row.get(1)?,
                reason: row.get(2)?,
                entry_type: row.get(3)?,
                verification_status: row.get(4)?,
                signature: row.get(5)?,
                public_key: row.get(6)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(entry.summary, subject_a);
        assert_eq!(entry.entry_type, "IMPLEMENTATION");
        assert_ne!(entry.verification_status.as_deref(), Some("verified"));
    }
    let cli_entry = entries
        .iter()
        .find(|entry| entry.tx_id == cli.0)
        .expect("CLI entry");
    let hook_entry = entries
        .iter()
        .find(|entry| entry.tx_id == hook_tx)
        .expect("HOOK entry");
    assert_eq!(hook_entry.reason, body_a);
    assert!(
        cli_entry.verification_status.is_none(),
        "CLI verification_status must stay SQL NULL, got {:?}",
        cli_entry.verification_status
    );
    assert_eq!(
        hook_entry.verification_status.as_deref(),
        Some("unverified")
    );
    assert_eq!(cli_entry.signature, sig_before);
    assert_eq!(cli_entry.public_key, key_before);
    assert_eq!(cli_entry.summary, summary_before);
    assert_eq!(cli_entry.reason, reason_before);
    assert!(cli_entry.signature.as_ref().is_some_and(|s| !s.is_empty()));
    assert!(cli_entry.public_key.as_ref().is_some_and(|s| !s.is_empty()));
    drop(entry_stmt);
    drop(tx_stmt);
    drop(db);

    std::fs::write(root.join("beta.rs"), "fn beta() {}\n").unwrap();
    let add_b = std::process::Command::new("git")
        .args(["add", "beta.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(add_b.status.success(), "git add beta.rs failed");
    let msg_b = format!("{subject_b}\n\n{body_b}\n");
    assert!(!msg_b.contains("Ledger:"));
    assert!(!msg_b.contains("Ledger-Tx:"));
    std::fs::write(&msg_file, &msg_b).unwrap();

    let hook_b = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        hook_b.status.success(),
        "second hook-commit-msg failed: stdout={} stderr={}",
        String::from_utf8_lossy(&hook_b.stdout),
        String::from_utf8_lossy(&hook_b.stderr)
    );
    let git_commit_b = std::process::Command::new("git")
        .args([
            "-c",
            &format!("core.hooksPath={}", empty_hooks.display()),
            "commit",
            "-F",
            msg_file.to_str().unwrap(),
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        git_commit_b.status.success(),
        "second git commit failed: {}",
        String::from_utf8_lossy(&git_commit_b.stderr)
    );
    let promote_b = std::process::Command::new(ledgerful_bin)
        .args(["internal", "hook-post-commit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        promote_b.status.success(),
        "beta post-commit failed: stdout={} stderr={}",
        String::from_utf8_lossy(&promote_b.stdout),
        String::from_utf8_lossy(&promote_b.stderr)
    );

    let db = rusqlite::Connection::open(&db_path).unwrap();
    let mut tx_stmt = db
        .prepare(
            "SELECT tx_id, source, status, entity FROM transactions \
             WHERE entity != 'ledgerful/gate-mode' ORDER BY tx_id",
        )
        .unwrap();
    let tx_rows: Vec<(String, String, String, String)> = tx_stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(tx_rows.len(), 3, "beta commit must append one HOOK row");
    assert!(tx_rows.iter().any(|row| row.0 == cli_tx));
    assert!(tx_rows.iter().any(|row| row.0 == hook_tx));
    let beta = tx_rows
        .iter()
        .find(|row| row.0 != cli_tx && row.0 != hook_tx)
        .expect("new HOOK row");
    assert_eq!(beta.1, "HOOK");
    assert_eq!(beta.2, "COMMITTED");
    assert_eq!(beta.3, "beta.rs");

    let beta_entry: (String, Option<String>) = db
        .query_row(
            "SELECT summary, verification_status FROM ledger_entries WHERE tx_id = ?1",
            rusqlite::params![&beta.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(beta_entry.0, subject_b);
    assert_eq!(beta_entry.1.as_deref(), Some("unverified"));

    let subject_a_rows: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries WHERE summary = ?1",
            rusqlite::params![subject_a],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(subject_a_rows, 2, "subject A must still have two rows");

    let cli_after: (Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT signature, public_key, verification_status FROM ledger_entries WHERE tx_id = ?1",
            rusqlite::params![&cli_tx],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(cli_after.0, sig_before);
    assert_eq!(cli_after.1, key_before);
    assert!(cli_after.0.as_ref().is_some_and(|s| !s.is_empty()));
    assert!(cli_after.1.as_ref().is_some_and(|s| !s.is_empty()));
    assert!(cli_after.2.is_none());
}
