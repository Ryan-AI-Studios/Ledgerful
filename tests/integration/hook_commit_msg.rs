use ledgerful::commands::hook_commit_msg::{
    canonical_entity, extract_trailers, is_trivial_commit, parse_category_from_message,
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

    // Verify the DB state: exactly one transaction (TX1) was ever created and it is now COMMITTED.
    // If the early-return on `matches_editmsg` is missing, the amend would call `start_change`
    // again, making total_count == 2.
    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();

    let committed_count: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE status = 'COMMITTED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        committed_count, 1,
        "There should be exactly one committed transaction"
    );

    let pending_count: i32 = db
        .query_row(
            "SELECT COUNT(*) FROM transactions WHERE status = 'PENDING'",
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
        .query_row("SELECT COUNT(*) FROM transactions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        total_count, 1,
        "Exactly one transaction should have been created across the initial commit + amend lifecycle (got {total_count})"
    );
}
