#[cfg(feature = "sync")]
use camino::Utf8Path;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::init::handle as handle_sync_init;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::pair::handle as handle_sync_pair;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::run::handle as handle_sync_run;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::status::handle as handle_sync_status;
#[cfg(feature = "sync")]
use ledgerful::state::storage::StorageManager;
#[cfg(feature = "sync")]
use tempfile::tempdir;

#[cfg(feature = "sync")]
use crate::common::{DirGuard, TempEnv, setup_git_repo};

const TEST_SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn sync_init_creates_device_keypair_and_sot_device_id() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();

    let result = handle_sync_init(false, Some(TEST_SECRET.to_string()));
    assert!(result.is_ok(), "init failed: {result:?}");

    let sync_dir = root.join(".ledgerful").join("sync");
    assert!(sync_dir.exists());
    assert!(sync_dir.join("device.key").exists());
    assert!(sync_dir.join("device.pub").exists());

    let layout = ledgerful::state::layout::Layout::new(root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    let conn = storage.get_connection();
    let device_id: String = conn
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .expect("SoT device_id must exist after init");
    assert!(
        device_id.starts_with("device-"),
        "unexpected device_id: {device_id}"
    );
    assert_ne!(device_id, "unknown");

    // Config mirror present; enabled must stay false.
    let config = ledgerful::config::load::load_config(&layout).unwrap();
    assert!(!config.sync.enabled);
    assert_eq!(config.sync.device_id.as_deref(), Some(device_id.as_str()));
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn sync_init_then_status_shows_sot_device_id_not_unknown() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    handle_sync_init(false, Some(TEST_SECRET.to_string())).unwrap();

    // Status must work without run; SoT device_id must not be "unknown".
    handle_sync_status().expect("status after init");

    let layout = ledgerful::state::layout::Layout::new(root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    let device_id: String = storage
        .get_connection()
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(device_id.starts_with("device-"));
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn sync_init_force_rewrites_keys_and_sot_device_id_together() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    handle_sync_init(false, Some(TEST_SECRET.to_string())).unwrap();

    let layout = ledgerful::state::layout::Layout::new(root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    let first_id: String = storage
        .get_connection()
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let first_key = std::fs::read(root.join(".ledgerful/sync/device.key")).unwrap();

    handle_sync_init(true, Some(TEST_SECRET.to_string())).unwrap();

    let second_id: String = storage
        .get_connection()
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let second_key = std::fs::read(root.join(".ledgerful/sync/device.key")).unwrap();
    let config = ledgerful::config::load::load_config(&layout).unwrap();

    assert_ne!(
        first_id, second_id,
        "force re-init must mint a new device_id"
    );
    assert_ne!(
        first_key, second_key,
        "force re-init must rewrite key material"
    );
    assert_eq!(config.sync.device_id.as_deref(), Some(second_id.as_str()));
    assert!(!config.sync.enabled, "force must never enable sync");
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn pair_accept_fails_closed_seeded_sot_nyi_not_missing_row() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    // Seed keys + sync_state via fixed init (avoids false-green on missing-row).
    handle_sync_init(false, Some(TEST_SECRET.to_string())).unwrap();

    let err = handle_sync_pair(Some("bogus-pair-code".to_string()))
        .expect_err("pair accept must fail closed");
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("not implemented")
            || msg.to_lowercase().contains("0111")
            || msg.to_lowercase().contains("experimental"),
        "expected NYI/not-implemented message, got: {msg}"
    );
    assert!(
        !msg.to_lowercase().contains("no rows")
            && !msg.to_lowercase().contains("query returned no rows"),
        "must not be a false-green missing-row error: {msg}"
    );
    assert!(
        !msg.to_lowercase().contains("paired successfully"),
        "must never claim success: {msg}"
    );
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn pair_without_code_after_init_emits_experimental_provisional_code() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    handle_sync_init(false, Some(TEST_SECRET.to_string())).unwrap();

    handle_sync_pair(None).expect("provisional code gen should succeed after init");
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn run_disabled_does_not_export() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    handle_sync_init(false, Some(TEST_SECRET.to_string())).unwrap();

    // Point target at a temp dir transport; enabled stays false (default).
    let target_dir = tmp.path().join("sync-target");
    std::fs::create_dir_all(&target_dir).unwrap();
    let layout = ledgerful::state::layout::Layout::new(root);
    let target_url = format!(
        "dir://{}",
        target_dir.display().to_string().replace('\\', "/")
    );
    ledgerful::commands::config::execute_config_set_in(
        &layout,
        &format!("sync.target=\"{target_url}\""),
    )
    .unwrap();

    // Must return Ok without writing outbox (enabled=false).
    handle_sync_run(true).expect("disabled run should return Ok with message");

    let devices = target_dir.join("devices");
    assert!(
        !devices.exists()
            || std::fs::read_dir(&devices)
                .map(|rd| rd.filter_map(|e| e.ok()).count() == 0)
                .unwrap_or(true),
        "disabled run must not create device outbox under {:?}",
        devices
    );
}
