use ledgerful::config::model::Config;
use ledgerful::index::symbols::{Symbol, SymbolKind};
use ledgerful::ledger::provenance::{ProvenanceAction, compute_symbol_diff};
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_ledger_token_provenance() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let repo_root = dir.path().to_path_buf();

    fs::write(repo_root.join("main.rs"), "").unwrap();

    let mut storage = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage, repo_root.clone(), Config::default());

    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // Simulate symbol changes
    let symbol_diff = vec![(
        Symbol {
            name: "my_func".to_string(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        },
        ProvenanceAction::Added,
    )];

    manager
        .record_token_provenance(&tx_id, symbol_diff)
        .expect("Should record provenance");

    manager
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                summary: "Added my_func".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    // Verify provenance via DB
    let db = LedgerDb::new(manager.get_connection());
    let prov = db.get_token_provenance_for_tx(&tx_id).unwrap();
    assert_eq!(prov.len(), 1);
    assert_eq!(prov[0].symbol_name, "my_func");
    assert_eq!(prov[0].action, ProvenanceAction::Added);
}

#[test]
fn test_symbol_diff_logic() {
    let old_symbols = vec![
        Symbol {
            name: "s1".to_string(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: Some(5),
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        },
        Symbol {
            name: "s2".to_string(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        },
    ];

    let new_symbols = vec![
        Symbol {
            name: "s1".to_string(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: Some(10), // Modified
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        },
        Symbol {
            name: "s3".to_string(), // Added
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        },
    ];

    let diff = compute_symbol_diff(&old_symbols, &new_symbols);
    assert_eq!(diff.len(), 3); // s1 mod, s3 add, s2 del

    assert!(
        diff.iter()
            .any(|(s, a)| s.name == "s1" && *a == ProvenanceAction::Modified)
    );
    assert!(
        diff.iter()
            .any(|(s, a)| s.name == "s3" && *a == ProvenanceAction::Added)
    );
    assert!(
        diff.iter()
            .any(|(s, a)| s.name == "s2" && *a == ProvenanceAction::Deleted)
    );
}
