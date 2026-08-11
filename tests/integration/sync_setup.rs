//! 0113 sync setup / readiness / status next-action integration tests.

#[cfg(feature = "sync")]
use camino::Utf8Path;
#[cfg(feature = "sync")]
use ed25519_dalek::SigningKey;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::init::handle as handle_sync_init;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::readiness::{ReadinessKind, TargetReachable, collect_readiness};
#[cfg(feature = "sync")]
use ledgerful::commands::sync::setup::handle as handle_sync_setup;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::status::handle as handle_sync_status;
#[cfg(feature = "sync")]
use ledgerful::state::storage::StorageManager;
#[cfg(feature = "sync")]
use ledgerful::sync::peers::trust_peer;
#[cfg(feature = "sync")]
use std::fs;
#[cfg(feature = "sync")]
use tempfile::tempdir;

#[cfg(feature = "sync")]
use crate::common::{DirGuard, TempEnv, run_cli, setup_git_repo};

#[cfg(feature = "sync")]
const TEST_SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

#[cfg(feature = "sync")]
fn init_device(root: &Utf8Path) -> String {
    ledgerful::commands::init::execute_init(false, false).unwrap();
    handle_sync_init(false, Some(TEST_SECRET.to_string())).unwrap();
    let layout = ledgerful::state::layout::Layout::new(root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    storage
        .get_connection()
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[cfg(feature = "sync")]
fn add_dummy_peer(root: &Utf8Path, peer_id: &str) {
    let sk = SigningKey::generate(&mut rand::rng());
    trust_peer(
        root.join(".ledgerful/sync").as_std_path(),
        peer_id,
        &sk.verifying_key().to_bytes(),
        false,
    )
    .unwrap();
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_checklist_exit_zero_when_incomplete() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    // No sync init — incomplete.
    handle_sync_setup(false, false).expect("setup checklist must exit 0 when incomplete");
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_enable_refuses_without_peers() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let _id = init_device(root);
    let share = tmp.path().join("share");
    fs::create_dir_all(&share).unwrap();
    let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
    ledgerful::commands::config::execute_config_set_in(
        &ledgerful::state::layout::Layout::new(root),
        &format!("sync.target=\"{target}\""),
    )
    .unwrap();

    let err = handle_sync_setup(true, false).expect_err("enable without peers must refuse");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("refuse") || msg.contains("peer") || msg.contains("gate"),
        "unexpected error: {msg}"
    );
    let cfg =
        ledgerful::config::load::load_config(&ledgerful::state::layout::Layout::new(root)).unwrap();
    assert!(!cfg.sync.enabled, "refused enable must not mutate config");
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_enable_refuses_not_initialized() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    let err = handle_sync_setup(true, false).expect_err("enable without init must refuse");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("refuse") || msg.contains("initialized") || msg.contains("gate"),
        "unexpected: {msg}"
    );
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_enable_success_when_all_green() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let _id = init_device(root);
    add_dummy_peer(root, "device-peer-int01");
    let share = tmp.path().join("share");
    fs::create_dir_all(&share).unwrap();
    let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
    let layout = ledgerful::state::layout::Layout::new(root);
    ledgerful::commands::config::execute_config_set_in(
        &layout,
        &format!("sync.target=\"{target}\""),
    )
    .unwrap();

    handle_sync_setup(true, false).expect("enable when green");
    let cfg = ledgerful::config::load::load_config(&layout).unwrap();
    assert!(cfg.sync.enabled);
    assert!(
        layout.state_dir.join("config.toml.bak").exists(),
        "sibling config.toml.bak required on enable success"
    );

    // Already enabled → ok, no error.
    handle_sync_setup(true, false).expect("already enabled is fine");
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_enable_refuses_unreachable_target() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let _id = init_device(root);
    add_dummy_peer(root, "device-peer-int02");
    let missing = tmp.path().join("no-share-here");
    let target = format!("dir://{}", missing.display().to_string().replace('\\', "/"));
    let layout = ledgerful::state::layout::Layout::new(root);
    ledgerful::commands::config::execute_config_set_in(
        &layout,
        &format!("sync.target=\"{target}\""),
    )
    .unwrap();

    let err = handle_sync_setup(true, false).expect_err("unreachable target");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("refuse") || msg.contains("reach") || msg.contains("gate"),
        "unexpected: {msg}"
    );
    assert!(
        !ledgerful::config::load::load_config(&layout)
            .unwrap()
            .sync
            .enabled
    );
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_json_is_parseable_schema_v1() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    // Capture via library readiness (CLI stdout not easily captured here).
    let layout = ledgerful::state::layout::Layout::new(root);
    let cfg = ledgerful::config::load::load_config(&layout).unwrap();
    let report = collect_readiness(&layout, &cfg).unwrap();
    let v = report.to_json_value();
    assert_eq!(v["schemaVersion"], 1);
    assert!(v.get("nextAction").is_some());
    assert!(v.get("readiness").is_some());
    assert!(v.get("targetReachable").is_some());
    // Pure object — no snake_case public keys.
    assert!(v.get("schema_version").is_none());

    handle_sync_setup(false, true).expect("setup --json");
}

/// F-003: `sync setup --json` stdout is a single pure JSON object (incomplete ok).
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_json_stdout_is_pure_json_object() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    // Incomplete readiness is fine — checklist --json must still exit 0.

    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "setup", "--json"]);
    assert_eq!(
        code, 0,
        "setup --json incomplete must exit 0; stderr={stderr}"
    );
    let trimmed = stdout.trim();
    assert!(
        !trimmed.contains("Set "),
        "config set noise must not pollute JSON stdout:\n{trimmed}"
    );
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!("stdout must be a single pure JSON object: {e}; got:\n{trimmed}")
    });
    assert_eq!(v["schemaVersion"], 1);
    assert!(v.get("nextAction").is_some());
    assert!(v.get("enabled").is_some());
}

/// F-003/F-004: `sync setup --enable --json` success → pure JSON, enabled true,
/// and config.toml never holds the secret env name or value.
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_enable_json_stdout_is_pure_json_enabled() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let _id = init_device(root);
    add_dummy_peer(root, "device-peer-json-enable");
    let share = tmp.path().join("share");
    fs::create_dir_all(&share).unwrap();
    let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
    let layout = ledgerful::state::layout::Layout::new(root);
    // Quiet set so test process stdout is clean; the CLI path under test uses quiet too.
    ledgerful::commands::config::execute_config_set_in_quiet(
        &layout,
        &format!("sync.target=\"{target}\""),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "setup", "--enable", "--json"]);
    assert_eq!(
        code, 0,
        "setup --enable --json success must exit 0; stderr={stderr}"
    );
    let trimmed = stdout.trim();
    assert!(
        !trimmed.contains("Set "),
        "human Set line must not prefix/pollute pure JSON:\n{trimmed}"
    );
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!("stdout must be a single pure JSON object: {e}; got:\n{trimmed}")
    });
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(
        v["enabled"], true,
        "enable success must report enabled=true; got {v}"
    );

    // F-004: secret material never lands in config.toml.
    let cfg_text =
        fs::read_to_string(layout.config_file().as_std_path()).expect("config.toml after enable");
    assert!(
        !cfg_text.contains("LEDGERFUL_SYNC"),
        "config.toml must not contain LEDGERFUL_SYNC*: {cfg_text}"
    );
    assert!(
        !cfg_text.contains(TEST_SECRET),
        "config.toml must not contain the test secret"
    );
    assert!(
        cfg_text.contains("enabled") && cfg_text.to_lowercase().contains("true"),
        "config should show enabled=true: {cfg_text}"
    );
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn status_next_action_when_incomplete() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let _id = init_device(root);
    let layout = ledgerful::state::layout::Layout::new(root);
    let cfg = ledgerful::config::load::load_config(&layout).unwrap();
    let report = collect_readiness(&layout, &cfg).unwrap();
    assert_eq!(report.readiness, ReadinessKind::Disabled);
    assert!(
        report.next_action.contains("pair") || report.next_action.contains("target"),
        "next_action={}",
        report.next_action
    );
    handle_sync_status(false).expect("status human");
    handle_sync_status(true).expect("status --json");
}

/// P2-1: `sync status --json` stdout is a single pure JSON object.
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn status_json_stdout_is_pure_json_object() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    ledgerful::commands::init::execute_init(false, false).unwrap();

    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "status", "--json"]);
    assert_eq!(
        code, 0,
        "status --json incomplete must exit 0; stderr={stderr}"
    );
    let trimmed = stdout.trim();
    assert!(
        !trimmed.contains("Set "),
        "config set noise must not pollute JSON stdout:\n{trimmed}"
    );
    assert!(
        !trimmed.contains("Team Sync Status"),
        "human status banner must not pollute JSON stdout:\n{trimmed}"
    );
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!("stdout must be a single pure JSON object: {e}; got:\n{trimmed}")
    });
    assert_eq!(v["schemaVersion"], 1);
    assert!(v.get("nextAction").is_some());
    assert!(v.get("enabled").is_some());
    assert!(v.get("inboxCount").is_some());
    assert!(v.get("outboxCount").is_some());
}

/// P2-2: refuse path must leave config.toml bytes identical and create no bak.
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_enable_refuse_config_immutable_no_bak() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let _id = init_device(root);
    // No peers → refuse.
    let share = tmp.path().join("share");
    fs::create_dir_all(&share).unwrap();
    let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
    let layout = ledgerful::state::layout::Layout::new(root);
    ledgerful::commands::config::execute_config_set_in_quiet(
        &layout,
        &format!("sync.target=\"{target}\""),
    )
    .unwrap();

    let config_path = layout.config_file();
    let before = fs::read(config_path.as_std_path()).expect("config before refuse");
    let bak = layout.state_dir.join("config.toml.bak");
    // Ensure no stale bak from prior work.
    let _ = fs::remove_file(bak.as_std_path());

    let err = handle_sync_setup(true, false).expect_err("enable without peers must refuse");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("refuse") || msg.contains("peer") || msg.contains("gate"),
        "unexpected error: {msg}"
    );

    let after = fs::read(config_path.as_std_path()).expect("config after refuse");
    assert_eq!(
        before, after,
        "refuse must not mutate config.toml (byte-identical)"
    );
    assert!(!bak.exists(), "refuse must not create config.toml.bak");
    assert!(
        !ledgerful::config::load::load_config(&layout)
            .unwrap()
            .sync
            .enabled
    );
}

/// P2-2: success path bak content equals pre-enable config bytes; enabled true after.
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn setup_enable_success_bak_matches_pre_enable_bytes() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let _id = init_device(root);
    add_dummy_peer(root, "device-peer-bak-bytes");
    let share = tmp.path().join("share");
    fs::create_dir_all(&share).unwrap();
    let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
    let layout = ledgerful::state::layout::Layout::new(root);
    ledgerful::commands::config::execute_config_set_in_quiet(
        &layout,
        &format!("sync.target=\"{target}\""),
    )
    .unwrap();

    let config_path = layout.config_file();
    let before = fs::read(config_path.as_std_path()).expect("config before enable");
    let bak = layout.state_dir.join("config.toml.bak");
    let _ = fs::remove_file(bak.as_std_path());

    handle_sync_setup(true, false).expect("enable when green");

    assert!(
        bak.exists(),
        "sibling config.toml.bak required on enable success"
    );
    let bak_bytes = fs::read(bak.as_std_path()).expect("bak after enable");
    assert_eq!(
        bak_bytes, before,
        "bak must equal pre-enable config.toml bytes"
    );
    let cfg = ledgerful::config::load::load_config(&layout).unwrap();
    assert!(cfg.sync.enabled, "enabled must be true after success");
    let after = fs::read(config_path.as_std_path()).expect("config after enable");
    assert_ne!(
        after, before,
        "enable must mutate config.toml (enabled=true)"
    );
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn readiness_windows_style_target_parse_ok() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let _id = init_device(root);
    add_dummy_peer(root, "device-peer-win");
    let layout = ledgerful::state::layout::Layout::new(root);
    ledgerful::commands::config::execute_config_set_in(
        &layout,
        "sync.target=\"dir:///C:/Shared/ledgerful-0113\"",
    )
    .unwrap();
    let cfg = ledgerful::config::load::load_config(&layout).unwrap();
    let report = collect_readiness(&layout, &cfg).unwrap();
    assert!(
        report.target_parse_ok,
        "dir:///C:/… must parse via SyncTarget"
    );
    // Path does not exist → not Yes.
    assert_ne!(report.target_reachable, TargetReachable::Yes);
}
