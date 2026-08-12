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
    execute_init(false, false).unwrap();

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
    execute_init(false, false).unwrap();

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
    execute_init(false, false).unwrap();

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
        execute_init(false, false).unwrap();
        execute_federate_export(false, None).unwrap();
    }

    // Init and scan from repo1 subdirectory
    {
        let _guard = DirGuard::from_utf8(&repo1);
        execute_init(false, false).unwrap();

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
        // Happy-path: folder basename matches export repo_name (0184-F — keep).
        assert!(links.iter().any(|(name, _, _)| name == "repo2"));
    }
}

/// 0184: status collapses same-path dups to one basename row; husk/self omitted.
#[test]
#[serial(cwd)]
fn federate_status_collapses_dups_and_omits_husk() {
    let workspace = tempdir().unwrap();
    let workspace_path = Utf8Path::from_path(workspace.path()).unwrap();
    let repo = workspace_path.join("main-repo");
    let peer = workspace_path.join("LivePeer");
    let husk = workspace_path.join("DeadHusk");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&peer).unwrap();
    fs::create_dir_all(&husk).unwrap();
    setup_git_repo(repo.as_std_path());
    setup_git_repo(peer.as_std_path());

    // Peer has schema; husk does not (only a markdown file).
    {
        let _g = DirGuard::from_utf8(&peer);
        execute_init(false, false).unwrap();
        execute_federate_export(false, None).unwrap();
    }
    fs::write(husk.join("CLAUDE.md"), "residue").unwrap();

    let _guard = DirGuard::from_utf8(&repo);
    execute_init(false, false).unwrap();
    let db_path = repo.join(".ledgerful").join("state").join("ledger.db");
    let storage = ledgerful::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();
    let peer_s = peer.as_str();
    let husk_s = husk.as_str();
    let self_s = repo.as_str();
    ledgerful::federated::storage::update_federated_link(
        storage.get_connection(),
        "AI-Brains",
        peer_s,
        "2026-07-04T00:00:00Z",
    )
    .unwrap();
    ledgerful::federated::storage::update_federated_link(
        storage.get_connection(),
        "ai-brains",
        peer_s,
        "2026-08-12T00:00:00Z",
    )
    .unwrap();
    ledgerful::federated::storage::update_federated_link(
        storage.get_connection(),
        "changeguard",
        husk_s,
        "2026-07-21T00:00:00Z",
    )
    .unwrap();
    ledgerful::federated::storage::update_federated_link(
        storage.get_connection(),
        "self-link",
        self_s,
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    storage.shutdown().unwrap();

    // Status is RO — raw cache still has 4 rows after status.
    execute_federate_status().unwrap();
    let storage = ledgerful::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();
    let raw = ledgerful::federated::storage::get_federated_links(storage.get_connection()).unwrap();
    assert_eq!(raw.len(), 4, "status must not DELETE");
    let presented = ledgerful::federated::links::present_federated_links(raw.as_slice(), self_s);
    assert_eq!(presented.live.len(), 1);
    assert_eq!(presented.live[0].name, "LivePeer");
    assert!(presented.omitted_total() >= 3);
    storage.shutdown().unwrap();
}

/// 0184 R1: husk-only cache is pruned even when discovery finds zero siblings.
#[test]
#[serial(cwd)]
fn federate_scan_prunes_husk_when_no_siblings_discovered() {
    let workspace = tempdir().unwrap();
    let workspace_path = Utf8Path::from_path(workspace.path()).unwrap();
    // Isolate: only this repo in the workspace so scan finds no siblings.
    let repo = workspace_path.join("alone");
    let husk = workspace_path.join("DeadHusk");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&husk).unwrap();
    setup_git_repo(repo.as_std_path());
    fs::write(husk.join("CLAUDE.md"), "residue").unwrap();

    let _guard = DirGuard::from_utf8(&repo);
    execute_init(false, false).unwrap();
    let db_path = repo.join(".ledgerful").join("state").join("ledger.db");
    let storage = ledgerful::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();
    storage
        .save_packet(&ledgerful::impact::packet::ImpactPacket::default())
        .unwrap();
    ledgerful::federated::storage::update_federated_link(
        storage.get_connection(),
        "changeguard",
        husk.as_str(),
        "2026-07-21T00:00:00Z",
    )
    .unwrap();
    assert_eq!(
        ledgerful::federated::storage::get_federated_links(storage.get_connection())
            .unwrap()
            .len(),
        1
    );
    storage.shutdown().unwrap();

    execute_federate_scan().unwrap();

    let storage = ledgerful::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();
    let links =
        ledgerful::federated::storage::get_federated_links(storage.get_connection()).unwrap();
    assert!(
        links.is_empty(),
        "husk-only cache must prune on empty discovery scan, got {links:?}"
    );
    storage.shutdown().unwrap();
}

/// 0184-F: folder basename wins over stale schema.repo_name on scan persist.
#[test]
#[serial(cwd)]
fn federate_scan_stores_basename_not_schema_repo_name() {
    let workspace = tempdir().unwrap();
    let workspace_path = Utf8Path::from_path(workspace.path()).unwrap();
    let repo1 = workspace_path.join("repo1");
    let folder = workspace_path.join("ledgerful-peer");
    fs::create_dir_all(&repo1).unwrap();
    fs::create_dir_all(&folder).unwrap();
    setup_git_repo(repo1.as_std_path());
    setup_git_repo(folder.as_std_path());

    {
        let _g = DirGuard::from_utf8(&folder);
        execute_init(false, false).unwrap();
        execute_federate_export(false, None).unwrap();
        // Overwrite export with a stale schema.repo_name (folder ≠ name).
        let schema_path = folder.join(".ledgerful").join("state").join("schema.json");
        let mut schema: FederatedSchema =
            serde_json::from_str(&fs::read_to_string(&schema_path).unwrap()).unwrap();
        schema.repo_name = "changeguard".into();
        fs::write(&schema_path, serde_json::to_string_pretty(&schema).unwrap()).unwrap();
    }

    {
        let _g = DirGuard::from_utf8(&repo1);
        execute_init(false, false).unwrap();
        let db_path = repo1.join(".ledgerful").join("state").join("ledger.db");
        let storage =
            ledgerful::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();
        storage
            .save_packet(&ledgerful::impact::packet::ImpactPacket::default())
            .unwrap();
        // Seed old name for same path (case-variant leftover).
        ledgerful::federated::storage::update_federated_link(
            storage.get_connection(),
            "OldCachedName",
            folder.as_str(),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        storage.shutdown().unwrap();

        execute_federate_scan().unwrap();

        let storage =
            ledgerful::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();
        let links =
            ledgerful::federated::storage::get_federated_links(storage.get_connection()).unwrap();
        assert!(
            links.iter().any(|(n, _, _)| n == "ledgerful-peer"),
            "expected basename store name, got {links:?}"
        );
        assert!(
            !links.iter().any(|(n, _, _)| n == "changeguard"),
            "must not store stale schema.repo_name"
        );
        assert!(
            !links.iter().any(|(n, _, _)| n == "OldCachedName"),
            "same-path rename must drop old name"
        );
        storage.shutdown().unwrap();
    }
}
