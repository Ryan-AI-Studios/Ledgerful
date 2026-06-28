use chrono::Utc;
use ledgerful::commands::services_diff::empty_state_message;
use ledgerful::config::model::{Config, ServiceDefinition};
use ledgerful::state::storage::StorageManager;
use tempfile::tempdir;

/// Regression coverage for CG-F19: `services diff` used to always tell users
/// to declare services or run `index --incremental` even when the indexer
/// itself logged "Service inference disabled by coverage.services config."
/// -- advice that could never change the outcome. `empty_state_message`
/// must consult the same `coverage.enabled` / `coverage.services.enabled`
/// switches the indexer gates on, and only suggest reindexing when doing so
/// could plausibly help.
fn insert_file(storage: &StorageManager, path: &str, last_indexed_at: &str) {
    storage
        .get_connection()
        .execute(
            "INSERT INTO project_files (file_path, last_indexed_at) VALUES (?1, ?2)",
            (path, last_indexed_at),
        )
        .unwrap();
}

fn fresh_timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn stale_timestamp() -> String {
    (Utc::now() - chrono::Duration::days(30)).to_rfc3339()
}

#[test]
fn test_message_when_disabled_globally() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    let mut config = Config::default();
    config.coverage.enabled = false;
    config.coverage.services.enabled = true;

    let (_, msg) = empty_state_message(&storage, &config);
    assert!(msg.contains("coverage.enabled"), "got: {msg}");
    assert!(
        msg.contains("not change"),
        "should not suggest reindexing as a fix: {msg}"
    );
}

#[test]
fn test_message_when_disabled_for_services_specifically() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.services.enabled = false;

    let (_, msg) = empty_state_message(&storage, &config);
    assert!(msg.contains("coverage.services.enabled"), "got: {msg}");
    assert!(
        msg.contains("not change"),
        "should not suggest reindexing as a fix: {msg}"
    );
}

#[test]
fn test_message_when_enabled_no_declared_and_index_missing() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.services.enabled = true;

    // No project_files rows at all => index has never run.
    let (_, msg) = ledgerful::commands::services_diff::empty_state_message(&storage, &config);
    assert!(msg.contains("index --incremental"), "got: {msg}");
    assert!(msg.contains("never been built"), "got: {msg}");
}

#[test]
fn test_message_when_enabled_no_declared_and_index_fresh() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    insert_file(&storage, "src/lib.rs", &fresh_timestamp());

    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.services.enabled = true;

    let (_, msg) = empty_state_message(&storage, &config);
    assert!(
        msg.contains("unlikely to help"),
        "should not suggest reindexing when index is fresh and inference found nothing: {msg}"
    );
}

#[test]
fn test_message_when_declared_but_unassigned_and_index_fresh() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    insert_file(&storage, "src/lib.rs", &fresh_timestamp());

    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.services.enabled = true;
    config.services.definitions.push(ServiceDefinition {
        name: "billing".to_string(),
        root: "services/billing".to_string(),
        owners: vec![],
        runtime_name: None,
        queues: vec![],
        topics: vec![],
        rpc_endpoints: vec![],
    });

    let (_, msg) = empty_state_message(&storage, &config);
    assert!(msg.contains("billing") || msg.contains('1'), "got: {msg}");
    assert!(
        msg.contains("unlikely to help"),
        "fresh index + declared-but-unassigned should not blame staleness: {msg}"
    );
}

#[test]
fn test_message_when_declared_but_unassigned_and_index_stale() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    insert_file(&storage, "src/lib.rs", &stale_timestamp());

    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.services.enabled = true;
    config.services.definitions.push(ServiceDefinition {
        name: "billing".to_string(),
        root: "services/billing".to_string(),
        owners: vec![],
        runtime_name: None,
        queues: vec![],
        topics: vec![],
        rpc_endpoints: vec![],
    });

    let (_, msg) = empty_state_message(&storage, &config);
    assert!(
        msg.contains("index --incremental"),
        "stale index with declared services should suggest reindexing: {msg}"
    );
}
