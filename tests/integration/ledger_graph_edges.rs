use ledgerful::commands::ledger::{
    LedgerCommitGitOptions, execute_ledger_adopt, execute_ledger_commit,
};
use ledgerful::config::model::Config;
use ledgerful::ledger::*;
use ledgerful::platform::urn::build_urn;
use ledgerful::state::graph_kinds::NodeKind;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

#[test]
fn test_commit_writes_kg_edges() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    setup_git_repo(&root);

    // Create a dummy file in the repo
    let file_path = "src/api.rs";
    let full_file_path = root.join(file_path);
    fs::create_dir_all(full_file_path.parent().unwrap()).unwrap();
    fs::write(&full_file_path, "fn dummy() {}").unwrap();

    let _guard = DirGuard::new(&root);

    // Initialize the directory layout and database
    let db_path = root.join(".ledgerful/state/ledger.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let mut storage = StorageManager::init(&db_path).unwrap();
    let cozo = storage
        .cozo
        .as_ref()
        .expect("Cozo storage should be available");

    // Pre-create node for the file in CozoDB (simulating indexer output)
    let file_urn = build_urn(NodeKind::File, file_path);
    let mut params = std::collections::BTreeMap::new();
    params.insert("id".into(), cozo::DataValue::Str(file_urn.clone().into()));
    let node_query = "?[id, label, category, risk_score, metadata] <- [[$id, 'api.rs', 'file', 0.0, {}]] :put node";
    cozo.run_script_with_params(node_query, params, cozo::ScriptMutability::Mutable)
        .unwrap();

    // Start a transaction for src/api.rs
    let mut manager = TransactionManager::new(&mut storage, root.clone(), Config::default());

    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: file_path.to_string(),
            ..Default::default()
        })
        .unwrap();

    // We must drop manager to release database connection lock for execute_ledger_commit
    drop(manager);
    drop(storage);

    // Run execute_ledger_commit
    let result = execute_ledger_commit(
        Some(tx_id.clone()),
        "Feature change",
        "Adds new API",
        false,
        false, // force
        LedgerCommitGitOptions::default(),
    );
    assert!(result.is_ok(), "Commit failed: {:?}", result.err());

    // Open read-only to verify Cozo DB state
    let storage_read =
        StorageManager::open_read_only(camino::Utf8Path::from_path(&root).unwrap()).unwrap();
    let cozo_read = storage_read
        .cozo
        .as_ref()
        .expect("Cozo storage should be available");

    let tx_urn = build_urn(NodeKind::LedgerTransaction, &tx_id);

    // Verify LedgerTransaction node exists in CozoDB
    let node_res = cozo_read
        .run_script_with_params(
            "?[id, category] := *node{id, category}, id = $id",
            {
                let mut p = std::collections::BTreeMap::new();
                p.insert("id".into(), cozo::DataValue::Str(tx_urn.clone().into()));
                p
            },
            cozo::ScriptMutability::Immutable,
        )
        .unwrap();
    assert_eq!(
        node_res.rows.len(),
        1,
        "LedgerTransaction node should exist in CozoDB"
    );
    assert_eq!(
        node_res.rows[0][1],
        cozo::DataValue::Str("ledger_transaction".into())
    );

    // Verify Edge exists in CozoDB: source=tx_urn, target=file_urn, relation=affects
    let edge_res = cozo_read.query_edges_by_source(&tx_urn, "affects").unwrap();
    assert!(
        !edge_res.rows.is_empty(),
        "Edges from transaction URN should exist in CozoDB"
    );

    let mut found = false;
    for row in edge_res.rows {
        if let (
            Some(cozo::DataValue::Str(src)),
            Some(cozo::DataValue::Str(tgt)),
            Some(cozo::DataValue::Str(rel)),
        ) = (row.first(), row.get(1), row.get(2))
            && src == &tx_urn
            && tgt == &file_urn
            && rel == "affects"
        {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "Should find the transaction -> file 'affects' edge in CozoDB"
    );
}

#[test]
fn test_urn_construction_normalization() {
    let file_path = "src\\commands\\ledger.rs";
    let expected_urn = "urn:ledgerful:file:src/commands/ledger.rs";
    let constructed_urn = build_urn(NodeKind::File, file_path);
    assert_eq!(constructed_urn, expected_urn);
}

/// Validates Codex finding #1: `execute_ledger_adopt` must write KG `Affects` edges
/// pointing to the **real adopted file paths** — not the synthetic `"drift_adoption"` entity.
#[test]
fn test_adopt_writes_kg_edges_with_real_files() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    setup_git_repo(&root);

    let drift_file = "src/drifted.rs";
    let full_drift = root.join(drift_file);
    fs::create_dir_all(full_drift.parent().unwrap()).unwrap();
    fs::write(&full_drift, "// drifted file").unwrap();

    let _guard = DirGuard::new(&root);

    let db_path = root.join(".ledgerful/state/ledger.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let storage = StorageManager::init(&db_path).unwrap();

    // Manually insert a drift (UNAUDITED) transaction for drift_file
    {
        use ledgerful::ledger::db::LedgerDb;
        let drift_tx = Transaction {
            tx_id: uuid::Uuid::new_v4().to_string(),
            operation_id: None,
            status: "UNAUDITED".to_string(),
            category: Category::Chore,
            entity: drift_file.to_string(),
            entity_normalized: drift_file.to_string(),
            planned_action: None,
            session_id: "test-session".to_string(),
            source: "WATCHER".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            resolved_at: None,
            detected_at: None,
            drift_count: 1,
            first_seen_at: None,
            last_seen_at: None,
            issue_ref: None,
            snapshot_id: None,
        };
        let db = LedgerDb::new(storage.get_connection());
        db.insert_transaction(&drift_tx).unwrap();
    }

    drop(storage);

    // Run execute_ledger_adopt with --all flag
    let result = execute_ledger_adopt(None, true, "CHORE", "Adopt drifted files", "CI cleanup");
    assert!(result.is_ok(), "adopt failed: {:?}", result.err());

    // Open read-only and verify KG edges reference the real file, not "drift_adoption"
    let storage_read =
        StorageManager::open_read_only(camino::Utf8Path::from_path(&root).unwrap()).unwrap();
    let cozo_read = storage_read
        .cozo
        .as_ref()
        .expect("Cozo storage should be available");

    // There must be at least one LedgerTransaction node
    let nodes = cozo_read
        .run_script("?[id, category] := *node{id, category}, category = 'ledger_transaction'")
        .unwrap();
    assert!(
        !nodes.rows.is_empty(),
        "A LedgerTransaction node should have been written by adopt"
    );

    // There must be at least one edge whose target URN contains the real file path
    let file_urn = build_urn(NodeKind::File, drift_file);
    let edges = cozo_read
        .run_script_with_params(
            "?[source, target, relation] := *edge{source, target, relation}, target = $tgt",
            {
                let mut p = std::collections::BTreeMap::new();
                p.insert("tgt".into(), cozo::DataValue::Str(file_urn.clone().into()));
                p
            },
            cozo::ScriptMutability::Immutable,
        )
        .unwrap();
    assert!(
        !edges.rows.is_empty(),
        "Expected KG Affects edge targeting real file URN '{}', but none found. \
         The adopt command must NOT use the synthetic 'drift_adoption' entity.",
        file_urn
    );
}
