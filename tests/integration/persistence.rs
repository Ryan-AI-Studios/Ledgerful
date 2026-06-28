use ledgerful::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket};
use ledgerful::index::symbols::{Symbol, SymbolKind};
use ledgerful::state::storage::StorageManager;
use rusqlite::Connection;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn test_persistence_integration() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("test_ledger.db");

    // 1. Initialize and save
    {
        let storage = StorageManager::init(&db_path).unwrap();
        let mut packet = ImpactPacket {
            head_hash: Some("commit_1".to_string()),
            ..ImpactPacket::default()
        };
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/main.rs"),
            status: "Added".to_string(),
            old_path: None,
            is_staged: true,

            symbols: Some(vec![Symbol {
                name: "run".to_string(),
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
                metadata: [("reexport".to_string(), "true".to_string())].into(),
            }]),

            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });

        storage.save_packet(&packet).unwrap();
    }

    // 2. Re-open and verify
    {
        let storage = StorageManager::init(&db_path).unwrap();
        let latest = storage.get_latest_packet().unwrap().unwrap();
        assert_eq!(latest.head_hash, Some("commit_1".to_string()));
        assert_eq!(latest.changes.len(), 1);
        assert_eq!(latest.changes[0].path, PathBuf::from("src/main.rs"));
    }

    let conn = Connection::open(&db_path).unwrap();
    let symbol_count: i64 = conn
        .query_row("SELECT count(*) FROM symbols", [], |row| row.get(0))
        .unwrap();
    assert_eq!(symbol_count, 1);

    // Verify metadata was persisted
    let metadata: String = conn
        .query_row("SELECT metadata FROM symbols LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(metadata.contains("reexport"));
}
