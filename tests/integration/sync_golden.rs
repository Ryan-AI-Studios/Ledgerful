//! 0112 two-layout golden path + poison quarantine (full crypto chain).

#[cfg(feature = "sync")]
use camino::Utf8Path;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::init::handle as handle_sync_init;
#[cfg(feature = "sync")]
use ledgerful::config::load::load_config;
#[cfg(feature = "sync")]
use ledgerful::config::model::Config;
#[cfg(feature = "sync")]
use ledgerful::ledger::*;
#[cfg(feature = "sync")]
use ledgerful::state::layout::Layout;
#[cfg(feature = "sync")]
use ledgerful::state::storage::StorageManager;
#[cfg(feature = "sync")]
use ledgerful::sync::peers::trust_peer;
#[cfg(feature = "sync")]
use rusqlite::Connection;
#[cfg(feature = "sync")]
use std::fs;
#[cfg(feature = "sync")]
use tempfile::tempdir;

#[cfg(feature = "sync")]
use crate::common::{DirGuard, TempEnv, setup_git_repo};

#[cfg(feature = "sync")]
const TEST_SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

#[cfg(feature = "sync")]
fn init_device(root: &Utf8Path) -> String {
    ledgerful::commands::init::execute_init(false, false).unwrap();
    handle_sync_init(false, Some(TEST_SECRET.to_string())).unwrap();
    let layout = Layout::new(root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    storage
        .get_connection()
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[cfg(feature = "sync")]
fn read_pub(root: &Utf8Path) -> [u8; 32] {
    let bytes = fs::read(root.join(".ledgerful/sync/device.pub")).unwrap();
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&bytes);
    pk
}

#[cfg(feature = "sync")]
fn commit_local_entry(root: &Utf8Path, entity: &str, summary: &str) -> String {
    let layout = Layout::new(root);
    let mut storage = StorageManager::init_with_layout(&layout).unwrap();
    let entity_path = root.as_std_path().join(entity);
    if let Some(parent) = entity_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&entity_path, b"// golden").unwrap();

    let mut tx_mgr = TransactionManager::new(
        &mut storage,
        root.as_std_path().to_path_buf(),
        Config::default(),
    );
    tx_mgr
        .atomic_change(
            TransactionRequest {
                category: Category::Feature,
                entity: entity.to_string(),
                ..Default::default()
            },
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: summary.to_string(),
                reason: "golden path".to_string(),
                ..Default::default()
            },
            false,
        )
        .expect("commit entry")
}

#[cfg(feature = "sync")]
fn enable_sync_config(root: &Utf8Path, share: &std::path::Path) -> Config {
    let layout = Layout::new(root);
    let mut config = load_config(&layout).unwrap();
    config.sync.enabled = true;
    // dir:// path — on Windows Path::display is fine for DirTransport parse.
    let share_str = share
        .canonicalize()
        .unwrap_or_else(|_| share.to_path_buf())
        .to_string_lossy()
        .to_string();
    config.sync.target = format!("dir://{share_str}");
    config
}

#[cfg(feature = "sync")]
fn device_id_sot(root: &Utf8Path) -> String {
    let layout = Layout::new(root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    storage
        .get_connection()
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[cfg(feature = "sync")]
fn peer_origin_tx(root: &Utf8Path, tx_id: &str) -> Option<String> {
    let db = root.join(".ledgerful/state/ledger.db");
    let conn = Connection::open(db.as_std_path()).unwrap();
    conn.query_row(
        "SELECT origin FROM ledger_entries WHERE tx_id = ?1",
        [tx_id],
        |row| row.get(0),
    )
    .ok()
}

/// Full crypto chain: A commit → encrypt → put → B decrypt → parse → apply → PEER row.
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn two_layout_golden_path_full_crypto_chain() {
    let share_tmp = tempdir().unwrap();
    let share = share_tmp.path();

    let tmp_a = tempdir().unwrap();
    let root_a = Utf8Path::from_path(tmp_a.path()).unwrap();
    setup_git_repo(tmp_a.path());
    let _ga = DirGuard::from_utf8(root_a);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);

    let id_a = init_device(root_a);
    let pub_a = read_pub(root_a);

    let tmp_b = tempdir().unwrap();
    let root_b = Utf8Path::from_path(tmp_b.path()).unwrap();
    setup_git_repo(tmp_b.path());
    let _gb = DirGuard::from_utf8(root_b);
    let id_b = init_device(root_b);
    let pub_b = read_pub(root_b);
    assert_ne!(id_a, id_b);

    // Mutual trust (library path — same peer store as pair accept)
    trust_peer(
        root_a.join(".ledgerful/sync").as_std_path(),
        &id_b,
        &pub_b,
        false,
    )
    .unwrap();
    trust_peer(
        root_b.join(".ledgerful/sync").as_std_path(),
        &id_a,
        &pub_a,
        false,
    )
    .unwrap();

    // Commit on A while CWD is still B from last DirGuard — fix: re-enter A for commit.
    drop(_gb);
    let _ga2 = DirGuard::from_utf8(root_a);
    let tx_id = commit_local_entry(root_a, "src/golden.rs", "golden commit from A");

    let config_a = enable_sync_config(root_a, share);
    assert!(config_a.sync.enabled);
    ledgerful::sync::run(
        &config_a,
        root_a.join(".ledgerful").as_std_path(),
        TEST_SECRET.as_bytes(),
    )
    .expect("run A export");

    // A outbox should have a .lfbundle
    let outbox_a = share.join("devices").join(&id_a);
    let a_bundles: Vec<_> = fs::read_dir(&outbox_a)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x == "lfbundle")
        })
        .collect();
    assert_eq!(
        a_bundles.len(),
        1,
        "A should export exactly one .lfbundle, got {a_bundles:?}"
    );

    let id_a_before = device_id_sot(root_a);

    drop(_ga2);
    let _gb2 = DirGuard::from_utf8(root_b);
    let id_b_before = device_id_sot(root_b);

    let config_b = enable_sync_config(root_b, share);
    ledgerful::sync::run(
        &config_b,
        root_b.join(".ledgerful").as_std_path(),
        TEST_SECRET.as_bytes(),
    )
    .expect("run B import");

    let origin = peer_origin_tx(root_b, &tx_id).expect("B must have A's tx_id");
    assert_eq!(
        origin, "PEER",
        "imported row must be PEER origin, got {origin}"
    );

    assert_eq!(device_id_sot(root_a), id_a_before, "A SoT device_id stable");
    assert_eq!(device_id_sot(root_b), id_b_before, "B SoT device_id stable");
    assert_eq!(device_id_sot(root_b), id_b);
}

/// Poison/tampered ciphertext in outbox → quarantine; no bogus ledger row.
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn golden_negative_poison_quarantines_no_bogus_row() {
    let share_tmp = tempdir().unwrap();
    let share = share_tmp.path();

    let tmp_a = tempdir().unwrap();
    let root_a = Utf8Path::from_path(tmp_a.path()).unwrap();
    setup_git_repo(tmp_a.path());
    let _ga = DirGuard::from_utf8(root_a);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id_a = init_device(root_a);
    let pub_a = read_pub(root_a);

    let tmp_b = tempdir().unwrap();
    let root_b = Utf8Path::from_path(tmp_b.path()).unwrap();
    setup_git_repo(tmp_b.path());
    let _gb = DirGuard::from_utf8(root_b);
    let id_b = init_device(root_b);
    let pub_b = read_pub(root_b);

    trust_peer(
        root_a.join(".ledgerful/sync").as_std_path(),
        &id_b,
        &pub_b,
        false,
    )
    .unwrap();
    trust_peer(
        root_b.join(".ledgerful/sync").as_std_path(),
        &id_a,
        &pub_a,
        false,
    )
    .unwrap();

    // Plant poison as if from A into A's outbox (B will list peer dirs)
    let a_outbox = share.join("devices").join(&id_a);
    fs::create_dir_all(&a_outbox).unwrap();
    fs::write(
        a_outbox.join("poison.lfbundle"),
        b"not-valid-ciphertext!!!!",
    )
    .unwrap();

    let count_before: i64 = {
        let db = root_b.join(".ledgerful/state/ledger.db");
        let conn = Connection::open(db.as_std_path()).unwrap();
        conn.query_row("SELECT COUNT(*) FROM ledger_entries", [], |r| r.get(0))
            .unwrap()
    };

    let config_b = enable_sync_config(root_b, share);
    ledgerful::sync::run(
        &config_b,
        root_b.join(".ledgerful").as_std_path(),
        TEST_SECRET.as_bytes(),
    )
    .expect("run B should succeed even with poison (quarantine path)");

    // Poison moved to B quarantine
    let q = share
        .join("devices")
        .join(&id_b)
        .join("quarantine")
        .join(format!("{id_a}__poison.lfbundle"));
    assert!(
        q.exists() || !a_outbox.join("poison.lfbundle").exists(),
        "poison must be quarantined or removed from peer outbox"
    );
    assert!(q.exists(), "expected quarantine file at {}", q.display());

    let count_after: i64 = {
        let db = root_b.join(".ledgerful/state/ledger.db");
        let conn = Connection::open(db.as_std_path()).unwrap();
        conn.query_row("SELECT COUNT(*) FROM ledger_entries", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        count_before, count_after,
        "poison must not insert ledger rows"
    );
}

/// Far-future bundle_hlc → quarantine via full run path.
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn clock_drift_over_max_quarantines() {
    use ed25519_dalek::SigningKey;
    use ledgerful::sync::bundle::{Bundle, Manifest};
    use ledgerful::sync::hlc::HLC;

    let share_tmp = tempdir().unwrap();
    let share = share_tmp.path();

    let tmp_a = tempdir().unwrap();
    let root_a = Utf8Path::from_path(tmp_a.path()).unwrap();
    setup_git_repo(tmp_a.path());
    let _ga = DirGuard::from_utf8(root_a);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id_a = init_device(root_a);
    // Re-read A's signing key to craft a valid-sig far-future bundle
    let key_bytes = fs::read(root_a.join(".ledgerful/sync/device.key")).unwrap();
    let mut key_arr = [0u8; 32];
    key_arr.copy_from_slice(&key_bytes);
    let sign_key = SigningKey::from_bytes(&key_arr);
    let pub_a = read_pub(root_a);

    let tmp_b = tempdir().unwrap();
    let root_b = Utf8Path::from_path(tmp_b.path()).unwrap();
    setup_git_repo(tmp_b.path());
    let _gb = DirGuard::from_utf8(root_b);
    let _id_b = init_device(root_b);
    trust_peer(
        root_b.join(".ledgerful/sync").as_std_path(),
        &id_a,
        &pub_a,
        false,
    )
    .unwrap();

    // Far-future physical_ms (year ~2286 if ms epoch)
    let future_ms = 10_000_000_000_000u64;
    let mut manifest = Manifest {
        version: 1,
        device_id: id_a.clone(),
        bundle_hlc: HLC {
            physical_ms: future_ms,
            logical: 0,
            node_id: id_a.clone(),
        },
        manifest_sha256: String::new(),
        entry_count: 0,
        entries: vec![],
        tombstones: vec![],
    };
    let (zip_bytes, _) = Bundle::build(&mut manifest, &sign_key).unwrap();
    let encrypted = Bundle::encrypt(&zip_bytes, TEST_SECRET.as_bytes()).unwrap();
    let filename = manifest.filename();

    let a_outbox = share.join("devices").join(&id_a);
    fs::create_dir_all(&a_outbox).unwrap();
    fs::write(a_outbox.join(&filename), &encrypted).unwrap();

    let mut config_b = enable_sync_config(root_b, share);
    config_b.sync.max_clock_drift_seconds = 300; // 5 min

    ledgerful::sync::run(
        &config_b,
        root_b.join(".ledgerful").as_std_path(),
        TEST_SECRET.as_bytes(),
    )
    .expect("run B");

    let q_dir = share
        .join("devices")
        .join(device_id_sot(root_b))
        .join("quarantine");
    assert!(
        q_dir.exists()
            && fs::read_dir(&q_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().contains(&filename)),
        "far-future bundle must be quarantined"
    );
    assert!(
        !a_outbox.join(&filename).exists(),
        "quarantined bundle must leave peer outbox"
    );
}

/// verify command path uses load_peer_keys (smoke: unknown peer fails closed).
#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn verify_uses_load_peer_keys_fail_closed() {
    use ed25519_dalek::SigningKey;
    use ledgerful::commands::sync::verify::handle as handle_verify;
    use ledgerful::sync::bundle::{Bundle, Manifest};
    use ledgerful::sync::hlc::HLC;

    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _g = DirGuard::from_utf8(root);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    init_device(root);

    // Bundle signed by a random untrusted key with a foreign device_id
    let foreign = SigningKey::generate(&mut rand::rng());
    let foreign_id = "device-foreign1";
    let mut manifest = Manifest {
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
    let (zip, _) = Bundle::build(&mut manifest, &foreign).unwrap();
    let enc = Bundle::encrypt(&zip, TEST_SECRET.as_bytes()).unwrap();
    let path = tmp.path().join("foreign.lfbundle");
    fs::write(&path, &enc).unwrap();

    let err = handle_verify(path.to_str().unwrap()).expect_err("unknown peer");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("unknown") || msg.contains("verify") || msg.contains("signature"),
        "expected fail-closed verify, got: {msg}"
    );
}
