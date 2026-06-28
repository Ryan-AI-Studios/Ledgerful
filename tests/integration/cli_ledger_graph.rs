use crate::common::{DirGuard, setup_git_repo};
use ledgerful::commands::ledger::{LedgerCommitGitOptions, execute_ledger_commit};
use ledgerful::config::model::Config;
use ledgerful::ledger::{Category, TransactionManager, TransactionRequest};
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn ledger_graph_fallback_returns_empty_graph() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    setup_git_repo(&root);

    let file_path = "src/api.rs";
    let full_file_path = root.join(file_path);
    fs::create_dir_all(full_file_path.parent().unwrap()).unwrap();
    fs::write(&full_file_path, "fn dummy() {}").unwrap();

    let _guard = DirGuard::new(&root);

    let db_path = root.join(".ledgerful/state/ledger.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let mut storage = StorageManager::init(&db_path).unwrap();

    // Start a transaction for config (not a real file) with a ticket ref
    let mut manager = TransactionManager::new(&mut storage, root.clone(), Config::default());

    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "config".to_string(),
            issue_ref: Some("GH-123".to_string()),
            ..Default::default()
        })
        .unwrap();

    drop(manager);
    drop(storage);

    // Run CLI command to capture output and verify JSON fallback structure
    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["ledger", "graph", &tx_id, "--json"])
        .current_dir(&root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    // Verify fallback exact and derived lists are empty, but heuristic list is populated
    assert!(json["exact"].as_array().unwrap().is_empty());
    assert!(json["derived"].as_array().unwrap().is_empty());

    let heuristic_list = json["heuristic"].as_array().unwrap();
    assert!(
        !heuristic_list.is_empty(),
        "Heuristic fallback list should not be empty"
    );
    let labels: Vec<String> = heuristic_list
        .iter()
        .map(|v| v["label"].as_str().unwrap().to_string())
        .collect();
    assert!(labels.contains(&"GH-123".to_string()));
}

#[test]
fn ledger_graph_rich_returns_populated_graph() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    setup_git_repo(&root);

    let file_path = "src/api.rs";
    let full_file_path = root.join(file_path);
    fs::create_dir_all(full_file_path.parent().unwrap()).unwrap();
    fs::write(&full_file_path, "pub fn dummy_fn() {}").unwrap();

    let _guard = DirGuard::new(&root);

    let db_path = root.join(".ledgerful/state/ledger.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let mut storage = StorageManager::init(&db_path).unwrap();

    // Start a transaction for src/api.rs
    let mut manager = TransactionManager::new(&mut storage, root.clone(), Config::default());

    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: file_path.to_string(),
            ..Default::default()
        })
        .unwrap();

    drop(manager);
    drop(storage);

    // Commit transaction
    execute_ledger_commit(
        Some(tx_id.clone()),
        "Rich graph change",
        "Adds rich API structure",
        false,
        false,
        LedgerCommitGitOptions::default(),
    )
    .unwrap();

    // Populate SQLite provenance and CozoDB manually
    let storage = StorageManager::init(&db_path).unwrap();
    let conn = storage.get_connection();

    // Create a mock snapshot and link it to our transaction
    conn.execute(
        "INSERT INTO snapshots (timestamp, is_clean, packet_json) VALUES (datetime('now'), 1, '{}')",
        [],
    )
    .unwrap();
    let snapshot_id = conn.last_insert_rowid();

    conn.execute(
        "UPDATE transactions SET snapshot_id = ?1 WHERE tx_id = ?2",
        rusqlite::params![snapshot_id, &tx_id],
    )
    .unwrap();

    // 1. changed_files
    conn.execute(
        "INSERT INTO changed_files (snapshot_id, path, status, is_staged) VALUES (?1, ?2, 'MODIFIED', 1)",
        rusqlite::params![snapshot_id, "src/api.rs"],
    ).unwrap();

    // 1.5 transaction_links
    conn.execute(
        "INSERT INTO transaction_links (tx_id, entity_type, entity_name, entity_normalized, linked_at) VALUES (?1, 'FILE', 'src/linked_file.rs', 'src/linked_file.rs', datetime('now'))",
        rusqlite::params![tx_id],
    ).unwrap();

    // 2. token_provenance
    conn.execute(
        "INSERT INTO token_provenance (tx_id, entity, entity_normalized, symbol_name, symbol_type, action) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![tx_id, "src/api.rs", "src/api.rs", "dummy_fn", "Function", "MODIFIED"],
    ).unwrap();

    // 2.5 Historical token provenance for missing_fn and missing_file
    conn.execute(
        "INSERT INTO token_provenance (tx_id, entity, entity_normalized, symbol_name, symbol_type, action) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![tx_id, "src/missing.rs", "src/missing.rs", "missing_fn", "Function", "MODIFIED"],
    ).unwrap();

    // 3. CozoDB
    let cozo = storage.cozo.as_ref().unwrap();

    // project_symbol columns: id, file_path, qualified_name, symbol_name, symbol_kind, is_public, line_start, line_end
    cozo.run_script(
        "?[id, file_path, qualified_name, symbol_name, symbol_kind, is_public, line_start, line_end] <- [ \
            [1, 'src/api.rs', 'crate::api::dummy_fn', 'dummy_fn', 'Function', true, 1, 5] \
         ] :put project_symbol"
    ).unwrap();

    // node columns: id, label, category, risk_score, metadata
    cozo.run_script(&format!(
        "?[id, label, category, risk_score, metadata] <- [ \
            ['urn:ledgerful:symbol:crate::api::dummy_fn', 'dummy_fn', 'symbol', 0.0, '{{}}'], \
            ['urn:ledgerful:symbol:crate::api::caller_fn', 'caller_fn', 'symbol', 0.0, '{{}}'], \
            ['urn:ledgerful:symbol:crate::api::neighbor_fn', 'neighbor_fn', 'symbol', 0.0, '{{}}'], \
            ['urn:ledgerful:file:src/api.rs', 'src/api.rs', 'file', 0.0, '{{}}'], \
            ['urn:ledgerful:ledger_transaction:historical_tx_1', 'historical_tx_1', 'ledger_transaction', 0.0, '{{}}'], \
            ['urn:ledgerful:adr:adr_1', 'adr_1', 'adr', 0.0, '{{}}'], \
            ['urn:ledgerful:file:src/kg_file.rs', 'src/kg_file.rs', 'file', 0.0, '{{}}'], \
            ['urn:ledgerful:ledger_transaction:{}', '{}', 'ledger_transaction', 0.0, '{{}}'] \
         ] :put node",
         tx_id, tx_id
    )).unwrap();

    // edge columns: source, target, relation, confidence, provenance_id
    cozo.run_script(&format!(
        "?[source, target, relation, confidence, provenance_id] <- [ \
            ['urn:ledgerful:symbol:crate::api::dummy_fn', 'urn:ledgerful:symbol:crate::api::caller_fn', 'calls', 1.0, ''], \
            ['urn:ledgerful:symbol:crate::api::caller_fn', 'urn:ledgerful:symbol:crate::api::neighbor_fn', 'calls', 1.0, ''], \
            ['urn:ledgerful:ledger_transaction:historical_tx_1', 'urn:ledgerful:symbol:crate::api::caller_fn', 'affects', 1.0, ''], \
            ['urn:ledgerful:symbol:crate::api::caller_fn', 'urn:ledgerful:adr:adr_1', 'governs', 1.0, ''], \
            ['urn:ledgerful:ledger_transaction:{}', 'urn:ledgerful:file:src/kg_file.rs', 'affects', 1.0, ''] \
         ] :put edge",
         tx_id
    )).unwrap();

    storage.shutdown().unwrap();

    // Execute ledgerful binary and capture JSON output
    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["ledger", "graph", &tx_id, "--json"])
        .current_dir(&root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI JSON command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Rich graph JSON: {}", stdout);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    // Assert exact nodes: src/api.rs, dummy_fn, [HISTORICAL] missing_fn, [HISTORICAL] src/missing.rs, [HISTORICAL] src/linked_file.rs, [HISTORICAL] src/kg_file.rs
    let exact_list = json["exact"].as_array().unwrap();
    let exact_labels: Vec<String> = exact_list
        .iter()
        .map(|v| v["label"].as_str().unwrap().to_string())
        .collect();

    assert!(exact_labels.contains(&"src/api.rs".to_string()));
    assert!(exact_labels.contains(&"dummy_fn".to_string()));
    assert!(exact_labels.contains(&"[HISTORICAL] missing_fn".to_string()));
    assert!(exact_labels.contains(&"[HISTORICAL] src/missing.rs".to_string()));
    assert!(exact_labels.contains(&"[HISTORICAL] src/linked_file.rs".to_string()));
    assert!(exact_labels.contains(&"[HISTORICAL] src/kg_file.rs".to_string()));

    // Verify sorting order of exact relations (by entity_id)
    let exact_ids: Vec<String> = exact_list
        .iter()
        .map(|v| v["entity_id"].as_str().unwrap().to_string())
        .collect();
    let mut sorted_exact_ids = exact_ids.clone();
    sorted_exact_ids.sort();
    assert_eq!(
        exact_ids, sorted_exact_ids,
        "Exact relations must be sorted by entity_id"
    );

    // Assert derived nodes (BFS traversal)
    let derived_list = json["derived"].as_array().unwrap();
    let derived_labels: Vec<String> = derived_list
        .iter()
        .map(|v| v["label"].as_str().unwrap().to_string())
        .collect();

    assert!(derived_labels.contains(&"caller_fn".to_string()));
    assert!(derived_labels.contains(&"neighbor_fn".to_string()));

    // Verify sorting order of derived relations (by entity_id)
    let derived_ids: Vec<String> = derived_list
        .iter()
        .map(|v| v["entity_id"].as_str().unwrap().to_string())
        .collect();
    let mut sorted_derived_ids = derived_ids.clone();
    sorted_derived_ids.sort();
    assert_eq!(
        derived_ids, sorted_derived_ids,
        "Derived relations must be sorted by entity_id"
    );

    // Ensure transaction/ADR nodes were excluded from derived relations
    assert!(!derived_labels.contains(&"historical_tx_1".to_string()));
    assert!(!derived_labels.contains(&"adr_1".to_string()));

    // Assert attribution source in exact relationships
    for node in exact_list {
        let label = node["label"].as_str().unwrap();
        let src = node["attribution_source"].as_str().unwrap();
        if label == "src/api.rs" {
            assert_eq!(
                src, "token_provenance",
                "token provenance is the highest-priority exact source per CG-F28 requirement 1 and must win deterministically when multiple exact sources reference the same file"
            );
        } else if label == "dummy_fn" {
            assert_eq!(src, "token_provenance");
        } else if label == "[HISTORICAL] src/kg_file.rs" {
            assert_eq!(src, "knowledge_graph");
        }
    }

    // Assert human-readable rendering (non-JSON mode)
    let output_human = Command::new(ledgerful_bin)
        .args(["ledger", "graph", &tx_id])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output_human.status.success(),
        "CLI human command failed: {:?}",
        String::from_utf8_lossy(&output_human.stderr)
    );
    let stdout_human = String::from_utf8_lossy(&output_human.stdout);

    assert!(stdout_human.contains("Exact Relations"));
    assert!(stdout_human.contains("Derived Relations"));
    assert!(stdout_human.contains("src/api.rs"));
    assert!(stdout_human.contains("dummy_fn"));
}

#[test]
fn test_ledger_graph_max_nodes_cap_is_deterministic() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    setup_git_repo(&root);

    let file_path = "src/api.rs";
    let full_file_path = root.join(file_path);
    fs::create_dir_all(full_file_path.parent().unwrap()).unwrap();
    fs::write(&full_file_path, "pub fn dummy_fn() {}").unwrap();

    let _guard = DirGuard::new(&root);

    let db_path = root.join(".ledgerful/state/ledger.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let mut storage = StorageManager::init(&db_path).unwrap();

    // Start a transaction for src/api.rs (our single exact entry point)
    let mut manager = TransactionManager::new(&mut storage, root.clone(), Config::default());

    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: file_path.to_string(),
            ..Default::default()
        })
        .unwrap();

    drop(manager);
    drop(storage);

    // Commit transaction
    execute_ledger_commit(
        Some(tx_id.clone()),
        "Large neighborhood change",
        "Exercises the max_nodes cap during BFS traversal",
        false,
        false,
        LedgerCommitGitOptions::default(),
    )
    .unwrap();

    // Populate SQLite provenance (one exact entry point) and a large CozoDB neighborhood
    let storage = StorageManager::init(&db_path).unwrap();
    let conn = storage.get_connection();

    conn.execute(
        "INSERT INTO snapshots (timestamp, is_clean, packet_json) VALUES (datetime('now'), 1, '{}')",
        [],
    )
    .unwrap();
    let snapshot_id = conn.last_insert_rowid();

    conn.execute(
        "UPDATE transactions SET snapshot_id = ?1 WHERE tx_id = ?2",
        rusqlite::params![snapshot_id, &tx_id],
    )
    .unwrap();

    // Single exact entry point via token_provenance
    conn.execute(
        "INSERT INTO token_provenance (tx_id, entity, entity_normalized, symbol_name, symbol_type, action) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![tx_id, "src/api.rs", "src/api.rs", "dummy_fn", "Function", "MODIFIED"],
    ).unwrap();

    let cozo = storage.cozo.as_ref().unwrap();

    cozo.run_script(
        "?[id, file_path, qualified_name, symbol_name, symbol_kind, is_public, line_start, line_end] <- [ \
            [1, 'src/api.rs', 'crate::api::dummy_fn', 'dummy_fn', 'Function', true, 1, 5] \
         ] :put project_symbol",
    )
    .unwrap();

    // Build a hub node connected to >150 distinct leaf nodes, reachable from
    // src/api.rs (the entry point) within depth 2: api.rs -> hub -> leaf_i.
    const LEAF_COUNT: usize = 160;
    let hub_urn = "urn:ledgerful:symbol:crate::hub::hub_fn".to_string();
    let api_urn = "urn:ledgerful:file:src/api.rs".to_string();

    let mut node_rows = String::new();
    node_rows.push_str(&format!(
        "['{}', 'src/api.rs', 'file', 0.0, '{{}}'],",
        api_urn
    ));
    node_rows.push_str(&format!(
        "['{}', 'hub_fn', 'symbol', 0.0, '{{}}'],",
        hub_urn
    ));
    let mut edge_rows = String::new();
    edge_rows.push_str(&format!(
        "['{}', '{}', 'affects', 1.0, ''],",
        api_urn, hub_urn
    ));

    let mut leaf_urns: Vec<String> = Vec::with_capacity(LEAF_COUNT);
    for i in 0..LEAF_COUNT {
        let leaf_urn = format!("urn:ledgerful:symbol:crate::hub::leaf_fn_{}", i);
        node_rows.push_str(&format!(
            "['{}', 'leaf_fn_{}', 'symbol', 0.0, '{{}}'],",
            leaf_urn, i
        ));
        edge_rows.push_str(&format!(
            "['{}', '{}', 'calls', 1.0, ''],",
            hub_urn, leaf_urn
        ));
        leaf_urns.push(leaf_urn);
    }
    // Trim trailing commas
    let node_rows = node_rows.trim_end_matches(',');
    let edge_rows = edge_rows.trim_end_matches(',');

    cozo.run_script(&format!(
        "?[id, label, category, risk_score, metadata] <- [ {} ] :put node",
        node_rows
    ))
    .unwrap();

    cozo.run_script(&format!(
        "?[source, target, relation, confidence, provenance_id] <- [ {} ] :put edge",
        edge_rows
    ))
    .unwrap();

    storage.shutdown().unwrap();

    // Run the CLI twice and compare byte-for-byte
    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    let output1 = Command::new(ledgerful_bin)
        .args(["ledger", "graph", &tx_id, "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output1.status.success(),
        "First CLI invocation failed: {:?}",
        String::from_utf8_lossy(&output1.stderr)
    );

    let output2 = Command::new(ledgerful_bin)
        .args(["ledger", "graph", &tx_id, "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output2.status.success(),
        "Second CLI invocation failed: {:?}",
        String::from_utf8_lossy(&output2.stderr)
    );

    assert_eq!(
        output1.stdout, output2.stdout,
        "ledger graph output must be byte-identical across repeated runs when the \
         max_nodes cap truncates a neighborhood larger than 150 nodes"
    );

    let stdout = String::from_utf8_lossy(&output1.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    let exact_len = json["exact"].as_array().unwrap().len();
    let derived_len = json["derived"].as_array().unwrap().len();
    let heuristic_len = json["heuristic"].as_array().unwrap().len();
    let total = exact_len + derived_len + heuristic_len;

    assert!(
        total <= 150,
        "total relation count {} exceeds the max_nodes cap of 150",
        total
    );

    // The candidate neighborhood (api.rs + hub + LEAF_COUNT leaves) is larger
    // than the cap, so truncation must actually have kicked in.
    assert_eq!(
        total,
        150,
        "expected the max_nodes cap to be hit exactly, given a neighborhood of {} candidate nodes",
        leaf_urns.len() + 2
    );
}
