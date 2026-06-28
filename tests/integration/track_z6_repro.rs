use crate::common::setup_git_repo;
use camino::Utf8Path;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_ledger_graph_edges_ingestion() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::write(root.join("test.rs"), "content").unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Init and start tx
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .args([
            "ledger",
            "start",
            "test.rs",
            "--category",
            "BUGFIX",
            "--message",
            "fix bug",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    // Commit tx
    let commit_out = Command::new(ledgerful_bin)
        .args([
            "ledger",
            "commit",
            "--summary",
            "fixed it",
            "--reason",
            "because",
        ])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&commit_out.stdout);
    eprintln!("Commit stdout: {}", stdout);

    // Check CozoDB
    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let cozo = storage.cozo.as_ref().expect("CozoDB should be initialized");

    // 1. Verify transaction node
    let node_res = cozo.query_nodes_by_category("ledger_transaction").unwrap();
    assert!(
        !node_res.rows.is_empty(),
        "CozoDB should have transaction nodes"
    );

    // 2. Verify affects edge
    let edge_res = cozo
        .run_script_with_params(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: $rel}",
            {
                let mut p = std::collections::BTreeMap::new();
                p.insert("rel".into(), cozo::DataValue::Str("affects".into()));
                p
            },
            cozo::ScriptMutability::Immutable,
        )
        .unwrap();
    assert!(
        !edge_res.rows.is_empty(),
        "CozoDB should have affects edges. Rows: {:?}",
        edge_res.rows
    );

    let found_target = edge_res.rows.iter().any(|row| {
        row.get(1)
            .map(|v| match v {
                cozo::DataValue::Str(s) => s.as_str() == "urn:ledgerful:file:test.rs",
                _ => false,
            })
            .unwrap_or(false)
    });
    assert!(found_target, "CozoDB should have edge to test.rs");
}
