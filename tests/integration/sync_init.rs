#[cfg(feature = "sync")]
use camino::Utf8Path;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::init::handle as handle_sync_init;
#[cfg(feature = "sync")]
use tempfile::tempdir;

#[cfg(feature = "sync")]
use crate::common::{DirGuard, setup_git_repo};

#[test]
#[cfg(feature = "sync")]
fn test_sync_init_creates_device_keypair() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    // Initialize regular ledgerful first
    ledgerful::commands::init::execute_init(false).unwrap();

    // Use a fixed secret for deterministic test
    let test_secret = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    let result = handle_sync_init(false, Some(test_secret.to_string()));
    assert!(result.is_ok());

    let sync_dir = root.join(".ledgerful").join("sync");
    assert!(sync_dir.exists());
    assert!(sync_dir.join("device.key").exists());
}
