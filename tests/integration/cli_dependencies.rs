use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::init::execute_init;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn dependencies_list_returns_package_list() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    // Now populate CozoDB with mock packages
    {
        let state_dir = root.join(".ledgerful").join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let cozo_path = state_dir.join("ledger.cozo");
        let cozo = ledgerful::state::storage_cozo::CozoStorage::new(cozo_path.as_path()).unwrap();

        cozo.run_script(
            "?[id, label, category, risk_score, metadata] <- [
                ['urn:pkg1', 'package-dup@1.0.0', 'package', 0.0, {\"name\": \"package-dup\", \"version\": \"1.0.0\", \"ecosystem\": \"rust/cargo\"}],
                ['urn:pkg2', 'package-dup@1.0.0', 'package', 0.0, {\"name\": \"package-dup\", \"version\": \"1.0.0\", \"ecosystem\": \"rust/cargo\", \"source\": \"https://github.com/dup/repo\"}],
                ['urn:pkg3', 'package-dup@1.0.0', 'package', 0.0, {\"name\": \"package-dup\", \"version\": \"1.0.0\", \"ecosystem\": \"rust/cargo\", \"source\": \"https://github.com/dup/repo\"}],
                ['urn:pkg4', 'local-pkg@2.0.0', 'package', 0.0, {\"name\": \"local-pkg\", \"version\": \"2.0.0\", \"ecosystem\": \"rust/cargo\"}],
                ['urn:pkg5', 'external-pkg@3.1.0', 'package', 0.0, {\"name\": \"external-pkg\", \"version\": \"3.1.0\", \"ecosystem\": \"rust/cargo\", \"source\": \"https://github.com/ext/repo\"}],
                ['urn:pkg6', 'no-meta-name@4.0.0', 'package', 0.0, {\"version\": \"4.0.0\", \"ecosystem\": \"rust/cargo\"}],
                ['urn:conflict1', 'conflict-pkg@1.0.0', 'package', 0.0, {\"name\": \"conflict-pkg\", \"version\": \"1.0.0\", \"ecosystem\": \"rust/cargo\", \"source\": \"https://github.com/forkA/repo\"}],
                ['urn:conflict2', 'conflict-pkg@1.0.0', 'package', 0.0, {\"name\": \"conflict-pkg\", \"version\": \"1.0.0\", \"ecosystem\": \"rust/cargo\", \"source\": \"https://github.com/forkB/repo\"}]
            ] :put node",
        )
        .unwrap();
    } // cozo dropped, database lock released

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // 1. JSON output check
    let output_json = Command::new(ledgerful_bin)
        .args(["dependencies", "list", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(output_json.status.success());
    let stdout_json = String::from_utf8_lossy(&output_json.stdout);

    // Parse the JSON output to verify details
    let json_val: serde_json::Value = serde_json::from_str(&stdout_json).unwrap();
    let arr = json_val.as_array().expect("Expected a JSON array");

    // Verify literal duplicates collapse, metadata-incomplete entries merge with
    // their sourced counterpart, and genuinely conflicting sources are kept distinct.
    //
    // urn:pkg1 (package-dup, no source) and urn:pkg2/urn:pkg3 (package-dup, same
    // source "https://github.com/dup/repo") share (name, version, ecosystem) and at
    // most one distinct non-null source among them, so all three merge into a single
    // row: a missing source is metadata incompleteness, not a separate identity.
    //
    // urn:conflict1/urn:conflict2 (conflict-pkg) share (name, version, ecosystem) but
    // carry two different non-null sources (forkA vs forkB) -- a genuine identity
    // collision -- so they must remain as two separate rows. This is the regression
    // guard for the High-severity identity-loss bug the Codex review flagged.
    //
    // Total rows: local-pkg, no-meta-name (local); package-dup [merged], external-pkg,
    // conflict-pkg [forkA], conflict-pkg [forkB] (external) = 6.
    assert_eq!(arr.len(), 6, "Expected exactly 6 packages, got: {:?}", arr);

    // Assert sorting order: local dependencies first (sorted by name), then external
    // (sorted by name). Local group: local-pkg, no-meta-name.
    assert_eq!(arr[0]["name"].as_str().unwrap(), "local-pkg");
    assert_eq!(arr[0]["version"].as_str().unwrap(), "2.0.0");
    assert!(arr[0]["is_local"].as_bool().unwrap());
    assert!(arr[0]["source"].is_null());

    assert_eq!(arr[1]["name"].as_str().unwrap(), "no-meta-name");
    assert_eq!(arr[1]["version"].as_str().unwrap(), "4.0.0");
    assert!(arr[1]["is_local"].as_bool().unwrap());
    assert!(arr[1]["source"].is_null());

    // External group (sorted by name): conflict-pkg x2, external-pkg, package-dup.
    // The two conflict-pkg rows share name/version/ecosystem, so their relative order
    // is not guaranteed by the sort key -- collect both and assert on the set of
    // sources rather than a fixed index.
    let external_entries: Vec<&serde_json::Value> = arr[2..].iter().collect();
    assert_eq!(external_entries.len(), 4);
    for entry in &external_entries {
        assert!(!entry["is_local"].as_bool().unwrap());
    }

    let conflict_entries: Vec<&&serde_json::Value> = external_entries
        .iter()
        .filter(|e| e["name"].as_str().unwrap() == "conflict-pkg")
        .collect();
    assert_eq!(
        conflict_entries.len(),
        2,
        "expected conflict-pkg to remain as two distinct rows, got: {:?}",
        conflict_entries
    );
    let mut conflict_sources: Vec<&str> = conflict_entries
        .iter()
        .map(|e| e["source"].as_str().unwrap())
        .collect();
    conflict_sources.sort_unstable();
    assert_eq!(
        conflict_sources,
        vec![
            "https://github.com/forkA/repo",
            "https://github.com/forkB/repo"
        ]
    );
    for entry in &conflict_entries {
        assert_eq!(entry["version"].as_str().unwrap(), "1.0.0");
    }

    let external_pkg_entry = external_entries
        .iter()
        .find(|e| e["name"].as_str().unwrap() == "external-pkg")
        .expect("external-pkg row present");
    assert_eq!(external_pkg_entry["version"].as_str().unwrap(), "3.1.0");
    assert_eq!(
        external_pkg_entry["source"].as_str().unwrap(),
        "https://github.com/ext/repo"
    );

    // urn:pkg1/urn:pkg2/urn:pkg3 merged: package-dup must appear exactly once, sourced
    // from the only distinct non-null source among the three ("https://github.com/dup/repo"),
    // and must no longer be local now that a source is known.
    let package_dup_entries: Vec<&&serde_json::Value> = external_entries
        .iter()
        .filter(|e| e["name"].as_str().unwrap() == "package-dup")
        .collect();
    assert_eq!(
        package_dup_entries.len(),
        1,
        "expected package-dup (local + sourced duplicates) to merge into one row, got: {:?}",
        package_dup_entries
    );
    assert_eq!(package_dup_entries[0]["version"].as_str().unwrap(), "1.0.0");
    assert_eq!(
        package_dup_entries[0]["source"].as_str().unwrap(),
        "https://github.com/dup/repo"
    );

    // 2. Default/human output (non-verbose) check
    let output_default = Command::new(ledgerful_bin)
        .args(["dependencies", "list"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(output_default.status.success());
    let stdout_default = String::from_utf8_lossy(&output_default.stdout);

    // Default output should contain local dependencies table and external dependencies count.
    // Local group has 2 entries (local-pkg, no-meta-name); external group has 4 entries
    // (package-dup [merged], external-pkg, conflict-pkg [forkA], conflict-pkg [forkB]).
    assert!(stdout_default.contains("Local Dependencies"));
    assert!(stdout_default.contains("local-pkg"));
    assert!(stdout_default.contains("no-meta-name"));
    assert!(!stdout_default.contains("package-dup"));
    assert!(stdout_default.contains("External dependencies count: 4"));
    // Default output should NOT contain the external dependencies table or external-pkg details
    assert!(!stdout_default.contains("External Dependencies"));
    assert!(!stdout_default.contains("external-pkg"));

    // 3. Verbose output check
    let output_verbose = Command::new(ledgerful_bin)
        .args(["dependencies", "list", "--verbose"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(output_verbose.status.success());
    let stdout_verbose = String::from_utf8_lossy(&output_verbose.stdout);

    // Verbose output should show both tables
    assert!(stdout_verbose.contains("Local Dependencies"));
    assert!(stdout_verbose.contains("local-pkg"));
    assert!(stdout_verbose.contains("no-meta-name"));
    assert!(stdout_verbose.contains("External Dependencies"));
    assert!(stdout_verbose.contains("external-pkg"));
    assert!(stdout_verbose.contains("package-dup"));
    assert!(stdout_verbose.contains("conflict-pkg"));
    assert!(stdout_verbose.contains("https://github.com/ext/repo"));
    assert!(stdout_verbose.contains("https://github.com/dup/repo"));
    assert!(stdout_verbose.contains("https://github.com/forkA/repo"));
    assert!(stdout_verbose.contains("https://github.com/forkB/repo"));
}
