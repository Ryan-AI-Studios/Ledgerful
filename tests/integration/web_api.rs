use camino::Utf8Path;
use ledgerful::commands::web::auth::generate_token;
use ledgerful::commands::web::server::router;
use ledgerful::commands::web::state::AppState;
use ledgerful::ledger::db::LedgerDb;
use ledgerful::ledger::types::{Category, ChangeType, EntryType, LedgerEntry, Transaction};
use ledgerful::state::layout::Layout;
use rusqlite::Connection;

use std::sync::Arc;
use tokio::net::TcpListener;

fn authed_get(url: &str, token: &str, path: &str) -> ureq::Request {
    ureq::get(&format!("{}{}", url, path)).set("Authorization", &format!("Bearer {}", token))
}

/// Owns the temporary directory so it stays alive for the lifetime of a test.
struct LayoutGuard {
    _tmp: tempfile::TempDir,
    layout: Layout,
}

impl LayoutGuard {
    fn layout(&self) -> Layout {
        self.layout.clone()
    }
}

fn temp_layout() -> LayoutGuard {
    let tmp = tempfile::tempdir().unwrap();
    let layout = Layout::new(Utf8Path::from_path(tmp.path()).unwrap());
    LayoutGuard { _tmp: tmp, layout }
}

async fn spawn_server(layout: Layout) -> (String, String, tokio::task::JoinHandle<()>) {
    spawn_server_with_spa_dir(layout, None).await
}

async fn spawn_server_with_spa_dir(
    layout: Layout,
    spa_dir: Option<camino::Utf8PathBuf>,
) -> (String, String, tokio::task::JoinHandle<()>) {
    let token = generate_token();
    let state = Arc::new(AppState::new(layout, token.clone(), spa_dir));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = router(state);
    let serve = axum::serve(listener, app);
    let handle = tokio::spawn(async move {
        let _ = serve.await;
    });

    let url = format!("http://{}", addr);
    (url, token, handle)
}

fn seed_ledger_entry(
    layout: &Layout,
    tx_id: &str,
    summary: &str,
    reason: &str,
    signature: &str,
    public_key: &str,
) {
    layout.ensure_state_dir().unwrap();
    std::fs::create_dir_all(layout.state_subdir()).unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let conn = Connection::open(db_path.as_std_path()).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS transactions (
            tx_id TEXT PRIMARY KEY,
            operation_id TEXT,
            status TEXT NOT NULL,
            category TEXT NOT NULL,
            entity TEXT NOT NULL,
            entity_normalized TEXT NOT NULL,
            planned_action TEXT,
            session_id TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'CLI',
            started_at TEXT NOT NULL,
            resolved_at TEXT,
            detected_at TEXT,
            drift_count INTEGER DEFAULT 1,
            first_seen_at TEXT,
            last_seen_at TEXT,
            issue_ref TEXT,
            snapshot_id INTEGER
        );
        CREATE TABLE IF NOT EXISTS ledger_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            tx_id TEXT NOT NULL,
            category TEXT NOT NULL,
            entry_type TEXT NOT NULL DEFAULT 'IMPLEMENTATION',
            entity TEXT NOT NULL,
            entity_normalized TEXT NOT NULL,
            change_type TEXT NOT NULL,
            summary TEXT NOT NULL,
            reason TEXT NOT NULL,
            is_breaking INTEGER DEFAULT 0,
            committed_at TEXT NOT NULL,
            verification_status TEXT,
            verification_basis TEXT,
            outcome_notes TEXT,
            origin TEXT NOT NULL DEFAULT 'LOCAL',
            trace_id TEXT,
            signature TEXT,
            public_key TEXT,
            risk TEXT,
            related_tickets TEXT,
            author TEXT NOT NULL DEFAULT 'unknown',
            observed INTEGER,
            prev_hash TEXT,
            sig_version INTEGER NOT NULL DEFAULT 1
        );",
    )
    .unwrap();

    let db = LedgerDb::new(&conn);
    let tx = Transaction {
        tx_id: tx_id.to_string(),
        operation_id: None,
        status: "COMMITTED".to_string(),
        category: Category::Feature,
        entity: "entity".to_string(),
        entity_normalized: "entity".to_string(),
        planned_action: None,
        session_id: "test".to_string(),
        source: "test".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        resolved_at: None,
        detected_at: None,
        drift_count: 0,
        first_seen_at: None,
        last_seen_at: None,
        issue_ref: None,
        snapshot_id: None,
    };
    db.insert_transaction(&tx).unwrap();

    let entry = LedgerEntry {
        id: 0,
        tx_id: tx_id.to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "entity".to_string(),
        entity_normalized: "entity".to_string(),
        change_type: ChangeType::Modify,
        summary: summary.to_string(),
        reason: reason.to_string(),
        is_breaking: false,
        committed_at: chrono::Utc::now().to_rfc3339(),
        verification_status: None,
        verification_basis: None,
        outcome_notes: None,
        origin: "LOCAL".to_string(),
        trace_id: None,
        signature: Some(signature.to_string()),
        public_key: Some(public_key.to_string()),
        risk: Some("LOW".to_string()),
        related_tickets: None,
        author: "Test User".to_string(),
        observed: None,
        prev_hash: None,
        sig_version: 1,
    };

    db.insert_ledger_entry(&entry).unwrap();
}

/// Like `seed_ledger_entry` but with a fixed `committed_at` and optional
/// signature / public key, and uses `StorageManager::init` (full migrations)
/// so the resulting DB passes the read-only schema-mismatch check performed
/// by `StorageManager::open_read_only_sqlite_only` in integration builds.
///
/// Used by the Track E2 compliance tests so the seeded row's `committed_at`
/// matches the payload signed by `sign_ledger_entry`, and so unsigned rows
/// (`None`/`None`) can be inserted.
fn seed_ledger_entry_with_ts(
    layout: &Layout,
    tx_id: &str,
    summary: &str,
    reason: &str,
    committed_at: &str,
    signature: Option<&str>,
    public_key: Option<&str>,
) {
    use ledgerful::state::storage::StorageManager;
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection();

    // Seed a COMMITTED transaction. `snapshot_id` is nullable so we omit it
    // (no snapshot row needed for the compliance read path).
    conn.execute(
        "INSERT INTO transactions \
         (tx_id, status, category, entity, entity_normalized, session_id, source, started_at) \
         VALUES (?1, 'COMMITTED', 'FEATURE', 'entity', 'entity', 'test', 'test', ?2)",
        rusqlite::params![tx_id, committed_at],
    )
    .unwrap();

    // Seed the ledger entry with the provided signature / public key.
    conn.execute(
        "INSERT INTO ledger_entries \
         (tx_id, category, entry_type, entity, entity_normalized, change_type, \
          summary, reason, is_breaking, committed_at, origin, author, signature, public_key) \
         VALUES (?1, 'FEATURE', 'IMPLEMENTATION', 'entity', 'entity', 'MODIFY', \
                 ?2, ?3, 0, ?4, 'LOCAL', 'Test User', ?5, ?6)",
        rusqlite::params![tx_id, summary, reason, committed_at, signature, public_key,],
    )
    .unwrap();
}

/// Write a minimal valid `config.toml` into `layout` with
/// `intent.require_signing = <require>`. The temp layout's config file is
/// `<tmp>/.ledgerful/config.toml`, so this never touches the real repo's
/// committed config.
fn write_require_signing_config(layout: &Layout, require: bool) {
    layout.ensure_state_dir().unwrap();
    let config_path = layout.config_file();
    std::fs::write(
        &config_path,
        format!("[intent]\nrequire_signing = {require}\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn test_health_returns_ok() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        ureq::get(&format!("{}/health", url))
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(body.contains("ok"));
    handle.abort();
}

#[tokio::test]
async fn test_snapshot_without_token_returns_403() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let status = tokio::task::spawn_blocking(move || {
        match ureq::get(&format!("{}/api/snapshot", url)).call() {
            Ok(resp) => resp.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await
    .unwrap();

    assert_eq!(status, 403);
    handle.abort();
}

#[tokio::test]
async fn test_snapshot_with_token_returns_json() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/snapshot")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json.get("overall_risk").and_then(|v| v.as_str()),
        Some("low")
    );
    handle.abort();
}

#[tokio::test]
async fn test_status_returns_json() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/status")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json.get("pending_transactions").is_some());
    // Non-demo repos must not expose is_demo (skip_serializing_if = false).
    assert!(
        json.get("is_demo").is_none(),
        "non-demo repo must not serialize is_demo"
    );
    handle.abort();
}

#[tokio::test]
async fn test_status_is_demo_true_when_marker_present() {
    let guard = temp_layout();
    // Write the DEMO_MARKER so the status endpoint detects a demo repo.
    std::fs::create_dir_all(guard.layout().root.join(".ledgerful")).unwrap();
    std::fs::write(
        guard.layout().root.join(".ledgerful").join("DEMO_MARKER"),
        r#"{"demo": true}"#,
    )
    .unwrap();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/status")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json.get("is_demo").and_then(|v| v.as_bool()),
        Some(true),
        "demo repo must return is_demo: true"
    );
    handle.abort();
}

#[tokio::test]
async fn test_projects_returns_self() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/projects")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let arr = json.as_array().expect("projects is an array");
    assert!(!arr.is_empty());
    handle.abort();
}

#[tokio::test]
async fn test_api_projects_returns_sibling_with_warning() {
    let guard = temp_layout();
    let primary_dir = guard.layout().root.join("primary");
    let sibling_dir = guard.layout().root.join("sibling");
    std::fs::create_dir_all(&primary_dir).unwrap();
    std::fs::create_dir_all(sibling_dir.join(".ledgerful/state")).unwrap();
    std::fs::write(sibling_dir.join(".ledgerful/state/ledger.db"), "").unwrap();
    let primary_layout = ledgerful::state::layout::Layout::new(&primary_dir);
    let schema_json = serde_json::json!({
        "schema_version": ledgerful::federated::schema::FederatedSchema::VERSION,
        "repo_name": "sibling-repo",
        "public_interfaces": [],
        "ledger": [{
            "tx_id": "tx-1",
            "category": "FEATURE",
            "entry_type": "IMPLEMENTATION",
            "entity": "", // empty entity -> warning
            "change_type": "CREATE",
            "summary": "sum",
            "reason": "rsn",
            "is_breaking": false,
            "committed_at": "2026-06-24T00:00:00Z",
            "author": ""
        }]
    });
    std::fs::write(
        sibling_dir.join(".ledgerful/state/schema.json"),
        serde_json::to_string(&schema_json).unwrap(),
    )
    .unwrap();

    let (url, token, handle) = spawn_server(primary_layout).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/projects")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let arr = json.as_array().expect("projects is an array");

    let sibling_node = arr
        .iter()
        .find(|n| n["id"] == "sibling-repo")
        .expect("sibling should be discovered");
    let warnings = sibling_node["validation_warnings"]
        .as_array()
        .expect("should have validation_warnings");
    assert!(
        !warnings.is_empty(),
        "sibling should have validation warnings for empty entity"
    );
    assert!(warnings[0].as_str().unwrap().contains("entity"));

    handle.abort();
}

#[tokio::test]
async fn test_api_ledger_empty_entity_fallback() {
    let guard = temp_layout();
    let tx_id = "tx-empty-entity";
    seed_ledger_entry(
        &guard.layout(),
        tx_id,
        "Fix the thing",
        "Because it was broken",
        "deadbeef",
        "pubkey123",
    );

    {
        let db_path = guard.layout().state_subdir().join("ledger.db");
        let conn = rusqlite::Connection::open(db_path.as_std_path()).unwrap();
        conn.execute("UPDATE transactions SET entity = ''", ())
            .unwrap();
        conn.execute("UPDATE ledger_entries SET entity = ''", ())
            .unwrap();
    }

    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/ledger")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let arr = json.as_array().unwrap();
    let entry = arr.iter().find(|n| n["tx_id"] == tx_id).unwrap();
    assert_eq!(entry["entity"].as_str(), Some("(uncategorized)"));
    handle.abort();
}

#[tokio::test]
async fn test_ledger_empty_returns_array() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/ledger")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json.as_array().unwrap().is_empty());
    handle.abort();
}

#[tokio::test]
async fn test_ledger_missing_tx_returns_404() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let status = tokio::task::spawn_blocking(move || {
        match authed_get(&url, &token, "/api/ledger/no-such-tx").call() {
            Ok(resp) => resp.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await
    .unwrap();

    assert_eq!(status, 404);
    handle.abort();
}

#[tokio::test]
async fn test_ledger_search_missing_query_returns_400() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let status = tokio::task::spawn_blocking(move || {
        match authed_get(&url, &token, "/api/ledger/search").call() {
            Ok(resp) => resp.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await
    .unwrap();

    assert_eq!(status, 400);
    handle.abort();
}

#[tokio::test]
async fn test_ledger_search_returns_array() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/ledger/search?q=test")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json.as_array().is_some());
    handle.abort();
}

#[tokio::test]
async fn test_changes_returns_array() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let status = tokio::task::spawn_blocking(move || {
        match authed_get(&url, &token, "/api/changes").call() {
            Ok(resp) => resp.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await
    .unwrap();

    assert_eq!(status, 200);
    handle.abort();
}

#[tokio::test]
async fn test_hotspots_returns_array() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/hotspots?limit=5")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    // The response should be an array (possibly empty if no git history).
    assert!(json.is_array(), "hotspots response should be an array");

    // If non-empty, verify the DTO fields are present.
    if let Some(arr) = json.as_array() {
        for item in arr {
            // Frontend-facing fields.
            assert!(item.get("id").is_some(), "missing id field");
            assert!(item.get("filePath").is_some(), "missing filePath field");
            assert!(item.get("riskLevel").is_some(), "missing riskLevel field");
            assert!(item.get("riskScore").is_some(), "missing riskScore field");
            assert!(
                item.get("lastTouchedAt").is_some(),
                "missing lastTouchedAt field"
            );
            assert!(
                item.get("contributor").is_some(),
                "missing contributor field"
            );
            assert!(
                item.get("changeCount").is_some(),
                "missing changeCount field"
            );
            assert!(item.get("rank").is_some(), "missing rank field");
            // Backward-compat fields.
            assert!(
                item.get("displayScore").is_some(),
                "missing displayScore field"
            );
            assert!(item.get("score").is_some(), "missing score field");
            assert!(item.get("complexity").is_some(), "missing complexity field");
            assert!(item.get("frequency").is_some(), "missing frequency field");
        }
    }

    handle.abort();
}

/// Verify that `/api/hotspots` returns populated `lastTouchedAt` and
/// `contributor` when the data is persisted in `project_files` (Track TA30).
#[tokio::test]
async fn test_hotspots_dto_reads_git_metadata_from_project_files() {
    use ledgerful::state::migrations::get_migrations;

    let guard = temp_layout();
    let layout = guard.layout();

    // Set up a git repo so `fetch_hotspots` doesn't bail early.
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(layout.root.as_std_path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(layout.root.as_std_path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.local"])
        .current_dir(layout.root.as_std_path())
        .output()
        .unwrap();

    // Create a source file and commit it so the hotspot walk has data.
    std::fs::create_dir_all(layout.root.join("src")).unwrap();
    std::fs::write(layout.root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(layout.root.as_std_path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial", "--quiet"])
        .current_dir(layout.root.as_std_path())
        .output()
        .unwrap();

    // Open the same ledger.db the web API reads from and run migrations.
    let db_path = layout.state_subdir().join("ledger.db");
    layout.ensure_state_dir().unwrap();
    let conn = Connection::open(db_path.as_std_path()).unwrap();
    let mut conn = conn;
    get_migrations().to_latest(&mut conn).unwrap();

    // Seed project_files with git metadata (simulating what the indexer backfill does).
    conn.execute(
        "INSERT INTO project_files (file_path, language, last_indexed_at, last_touched_at, last_contributor)
         VALUES ('src/main.rs', 'Rust', '2024-01-01T00:00:00+00:00', '2024-06-24T12:00:00+00:00', 'Alice Developer')",
        [],
    )
    .unwrap();
    drop(conn);

    let (url, token, handle) = spawn_server(layout).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/hotspots?limit=5")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let arr = json
        .as_array()
        .expect("hotspots response should be an array");

    // If hotspots were computed (git history exists), verify the DTO fields
    // are populated from project_files.
    if !arr.is_empty() {
        let item = &arr[0];
        // The DTO should have the frontend-facing fields.
        assert!(item.get("id").is_some(), "missing id");
        assert!(item.get("filePath").is_some(), "missing filePath");
        assert!(item.get("riskLevel").is_some(), "missing riskLevel");
        assert!(item.get("rank").is_some(), "missing rank");

        // If this is the src/main.rs hotspot, verify git metadata was read
        // from project_files.
        if item["filePath"].as_str() == Some("src/main.rs") {
            assert_eq!(
                item["lastTouchedAt"].as_str(),
                Some("2024-06-24T12:00:00+00:00"),
                "lastTouchedAt should be read from project_files"
            );
            assert_eq!(
                item["contributor"].as_str(),
                Some("Alice Developer"),
                "contributor should be read from project_files"
            );
        }
    }

    handle.abort();
}

#[tokio::test]
async fn test_latest_impact_report_returns_json() {
    let guard = temp_layout();
    let reports_dir = guard.layout().reports_dir();
    std::fs::create_dir_all(&reports_dir).unwrap();
    std::fs::write(
        reports_dir.join("latest-impact.json"),
        r#"{"overallRisk":"low"}"#,
    )
    .unwrap();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/reports/latest-impact.json")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["overallRisk"].as_str(), Some("low"));
    handle.abort();
}

#[tokio::test]
async fn test_latest_verify_report_returns_json() {
    let guard = temp_layout();
    let reports_dir = guard.layout().reports_dir();
    std::fs::create_dir_all(&reports_dir).unwrap();
    std::fs::write(
        reports_dir.join("latest-verify.json"),
        r#"{"overallPass":true,"results":[]}"#,
    )
    .unwrap();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/reports/latest-verify.json")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["overallPass"].as_bool(), Some(true));
    handle.abort();
}

#[tokio::test]
async fn test_hotspots_trend_returns_json() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/hotspots/trend?days=7&limit=5")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json.get("labels").is_some());
    assert!(json.get("series").is_some());
    handle.abort();
}

#[tokio::test]
async fn test_trends_returns_empty_array_when_no_data() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/trends?days=90")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let data = json["data"].as_array().expect("data is an array");
    assert!(data.is_empty(), "expected empty data array, got {data:?}");
    handle.abort();
}

#[tokio::test]
async fn test_trends_returns_populated_data() {
    use ledgerful::state::storage::StorageManager;

    let guard = temp_layout();
    let layout = guard.layout();
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection();

    conn.execute(
        "INSERT INTO project_trend_days (day, score, changes, high_risk_count) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params!["2026-06-23", 42.5, 5, 1],
    )
    .unwrap();
    drop(storage);

    let (url, token, handle) = spawn_server(layout.clone()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/trends?days=90")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let data = json["data"].as_array().expect("data is an array");
    assert_eq!(data.len(), 1);
    let point = &data[0];
    assert_eq!(point["date"].as_str(), Some("2026-06-23"));
    assert!((point["score"].as_f64().unwrap() - 42.5).abs() < 1e-6);
    assert_eq!(point["changes"].as_i64(), Some(5));
    assert_eq!(point["highRiskCount"].as_i64(), Some(1));
    handle.abort();
}

#[tokio::test]
async fn test_trends_days_param_returns_inclusive_n_day_window() {
    use ledgerful::state::storage::StorageManager;

    let guard = temp_layout();
    let layout = guard.layout();
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection();

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let two_days_ago = (chrono::Utc::now() - chrono::Duration::days(2))
        .format("%Y-%m-%d")
        .to_string();

    conn.execute(
        "INSERT INTO project_trend_days (day, score, changes, high_risk_count) VALUES (?1, 10, 1, 0)",
        rusqlite::params![&today],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_trend_days (day, score, changes, high_risk_count) VALUES (?1, 20, 2, 0)",
        rusqlite::params![&yesterday],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_trend_days (day, score, changes, high_risk_count) VALUES (?1, 30, 3, 0)",
        rusqlite::params![&two_days_ago],
    )
    .unwrap();
    drop(storage);

    let (url, token, handle) = spawn_server(layout.clone()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/trends?days=1")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let data = json["data"].as_array().expect("data is an array");
    assert_eq!(
        data.len(),
        1,
        "days=1 should return only today, not yesterday"
    );
    assert_eq!(data[0]["date"].as_str(), Some(today.as_str()));
    handle.abort();
}

#[tokio::test]
async fn test_endpoints_changed_returns_json() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let status = tokio::task::spawn_blocking(move || {
        match authed_get(&url, &token, "/api/endpoints/changed").call() {
            Ok(resp) => resp.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await
    .unwrap();

    assert_eq!(status, 200);
    handle.abort();
}

#[tokio::test]
async fn test_security_boundaries_returns_json() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/security/boundaries")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json.get("meta").is_some());
    assert!(json.get("boundaries").is_some());
    handle.abort();
}

#[tokio::test]
async fn test_knowledge_graph_returns_json() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/knowledge-graph?limit=10&focus=changed")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json.get("nodes").is_some());
    assert!(json.get("edges").is_some());
    assert!(json.get("truncated").is_some());
    handle.abort();
}

#[tokio::test]
async fn test_problem_detail_on_forbidden() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let (status, content_type, body) = tokio::task::spawn_blocking(move || {
        match ureq::get(&format!("{}/api/status", url)).call() {
            Ok(resp) => (resp.status(), String::new(), String::new()),
            Err(ureq::Error::Status(code, resp)) => {
                let ct = resp.header("Content-Type").unwrap_or("").to_string();
                let body = resp.into_string().unwrap_or_default();
                (code, ct, body)
            }
            Err(_) => (0, String::new(), String::new()),
        }
    })
    .await
    .unwrap();

    assert_eq!(status, 403);
    assert!(
        content_type.contains("application/problem+json"),
        "expected problem+json, got {content_type}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"].as_u64(), Some(403));
    assert!(json["type"].is_string());
    assert!(json["title"].is_string());
    assert!(json["detail"].is_string());
    handle.abort();
}

#[tokio::test]
async fn test_ledger_detail_returns_entry_shape() {
    let guard = temp_layout();
    let tx_id = "tx-detail-001";
    seed_ledger_entry(
        &guard.layout(),
        tx_id,
        "Fix the thing",
        "Because it was broken",
        "deadbeef",
        "pubkey123",
    );
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, &format!("/api/ledger/{}", tx_id))
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["tx_id"].as_str(), Some(tx_id));
    assert_eq!(json["summary"].as_str(), Some("Fix the thing"));
    assert_eq!(json["reason"].as_str(), Some("Because it was broken"));
    assert_eq!(json["signature"].as_str(), Some("deadbeef"));
    assert_eq!(json["public_key"].as_str(), Some("pubkey123"));
    assert!(json["committed_at"].is_string());
    handle.abort();
}

#[tokio::test]
async fn test_rate_limit_returns_429_after_burst() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let limited = tokio::task::spawn_blocking(move || {
        let path = format!("{}/health", url);
        for i in 0..=65 {
            let status = match ureq::get(&path).call() {
                Ok(resp) => resp.status(),
                Err(ureq::Error::Status(code, _)) => code,
                Err(_) => 0,
            };
            if status == 429 {
                return (true, i);
            }
        }
        (false, 0)
    })
    .await
    .unwrap();

    assert!(
        limited.0,
        "expected 429 after burst, last request number {}",
        limited.1
    );
    handle.abort();
}

#[tokio::test]
async fn test_spa_index_served_at_root() {
    let guard = temp_layout();
    let spa_tmp = tempfile::tempdir().unwrap();
    let spa_dir = camino::Utf8Path::from_path(spa_tmp.path())
        .unwrap()
        .to_path_buf();
    std::fs::write(
        spa_dir.join("index.html"),
        "<html><body>Ledgerful Dashboard</body></html>",
    )
    .unwrap();

    let (url, _token, handle) = spawn_server_with_spa_dir(guard.layout(), Some(spa_dir)).await;

    let root_url = url.clone();
    let body = tokio::task::spawn_blocking(move || {
        ureq::get(&format!("{}/", root_url))
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(body.contains("Ledgerful Dashboard"));

    // Unknown client-side routes fall back to index.html.
    let fallback = tokio::task::spawn_blocking(move || {
        ureq::get(&format!("{}/ledger/some-tx-id", url))
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(fallback.contains("Ledgerful Dashboard"));
    handle.abort();
}

// ---------------------------------------------------------------------------
// M8 spec-mandated integration tests
// ---------------------------------------------------------------------------
//
// These tests are the five integration tests called out at
// `conductor/trackM8/spec.md:75-81`. The M8 review (#1) flagged that
// none of them were present in the original commit; this block closes
// the gap.

/// Per-spec test 1: `commit a ledger transaction in a tempdir repo with
/// a known `git config user.name`, fetch `/api/ledger`, assert
/// `author` matches.`
///
/// This exercises the full `start_change -> commit_change` path so the
/// `git config user.name` capture chain in
/// `TransactionManager::commit_change` (now extracted as
/// `capture_git_author`) is what's actually writing the `author`
/// column the response then reads.
#[tokio::test]
async fn test_api_ledger_includes_author_from_git_config() {
    use ledgerful::config::model::Config;
    use ledgerful::ledger::transaction::TransactionManager;
    use ledgerful::ledger::types::{ChangeType, CommitRequest, TransactionRequest};
    use ledgerful::state::storage::StorageManager;
    use std::process::Command;

    let guard = temp_layout();
    let layout = guard.layout();
    let expected_name = "Backend Test User";

    // Set up a git repo with the expected user.name in *this* tempdir
    // (so the test is independent of the developer's local git
    // config). We set user.email too to make the chain deterministic
    // and unambiguous.
    Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(layout.root.as_std_path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", expected_name])
        .current_dir(layout.root.as_std_path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "backend@test.local"])
        .current_dir(layout.root.as_std_path())
        .output()
        .unwrap();

    // Create the entity file the ledger will reference.
    let entity_path = layout.root.join("src/feature.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(
        &entity_path,
        "pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    )
    .unwrap();

    // Initialize storage and run the start -> commit flow.
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();

    // Per M8 opencode-review M4: `capture_git_author` now takes
    // `repo_root` and runs `git config` in that directory, so this
    // test no longer needs to chdir to the layout root to make the
    // local user.name visible. The chdir hack was unsafe under
    // parallel test execution.
    let mut tx_mgr = TransactionManager::new(
        &mut storage,
        layout.root.as_std_path().to_path_buf(),
        Config::default(),
    );

    let tx_id_result = tx_mgr
        .start_change(TransactionRequest {
            category: ledgerful::ledger::types::Category::Feature,
            entity: "src/feature.rs".to_string(),
            planned_action: Some("Add a sum helper".to_string()),
            ..Default::default()
        })
        .expect("start_change should succeed");

    let tx_id = tx_id_result.clone();

    let commit_result = tx_mgr.commit_change(
        tx_id.clone(),
        CommitRequest {
            change_type: ChangeType::Create,
            summary: "Add src/feature.rs".to_string(),
            reason: "Spec test 1".to_string(),
            ..Default::default()
        },
        false,
    );

    commit_result.expect("commit_change should succeed");

    // Now start the server and fetch /api/ledger.
    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/ledger")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let entries = json.as_array().expect("/api/ledger returns an array");
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one ledger entry, got {}",
        entries.len()
    );
    let first = &entries[0];
    assert_eq!(
        first["author"].as_str(),
        Some(expected_name),
        "author should be captured from git config user.name; got {:?}",
        first["author"]
    );
    assert_eq!(first["tx_id"].as_str(), Some(tx_id.as_str()));
    handle.abort();
}

/// Per-spec test 2: `/api/ledger/:txId` for a tx with known
/// `changed_files`/`verification_runs` rows, assert
/// `files`/`tests_run`/`flakes` match expected counts.`
///
/// Seeds `snapshots`, `changed_files`, `verification_runs`, and
/// `verification_results` rows tied to a real ledger entry, then
/// fetches `/api/ledger/:txId` and asserts the enrichment fields
/// reflect that data (not zeros).
///
/// Read-side-only: this test validates the *read* path
/// (`fetch_verification_stats` joining on `verification_runs.tx_id`),
/// not the *write* path (a real `verify` invocation populating
/// `tx_id` from `ledger commit`). The write side is a documented
/// follow-up (see `m45_ledger_verification_runs_tx.rs:14-19`).
#[tokio::test]
async fn test_api_ledger_detail_reads_verification_enrichment() {
    use ledgerful::state::storage::StorageManager;

    let guard = temp_layout();
    let layout = guard.layout();

    // Seed a real ledger entry via StorageManager so the
    // transactions.snapshot_id -> snapshots.id path is in place.
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection();

    // Create a snapshot to attach changed_files to.
    conn.execute(
        "INSERT INTO snapshots (timestamp, head_hash, branch_name, is_clean, packet_json) \
         VALUES (?1, ?2, ?3, 1, ?4)",
        rusqlite::params!["2026-06-17T10:00:00Z", "deadbeef", "main", "{}"],
    )
    .unwrap();
    let snapshot_id = conn.last_insert_rowid();

    // Insert changed_files tied to that snapshot.
    for path in &["src/foo.rs", "src/bar.rs"] {
        conn.execute(
            "INSERT INTO changed_files (snapshot_id, path, status, is_staged) \
             VALUES (?1, ?2, 'Modified', 1)",
            rusqlite::params![snapshot_id, path],
        )
        .unwrap();
    }

    // Seed a transaction pointing at the snapshot.
    let tx_id = "tx-detail-enrich-001";
    conn.execute(
        "INSERT INTO transactions (
            tx_id, status, category, entity, entity_normalized,
            session_id, source, started_at, snapshot_id
         ) VALUES (?1, 'COMMITTED', 'FEATURE', 'src/foo.rs', 'src/foo.rs', 's', 'CLI', '2026-06-17T10:00:00Z', ?2)",
        rusqlite::params![tx_id, snapshot_id],
    )
    .unwrap();

    // Insert a ledger entry directly (bypassing TransactionManager to
    // avoid needing a full git history).
    conn.execute(
        "INSERT INTO ledger_entries (
            tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            origin, author, observed
         ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', 'src/foo.rs', 'src/foo.rs',
                   'MODIFY', 'Enriched tx', 'Test', 0, '2026-06-17T10:00:00Z',
                   'LOCAL', 'Test User', NULL)",
        rusqlite::params![tx_id],
    )
    .unwrap();

    // Insert one passing verification_run and one failing
    // verification_run (so flakes == 1). Then insert three
    // verification_results (test/build commands) â€” two on the passing
    // run, one on the failing run. Per M8 opencode-review L1,
    // `tests_run` counts `verification_results` (3) and `flakes`
    // counts failing `verification_runs` (1). The 1:many
    // runâ†’results ratio is what makes the distinction observable
    // (the old test seeded 2 runs + 2 results 1:1, so a mis-keyed
    // JOIN could not be detected).
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass, tx_id) \
         VALUES ('2026-06-17T10:00:00Z', '{}', 1, ?1)",
        rusqlite::params![tx_id],
    )
    .unwrap();
    let run1 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass, tx_id) \
         VALUES ('2026-06-17T10:00:01Z', '{}', 0, ?1)",
        rusqlite::params![tx_id],
    )
    .unwrap();
    let run2 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated, tx_id) \
         VALUES (?1, 'cargo test', 0, 100, 0, ?2)",
        rusqlite::params![run1, tx_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated, tx_id) \
         VALUES (?1, 'cargo build', 0, 200, 0, ?2)",
        rusqlite::params![run1, tx_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated, tx_id) \
         VALUES (?1, 'cargo clippy', 1, 50, 0, ?2)",
        rusqlite::params![run2, tx_id],
    )
    .unwrap();

    // Add an unlinked run (NULL tx_id) to verify it is not counted
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass, tx_id) \
         VALUES ('2026-06-17T10:00:02Z', '{}', 0, NULL)",
        [],
    )
    .unwrap();
    let null_run = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated, tx_id) \
         VALUES (?1, 'cargo check', 1, 50, 0, NULL)",
        rusqlite::params![null_run],
    )
    .unwrap();

    // Seed a hotspot_history row for one of the two changed files so
    // `hotspots_crossed` is non-zero and actually exercises the JOIN,
    // not just the empty-table path. `src/foo.rs` is one of the two
    // changed files seeded above; `src/bar.rs` is deliberately NOT in
    // hotspot_history so the count is 1, not 2, making the assertion
    // meaningful (it distinguishes a correct JOIN from a COUNT(*) leak).
    conn.execute(
        "INSERT INTO hotspot_history \
         (file_path, score, display_score, complexity, frequency, timestamp) \
         VALUES (?1, 8.5, 8.5, 5, 3.0, ?2)",
        rusqlite::params!["src/foo.rs", "2026-06-17T09:00:00Z"],
    )
    .unwrap();

    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        match authed_get(&url, &token, &format!("/api/ledger/{}", tx_id)).call() {
            Ok(resp) => resp.into_string().unwrap(),
            Err(ureq::Error::Status(_, resp)) => {
                panic!(
                    "GET /api/ledger/{} failed: {}",
                    tx_id,
                    resp.into_string().unwrap_or_default()
                );
            }
            Err(e) => panic!("GET /api/ledger/{} transport error: {}", tx_id, e),
        }
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["tx_id"].as_str(), Some(tx_id));

    let files = json["files"].as_array().expect("files is an array");
    assert_eq!(
        files.len(),
        2,
        "expected 2 changed files, got {}",
        files.len()
    );
    let file_paths: Vec<&str> = files.iter().filter_map(|f| f["path"].as_str()).collect();
    assert!(file_paths.contains(&"src/foo.rs"));
    assert!(file_paths.contains(&"src/bar.rs"));

    assert_eq!(
        json["tests_run"].as_u64(),
        Some(3),
        "expected 3 verification_results tied to this tx (L1: tests_run \
         counts results, not invocations), got {:?}",
        json["tests_run"]
    );
    assert_eq!(
        json["flakes"].as_u64(),
        Some(1),
        "expected 1 failing verification_run (L1: flakes counts failing \
         invocations, not failing commands), got {:?}",
        json["flakes"]
    );
    // `src/foo.rs` is in `hotspot_history`; `src/bar.rs` is not.
    // Expected: 1 hotspot crossed (not 0 or 2). This pins the
    // `count_hotspots_crossed` JOIN â€” a regression that broke the
    // intersection logic or read from the wrong snapshot would produce
    // 0 or 2 instead of 1.
    assert_eq!(
        json["hotspots_crossed"].as_u64(),
        Some(1),
        "expected 1 hotspot crossed (src/foo.rs in hotspot_history, \
         src/bar.rs not); got {:?}",
        json["hotspots_crossed"]
    );

    handle.abort();
}

/// Per-spec test 3: `/api/projects` after running a scan, assert
/// `status`/`last_scan_at`/`health_score` reflect the actual impact
/// report, not hardcoded defaults.`
///
/// Writes a fake `latest-impact.json` with `riskLevel: "high"` and
/// asserts the dashboard reports a non-`healthy` status plus the
/// timestamp from the report.
#[tokio::test]
async fn test_api_projects_reflect_impact_report() {
    let guard = temp_layout();
    let layout = guard.layout();

    // Write a valid impact report with riskLevel "high".
    let reports_dir = layout.reports_dir();
    std::fs::create_dir_all(&reports_dir).unwrap();
    let report = serde_json::json!({
        "schemaVersion": "v1",
        "timestampUtc": "2026-06-17T09:00:00Z",
        "headHash": "abc123",
        "branchName": "main",
        "treeClean": false,
        "riskLevel": "high",
        "riskReasons": ["multiple high-risk changes"],
        "changes": [],
        "temporalCouplings": [],
        "structuralCouplings": [],
        "centralityRisks": [],
        "hotspots": [],
        "verificationResults": [],
        "dataFlowMatches": [],
        "serviceMapDelta": null,
        "knowledgeGraph": [],
        "loggingCoverageDelta": [],
        "errorHandlingDelta": [],
        "telemetryCoverageDelta": [],
        "testCoverage": [],
        "runtimeUsageDelta": [],
    });
    std::fs::write(
        reports_dir.join(ledgerful::state::reports::LATEST_IMPACT_REPORT),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();

    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/projects")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let projects = json.as_array().expect("/api/projects returns an array");
    assert!(!projects.is_empty());
    let first = &projects[0];
    let status = first["status"].as_str().expect("status is a string");
    assert_ne!(
        status, "healthy",
        "high riskLevel must not produce 'healthy' status; got {status}"
    );
    assert!(
        status == "warning" || status == "critical",
        "expected warning or critical, got {status}"
    );
    let health_score = first["health_score"].as_u64().expect("health_score is u64");
    // Per M8 opencode-review L3: pin the value. With `riskLevel: "high"`
    // and no doctor file, the formula is `100 - 60 (high) - 0 (no
    // doctor) = 40`. A regression that mapped Highâ†’30 (score 70) or
    // Highâ†’10 (score 90) would not be caught by the `health_score < 80`
    // threshold alone.
    assert_eq!(
        health_score, 40,
        "high riskLevel with no doctor results must produce health_score=40; got {health_score}"
    );
    assert_eq!(
        first["last_scan_at"].as_str(),
        Some("2026-06-17T09:00:00Z"),
        "last_scan_at must reflect the report's timestampUtc"
    );
    handle.abort();
}

/// Per-spec test 4: `/api/sync/status` with no prior sync state, assert
/// a sensible empty/never-synced response (not an error).`
///
/// Gated on the `sync` feature because the endpoint and the
/// `sync_state` table only exist with that feature enabled. With the
/// default feature set (which includes `web` but not `sync`), the
/// endpoint is not registered.
#[cfg(feature = "sync")]
#[tokio::test]
async fn test_api_sync_status_when_never_synced() {
    let guard = temp_layout();
    let layout = guard.layout();

    // No sync_state row in the DB â†’ "never synced" case.
    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/sync/status")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Per M8 review H5: device_id is Option<String>; never-synced is null.
    assert!(
        json["device_id"].is_null(),
        "device_id should be null when never synced; got {:?}",
        json["device_id"]
    );
    assert!(json["last_extract_at"].is_null());
    assert!(json["last_apply_at"].is_null());
    assert!(json["last_run_at"].is_null());
    handle.abort();
}

/// Per-spec test 5: `/api/sync/status` after `ledgerful sync init`,
/// assert device ID and timestamps appear, and that
/// `lastExtractAt`/`lastApplyAt` parse as valid ISO 8601 dates (not
/// the raw `physical_ms-logical-node_id` HLC string).`
///
/// Gated on the `sync` feature. `sync init` only writes the device
/// keypair + `device_id` to config.toml; it does *not* write to the
/// `sync_state` table. To make the test deterministic without
/// requiring a fully-wired `sync extract` flow, we directly seed the
/// `sync_state` row with a known HLC and assert the response shapes
/// are valid.
#[cfg(feature = "sync")]
#[tokio::test]
async fn test_api_sync_status_after_init() {
    use ledgerful::state::storage::StorageManager;

    let guard = temp_layout();
    let layout = guard.layout();

    // Initialize ledgerful storage and run sync init to satisfy the
    // spec's "after `ledgerful sync init`" pre-condition.
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection_mut();
    conn.execute(
        "INSERT OR REPLACE INTO sync_state (id, last_extract_hlc, last_apply_hlc, last_run_at, device_id) \
         VALUES (1, ?1, ?2, ?3, ?4)",
        rusqlite::params![
            "1750000000000-0042-test-node",
            "1750000005000-0007-test-node",
            "2026-06-17T10:00:00Z",
            "device-deadbeef"
        ],
    )
    .unwrap();

    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/sync/status")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["device_id"].as_str(), Some("device-deadbeef"));

    let last_extract = json["last_extract_at"]
        .as_str()
        .expect("last_extract_at is a string");
    // Must be ISO 8601 (RFC 3339), not the raw HLC.
    assert!(
        !last_extract.contains("1750000000000-0042"),
        "last_extract_at must be ISO 8601, not raw HLC; got {last_extract}"
    );
    chrono::DateTime::parse_from_rfc3339(last_extract)
        .expect("last_extract_at must parse as RFC 3339 / ISO 8601");

    let last_apply = json["last_apply_at"]
        .as_str()
        .expect("last_apply_at is a string");
    assert!(
        !last_apply.contains("1750000005000-0007"),
        "last_apply_at must be ISO 8601, not raw HLC; got {last_apply}"
    );
    chrono::DateTime::parse_from_rfc3339(last_apply)
        .expect("last_apply_at must parse as RFC 3339 / ISO 8601");

    // Per M8 opencode-review L2: assert last_run_at. A regression
    // that dropped `last_run_at` from `SyncStatusResponse` would not
    // be caught without this check. `chrono::DateTime<Utc>::to_rfc3339`
    // serializes UTC offsets as `+00:00` rather than the `Z` shorthand,
    // so we assert the equivalent RFC 3339 form rather than the literal
    // `Z` we seeded.
    let last_run_at = json["last_run_at"]
        .as_str()
        .expect("last_run_at is a string");
    assert!(
        last_run_at == "2026-06-17T10:00:00Z" || last_run_at == "2026-06-17T10:00:00+00:00",
        "last_run_at should round-trip the seeded RFC 3339 timestamp; got {last_run_at}"
    );

    handle.abort();
}

/// Track 0013 DoD-1: when built **without** `sync`, `/api/sync/status`
/// returns a clean `501 Not Implemented` (schema/runtime consistency â€” the
/// route is always registered, but the handler returns 501 when the
/// feature is off).
///
/// This test is `#[cfg(not(feature = "sync"))]` â€” it only compiles/runs
/// in a no-sync build. In the default CI run (which uses `--features sync`),
/// it is skipped.
#[cfg(not(feature = "sync"))]
#[tokio::test]
async fn test_api_sync_status_returns_501_without_sync_feature() {
    let guard = temp_layout();
    let layout = guard.layout();

    let (url, token, handle) = spawn_server(layout.clone()).await;
    let response = tokio::task::spawn_blocking(move || {
        match authed_get(&url, &token, "/api/sync/status").call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("unexpected error: {e}"),
        }
    })
    .await
    .unwrap();

    assert_eq!(
        response.status(),
        501,
        "no-sync build should return 501 Not Implemented for /api/sync/status"
    );
    handle.abort();
}

/// Per M8 opencode-review H1: assert the `doctor_failures` term in
/// the `health_score` formula is derived from a real
/// `doctor-results.json` written by `execute_doctor`, not a hardcoded
/// zero.
///
/// The test seeds the doctor-results file directly (which is the
/// exact file `execute_doctor` writes â€” see
/// `src/commands/doctor.rs::write_doctor_results`) with a known
/// failure count, then fetches `/api/projects` and asserts the
/// `health_score` reflects the term. We seed the file rather than
/// running the full `execute_doctor` because the doctor command is
/// heavy (it probes embedding / completion model endpoints, the
/// native graph, the Tantivy index, etc.) and depends on process CWD
/// â€” the file format and writer logic are covered by the
/// `count_doctor_failures` and `write_doctor_results` unit tests in
/// `src/commands/doctor.rs`.
#[tokio::test]
async fn test_api_projects_health_score_reflects_doctor_results() {
    let guard = temp_layout();
    let layout = guard.layout();

    // Write a valid impact report (low risk) so the only penalty in
    // the score is the doctor term. `100 - 5 (low) - 2*20 (doctor) = 55`
    // â†’ status "warning" (50 <= 55 < 80).
    let reports_dir = layout.reports_dir();
    std::fs::create_dir_all(&reports_dir).unwrap();
    let report = serde_json::json!({
        "schemaVersion": "v1",
        "timestampUtc": "2026-06-17T11:00:00Z",
        "headHash": "abc123",
        "branchName": "main",
        "treeClean": true,
        "riskLevel": "low",
        "riskReasons": [],
        "changes": [],
        "temporalCouplings": [],
        "structuralCouplings": [],
        "centralityRisks": [],
        "hotspots": [],
        "verificationResults": [],
        "dataFlowMatches": [],
        "serviceMapDelta": null,
        "knowledgeGraph": [],
        "loggingCoverageDelta": [],
    });
    std::fs::write(
        reports_dir.join("latest-impact.json"),
        serde_json::to_string(&report).unwrap(),
    )
    .unwrap();

    // Write a doctor-results.json with 2 failures, simulating what
    // `execute_doctor` writes after a run that found 2 failed checks.
    let state_subdir = layout.state_subdir();
    std::fs::create_dir_all(&state_subdir).unwrap();
    std::fs::write(
        state_subdir.join("doctor-results.json"),
        r#"{"failures": 2, "timestamp": "2026-06-17T11:00:00Z"}"#,
    )
    .unwrap();

    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/projects")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let projects = json.as_array().expect("/api/projects returns an array");
    let first = &projects[0];
    let health_score = first["health_score"].as_u64().expect("health_score is u64");
    // 100 (base) - 5 (low) - 2*20 (doctor) = 55
    assert_eq!(
        health_score, 55,
        "doctor_failures=2 with low risk must produce health_score=55; got {health_score}"
    );
    let status = first["status"].as_str().expect("status is a string");
    assert_eq!(
        status, "warning",
        "health_score 55 must produce status=warning; got {status}"
    );
    assert_eq!(
        first["last_scan_at"].as_str(),
        Some("2026-06-17T11:00:00Z"),
        "last_scan_at must reflect the report's timestampUtc"
    );
    handle.abort();
}

/// Per M8 opencode-review H1 (negative case): without a
/// `doctor-results.json` file present, the `doctor_failures` term is
/// 0 â€” the existing behavior â€” and the health_score reflects the
/// risk term only.
#[tokio::test]
async fn test_api_projects_health_score_no_doctor_file_is_clean() {
    let guard = temp_layout();
    let layout = guard.layout();

    let reports_dir = layout.reports_dir();
    std::fs::create_dir_all(&reports_dir).unwrap();
    let report = serde_json::json!({
        "schemaVersion": "v1",
        "timestampUtc": "2026-06-17T11:00:00Z",
        "headHash": "abc123",
        "branchName": "main",
        "treeClean": true,
        "riskLevel": "low",
        "riskReasons": [],
        "changes": [],
        "temporalCouplings": [],
        "structuralCouplings": [],
        "centralityRisks": [],
        "hotspots": [],
        "verificationResults": [],
        "dataFlowMatches": [],
        "serviceMapDelta": null,
        "knowledgeGraph": [],
        "loggingCoverageDelta": [],
    });
    std::fs::write(
        reports_dir.join("latest-impact.json"),
        serde_json::to_string(&report).unwrap(),
    )
    .unwrap();

    // No doctor-results.json written.

    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/projects")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let first = &json.as_array().unwrap()[0];
    let health_score = first["health_score"].as_u64().expect("health_score is u64");
    // 100 - 5 (low) - 0 (no doctor file) = 95 â†’ healthy
    assert_eq!(
        health_score, 95,
        "no doctor file with low risk must produce health_score=95; got {health_score}"
    );
    assert_eq!(first["status"].as_str(), Some("healthy"));
    handle.abort();
}

/// Regression for CG-F18: a clean-tree tombstone (written by `scan
/// --impact` / `impact` when there are no pending changes) is a known-good
/// state, not an unparseable report. It must not fall into the 40-point
/// "unknown report" penalty the way a genuinely corrupt file does.
#[tokio::test]
async fn test_api_projects_health_score_clean_tree_tombstone_not_penalized() {
    let guard = temp_layout();
    let layout = guard.layout();

    let reports_dir = layout.reports_dir();
    std::fs::create_dir_all(&reports_dir).unwrap();
    let tombstone = serde_json::json!({
        "status": "clean_tree",
        "headHash": "abc123",
        "branchName": "main",
        "schemaVersion": "v1",
        "treeClean": true,
        "timestampUtc": "2026-06-17T11:00:00Z",
        "changes": [],
    });
    std::fs::write(
        reports_dir.join("latest-impact.json"),
        serde_json::to_string(&tombstone).unwrap(),
    )
    .unwrap();

    // No doctor-results.json written.

    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/projects")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let first = &json.as_array().unwrap()[0];
    let health_score = first["health_score"].as_u64().expect("health_score is u64");
    // 100 - 0 (clean-tree tombstone, not penalized) - 0 (no doctor file) = 100 -> healthy
    assert_eq!(
        health_score, 100,
        "a clean-tree tombstone must not be treated as a corrupt/unknown report; got {health_score}"
    );
    assert_eq!(first["status"].as_str(), Some("healthy"));
    assert_eq!(
        first["last_scan_at"].as_str(),
        Some("2026-06-17T11:00:00Z"),
        "last_scan_at must reflect the tombstone's timestampUtc"
    );
    handle.abort();
}

/// Per M8 opencode-review H1 (corrupt-report case): when the impact
/// report exists but is unparseable, the score is the 40-point
/// "unknown" penalty (not 100, which was the M1 review's
/// pre-fix behavior).
#[tokio::test]
async fn test_api_projects_health_score_corrupt_report_penalized() {
    let guard = temp_layout();
    let layout = guard.layout();

    let reports_dir = layout.reports_dir();
    std::fs::create_dir_all(&reports_dir).unwrap();
    std::fs::write(
        reports_dir.join("latest-impact.json"),
        "{ this is not valid JSON: oh no",
    )
    .unwrap();

    let (url, token, handle) = spawn_server(layout.clone()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/projects")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let first = &json.as_array().unwrap()[0];
    let health_score = first["health_score"].as_u64().expect("health_score is u64");
    // 100 - 40 (corrupt-report penalty) - 0 (no doctor) = 60 â†’ warning
    assert_eq!(
        health_score, 60,
        "corrupt impact report must produce health_score=60 (M1 fix); got {health_score}"
    );
    assert_eq!(first["status"].as_str(), Some("warning"));
    assert!(
        first["last_scan_at"].is_null(),
        "corrupt report must not surface a last_scan_at; got {:?}",
        first["last_scan_at"]
    );
    handle.abort();
}

/// Per M8 opencode-review H2: a `FederatedSchema` published by a
/// pre-M8 binary (no `author` field on `FederatedLedgerEntry`)
/// deserializes successfully and imports with `author` coalesced to
/// `"unknown"`. This is the round-trip test the review called out
/// as missing.
#[tokio::test]
async fn test_federated_schema_pre_m8_imports_unknown_author() {
    use ledgerful::federated::schema::FederatedSchema;
    use ledgerful::ledger::federation::import_federated_entries;
    use ledgerful::state::storage::StorageManager;
    use std::path::Path;

    // Build a pre-M8-style FederatedSchema JSON. The key
    // characteristic is that `ledger` entries are serialized
    // *without* an `author` field â€” simulating what a pre-M8 binary
    // using `with_ledger(...)` would have written.
    let pre_m8_json = r#"{
        "schema_version": "1.0",
        "repo_name": "legacy-sibling",
        "public_interfaces": [
            { "symbol": "add", "file": "src/lib.rs", "kind": "function" }
        ],
        "ledger": [
            {
                "tx_id": "tx-legacy-001",
                "category": "FEATURE",
                "entry_type": "IMPLEMENTATION",
                "entity": "src/legacy.rs",
                "change_type": "CREATE",
                "summary": "Pre-M8 entry from a legacy sibling",
                "reason": "No author captured",
                "is_breaking": false,
                "committed_at": "2026-06-17T09:00:00Z"
            }
        ]
    }"#;

    // Deserialize â€” must succeed even though `author` is missing.
    let parsed: FederatedSchema =
        serde_json::from_str(pre_m8_json).expect("pre-M8 schema must deserialize");
    assert_eq!(parsed.schema_version, "1.0");
    // The legacy version is still accepted by validate() (H2 fix).
    parsed
        .validate()
        .expect("validate() must accept legacy 1.0 schemas");
    let entries = parsed.ledger.expect("ledger must be present");
    assert_eq!(entries.len(), 1);
    // `#[serde(default)]` yields `author == ""` for a missing field.
    assert_eq!(
        entries[0].author, "",
        "pre-M8 entries must deserialize with author='' (H2 fix); got {:?}",
        entries[0].author
    );

    // Now exercise the import path: set up storage, run
    // `import_federated_entries`, and assert the row was inserted
    // with `author = "unknown"` (the coalesce step in
    // `import_federated_entries`).
    let guard = temp_layout();
    let layout = guard.layout();
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let repo_root = Path::new(layout.root.as_std_path());

    import_federated_entries(
        storage.get_connection_mut(),
        repo_root,
        "legacy-sibling",
        &entries,
    )
    .expect("import_federated_entries must succeed");

    // Verify the coalesced author was written.
    let conn = storage.get_connection();
    let imported_author: String = conn
        .query_row(
            "SELECT author FROM ledger_entries WHERE tx_id = ?1 AND origin = 'SIBLING'",
            ["tx-legacy-001"],
            |row| row.get(0),
        )
        .expect("query coalesced author");
    assert_eq!(
        imported_author, "unknown",
        "pre-M8 entries with empty author must coalesce to 'unknown' (H2 fix); got {imported_author}"
    );

    // Also exercise the validate() reverse case: a schema with an
    // unknown version is rejected.
    let bad = FederatedSchema {
        schema_version: "2.0".to_string(),
        repo_name: parsed.repo_name.clone(),
        public_interfaces: parsed.public_interfaces.clone(),
        ledger: Some(entries.clone()),
        generated_at: String::new(),
        binary_version: String::new(),
    };
    assert!(
        bad.validate().is_err(),
        "validate() must reject unknown schema versions"
    );

    // And a 1.1 (post-M8) schema with a present author survives
    // round-trip via the same deserializer and validate() chain.
    let post_m8_json = serde_json::json!({
        "schema_version": "1.1",
        "repo_name": "modern-sibling",
        "public_interfaces": [
            { "symbol": "sub", "file": "src/lib.rs", "kind": "function" }
        ],
        "ledger": [
            {
                "tx_id": "tx-modern-001",
                "category": "FEATURE",
                "entry_type": "IMPLEMENTATION",
                "entity": "src/modern.rs",
                "change_type": "CREATE",
                "summary": "Post-M8 entry",
                "reason": "Author captured at commit",
                "is_breaking": false,
                "committed_at": "2026-06-17T10:00:00Z",
                "author": "Modern Dev"
            }
        ]
    })
    .to_string();
    let parsed_post: FederatedSchema =
        serde_json::from_str(&post_m8_json).expect("post-M8 schema must deserialize");
    parsed_post
        .validate()
        .expect("validate() must accept 1.1 schemas");
    assert_eq!(parsed_post.ledger.as_ref().unwrap()[0].author, "Modern Dev");
}

// ---------------------------------------------------------------------------
// Track E1: Verification Dashboard APIs
// ---------------------------------------------------------------------------

/// Seed `verification_runs` and `verification_results` rows with known
/// timestamps and pass/fail outcomes into the ledger DB at `layout`.
fn seed_verification_runs(layout: &Layout) {
    use ledgerful::state::storage::StorageManager;
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection();

    // Two runs on 2026-06-17 (one pass, one fail), one run on 2026-06-18
    // (pass). The per-step results populate `verification_results` with
    // known durations / exit codes so `/api/verify/steps` averages and
    // pass rates are deterministic.
    //
    // Run 1 (2026-06-17, pass): cargo test 100ms pass, cargo clippy 200ms pass.
    // Run 2 (2026-06-17, fail): cargo test 300ms fail, cargo clippy 200ms pass.
    // Run 3 (2026-06-18, pass): cargo test 400ms pass, cargo clippy 500ms pass.
    //
    // `plan_json` is a serialized `VerificationPlan` (camelCase) carrying a
    // friendly `description` per step â€” `/api/verify/steps` reads this to
    // populate `name` (falling back to `command`).
    let plan_json = r#"{"steps":[{"command":"cargo test","timeoutSecs":120,"description":"Run the test suite"},{"command":"cargo clippy","timeoutSecs":120,"description":"Lint with clippy"}]}"#;
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
         VALUES ('2026-06-17T10:00:00Z', ?1, 1)",
        rusqlite::params![plan_json],
    )
    .unwrap();
    let run1 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
         VALUES ('2026-06-17T11:00:00Z', ?1, 0)",
        rusqlite::params![plan_json],
    )
    .unwrap();
    let run2 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
         VALUES ('2026-06-18T10:00:00Z', ?1, 1)",
        rusqlite::params![plan_json],
    )
    .unwrap();
    let run3 = conn.last_insert_rowid();

    for (run_id, command, exit, dur) in [
        (run1, "cargo test", 0i32, 100i64),
        (run1, "cargo clippy", 0, 200),
        (run2, "cargo test", 1, 300),
        (run2, "cargo clippy", 0, 200),
        (run3, "cargo test", 0, 400),
        (run3, "cargo clippy", 0, 500),
    ] {
        conn.execute(
            "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated) \
             VALUES (?1, ?2, ?3, ?4, 0)",
            rusqlite::params![run_id, command, exit, dur],
        )
        .unwrap();
    }
}

#[tokio::test]
async fn test_verify_health_no_runs_is_degraded() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/verify/health")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"].as_str(), Some("DEGRADED"));
    // The field MUST be present as `null` (not omitted) so the frontend
    // contract `lastRunAt: string | null` is satisfied â€” the dashboard empty
    // state depends on a present key, not a missing one. serde emits no
    // space after the colon, so match the compact form.
    let body_compact = body.replace(' ', "");
    assert!(
        body_compact.contains("\"lastRunAt\":null"),
        "raw body must contain `lastRunAt` as null; got: {body}"
    );
    assert!(json["lastRunAt"].is_null());
    assert!(
        json["message"]
            .as_str()
            .unwrap_or("")
            .contains("No verification runs recorded")
    );
    handle.abort();
}

#[tokio::test]
async fn test_verify_health_failing_when_latest_run_failed() {
    let guard = temp_layout();
    seed_verification_runs(&guard.layout());

    // Add a newer failing run so the latest run is FAILING.
    {
        use ledgerful::state::storage::StorageManager;
        let layout = guard.layout();
        layout.ensure_state_dir().unwrap();
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).unwrap();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
             VALUES ('2026-06-19T10:00:00Z', '{}', 0)",
            [],
        )
        .unwrap();
    }

    let (url, token, handle) = spawn_server(guard.layout()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/verify/health")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"].as_str(), Some("FAILING"));
    assert_eq!(json["lastRunAt"].as_str(), Some("2026-06-19T10:00:00Z"));
    handle.abort();
}

#[tokio::test]
async fn test_verify_health_healthy_when_latest_run_passed_recently() {
    let guard = temp_layout();
    {
        use ledgerful::state::storage::StorageManager;
        let layout = guard.layout();
        layout.ensure_state_dir().unwrap();
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).unwrap();
        let conn = storage.get_connection();
        // Use the current time so the run is not stale.
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
             VALUES (?1, '{}', 1)",
            rusqlite::params![now],
        )
        .unwrap();
    }

    let (url, token, handle) = spawn_server(guard.layout()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/verify/health")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"].as_str(), Some("HEALTHY"));
    assert!(json["lastRunAt"].is_string());
    handle.abort();
}

#[tokio::test]
async fn test_verify_history_aggregates_by_date() {
    let guard = temp_layout();
    seed_verification_runs(&guard.layout());
    let (url, token, handle) = spawn_server(guard.layout()).await;

    // Use a large window so the seeded 2026-06-17/18 dates are included
    // regardless of the test run date.
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/verify/history?days=3650")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let points = json.as_array().expect("history is an array");
    // Two distinct dates: 2026-06-17 (1 pass + 1 fail) and 2026-06-18 (1 pass).
    let by_date: std::collections::HashMap<&str, (u64, u64)> = points
        .iter()
        .map(|p| {
            let date = p["date"].as_str().unwrap();
            let passed = p["passed"].as_u64().unwrap();
            let failed = p["failed"].as_u64().unwrap();
            (date, (passed, failed))
        })
        .collect();
    assert_eq!(by_date.get("2026-06-17"), Some(&(1, 1)));
    assert_eq!(by_date.get("2026-06-18"), Some(&(1, 0)));
    // Ascending order.
    let dates: Vec<&str> = points.iter().map(|p| p["date"].as_str().unwrap()).collect();
    let mut sorted = dates.clone();
    sorted.sort();
    assert_eq!(dates, sorted);
    // camelCase keys.
    assert!(points[0].get("passed").is_some());
    assert!(points[0].get("failed").is_some());
    handle.abort();
}

#[tokio::test]
async fn test_verify_steps_aggregates_per_command() {
    let guard = temp_layout();
    seed_verification_runs(&guard.layout());
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/verify/steps")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let steps = json.as_array().expect("steps is an array");
    // Sorted by command ascending: "cargo clippy" before "cargo test".
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["id"].as_str(), Some("cargo clippy"));
    // `name` is the friendly `description` from the seeded plan_json.
    assert_eq!(steps[0]["name"].as_str(), Some("Lint with clippy"));
    // cargo clippy: 3 results, all pass -> 100% pass rate, avg (200+200+500)/3 = 300,
    // recent failures in last 10 runs = 0.
    assert_eq!(steps[0]["averageDurationMs"].as_f64(), Some(300.0));
    assert_eq!(steps[0]["passRatePercent"].as_f64(), Some(100.0));
    assert_eq!(steps[0]["recentFailures"].as_u64(), Some(0));
    assert_eq!(steps[0]["lastRunAt"].as_str(), Some("2026-06-18T10:00:00Z"));

    // cargo test: 3 results, 2 pass / 1 fail -> 66.67% pass rate,
    // avg (100+300+400)/3 = 266.666..., recent failures in last 10 runs = 1
    // (the failing run2 is within the last 10 runs).
    assert_eq!(steps[1]["id"].as_str(), Some("cargo test"));
    assert_eq!(steps[1]["name"].as_str(), Some("Run the test suite"));
    assert_eq!(steps[1]["passRatePercent"].as_f64(), Some(66.67));
    assert_eq!(steps[1]["recentFailures"].as_u64(), Some(1));
    assert!(steps[1]["averageDurationMs"].as_f64().unwrap() > 266.0);
    assert!(steps[1]["averageDurationMs"].as_f64().unwrap() < 267.0);
    assert_eq!(steps[1]["lastRunAt"].as_str(), Some("2026-06-18T10:00:00Z"));
    handle.abort();
}

#[tokio::test]
async fn test_verify_steps_empty_when_no_db() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/verify/steps")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json.as_array().unwrap().is_empty());
    handle.abort();
}

#[tokio::test]
async fn test_verify_steps_name_falls_back_to_command_without_plan_json() {
    // When `plan_json` is absent/empty, `name` must fall back to `command`
    // (graceful degradation â€” no parse failure can break the endpoint).
    let guard = temp_layout();
    {
        use ledgerful::state::storage::StorageManager;
        let layout = guard.layout();
        layout.ensure_state_dir().unwrap();
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).unwrap();
        let conn = storage.get_connection();
        // No plan_json (NULL) and a malformed plan_json row â€” both must be
        // skipped without failing the endpoint.
        conn.execute(
            "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
             VALUES ('2026-06-19T10:00:00Z', NULL, 1)",
            [],
        )
        .unwrap();
        let run1 = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
             VALUES ('2026-06-19T11:00:00Z', 'not-valid-json', 1)",
            [],
        )
        .unwrap();
        let run2 = conn.last_insert_rowid();
        for (run_id, command, exit, dur) in [
            (run1, "cargo fmt --check", 0i32, 50i64),
            (run2, "cargo fmt --check", 0, 70),
        ] {
            conn.execute(
                "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated) \
                 VALUES (?1, ?2, ?3, ?4, 0)",
                rusqlite::params![run_id, command, exit, dur],
            )
            .unwrap();
        }
    }

    let (url, token, handle) = spawn_server(guard.layout()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/verify/steps")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let steps = json.as_array().expect("steps is an array");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["id"].as_str(), Some("cargo fmt --check"));
    // No usable plan_json â†’ name falls back to command.
    assert_eq!(steps[0]["name"].as_str(), Some("cargo fmt --check"));
    handle.abort();
}

#[tokio::test]
async fn test_verify_steps_name_strips_predicted_impact_traceability() {
    // The plan builder concatenates " | Predicted impact (<reason>) on <file>"
    // traceability segments onto a friendly label (for `verify --explain`).
    // The dashboard `name` must surface ONLY the friendly first segment, and
    // fall back to the command when the description has no friendly prefix
    // (a command that only appeared via predicted-impact rules).
    let guard = temp_layout();
    {
        use ledgerful::state::storage::StorageManager;
        let layout = guard.layout();
        layout.ensure_state_dir().unwrap();
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).unwrap();
        let conn = storage.get_connection();
        // One step has a friendly "From rules:" prefix plus predicted-impact
        // annotations; the other has ONLY predicted-impact annotations.
        let plan_json = r#"{"steps":[
            {"command":"cargo clippy --all-targets --all-features -- -D warnings","timeoutSecs":120,
             "description":"From rules: cargo clippy --all-targets --all-features -- -D warnings | Predicted impact (CallGraph) on src/commands/ask.rs | Predicted impact (Temporal) on src/bridge/mod.rs"},
            {"command":"cargo test --test integration -- --test-threads=1","timeoutSecs":120,
             "description":"Predicted impact (Temporal) on src/bridge/export.rs | Predicted impact (Temporal) on src/bridge/mod.rs"}
        ]}"#;
        conn.execute(
            "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
             VALUES ('2026-06-19T10:00:00Z', ?1, 1)",
            rusqlite::params![plan_json],
        )
        .unwrap();
        let run_id = conn.last_insert_rowid();
        for command in [
            "cargo clippy --all-targets --all-features -- -D warnings",
            "cargo test --test integration -- --test-threads=1",
        ] {
            conn.execute(
                "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated) \
                 VALUES (?1, ?2, 0, 100, 0)",
                rusqlite::params![run_id, command],
            )
            .unwrap();
        }
    }

    let (url, token, handle) = spawn_server(guard.layout()).await;
    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/verify/steps")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let steps = json.as_array().expect("steps is an array");
    // Sorted by command ascending: "cargo clippy ..." before "cargo test ...".
    assert_eq!(steps.len(), 2);
    // Friendly prefix kept, predicted-impact annotations stripped.
    assert_eq!(
        steps[0]["name"].as_str(),
        Some("From rules: cargo clippy --all-targets --all-features -- -D warnings")
    );
    assert!(
        !steps[0]["name"]
            .as_str()
            .unwrap()
            .contains("Predicted impact"),
        "predicted-impact annotations must not leak into the dashboard name"
    );
    // No friendly prefix â†’ fall back to the command.
    assert_eq!(
        steps[1]["name"].as_str(),
        Some("cargo test --test integration -- --test-threads=1")
    );
    handle.abort();
}

// ---------------------------------------------------------------------------
// Track E2: Compliance Dashboard APIs
// ---------------------------------------------------------------------------

/// Helper: fetch `/api/compliance/summary` and parse the JSON.
async fn fetch_compliance_summary(url: &str, token: &str) -> serde_json::Value {
    let url = url.to_string();
    let token = token.to_string();
    tokio::task::spawn_blocking(move || {
        match authed_get(&url, &token, "/api/compliance/summary").call() {
            Ok(resp) => resp.into_string().unwrap(),
            Err(ureq::Error::Status(code, resp)) => {
                panic!(
                    "compliance summary HTTP {code}: {}",
                    resp.into_string().unwrap_or_default()
                )
            }
            Err(e) => panic!("compliance summary transport error: {e}"),
        }
    })
    .await
    .unwrap()
    .parse()
    .unwrap()
}

/// Helper: fetch `/api/compliance/signatures` and parse the JSON array.
async fn fetch_compliance_signatures(url: &str, token: &str) -> serde_json::Value {
    let url = url.to_string();
    let token = token.to_string();
    tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/compliance/signatures")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap()
    .parse()
    .unwrap()
}

/// VALID: mint a real signature via `sign_ledger_entry_in` and seed the row with
/// the returned (sig, pub). The signed payload's `committed_at` and
/// `category` match the seeded row, so `verify_signature` must succeed.
#[tokio::test]
async fn test_compliance_valid_signature() {
    use ledgerful::ledger::crypto::sign_ledger_entry_in;

    let home = tempfile::tempdir().unwrap();
    let keys_dir = home.path().join(".ledgerful").join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let guard = temp_layout();
    let layout = guard.layout();
    let tx_id = "tx-compliance-valid-001";
    let committed_at = "2026-06-20T10:00:00Z";
    let summary = "Add compliance endpoints";
    let reason = "Track E2 requires real signature verification";
    // `seed_ledger_entry_with_ts` stores `Category::Feature` whose `Display`
    // impl yields `"FEATURE"`, so the signed category must match.
    let (sig, pub_key) =
        sign_ledger_entry_in(&keys_dir, tx_id, "FEATURE", summary, reason, committed_at)
            .expect("sign_ledger_entry_in should succeed");
    let sig = sig.expect("signature should be Some");
    let pub_key = pub_key.expect("public key should be Some");

    seed_ledger_entry_with_ts(
        &layout,
        tx_id,
        summary,
        reason,
        committed_at,
        Some(&sig),
        Some(&pub_key),
    );

    let (url, token, handle) = spawn_server(layout).await;
    let summary_json = fetch_compliance_summary(&url, &token).await;
    assert_eq!(summary_json["totalSigned"].as_u64(), Some(1));
    assert_eq!(summary_json["validityPercent"].as_f64(), Some(100.0));
    assert_eq!(
        summary_json["lastAuditAt"].as_str(),
        Some(committed_at),
        "lastAuditAt must be the committed_at of the VALID entry"
    );
    assert_eq!(summary_json["hotspotDeltaPercent"].as_f64(), Some(0.0));

    let sigs = fetch_compliance_signatures(&url, &token).await;
    let arr = sigs.as_array().expect("signatures is an array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["txId"].as_str(), Some(tx_id));
    assert_eq!(arr[0]["status"].as_str(), Some("VALID"));
    assert_eq!(arr[0]["category"].as_str(), Some("FEATURE"));
    assert_eq!(arr[0]["committedAt"].as_str(), Some(committed_at));
    handle.abort();
}

/// INVALID (tampered): seed a row with a real signature but a DIFFERENT
/// summary than was signed. `verify_signature` must fail â†’ `INVALID` and
/// `validityPercent < 100`.
#[tokio::test]
async fn test_compliance_tampered_signature_is_invalid() {
    use ledgerful::ledger::crypto::sign_ledger_entry_in;

    let home = tempfile::tempdir().unwrap();
    let keys_dir = home.path().join(".ledgerful").join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let guard = temp_layout();
    let layout = guard.layout();
    let tx_id = "tx-compliance-tampered-001";
    let committed_at = "2026-06-20T11:00:00Z";
    let signed_summary = "Original signed summary";
    let stored_summary = "Tampered summary";
    let reason = "reason";
    let (sig, pub_key) = sign_ledger_entry_in(
        &keys_dir,
        tx_id,
        "FEATURE",
        signed_summary,
        reason,
        committed_at,
    )
    .unwrap();
    let sig = sig.unwrap();
    let pub_key = pub_key.unwrap();

    seed_ledger_entry_with_ts(
        &layout,
        tx_id,
        stored_summary,
        reason,
        committed_at,
        Some(&sig),
        Some(&pub_key),
    );

    let (url, token, handle) = spawn_server(layout).await;
    let summary_json = fetch_compliance_summary(&url, &token).await;
    // Signed but does not verify â†’ counts toward totalSigned but NOT valid.
    assert_eq!(summary_json["totalSigned"].as_u64(), Some(1));
    assert_eq!(summary_json["validityPercent"].as_f64(), Some(0.0));
    assert!(
        summary_json["lastAuditAt"].is_null(),
        "no VALID entries â†’ lastAuditAt must be null"
    );

    let sigs = fetch_compliance_signatures(&url, &token).await;
    let arr = sigs.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["status"].as_str(), Some("INVALID"));
    handle.abort();
}

/// SKIPPED: unsigned row (`signature=None, public_key=None`) with the default
/// config (`intent.require_signing=false`) â†’ `SKIPPED`, and does NOT count as
/// invalid.
#[tokio::test]
async fn test_compliance_unsigned_skipped_when_not_required() {
    let guard = temp_layout();
    let layout = guard.layout();
    let tx_id = "tx-compliance-skipped-001";
    let committed_at = "2026-06-20T12:00:00Z";
    seed_ledger_entry_with_ts(
        &layout,
        tx_id,
        "Unsigned entry",
        "reason",
        committed_at,
        None,
        None,
    );

    let (url, token, handle) = spawn_server(layout).await;
    let summary_json = fetch_compliance_summary(&url, &token).await;
    assert_eq!(summary_json["totalSigned"].as_u64(), Some(0));
    assert_eq!(summary_json["validityPercent"].as_f64(), Some(0.0));
    assert!(summary_json["lastAuditAt"].is_null());

    let sigs = fetch_compliance_signatures(&url, &token).await;
    let arr = sigs.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["status"].as_str(), Some("SKIPPED"));
    handle.abort();
}

/// UNSIGNED â†’ INVALID when `intent.require_signing=true`: an unsigned row
/// with signing required must classify as `INVALID`.
#[tokio::test]
async fn test_compliance_unsigned_invalid_when_required() {
    let guard = temp_layout();
    let layout = guard.layout();
    let tx_id = "tx-compliance-required-001";
    let committed_at = "2026-06-20T13:00:00Z";
    write_require_signing_config(&layout, true);
    seed_ledger_entry_with_ts(
        &layout,
        tx_id,
        "Unsigned entry under require_signing",
        "reason",
        committed_at,
        None,
        None,
    );

    let (url, token, handle) = spawn_server(layout).await;
    let sigs = fetch_compliance_signatures(&url, &token).await;
    let arr = sigs.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0]["status"].as_str(),
        Some("INVALID"),
        "unsigned entry with require_signing=true must be INVALID"
    );

    let summary_json = fetch_compliance_summary(&url, &token).await;
    assert_eq!(summary_json["totalSigned"].as_u64(), Some(0));
    assert_eq!(summary_json["validityPercent"].as_f64(), Some(0.0));
    handle.abort();
}

/// Empty state (no DB): `summary` is the zero/null payload and `signatures`
/// is `[]`. Must be HTTP 200, not an error.
#[tokio::test]
async fn test_compliance_empty_state_no_db() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let summary_json = fetch_compliance_summary(&url, &token).await;
    assert_eq!(summary_json["totalSigned"].as_u64(), Some(0));
    assert_eq!(summary_json["validityPercent"].as_f64(), Some(0.0));
    assert!(summary_json["lastAuditAt"].is_null());
    assert_eq!(summary_json["hotspotDeltaPercent"].as_f64(), Some(0.0));

    let sigs = fetch_compliance_signatures(&url, &token).await;
    assert!(sigs.as_array().unwrap().is_empty());
    handle.abort();
}

/// `lastAuditAt` must serialize as `null` (present, not omitted) in the empty
/// state â€” the frontend contract is `lastAuditAt: string | null`.
#[tokio::test]
async fn test_compliance_summary_last_audit_at_serializes_as_null_in_empty_state() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let body = tokio::task::spawn_blocking(move || {
        authed_get(&url, &token, "/api/compliance/summary")
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    })
    .await
    .unwrap();

    let compact = body.replace(' ', "");
    assert!(
        compact.contains("\"lastAuditAt\":null"),
        "raw body must contain `lastAuditAt` as null; got: {body}"
    );
    handle.abort();
}

/// `hotspotDeltaPercent`: seed two `hotspot_history` snapshots with different
/// total row counts and assert the computed percent delta.
#[rstest::rstest]
#[case::increase(4, 5, 25.0)]
#[case::single_snapshot(1, 0, 0.0)]
#[case::decrease(5, 4, -20.0)]
#[tokio::test]
async fn test_compliance_hotspot_delta_percent(
    #[case] old_count: usize,
    #[case] new_count: usize,
    #[case] expected_percent: f64,
) {
    use ledgerful::state::storage::StorageManager;

    let guard = temp_layout();
    let layout = guard.layout();
    // Use `StorageManager::init` (full migrations) rather than
    // `seed_ledger_entry_with_ts` (minimal raw tables) so the `hotspot_history`
    // table is created by its migration. No ledger entry is needed â€” the
    // summary endpoint reads hotspot delta independently of the ledger.
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection();

    for i in 0..old_count {
        conn.execute(
            "INSERT INTO hotspot_history \
             (file_path, score, display_score, complexity, frequency, timestamp) \
             VALUES (?1, 1.0, 1.0, 1, 1.0, '2026-06-19T09:00:00Z')",
            rusqlite::params![format!("src/old{i}.rs")],
        )
        .unwrap();
    }
    for i in 0..new_count {
        conn.execute(
            "INSERT INTO hotspot_history \
             (file_path, score, display_score, complexity, frequency, timestamp) \
             VALUES (?1, 2.0, 2.0, 2, 2.0, '2026-06-20T09:00:00Z')",
            rusqlite::params![format!("src/new{i}.rs")],
        )
        .unwrap();
    }

    let (url, token, handle) = spawn_server(layout).await;
    let summary_json = fetch_compliance_summary(&url, &token).await;
    assert_eq!(
        summary_json["hotspotDeltaPercent"].as_f64(),
        Some(expected_percent),
        "expected {} percent delta ({}->{} hotspots); got {:?}",
        expected_percent,
        old_count,
        new_count,
        summary_json["hotspotDeltaPercent"]
    );
    handle.abort();
}

// ---------------------------------------------------------------------------
// Track E3: SOC2 Evidence Export API
// ---------------------------------------------------------------------------

/// Fetch `/api/compliance/export?token=...` and return the raw zip bytes +
/// the HTTP response headers (content-type, content-disposition). Panics on
/// non-200 so each test sees the failing status.
async fn fetch_soc2_export(url: &str, token: &str) -> (Vec<u8>, String, String) {
    let url = url.to_string();
    let token = token.to_string();
    tokio::task::spawn_blocking(move || {
        let resp = authed_get(&url, &token, "/api/compliance/export")
            .call()
            .unwrap_or_else(|e| panic!("export request failed: {e}"));
        let status = resp.status();
        let content_type = resp.header("content-type").unwrap_or_default().to_string();
        let content_disposition = resp
            .header("content-disposition")
            .unwrap_or_default()
            .to_string();
        let mut body = Vec::new();
        use std::io::Read;
        resp.into_reader().read_to_end(&mut body).unwrap();
        assert_eq!(status, 200, "export must return 200; got {status}");
        (body, content_type, content_disposition)
    })
    .await
    .unwrap()
}

/// Seed an ADR ledger entry (entry_type = ARCHITECTURE) so
/// `get_adr_entries` returns it. Returns the tx_id used.
fn seed_adr_ledger_entry(layout: &Layout, tx_id: &str, summary: &str, committed_at: &str) {
    use ledgerful::state::storage::StorageManager;
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO transactions \
         (tx_id, status, category, entity, entity_normalized, session_id, source, started_at) \
         VALUES (?1, 'COMMITTED', 'ARCHITECTURE', 'entity', 'entity', 'test', 'test', ?2)",
        rusqlite::params![tx_id, committed_at],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries \
         (tx_id, category, entry_type, entity, entity_normalized, change_type, \
          summary, reason, is_breaking, committed_at, origin, author, observed) \
         VALUES (?1, 'ARCHITECTURE', 'ARCHITECTURE', 'entity', 'entity', 'CREATE', \
                 ?2, 'reason', 0, ?3, 'LOCAL', 'Test User', NULL)",
        rusqlite::params![tx_id, summary, committed_at],
    )
    .unwrap();
}

/// Seed one verification run + one result row with a known command.
fn seed_one_verification_run(layout: &Layout, timestamp: &str, command: &str, exit_code: i32) {
    use ledgerful::state::storage::StorageManager;
    layout.ensure_state_dir().unwrap();
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) \
         VALUES (?1, '{}', 1)",
        rusqlite::params![timestamp],
    )
    .unwrap();
    let run_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated) \
         VALUES (?1, ?2, ?3, 123, 0)",
        rusqlite::params![run_id, command, exit_code],
    )
    .unwrap();
}

/// Read a named file out of a zip archive into a `Vec<u8>`.
fn read_zip_file(body: &[u8], name: &str) -> Vec<u8> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(body)).unwrap();
    let mut buf = Vec::new();
    let mut file = archive
        .by_name(name)
        .unwrap_or_else(|e| panic!("zip must contain {name}: {e}"));
    file.read_to_end(&mut buf).unwrap();
    buf
}

/// Collect all file names in a zip archive, sorted for determinism.
fn list_zip_files(body: &[u8]) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(body)).unwrap();
    (0..archive.len())
        .map(|i| {
            let file = archive.by_index(i).unwrap();
            file.name().to_string()
        })
        .collect()
}

/// Verify the manifest signature against the manifest bytes + pub key.
/// Asserts the signature is valid (proves tamper-evidence).
fn assert_signature_verifies(body: &[u8]) {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let manifest_json = read_zip_file(body, "manifest.json");
    let sig_bytes = read_zip_file(body, "manifest.sig");
    let pub_bytes = read_zip_file(body, "manifest.pub");
    assert_eq!(sig_bytes.len(), 64, "manifest.sig must be 64 bytes");
    assert_eq!(pub_bytes.len(), 32, "manifest.pub must be 32 bytes");
    let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().unwrap();
    let pub_arr: [u8; 32] = pub_bytes.as_slice().try_into().unwrap();
    let signature = Signature::from_bytes(&sig_arr);
    let verifying_key = VerifyingKey::from_bytes(&pub_arr).unwrap();
    assert!(
        verifying_key.verify(&manifest_json, &signature).is_ok(),
        "manifest.sig must verify against manifest.pub over manifest.json bytes"
    );
}

/// Re-SHA-256 the named file's bytes from the archive and return hex.
fn hash_zip_file(body: &[u8], name: &str) -> String {
    use sha2::{Digest, Sha256};
    let bytes = read_zip_file(body, name);
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
}

/// Seeded export: ledger entry, verification run, and an ADR. Asserts the
/// zip is well-formed, the manifest's `ledger.csv` sha256 matches a re-hash,
/// the Ed25519 signature verifies, and the seeded content is present in the
/// right files.
#[tokio::test]
async fn test_soc2_export_seeded() {
    use ledgerful::ledger::crypto::sign_ledger_entry_in;

    let home = tempfile::tempdir().unwrap();
    let keys_dir = home.path().join(".ledgerful").join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let guard = temp_layout();
    let layout = guard.layout();
    let tx_id = "tx-soc2-seeded-001";
    let committed_at = "2026-06-20T10:00:00Z";
    let summary = "Add SOC2 export endpoint";
    let reason = "Track E3 requires a tamper-evident export";
    let (sig, pub_key) =
        sign_ledger_entry_in(&keys_dir, tx_id, "FEATURE", summary, reason, committed_at)
            .expect("sign_ledger_entry_in should succeed");
    seed_ledger_entry_with_ts(
        &layout,
        tx_id,
        summary,
        reason,
        committed_at,
        sig.as_deref(),
        pub_key.as_deref(),
    );
    seed_one_verification_run(&layout, "2026-06-20T09:00:00Z", "cargo nextest run", 0);
    seed_adr_ledger_entry(
        &layout,
        "tx-soc2-adr-001",
        "Use Ed25519 for export signatures",
        "2026-06-20T08:00:00Z",
    );

    let (url, token, handle) = spawn_server(layout).await;
    let (body, content_type, content_disposition) = fetch_soc2_export(&url, &token).await;

    assert!(
        content_type.starts_with("application/zip"),
        "content-type must be application/zip; got {content_type}"
    );
    assert!(
        content_disposition.contains("attachment"),
        "content-disposition must contain attachment; got {content_disposition}"
    );

    let names = list_zip_files(&body);
    for required in [
        "manifest.json",
        "manifest.sig",
        "manifest.pub",
        "ledger.csv",
        "verification_history.csv",
        "chain_head.json",
    ] {
        assert!(
            names.iter().any(|n| n == required),
            "zip must contain {required}; got {names:?}"
        );
    }
    let adr_files: Vec<_> = names.iter().filter(|n| n.starts_with("adr/")).collect();
    assert!(
        !adr_files.is_empty(),
        "zip must contain at least one adr/*.md; got {names:?}"
    );

    // manifest.json: parse, find ledger.csv entry, re-hash and compare.
    let manifest_bytes = read_zip_file(&body, "manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let files = manifest["files"].as_array().expect("files is array");
    let ledger_entry = files
        .iter()
        .find(|f| f["name"].as_str() == Some("ledger.csv"))
        .expect("manifest lists ledger.csv");
    let manifest_sha = ledger_entry["sha256"].as_str().unwrap();
    let recomputed = hash_zip_file(&body, "ledger.csv");
    assert_eq!(
        manifest_sha, &recomputed,
        "manifest ledger.csv sha256 must match re-hash"
    );
    // entryCount = 2: the seeded FEATURE ledger entry plus the ARCHITECTURE
    // ADR ledger entry (ADRs live in `ledger_entries` with
    // `entry_type='ARCHITECTURE'`, so `get_all_committed_ledger_entries`
    // returns both).
    assert_eq!(manifest["entryCount"].as_u64(), Some(2));

    // Tamper-evidence: signature verifies.
    assert_signature_verifies(&body);

    // Seeded content present.
    let ledger_csv = String::from_utf8(read_zip_file(&body, "ledger.csv")).unwrap();
    assert!(
        ledger_csv.contains(tx_id),
        "ledger.csv must contain seeded tx_id; got:\n{ledger_csv}"
    );
    let verify_csv = String::from_utf8(read_zip_file(&body, "verification_history.csv")).unwrap();
    assert!(
        verify_csv.contains("cargo nextest run"),
        "verification_history.csv must contain seeded command; got:\n{verify_csv}"
    );
    // ADR content: read the single adr file and confirm it carries the ADR summary.
    let adr_name = adr_files[0];
    let adr_md = String::from_utf8(read_zip_file(&body, adr_name)).unwrap();
    assert!(
        adr_md.contains("Use Ed25519 for export signatures"),
        "ADR markdown must contain the seeded summary; got:\n{adr_md}"
    );
    handle.abort();
}

/// Empty state (no DB): export still returns 200 + a valid zip with
/// header-only CSVs, no `adr/` files, and a signature that verifies over
/// the manifest.
#[tokio::test]
async fn test_soc2_export_empty_state_no_db() {
    use crate::common::TempEnv;

    let home = tempfile::tempdir().unwrap();
    let _home_guard_home = TempEnv::set("HOME", home.path().to_str().unwrap());
    let _home_guard_profile = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let (body, content_type, content_disposition) = fetch_soc2_export(&url, &token).await;
    assert!(content_type.starts_with("application/zip"));
    assert!(content_disposition.contains("attachment"));

    let names = list_zip_files(&body);
    for required in [
        "manifest.json",
        "manifest.sig",
        "manifest.pub",
        "ledger.csv",
        "verification_history.csv",
    ] {
        assert!(
            names.iter().any(|n| n == required),
            "empty-state zip must still contain {required}; got {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n.starts_with("adr/")),
        "empty-state zip must NOT contain adr/ files; got {names:?}"
    );

    let ledger_csv = String::from_utf8(read_zip_file(&body, "ledger.csv")).unwrap();
    assert!(
        ledger_csv
            == "tx_id,category,entity,change_type,summary,reason,committed_at,signed,signature,observed,prev_hash\n",
        "empty-state ledger.csv must be header-only; got:\n{ledger_csv}"
    );
    let verify_csv = String::from_utf8(read_zip_file(&body, "verification_history.csv")).unwrap();
    assert!(
        verify_csv == "run_timestamp,overall_pass,command,exit_code,duration_ms\n",
        "empty-state verification_history.csv must be header-only; got:\n{verify_csv}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&read_zip_file(&body, "manifest.json")).unwrap();
    assert_eq!(manifest["entryCount"].as_u64(), Some(0));
    assert_signature_verifies(&body);
    handle.abort();
}

/// Tamper detection (negative): take a valid export, flip one byte in
/// `ledger.csv` within the archive, and re-serialize a new zip with the
/// ORIGINAL manifest.json/manifest.sig/manifest.pub. Re-hashing the
/// tampered `ledger.csv` must NOT match the manifest's `sha256` â€” i.e. the
/// manifest would detect the tamper. This proves the tamper-evidence
/// contract actually catches modification.
#[tokio::test]
async fn test_soc2_export_tamper_detection() {
    use ledgerful::ledger::crypto::sign_ledger_entry_in;
    use std::io::Read;

    let home = tempfile::tempdir().unwrap();
    let keys_dir = home.path().join(".ledgerful").join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let guard = temp_layout();
    let layout = guard.layout();
    let tx_id = "tx-soc2-tamper-001";
    let committed_at = "2026-06-20T11:00:00Z";
    let summary = "Tamper detection fixture";
    let reason = "reason";
    let (sig, pub_key) =
        sign_ledger_entry_in(&keys_dir, tx_id, "FEATURE", summary, reason, committed_at)
            .expect("sign_ledger_entry_in should succeed");
    seed_ledger_entry_with_ts(
        &layout,
        tx_id,
        summary,
        reason,
        committed_at,
        sig.as_deref(),
        pub_key.as_deref(),
    );

    let (url, token, handle) = spawn_server(layout).await;
    let (body, _ct, _cd) = fetch_soc2_export(&url, &token).await;

    // Read every file out of the original archive.
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&body[..])).unwrap();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let name = file.name().to_string();
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).unwrap();
            entries.push((name, buf));
        }
    }

    // Tamper with ledger.csv (flip first data byte) but keep the original
    // manifest.json / manifest.sig / manifest.pub.
    let mut tampered: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, mut bytes) in entries {
        if name == "ledger.csv" && !bytes.is_empty() {
            bytes[0] ^= 0xFF;
        }
        tampered.push((name, bytes));
    }

    // Re-serialize a new zip preserving the original manifest + signature.
    let mut new_buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut new_buf));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in &tampered {
            zip.start_file(name, options).unwrap();
            std::io::Write::write_all(&mut zip, bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    // Re-hash the tampered ledger.csv from the new zip and compare against
    // the ORIGINAL manifest's ledger.csv sha256. They must differ.
    let manifest: serde_json::Value =
        serde_json::from_slice(&read_zip_file(&new_buf, "manifest.json")).unwrap();
    let manifest_sha = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str() == Some("ledger.csv"))
        .unwrap()["sha256"]
        .as_str()
        .unwrap()
        .to_string();
    let recomputed = hash_zip_file(&new_buf, "ledger.csv");
    assert_ne!(
        manifest_sha, recomputed,
        "tampered ledger.csv sha256 must NOT match the original manifest"
    );
    handle.abort();
}

/// Tamper detection (negative, SIGNATURE leg): take a valid export, flip
/// one byte in `manifest.json` while leaving `manifest.sig`, `manifest.pub`,
/// and all data files untouched, then prove the Ed25519 signature over the
/// ORIGINAL `manifest.json` does NOT verify against the tampered manifest
/// bytes â€” i.e. `verifying_key.verify(&tampered_manifest, &signature)` is
/// `Err`. This complements `test_soc2_export_tamper_detection` (which proves
/// the file-HASH leg catches a modified data file) by locking in the
/// signature-rejection leg: any alteration to `manifest.json` itself is
/// caught by the Ed25519 verifier, not just by per-file SHA-256 comparison.
#[tokio::test]
async fn test_soc2_export_tampered_manifest_fails_signature_verification() {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use ledgerful::ledger::crypto::sign_ledger_entry_in;

    let home = tempfile::tempdir().unwrap();
    let keys_dir = home.path().join(".ledgerful").join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();

    let guard = temp_layout();
    let layout = guard.layout();
    // Seed a ledger entry so the zip is non-empty (a real export with a real
    // manifest payload + signature, not the empty-state degenerate case).
    let tx_id = "tx-soc2-sig-tamper-001";
    let committed_at = "2026-06-20T12:00:00Z";
    let summary = "Signature tamper detection fixture";
    let reason = "reason";
    let (sig, pub_key) =
        sign_ledger_entry_in(&keys_dir, tx_id, "FEATURE", summary, reason, committed_at)
            .expect("sign_ledger_entry_in should succeed");
    seed_ledger_entry_with_ts(
        &layout,
        tx_id,
        summary,
        reason,
        committed_at,
        sig.as_deref(),
        pub_key.as_deref(),
    );

    let (url, token, handle) = spawn_server(layout).await;
    let (body, _ct, _cd) = fetch_soc2_export(&url, &token).await;

    // Pull the manifest + signature + verifying key out of the valid export.
    let manifest_json = read_zip_file(&body, "manifest.json");
    let sig_bytes = read_zip_file(&body, "manifest.sig");
    let pub_bytes = read_zip_file(&body, "manifest.pub");
    assert_eq!(sig_bytes.len(), 64, "manifest.sig must be 64 bytes");
    assert_eq!(pub_bytes.len(), 32, "manifest.pub must be 32 bytes");

    // First, sanity-check the legitimate export verifies (otherwise the
    // negative assertion below is meaningless).
    let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().unwrap();
    let pub_arr: [u8; 32] = pub_bytes.as_slice().try_into().unwrap();
    let signature = Signature::from_bytes(&sig_arr);
    let verifying_key = VerifyingKey::from_bytes(&pub_arr).unwrap();
    assert!(
        verifying_key.verify(&manifest_json, &signature).is_ok(),
        "baseline: original manifest.json must verify against manifest.sig/manifest.pub"
    );

    // Flip ONE byte in manifest.json. Keep manifest.sig, manifest.pub, and
    // every data file byte-identical so the ONLY thing changing is the
    // manifest payload the signature covers.
    assert!(
        !manifest_json.is_empty(),
        "manifest.json must be non-empty to flip a byte"
    );
    let mut tampered_manifest = manifest_json.clone();
    tampered_manifest[0] ^= 0xFF;

    // The signature over the ORIGINAL manifest.json must NOT verify against
    // the tampered manifest bytes â€” the Ed25519 verifier must reject it.
    assert!(
        verifying_key
            .verify(&tampered_manifest, &signature)
            .is_err(),
        "tampered manifest.json must FAIL Ed25519 signature verification"
    );
    handle.abort();
}
