#[cfg(feature = "daemon")]
use ledgerful::daemon::lifecycle::DaemonLifecycle;
#[cfg(feature = "daemon")]
use ledgerful::daemon::state::ReadOnlyStorage;
#[cfg(feature = "daemon")]
use ledgerful::daemon::{Backend, handlers::uri_to_path};
#[cfg(feature = "daemon")]
use ledgerful::impact::packet::{
    ChangedFile, FileAnalysisStatus, ImpactPacket, RiskLevel, TemporalCoupling,
};
#[cfg(feature = "daemon")]
use ledgerful::index::symbols::{Symbol, SymbolKind};
#[cfg(feature = "daemon")]
use rusqlite::Connection;
#[cfg(feature = "daemon")]
use std::fs;
#[cfg(feature = "daemon")]
use std::path::{Path, PathBuf};
#[cfg(feature = "daemon")]
use tempfile::tempdir;
#[cfg(feature = "daemon")]
use tower_lsp_server::LanguageServer;
#[cfg(feature = "daemon")]
use tower_lsp_server::LspService;
#[cfg(feature = "daemon")]
use tower_lsp_server::ls_types::{
    CodeLensParams, HoverContents, HoverParams, MarkedString, Position, TextDocumentIdentifier,
    TextDocumentPositionParams, Uri,
};

#[test]
#[cfg(feature = "daemon")]
fn test_daemon_pid_lifecycle() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    let lifecycle = DaemonLifecycle::new(root, None);

    // 1. Initial setup
    lifecycle.setup().expect("Initial setup should succeed");
    let pid_file = root.join(".ledgerful").join("daemon.pid");
    assert!(pid_file.exists());

    let pid_content = fs::read_to_string(&pid_file).unwrap();
    let pid: u32 = pid_content.trim().parse().unwrap();
    assert_eq!(pid, std::process::id());

    // 2. Setup when already running (this process is alive)
    let result = lifecycle.setup();
    assert!(
        result.is_err(),
        "Should fail if PID file exists and process is alive"
    );

    // 3. Cleanup
    lifecycle.cleanup().expect("Cleanup should succeed");
    assert!(!pid_file.exists());
}

#[test]
#[cfg(feature = "daemon")]
fn test_daemon_stale_pid_cleanup() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let cg_dir = root.join(".ledgerful");
    fs::create_dir_all(&cg_dir).unwrap();

    let pid_file = cg_dir.join("daemon.pid");
    // Write a likely non-existent PID
    fs::write(&pid_file, "999999").unwrap();

    let lifecycle = DaemonLifecycle::new(root, None);
    lifecycle
        .setup()
        .expect("Should clean up stale PID and succeed");

    let pid_content = fs::read_to_string(&pid_file).unwrap();
    let pid: u32 = pid_content.trim().parse().unwrap();
    assert_eq!(pid, std::process::id());
}

#[test]
#[cfg(feature = "daemon")]
#[allow(non_snake_case)]
fn test_daemon_readonly_sqlite_retry__slow() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("test.db");

    // Create a DB and keep it locked in a write transaction
    let conn = Connection::open(&db_path).unwrap();
    conn.execute("CREATE TABLE test (id INTEGER)", []).unwrap();
    conn.execute("BEGIN EXCLUSIVE", []).unwrap();

    let storage = ReadOnlyStorage::new(&db_path);

    // Attempt a query (should fail/retry and eventually return None/stale if it times out)
    // In our implementation, we return Result<QueryResult<Option<T>>>
    let result = storage.get_latest_packet();

    // Since we only retry 3 times with 100/200/400ms delay, and the lock is held here,
    // it should return Ok(QueryResult { data: None, data_stale: true })
    match result {
        Ok(qr) => {
            assert!(qr.data.is_none());
            assert!(qr.data_stale);
        }
        Err(e) => panic!(
            "Should not have returned hard error for busy DB, got {:?}",
            e
        ),
    }
}

#[test]
#[cfg(feature = "daemon")]
fn test_daemon_parent_liveness_check() {
    let tmp = tempdir().unwrap();
    let lifecycle = DaemonLifecycle::new(tmp.path(), Some(999999));

    assert!(!lifecycle.check_parent_alive());
}

#[test]
#[cfg(feature = "daemon")]
fn test_daemon_uri_normalization() {
    let tmp = tempdir().unwrap();
    let file_path = tmp.path().join("src").join("main.rs");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "fn main() {}").unwrap();

    let uri = file_uri(&file_path);

    assert_eq!(uri_to_path(&uri), Some(file_path));
}

#[tokio::test]
#[cfg(feature = "daemon")]
async fn test_daemon_hover_and_codelens_are_populated() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let file_path = root.join("src").join("main.rs");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "fn main() {}").unwrap();

    write_packet(&db_path, packet_for_file(&file_path));

    let lifecycle = DaemonLifecycle::new(root, Some(std::process::id()));
    let storage = ReadOnlyStorage::new(&db_path);
    let (service, _) = LspService::new(|client| Backend::new(client, lifecycle, storage));
    let backend = service.inner();
    let uri = file_uri(&file_path);

    let hover = backend
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap()
        .expect("hover should be present");

    let HoverContents::Scalar(contents) = hover.contents else {
        panic!("hover should use scalar markdown content");
    };
    let MarkedString::String(contents) = contents else {
        panic!("hover should use plain marked string content");
    };
    assert!(contents.contains("Ledgerful Impact"));

    let lenses = backend
        .code_lens(CodeLensParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap()
        .expect("code lens should be present");

    let titles: Vec<_> = lenses
        .iter()
        .filter_map(|lens| lens.command.as_ref())
        .map(|command| command.title.as_str())
        .collect();
    assert!(titles.iter().any(|title| title.starts_with("Risk:")));
    assert!(titles.iter().any(|title| title.starts_with("Complexity:")));
}

#[cfg(feature = "daemon")]
fn file_uri(path: &Path) -> Uri {
    let normalized = path.to_string_lossy().replace('\\', "/");
    format!("file:///{normalized}")
        .parse()
        .expect("file URI should parse")
}

#[cfg(feature = "daemon")]
fn write_packet(db_path: &Path, packet: ImpactPacket) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute(
        "CREATE TABLE snapshots (id INTEGER PRIMARY KEY, packet_json TEXT NOT NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO snapshots (packet_json) VALUES (?1)",
        [serde_json::to_string(&packet).unwrap()],
    )
    .unwrap();
}

#[cfg(feature = "daemon")]
fn packet_for_file(file_path: &Path) -> ImpactPacket {
    let mut packet = ImpactPacket {
        risk_level: RiskLevel::High,
        risk_reasons: vec!["High fan-out".to_string()],
        ..ImpactPacket::default()
    };

    packet.changes.push(ChangedFile {
        path: PathBuf::from(file_path),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: true,

        symbols: Some(vec![Symbol {
            name: "main".to_string(),
            kind: SymbolKind::Function,
            is_public: false,
            cognitive_complexity: Some(42),
            cyclomatic_complexity: Some(11),
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: Default::default(),
        }]),
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: vec!["analysis warning".to_string()],
        api_routes: vec![],
        data_models: vec![],
        ci_gates: vec![],
    });
    packet.temporal_couplings.push(TemporalCoupling {
        file_a: PathBuf::from(file_path),
        file_b: PathBuf::from("src/lib.rs"),
        score: 0.75,
    });
    packet
}
