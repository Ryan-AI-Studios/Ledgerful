use crate::common::setup_git_repo;
use camino::Utf8Path;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_test_mapping_ingestion() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }",
    )
    .unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    // Use 'use' to trigger IMPORT mapping (confidence 1.0)
    fs::write(
        root.join("tests/test_add.rs"),
        "use crate::add;\n#[test]\nfn test_add() { assert_eq!(add(2,2), 4); }",
    )
    .unwrap();

    // Commit so indexer sees them correctly
    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(root)
        .output()
        .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Init and Index (to get symbols and test mappings)
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .arg("index")
        .current_dir(root)
        .output()
        .unwrap();

    // Now run index --analyze-graph
    Command::new(ledgerful_bin)
        .args(["index", "--analyze-graph"])
        .current_dir(root)
        .output()
        .unwrap();

    // Check CozoDB
    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let cozo = storage.cozo.as_ref().expect("CozoDB should be initialized");

    // 1. Verify test node exists and carries metadata
    let res = cozo.query_nodes_by_category("test").unwrap();
    assert!(!res.rows.is_empty(), "CozoDB should have test nodes");

    let found_test_node = res.rows.iter().any(|row| {
        row.get(1)
            .map(|v| match v {
                cozo::DataValue::Str(s) => s.as_str() == "test_add",
                _ => false,
            })
            .unwrap_or(false)
    });
    assert!(found_test_node, "Should have a test node for test_add");

    // 2. Verify specific validates edge from test_add -> add with confidence 1.0
    let edge_res = cozo
        .run_script_with_params(
            "?[src, tgt, rel, conf] := \
             *node{id: src, category: 'test'}, \
             *node{id: tgt, category: 'symbol'}, \
             *edge{source: src, target: tgt, relation: rel, confidence: conf}, \
             rel = $rel",
            {
                let mut p = std::collections::BTreeMap::new();
                p.insert("rel".into(), cozo::DataValue::Str("validates".into()));
                p
            },
            cozo::ScriptMutability::Immutable,
        )
        .unwrap();
    let found_specific_edge = edge_res.rows.iter().any(|row| {
        let src_label = match row.first() {
            Some(cozo::DataValue::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let tgt_label = match row.get(1) {
            Some(cozo::DataValue::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let rel = match row.get(2) {
            Some(cozo::DataValue::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let conf = match row.get(3) {
            Some(cozo::DataValue::Num(cozo::Num::Float(f))) => *f,
            Some(cozo::DataValue::Num(cozo::Num::Int(i))) => *i as f64,
            _ => 0.0,
        };
        src_label.ends_with(":test_add")
            && tgt_label.ends_with(":add")
            && rel == "validates"
            && (conf - 1.0).abs() < 0.01
    });
    assert!(
        found_specific_edge,
        "CozoDB should have a validates edge from test_add to add with confidence 1.0. Rows: {:?}",
        edge_res.rows
    );
}
