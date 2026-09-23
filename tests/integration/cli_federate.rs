use crate::common::{DirGuard, git_add_and_commit, git_cmd, setup_git_repo};
use ledgerful::commands::federate::execute_federate_scan;
use ledgerful::commands::init::execute_init;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
#[allow(non_snake_case)]
fn test_federate_scan_no_remotes__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    // Stage and commit the .gitignore that init created, so scan has clean state
    git_add_and_commit(root, "after init");

    // Need a scan with impact first to produce the packet that federated_scan reads
    crate::common::execute_scan_impact_unbounded().unwrap();

    let result = execute_federate_scan();
    if let Err(ref e) = result {
        println!("execute_federate_scan error: {:?}", e);
    }
    // In a temp repo with no remotes, it should still succeed (no remotes to scan)
    assert!(result.is_ok());
}

/// CG-F35 (requirement #1, #6): `federate scan` uses the local cached impact
/// packet to drive cross-repo dependency discovery -- a more consequential
/// trust surface than a purely local query, since `federate status` and
/// sibling repos read the result. A stale local cache must be called out
/// clearly. Spawns the real binary (rather than calling
/// `execute_federate_scan` in-process) to confirm the warning actually
/// reaches process output rather than just being constructed and discarded.
#[test]
#[allow(non_snake_case)]
fn test_federate_scan_warns_on_stale_cached_impact_packet__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("a.rs"), "fn a() {}").unwrap();
    git_add_and_commit(root, "initial");

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    git_add_and_commit(root, "after init");

    // Record a packet via `scan --impact`, then advance HEAD past it so the
    // cached packet `federate scan` reads is stale.
    fs::write(root.join("a.rs"), "fn a() { /* v2 */ }").unwrap();
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
    // `scan --impact` may leave a pending ledger transaction, which this
    // repo's pre-commit hook blocks a plain `git commit` on; `--no-verify`
    // bypasses that hook the same way `cli_sparse_empty_states.rs` does.
    git_cmd(root, &["add", "-A"]);
    git_cmd(
        root,
        &[
            "commit",
            "--no-verify",
            "-m",
            "advance head past the cached packet",
        ],
    );

    let output = Command::new(ledgerful_bin)
        .args(["federate", "scan"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "federate scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_lowercase().contains("stale"),
        "expected a staleness warning in federate scan's progress output, got stdout: {stdout}"
    );
}

/// TA31 R1: a sibling whose schema.json has a data-quality problem (here,
/// an empty ledger `entity` -- the bridge use case) must still
/// be discovered by `federate scan` (not hard-skipped), with its
/// per-sibling warning printed to stdout so the user sees what needs
/// attention. Spawns the real binary so the assertion exercises the
/// actual CLI output path, not just the in-process return value.
#[test]
#[allow(non_snake_case)]
fn test_federate_scan_prints_warning_for_empty_entity_sibling__slow() {
    let workspace = tempdir().unwrap();
    let workspace_path = workspace.path();

    let repo1 = workspace_path.join("repo1");
    let sibling = workspace_path.join("example-sibling");
    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&sibling).unwrap();

    setup_git_repo(&repo1);
    setup_git_repo(&sibling);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Init the sibling and hand-write a schema.json with a data-quality
    // problem (empty ledger entity) rather than going through
    // `federate export`, so the test fixture is explicit about the
    // exact malformed shape being exercised.
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(&sibling)
        .output()
        .unwrap();
    let sibling_state_dir = sibling.join(".ledgerful").join("state");
    fs::create_dir_all(&sibling_state_dir).unwrap();
    let schema_json = serde_json::json!({
        "schema_version": "1.1",
        "repo_name": "example-sibling",
        "public_interfaces": [],
        "ledger": [
            {
                "tx_id": "tx-example-sibling-1",
                "category": "FEATURE",
                "entry_type": "IMPLEMENTATION",
                "entity": "",
                "change_type": "CREATE",
                "summary": "Entry with no entity recorded",
                "reason": "legacy export",
                "is_breaking": false,
                "committed_at": "2026-06-24T00:00:00Z",
                "author": ""
            }
        ]
    })
    .to_string();
    fs::write(sibling_state_dir.join("schema.json"), schema_json).unwrap();

    // Init repo1 and produce the local impact packet `federate scan` needs.
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(&repo1)
        .output()
        .unwrap();
    git_add_and_commit(&repo1, "after init");
    let scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(&repo1)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    let output = Command::new(ledgerful_bin)
        .args(["federate", "scan"])
        .current_dir(&repo1)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "federate scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("example-sibling"),
        "expected the empty-entity sibling 'example-sibling' to be discovered (not hard-skipped), \
         got stdout: {stdout}"
    );
    assert!(
        stdout.contains("WARN"),
        "expected a per-sibling WARN line for the empty entity, got stdout: {stdout}"
    );
}

/// TA31 R2: with `[federation] auto_sync_siblings = true` in the primary
/// repo's config, `federate scan` must auto-regenerate a sibling's missing
/// `schema.json` by shelling out to `ledgerful federate export` against the
/// sibling's own root. The sibling here is `init`-ed and has a populated
/// `ledger.db` (via `scan --impact`), but `federate export` is deliberately
/// never run on it, so `schema.json` does not exist before this test calls
/// `federate scan` from the primary repo. Spawns the real binary (not the
/// in-process function) so the assertion exercises the actual
/// `Command::new(current_exe).current_dir(sibling_root)` subprocess path,
/// not just a mocked function call.
#[test]
#[allow(non_snake_case)]
fn test_federate_scan_auto_syncs_missing_sibling_schema__slow() {
    let workspace = tempdir().unwrap();
    let workspace_path = workspace.path();

    let primary = workspace_path.join("primary-repo");
    let sibling = workspace_path.join("stale-sibling");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(&sibling).unwrap();

    setup_git_repo(&primary);
    setup_git_repo(&sibling);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Init + populate the sibling's ledger.db, but deliberately do NOT run
    // `federate export` -- no schema.json exists yet anywhere under
    // sibling/.ledgerful.
    let init_out = Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(&sibling)
        .output()
        .unwrap();
    assert!(
        init_out.status.success(),
        "sibling init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    fs::write(sibling.join("lib.rs"), "pub fn sibling_fn() {}").unwrap();
    git_add_and_commit(&sibling, "sibling initial commit");
    let sibling_scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(&sibling)
        .output()
        .unwrap();
    assert!(
        sibling_scan_out.status.success(),
        "sibling scan --impact failed: {}",
        String::from_utf8_lossy(&sibling_scan_out.stderr)
    );
    assert!(
        !sibling
            .join(".ledgerful")
            .join("state")
            .join("schema.json")
            .exists(),
        "test setup invariant violated: sibling must not have a schema.json before federate scan \
         auto-syncs it"
    );

    // Init the primary repo and opt into auto-sync via config.
    let primary_init_out = Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(&primary)
        .output()
        .unwrap();
    assert!(
        primary_init_out.status.success(),
        "primary init failed: {}",
        String::from_utf8_lossy(&primary_init_out.stderr)
    );
    git_add_and_commit(&primary, "after init");
    let primary_scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(&primary)
        .output()
        .unwrap();
    assert!(
        primary_scan_out.status.success(),
        "primary scan --impact failed: {}",
        String::from_utf8_lossy(&primary_scan_out.stderr)
    );

    let config_path = primary.join(".ledgerful").join("config.toml");
    fs::write(&config_path, "[federation]\nauto_sync_siblings = true\n").unwrap();

    let output = Command::new(ledgerful_bin)
        .args(["federate", "scan"])
        .current_dir(&primary)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "federate scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        sibling
            .join(".ledgerful")
            .join("state")
            .join("schema.json")
            .exists(),
        "expected federate scan (with auto_sync_siblings = true) to have generated the \
         sibling's schema.json"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("stale-sibling"),
        "expected the auto-synced sibling 'stale-sibling' to appear in federate scan's stdout, \
         got: {stdout}"
    );
}

fn federate_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ledgerful")
}

fn init_git_repo_ready_for_dirty_pub_fn(root: &std::path::Path) {
    setup_git_repo(root);
    fs::write(root.join("README.md"), "fixture\n").unwrap();
    git_add_and_commit(root, "initial");
    let init_out = Command::new(federate_bin())
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init_out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    git_add_and_commit(root, "after init");
}

fn write_dirty_pub_fn(root: &std::path::Path) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn hello() {}\n").unwrap();
}

#[test]
#[allow(non_snake_case)]
fn federate_export_json_preview_is_bounded_envelope__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_git_repo_ready_for_dirty_pub_fn(root);
    write_dirty_pub_fn(root);

    let index_out = Command::new(federate_bin())
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index --incremental failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );
    let scan_out = Command::new(federate_bin())
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    let schema = root.join(".ledgerful").join("state").join("schema.json");
    let before = schema
        .exists()
        .then(|| fs::metadata(&schema).unwrap().modified().unwrap());

    for args in [
        ["federate", "export", "--json"].as_slice(),
        ["federate", "export", "--dry-run", "--json"].as_slice(),
    ] {
        let output = Command::new(federate_bin())
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("Exporting"),
            "preview must not print Exporting: {stdout}"
        );
        let json_at = stdout.find('{').expect("json object");
        assert!(
            stdout[..json_at].chars().all(char::is_whitespace),
            "human text precedes JSON: {stdout}"
        );
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["kind"], "federateExportPreview");
        assert_eq!(v["dryRun"], true);
        assert_eq!(v["wireSchemaVersion"], "1.1");
        assert!(v["totalMatching"].as_u64().unwrap_or(0) >= 1);
        let line = v["interfaces"][0]["line"].as_i64();
        assert!(line.is_some_and(|n| n > 0), "expected line, got {v}");
    }

    let after = schema
        .exists()
        .then(|| fs::metadata(&schema).unwrap().modified().unwrap());
    assert_eq!(before, after, "preview must not write schema.json");
}

#[test]
#[allow(non_snake_case)]
fn federate_export_dry_run_human_has_no_json__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_git_repo_ready_for_dirty_pub_fn(root);
    write_dirty_pub_fn(root);
    let _ = Command::new(federate_bin())
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    let _ = Command::new(federate_bin())
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(federate_bin())
        .args(["federate", "export", "--dry-run"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.to_lowercase().contains("preview"));
    assert!(!stdout.contains("FEDERATED SCHEMA PREVIEW"));
    assert!(!stdout.contains('{'));
}

#[test]
#[allow(non_snake_case)]
fn federate_export_preview_conflicts_with_out__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let dry = Command::new(federate_bin())
        .args(["federate", "export", "--dry-run", "--out", "x.json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!dry.status.success());
    let err = String::from_utf8_lossy(&dry.stderr);
    assert!(
        err.contains("cannot be used") || err.to_lowercase().contains("conflict"),
        "expected clap conflict, got {err}"
    );
    let json = Command::new(federate_bin())
        .args(["federate", "export", "--json", "--out", "x.json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!json.status.success());
}

#[test]
#[allow(non_snake_case)]
fn federate_export_json_init_only_is_empty_envelope__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let init_out = Command::new(federate_bin())
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init_out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );

    let output = Command::new(federate_bin())
        .args(["federate", "export", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "init-only --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(v["kind"], "federateExportPreview");
    assert_eq!(v["totalMatching"], 0);
    assert_eq!(v["next"], "ledgerful scan --impact");
    let schema = root.join(".ledgerful").join("state").join("schema.json");
    assert!(
        !schema.exists(),
        "init-only preview must not write schema.json"
    );
}

#[test]
#[allow(non_snake_case)]
fn federate_export_next_scan_impact_repopulates_preview__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_git_repo_ready_for_dirty_pub_fn(root);

    let before = Command::new(federate_bin())
        .args(["federate", "export", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(before.status.success());
    let before_v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&before.stdout).trim()).expect("json");
    assert_eq!(before_v["totalMatching"], 0);
    assert_eq!(before_v["next"], "ledgerful scan --impact");

    write_dirty_pub_fn(root);
    let scan_out = Command::new(federate_bin())
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    let after = Command::new(federate_bin())
        .args(["federate", "export", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(after.status.success());
    let after_v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&after.stdout).trim()).expect("json");
    assert!(
        after_v["totalMatching"].as_u64().unwrap_or(0) > 0,
        "named next must populate snapshot symbols, got {after_v}"
    );
}
