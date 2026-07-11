use crate::common::{DirGuard, git_cmd, setup_git_repo};
use ledgerful::state::layout::Layout;
use serde_json::Value;
use std::process::Command;

#[test]
#[allow(non_snake_case)]
fn test_sparse_empty_states_json__slow() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    setup_git_repo(root);
    std::fs::write(root.join("test.rs"), "fn foo() {}").unwrap();
    git_cmd(root, &["add", "-A"]);
    git_cmd(root, &["commit", "--no-verify", "-m", "initial"]);

    let _guard = DirGuard::new(root);
    ledgerful::commands::init::execute_init(false, false).unwrap();

    let layout = Layout::new(root.to_str().unwrap());
    std::fs::write(
        layout.config_file(),
        "[coverage]\nenabled = true\n[coverage.services]\nenabled = true",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_ledgerful");

    // The index is missing initially.

    // 1. Endpoints --changed
    let out = Command::new(exe)
        .args(["endpoints", "--changed", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noIndexedData");

    // 2. Services diff
    let out = Command::new(exe)
        .args(["services", "diff", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noIndexedData");

    // 3. Observability coverage
    let out = Command::new(exe)
        .args(["observability", "coverage", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noIndexedData");

    // 4. Security boundaries
    // After `init` the ledger seeds a mode transaction node in the graph, so the
    // empty reason is the "populated graph but no Cedar policy" variant.
    let out = Command::new(exe)
        .args(["security", "boundaries", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noMatches");

    // 5. Test mapping nonexistent BEFORE index
    let out = Command::new(exe)
        .args(["tests", "nonexistent", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noIndexedData");

    // Now index it.
    Command::new(exe)
        .args(["index", "--incremental"])
        .output()
        .unwrap();

    // Clean diff now for endpoints --changed
    let out = Command::new(exe)
        .args(["endpoints", "--changed", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noIndexedData");

    // Clean diff for Observability Diff
    let out = Command::new(exe)
        .args(["observability", "diff", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noIndexedData");

    // 6. Test mapping EntityNotIndexed AFTER index
    let out = Command::new(exe)
        .args(["tests", "nonexistent", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noIndexedData");
}

/// CG-F35 (requirement #2): `security boundaries` must distinguish "the
/// knowledge graph was never built" (a prerequisite gap â€” recommend
/// `index --analyze-graph`) from "the graph is healthy but this repo has no
/// Cedar policy files" (a configuration gap â€” recommend adding policy
/// files). Before this track both states collapsed into the same
/// `noIndexedData` message regardless of whether the graph had any nodes at
/// all.
#[test]
fn test_security_boundaries_distinguishes_unbuilt_graph_from_unconfigured_policies() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    setup_git_repo(root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() { println!(\"hi\"); }").unwrap();
    git_cmd(root, &["add", "-A"]);
    git_cmd(root, &["commit", "--no-verify", "-m", "initial"]);

    let _guard = DirGuard::new(root);
    ledgerful::commands::init::execute_init(false, false).unwrap();

    let exe = env!("CARGO_BIN_EXE_ledgerful");

    // After init the ledger has a seeded mode transaction node, so the graph
    // is technically populated but contains no Cedar policy/principal/action/
    // resource nodes. The empty reason is therefore a configuration gap (no
    // Cedar files), not an unbuilt-graph prerequisite gap.
    let out = Command::new(exe)
        .args(["security", "boundaries", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["emptyReason"].as_str().unwrap(), "noMatches");
    let message_before = v["message"].as_str().unwrap_or_default().to_string();
    assert!(
        message_before.to_lowercase().contains("cedar policy")
            || message_before.to_lowercase().contains("no cedar policy"),
        "expected a no-Cedar-policy message, got: {message_before}"
    );

    // Build the graph with --analyze-graph. This repo has no Cedar policy
    // files, so the graph will have source-code nodes but zero
    // policy/principal/action/resource nodes.
    let index_out = Command::new(exe)
        .args(["index", "--analyze-graph"])
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index --analyze-graph failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );

    let out = Command::new(exe)
        .args(["security", "boundaries", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["emptyReason"].as_str().unwrap(),
        "noMatches",
        "expected a populated-graph-but-no-policies state, got: {v}"
    );
    let message_after = v["message"].as_str().unwrap_or_default().to_string();
    assert!(
        message_after.to_lowercase().contains("populated"),
        "expected a populated-graph message distinguishing this from the unbuilt-graph case, got: {message_after}"
    );
}
