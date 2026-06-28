use serial_test::serial;

use camino::Utf8Path;
use ledgerful::commands::federate::{
    execute_federate_export, execute_federate_scan, execute_federate_status,
};
use ledgerful::commands::init::execute_init;
use ledgerful::federated::schema::FederatedSchema;
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

#[test]
#[serial(cwd)]
fn test_federate_export_from_subdirectory() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(&root);
    execute_init(false).unwrap();

    let subdir = root.join("src").join("inner");
    fs::create_dir_all(&subdir).unwrap();

    // Switch to subdirectory
    let _subguard = DirGuard::from_utf8(&subdir);

    // This should find the repo root and work correctly
    execute_federate_export(false, None).unwrap();

    assert!(
        root.join(".ledgerful")
            .join("state")
            .join("schema.json")
            .exists()
    );
}

/// TA31 R4: `execute_federate_export` must stamp the exported
/// schema.json with a non-empty `generated_at` (a valid RFC 3339
/// timestamp) and `binary_version` (matching `CARGO_PKG_VERSION`), so
/// the scanner can later compare these against a sibling's last commit
/// time and binary version for staleness detection.
#[test]
#[serial(cwd)]
fn test_federate_export_stamps_generated_at_and_binary_version() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(&root);
    execute_init(false).unwrap();

    execute_federate_export(false, None).unwrap();

    let schema_path = root.join(".ledgerful").join("state").join("schema.json");
    let schema_json = fs::read_to_string(&schema_path).unwrap();
    let schema: FederatedSchema = serde_json::from_str(&schema_json).unwrap();

    assert!(
        !schema.generated_at.is_empty(),
        "generated_at must be populated on export"
    );
    chrono::DateTime::parse_from_rfc3339(&schema.generated_at).unwrap_or_else(|e| {
        panic!(
            "generated_at must be a valid RFC3339 timestamp, got {:?}: {e}",
            schema.generated_at
        )
    });

    assert_eq!(
        schema.binary_version,
        env!("CARGO_PKG_VERSION"),
        "binary_version must match CARGO_PKG_VERSION"
    );
}

#[test]
#[serial(cwd)]
fn test_federate_status_from_subdirectory() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(&root);
    execute_init(false).unwrap();

    let subdir = root.join("some").join("nested").join("dir");
    fs::create_dir_all(&subdir).unwrap();

    // Switch to subdirectory
    let _subguard = DirGuard::from_utf8(&subdir);

    // This should find the repo root and work correctly (even if no links yet)
    execute_federate_status().unwrap();
}

#[serial(cwd)]
#[test]
fn test_federate_scan_from_subdirectory() {
    // Setup sibling repo structure
    let workspace = tempdir().unwrap();
    let workspace_path = Utf8Path::from_path(workspace.path()).unwrap();

    let repo1 = workspace_path.join("repo1");
    let repo2 = workspace_path.join("repo2");

    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&repo2).unwrap();

    setup_git_repo(repo1.as_std_path());
    setup_git_repo(repo2.as_std_path());

    // Init and export repo2
    {
        let _guard = DirGuard::from_utf8(&repo2);
        execute_init(false).unwrap();
        execute_federate_export(false, None).unwrap();
    }

    // Init and scan from repo1 subdirectory
    {
        let _guard = DirGuard::from_utf8(&repo1);
        execute_init(false).unwrap();

        // Mock a scan packet so scan doesn't fail early
        let db_path = repo1.join(".ledgerful").join("state").join("ledger.db");
        let storage =
            ledgerful::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();
        let packet = ledgerful::impact::packet::ImpactPacket::default();
        storage.save_packet(&packet).unwrap();

        let conn = storage.get_connection();
        let _links_before = ledgerful::federated::storage::get_federated_links(conn).unwrap();

        storage.shutdown().unwrap();

        let subdir = repo1.join("src");
        fs::create_dir_all(&subdir).unwrap();
        let _subguard = DirGuard::from_utf8(&subdir);

        // This should find repo2 as a sibling
        execute_federate_scan().unwrap();

        // Re-open to verify
        let storage =
            ledgerful::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();
        let links =
            ledgerful::federated::storage::get_federated_links(storage.get_connection()).unwrap();
        assert!(links.iter().any(|(name, _, _)| name == "repo2"));
    }
}
