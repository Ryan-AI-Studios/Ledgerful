use std::fs;
use std::hash::{Hash, Hasher};

use ledgerful::commands::init::execute_init;
use ledgerful::commands::ledger_diagnose::{DiagnoseOpts, diagnose_layout};
use ledgerful::state::layout::Layout;
use serial_test::serial;

use crate::common::{DirGuard, setup_git_repo};

fn hash_bytes(path: &std::path::Path) -> Option<u64> {
    let bytes = fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

fn opts(json: bool, output: Option<std::path::PathBuf>, manifest: bool) -> DiagnoseOpts {
    DiagnoseOpts {
        json,
        limit: 100,
        offset: 0,
        output,
        manifest,
    }
}

#[test]
#[serial(cwd)]
fn chain_diagnose_read_only_hash() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());
    let root = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let _guard = DirGuard::from_utf8(&root);
    execute_init(false, false).unwrap();
    let state = root.join(".ledgerful").join("state");
    let db = state.join("ledger.db");
    let layout = Layout::new(&root);
    let _prime = ledgerful::state::storage::StorageManager::open_read_only_sqlite_only(&layout);
    let before = (
        hash_bytes(db.as_std_path()),
        hash_bytes(state.join("ledger.db-wal").as_std_path()),
        hash_bytes(state.join("ledger.db-shm").as_std_path()),
    );
    diagnose_layout(&layout, &opts(true, None, false)).unwrap();
    let after = (
        hash_bytes(db.as_std_path()),
        hash_bytes(state.join("ledger.db-wal").as_std_path()),
        hash_bytes(state.join("ledger.db-shm").as_std_path()),
    );
    assert_eq!(before, after);
}

#[test]
fn chain_diagnose_missing_database_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let err = diagnose_layout(&Layout::new(&root), &opts(true, None, false)).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Storage not initialized") || msg.contains("ledger.db"),
        "{msg}"
    );
}

#[test]
#[serial(cwd)]
fn chain_diagnose_manifest_omits_keys() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());
    let root = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let _guard = DirGuard::from_utf8(&root);
    execute_init(false, false).unwrap();
    let out = dir.path().join("manifest.json");
    let layout = Layout::new(&root);
    diagnose_layout(&layout, &opts(false, Some(out.clone()), true)).unwrap();
    let body = fs::read_to_string(&out).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["kind"], "ledgerRecoveryManifestProposal");
    assert_eq!(value["manifestVersion"], 1);
    assert!(value["candidateTxIds"].is_array());
    assert!(value.get("eligibleTxIds").is_none());
    assert!(value.get("publicKey").is_none());
    assert!(value.get("signature").is_none());
    assert!(body.contains("not authenticated chronology"));
}

#[test]
#[serial(cwd)]
fn chain_diagnose_refuses_existing_manifest_output() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());
    let root = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let _guard = DirGuard::from_utf8(&root);
    execute_init(false, false).unwrap();
    let out = dir.path().join("manifest.json");
    fs::write(&out, "canary-manifest").unwrap();
    let layout = Layout::new(&root);
    let err = diagnose_layout(&layout, &opts(false, Some(out.clone()), true)).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("output file already exists"), "{msg}");
    assert_eq!(fs::read_to_string(&out).unwrap(), "canary-manifest");
}

#[test]
#[serial(cwd)]
fn chain_diagnose_refuses_existing_full_output() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());
    let root = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let _guard = DirGuard::from_utf8(&root);
    execute_init(false, false).unwrap();
    let out = dir.path().join("diagnosis.json");
    fs::write(&out, "canary-full").unwrap();
    let layout = Layout::new(&root);
    let err = diagnose_layout(&layout, &opts(false, Some(out.clone()), false)).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("output file already exists"), "{msg}");
    assert_eq!(fs::read_to_string(&out).unwrap(), "canary-full");
}
