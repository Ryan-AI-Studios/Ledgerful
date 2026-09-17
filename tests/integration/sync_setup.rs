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

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn cursor_json_lag_reasons() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "cursor", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stdout.contains("Sync Cursors"),
        "human must not leak:\n{stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["initialized"], false);
    assert_eq!(v["lastExtractHlc"], serde_json::Value::Null);
    assert_eq!(v["lag"]["status"], "unknown");
    assert_eq!(v["lag"]["reason"], "notInitialized");
    assert_eq!(v["nextAction"], "ledgerful sync init");

    let _id = init_device(root);
    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "cursor", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["initialized"], true);
    assert_eq!(v["lag"]["reason"], "neverRun");
    assert_eq!(v["nextAction"], "ledgerful sync setup");

    let layout = ledgerful::state::layout::Layout::new(root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    storage
        .get_connection()
        .execute(
            "UPDATE sync_state SET last_extract_hlc = ?1, last_apply_hlc = ?2 WHERE id = 1",
            ["hlc-a", "hlc-b"],
        )
        .unwrap();
    storage.shutdown().unwrap();
    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "cursor", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["lag"]["reason"], "hlcNotWallClock");
    assert_eq!(v["lastExtractHlc"], "hlc-a");
    assert_eq!(v["lastApplyHlc"], "hlc-b");
    assert_eq!(v["watermarkCompare"], "incomparable");
    assert_eq!(v["lag"]["status"], "unknown");

    let (human, _, hcode) = run_cli(tmp.path(), &["sync", "cursor"]);
    assert_eq!(hcode, 0);
    assert!(human.contains("Last Extract HLC: hlc-a"));
    assert!(human.contains("Hybrid Logical Clock"));
    assert!(!human.contains("Extract is ahead of Apply."));
    assert!(!human.contains("Trusted peers"));
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn log_json_states() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "log", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stdout.contains("No sync log found") || stdout.starts_with('{'));
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["logState"], "neverInitialized");
    assert_eq!(v["nextAction"], "ledgerful sync init");
    assert_eq!(v["lineCount"], 0);

    let _id = init_device(root);
    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "log", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["logState"], "noLog");
    assert_eq!(v["nextAction"], "ledgerful sync setup");

    let layout = ledgerful::state::layout::Layout::new(root);
    let log_path = layout.state_dir.join("sync").join("sync.log");
    fs::write(
        log_path.as_std_path(),
        [b"keep-head\n".as_slice(), &[0xff, 0xff], b"\nkeep-tail\n"].concat(),
    )
    .unwrap();
    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "log", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["logState"], "partial");
    assert!(v["skippedLines"].as_u64().unwrap() >= 1);
    let lines = v["lines"].as_array().unwrap();
    assert!(lines.iter().any(|l| l.as_str() == Some("keep-tail")));
    assert_eq!(v["lineCount"].as_u64().unwrap(), 2);

    fs::remove_file(log_path.as_std_path()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::write(log_path.as_std_path(), "blocked").unwrap();
        fs::set_permissions(log_path.as_std_path(), PermissionsExt::from_mode(0o000)).unwrap();
        let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "log", "--json"]);
        let _ = fs::set_permissions(log_path.as_std_path(), PermissionsExt::from_mode(0o644));
        assert_eq!(code, 1, "unreadable --json must exit 1; stderr={stderr}");
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["logState"], "unreadable");
    }
    #[cfg(windows)]
    {
        fs::create_dir_all(log_path.as_std_path()).unwrap();
        let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "log", "--json"]);
        assert_eq!(code, 1, "unreadable --json must exit 1; stderr={stderr}");
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["logState"], "unreadable");
    }
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn cursor_json_watermark_compare_parseable_and_human() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);

    ledgerful::commands::init::execute_init(false, false).unwrap();
    let (stdout, _, code) = run_cli(tmp.path(), &["sync", "cursor", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v.get("watermarkCompare").is_none());

    let _id = init_device(root);
    let layout = ledgerful::state::layout::Layout::new(root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    storage
        .get_connection()
        .execute(
            "UPDATE sync_state SET last_extract_hlc = ?1, last_apply_hlc = ?2 WHERE id = 1",
            ["1700000000001-0000-n", "1700000000000-0000-n"],
        )
        .unwrap();
    storage.shutdown().unwrap();

    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "cursor", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["watermarkCompare"], "extractAhead");
    assert_eq!(v["lag"]["status"], "unknown");
    let (human, _, hcode) = run_cli(tmp.path(), &["sync", "cursor"]);
    assert_eq!(hcode, 0);
    assert!(human.contains("Extract is ahead of Apply."));
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn log_events_failed_filter_and_disabled_run_writes_nothing() {
    use ledgerful::config::load::load_config;
    use ledgerful::sync::event_log::{
        EVENT_EXTRACT, EVENT_QUARANTINE, SyncLogEvent, append_sync_event,
    };

    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let _id = init_device(root);

    let layout = ledgerful::state::layout::Layout::new(root);
    let config = load_config(&layout).unwrap();
    assert!(!config.sync.enabled);
    ledgerful::sync::run(
        &config,
        layout.state_dir.as_std_path(),
        TEST_SECRET.as_bytes(),
    )
    .unwrap();
    let log_path = layout.state_dir.join("sync").join("sync.log");
    assert!(
        !log_path.exists(),
        "disabled sync::run must not create sync.log"
    );

    let (stdout, _, code) = run_cli(tmp.path(), &["sync", "log", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["logState"], "noLog");

    append_sync_event(
        layout.state_dir.as_std_path(),
        &SyncLogEvent::new(EVENT_EXTRACT, true).with_bundle("one.lfbundle"),
    )
    .unwrap();
    append_sync_event(
        layout.state_dir.as_std_path(),
        &SyncLogEvent::new(EVENT_QUARANTINE, false).with_bundle("peer/bad.lfbundle"),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "log", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["logState"], "ok");
    assert_eq!(v["lineCount"], 2);
    assert_eq!(v["events"].as_array().unwrap().len(), 2);
    assert!(v.get("failed").is_none());

    let (stdout, stderr, code) = run_cli(tmp.path(), &["sync", "log", "--failed", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["failed"], true);
    assert_eq!(v["lineCount"], 2);
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    assert_eq!(v["events"][0]["event"], "quarantine");
    assert_eq!(v["events"][0]["ok"], false);

    let (human, _, hcode) = run_cli(tmp.path(), &["sync", "log", "--failed"]);
    assert_eq!(hcode, 0);
    assert!(human.contains("quarantine"));
    assert!(!human.contains("one.lfbundle"));
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn verify_json_verdicts_signed_tampered_missing() {
    use ed25519_dalek::{Signer, SigningKey};
    use ledgerful::sync::bundle::{Bundle, Manifest};
    use ledgerful::sync::hlc::HLC;
    use std::io::Read;

    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let device_id = init_device(root);

    let key_bytes = fs::read(root.join(".ledgerful/sync/device.key")).unwrap();
    let sign_key = SigningKey::from_bytes(&key_bytes.as_slice().try_into().unwrap());
    let mut manifest = Manifest {
        version: 1,
        device_id: device_id.clone(),
        bundle_hlc: HLC {
            physical_ms: 1_700_000_000_000,
            logical: 0,
            node_id: device_id.clone(),
        },
        manifest_sha256: String::new(),
        entry_count: 0,
        entries: vec![],
        tombstones: vec![],
    };
    let (zip, _) = Bundle::build(&mut manifest, &sign_key).unwrap();
    let enc = Bundle::encrypt(&zip, TEST_SECRET.as_bytes()).unwrap();
    let ok_path = tmp.path().join("ok.lfbundle");
    fs::write(&ok_path, &enc).unwrap();

    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["sync", "verify", ok_path.to_str().unwrap(), "--json"],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stdout.contains("Bundle Verification Success"));
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["ok"], true);
    assert_eq!(v["verdict"], "ok");
    assert_eq!(v["deviceId"], device_id);
    assert_eq!(v["entryCount"], 0);

    let (human, _, hcode) = run_cli(tmp.path(), &["sync", "verify", ok_path.to_str().unwrap()]);
    assert_eq!(hcode, 0);
    assert!(human.contains("Bundle Verification Success"));
    assert!(human.contains("Signature:      Valid (Ed25519)"));

    let mut flipped = enc.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x01;
    let bad_path = tmp.path().join("aead.lfbundle");
    fs::write(&bad_path, &flipped).unwrap();
    let (stdout, _, code) = run_cli(
        tmp.path(),
        &["sync", "verify", bad_path.to_str().unwrap(), "--json"],
    );
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["verdict"], "decryptFailed");

    let sig_tampered = rewrite_zip_member(&zip, "device.sig", &[0u8; 64]);
    let enc_sig = Bundle::encrypt(&sig_tampered, TEST_SECRET.as_bytes()).unwrap();
    let sig_path = tmp.path().join("sig.lfbundle");
    fs::write(&sig_path, &enc_sig).unwrap();
    let (stdout, _, code) = run_cli(
        tmp.path(),
        &["sync", "verify", sig_path.to_str().unwrap(), "--json"],
    );
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["verdict"], "signatureFailed");
    assert_eq!(v["ok"], false);

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip.clone())).unwrap();
    let mut manifest_json = Vec::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_end(&mut manifest_json)
        .unwrap();
    let mut val: serde_json::Value = serde_json::from_slice(&manifest_json).unwrap();
    val["manifest_sha256"] = serde_json::json!("00".repeat(32));
    let new_manifest = serde_json::to_vec(&val).unwrap();
    let new_sig = sign_key.sign(&new_manifest).to_bytes();
    let mut integrity_zip = rewrite_zip_member(&zip, "manifest.json", &new_manifest);
    integrity_zip = rewrite_zip_member(&integrity_zip, "device.sig", &new_sig);
    let enc_int = Bundle::encrypt(&integrity_zip, TEST_SECRET.as_bytes()).unwrap();
    let int_path = tmp.path().join("int.lfbundle");
    fs::write(&int_path, &enc_int).unwrap();
    let (stdout, _, code) = run_cli(
        tmp.path(),
        &["sync", "verify", int_path.to_str().unwrap(), "--json"],
    );
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["verdict"], "integrityFailed");

    let foreign = SigningKey::generate(&mut rand::rng());
    let foreign_id = "device-foreign1";
    let mut foreign_manifest = Manifest {
        version: 1,
        device_id: foreign_id.to_string(),
        bundle_hlc: HLC {
            physical_ms: 1_700_000_000_000,
            logical: 0,
            node_id: foreign_id.to_string(),
        },
        manifest_sha256: String::new(),
        entry_count: 0,
        entries: vec![],
        tombstones: vec![],
    };
    let (fzip, _) = Bundle::build(&mut foreign_manifest, &foreign).unwrap();
    let fenc = Bundle::encrypt(&fzip, TEST_SECRET.as_bytes()).unwrap();
    let fpath = tmp.path().join("foreign.lfbundle");
    fs::write(&fpath, &fenc).unwrap();
    let (stdout, _, code) = run_cli(
        tmp.path(),
        &["sync", "verify", fpath.to_str().unwrap(), "--json"],
    );
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["verdict"], "unknownDevice");
    assert!(
        v["message"].as_str().unwrap_or("").contains(foreign_id),
        "unknownDevice message must name device: {}",
        v["message"]
    );

    let missing = tmp.path().join("no-such.lfbundle");
    let (stdout, _, code) = run_cli(
        tmp.path(),
        &["sync", "verify", missing.to_str().unwrap(), "--json"],
    );
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["verdict"], "missingFile");
    assert_eq!(v["ok"], false);

    drop(_secret);
    let _gone = TempEnv::remove("LEDGERFUL_SYNC_SECRET");
    let (stdout, _, code) = run_cli(
        tmp.path(),
        &["sync", "verify", ok_path.to_str().unwrap(), "--json"],
    );
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["verdict"], "missingSecret");
    assert!(
        v["message"]
            .as_str()
            .unwrap_or("")
            .contains("LEDGERFUL_SYNC_SECRET")
    );
}

#[cfg(feature = "sync")]
fn rewrite_zip_member(zip_bytes: &[u8], name: &str, new_bytes: &[u8]) -> Vec<u8> {
    use std::io::{Read, Write};
    let mut src = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes.to_vec())).unwrap();
    let mut out = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let options = zip::write::SimpleFileOptions::default();
        for i in 0..src.len() {
            let mut file = src.by_index(i).unwrap();
            let file_name = file.name().to_string();
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).unwrap();
            writer.start_file(&file_name, options).unwrap();
            if file_name == name {
                writer.write_all(new_bytes).unwrap();
            } else {
                writer.write_all(&buf).unwrap();
            }
        }
        writer.finish().unwrap();
    }
    out
}
