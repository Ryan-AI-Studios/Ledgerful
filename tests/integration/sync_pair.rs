//! 0111 pairing invite accept / list / revoke / fail-closed tests.

#[cfg(feature = "sync")]
use camino::Utf8Path;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::init::handle as handle_sync_init;
#[cfg(feature = "sync")]
use ledgerful::commands::sync::pair::handle as handle_sync_pair;
#[cfg(feature = "sync")]
use ledgerful::state::storage::StorageManager;
#[cfg(feature = "sync")]
use ledgerful::sync::peers::{
    format_invite_v1, list_peers, load_peer_keys, parse_invite, trust_peer,
    validate_device_id_for_path, verify_invite,
};
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
fn read_pub(root: &Utf8Path) -> [u8; 32] {
    let bytes = std::fs::read(root.join(".ledgerful/sync/device.pub")).unwrap();
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&bytes);
    pk
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn invite_round_trip_accept_writes_peer_and_list_sees_device() {
    // Device A layout
    let tmp_a = tempdir().unwrap();
    let root_a = Utf8Path::from_path(tmp_a.path()).unwrap();
    setup_git_repo(tmp_a.path());
    let _guard_a = DirGuard::from_utf8(root_a);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id_a = init_device(root_a);
    let pub_a = read_pub(root_a);

    // Generate invite as A (via library for deterministic capture).
    let invite = format_invite_v1(&id_a, &pub_a, TEST_SECRET.as_bytes());
    assert!(invite.starts_with("LF-PAIR-1."));

    // Device B layout
    let tmp_b = tempdir().unwrap();
    let root_b = Utf8Path::from_path(tmp_b.path()).unwrap();
    setup_git_repo(tmp_b.path());
    let _guard_b = DirGuard::from_utf8(root_b);
    let id_b = init_device(root_b);
    assert_ne!(id_a, id_b);

    handle_sync_pair(Some(invite.clone()), false, None, false).expect("accept invite from A on B");

    let peers_dir = root_b.join(".ledgerful/sync/peers");
    assert!(peers_dir.join(format!("{id_a}.pub")).exists());
    let listed = list_peers(root_b.join(".ledgerful/sync").as_std_path()).unwrap();
    assert!(listed.contains(&id_a), "list missing {id_a}: {listed:?}");

    let layout = ledgerful::state::layout::Layout::new(root_b);
    let config = ledgerful::config::load::load_config(&layout).unwrap();
    assert!(!config.sync.enabled, "accept must never enable sync");
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn wrong_secret_accept_fails_no_peer_file() {
    let tmp_a = tempdir().unwrap();
    let root_a = Utf8Path::from_path(tmp_a.path()).unwrap();
    setup_git_repo(tmp_a.path());
    let _ga = DirGuard::from_utf8(root_a);
    let _secret = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id_a = init_device(root_a);
    let invite = format_invite_v1(&id_a, &read_pub(root_a), TEST_SECRET.as_bytes());

    let tmp_b = tempdir().unwrap();
    let root_b = Utf8Path::from_path(tmp_b.path()).unwrap();
    setup_git_repo(tmp_b.path());
    let _gb = DirGuard::from_utf8(root_b);
    init_device(root_b);

    let _wrong = TempEnv::set(
        "LEDGERFUL_SYNC_SECRET",
        "wrong-secret-material-xxxxxxxxxxxxxxxx",
    );
    let err = handle_sync_pair(Some(invite), false, None, false).expect_err("wrong secret");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("invalid") || msg.contains("secret"),
        "got: {msg}"
    );
    let peers = root_b.join(".ledgerful/sync/peers");
    assert!(
        !peers.exists() || std::fs::read_dir(peers.as_std_path()).unwrap().count() == 0,
        "no peer file on wrong secret"
    );
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn tampered_invite_fails() {
    let secret = TEST_SECRET.as_bytes();
    // Need a valid curve point for format; use a real key from init.
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _g = DirGuard::from_utf8(root);
    let _s = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id = init_device(root);
    let pk = read_pub(root);
    let invite = format_invite_v1(&id, &pk, secret);
    let parts: Vec<&str> = invite.split('.').collect();
    let mut bad_pub = parts[2].to_string();
    let flip = if bad_pub.starts_with('A') { 'B' } else { 'A' };
    bad_pub.replace_range(0..1, &flip.to_string());
    let tampered = format!("{}.{}.{}.{}", parts[0], parts[1], bad_pub, parts[3]);
    assert!(verify_invite(secret, &tampered).is_err());
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn self_pair_fails() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _g = DirGuard::from_utf8(root);
    let _s = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id = init_device(root);
    let invite = format_invite_v1(&id, &read_pub(root), TEST_SECRET.as_bytes());
    let err = handle_sync_pair(Some(invite), false, None, false).expect_err("self-pair");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("self") || msg.contains("own"),
        "expected self-pair error, got: {msg}"
    );
}

#[test]
#[cfg(feature = "sync")]
fn path_unsafe_device_id_rejected() {
    for id in ["../x", "a/b", "a.b", "unknown", "", ".", ".."] {
        assert!(
            validate_device_id_for_path(id).is_err(),
            "must reject {id:?}"
        );
    }
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn path_unsafe_invite_rejects_before_write() {
    // Craft invite with unsafe device_id (`..`) — 4-part parse, path-validate before write.
    let secret = TEST_SECRET.as_bytes();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _g = DirGuard::from_utf8(root);
    let _s = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    init_device(root);
    let pk = read_pub(root);

    let invite = format_invite_v1("..", &pk, secret);
    let err = handle_sync_pair(Some(invite), false, None, false).expect_err("path unsafe");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("path")
            || msg.contains("unsafe")
            || msg.contains("..")
            || msg.contains("device_id")
            || msg.contains("invalid"),
        "got: {msg}"
    );
    let peers = root.join(".ledgerful/sync/peers");
    assert!(!peers.exists() || std::fs::read_dir(peers.as_std_path()).unwrap().count() == 0);
}

#[test]
#[cfg(feature = "sync")]
fn invalid_curve_point_rejected_before_write() {
    let tmp = tempdir().unwrap();
    let sync_dir = tmp.path().join("sync");
    std::fs::create_dir_all(&sync_dir).unwrap();
    let mut bad = [0xFFu8; 32];
    bad[0] = 0x01;
    if ed25519_dalek::VerifyingKey::from_bytes(&bad).is_err() {
        assert!(trust_peer(&sync_dir, "device-badcurve", &bad, false).is_err());
        assert!(!sync_dir.join("peers/device-badcurve.pub").exists());
    }
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn reaccept_same_idempotent_different_key_needs_force() {
    let tmp_a = tempdir().unwrap();
    let root_a = Utf8Path::from_path(tmp_a.path()).unwrap();
    setup_git_repo(tmp_a.path());
    let _ga = DirGuard::from_utf8(root_a);
    let _s = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id_a = init_device(root_a);
    let pub_a = read_pub(root_a);
    let invite = format_invite_v1(&id_a, &pub_a, TEST_SECRET.as_bytes());

    let tmp_b = tempdir().unwrap();
    let root_b = Utf8Path::from_path(tmp_b.path()).unwrap();
    setup_git_repo(tmp_b.path());
    let _gb = DirGuard::from_utf8(root_b);
    init_device(root_b);

    handle_sync_pair(Some(invite.clone()), false, None, false).unwrap();
    handle_sync_pair(Some(invite.clone()), false, None, false).expect("idempotent re-accept");

    // Different pubkey same device_id: mint a second real key and forge invite for id_a.
    let tmp_c = tempdir().unwrap();
    let root_c = Utf8Path::from_path(tmp_c.path()).unwrap();
    setup_git_repo(tmp_c.path());
    let _gc = DirGuard::from_utf8(root_c);
    let _ = init_device(root_c);
    let other_pk = read_pub(root_c);
    // Forge invite claiming id_a with other_pk under same secret
    let rekey_invite = format_invite_v1(&id_a, &other_pk, TEST_SECRET.as_bytes());

    drop(_gc);
    let _gb2 = DirGuard::from_utf8(root_b);
    let err = handle_sync_pair(Some(rekey_invite.clone()), false, None, false)
        .expect_err("different key without force");
    assert!(
        format!("{err:#}").to_lowercase().contains("force")
            || format!("{err:#}").to_lowercase().contains("different"),
        "got: {err:#}"
    );
    handle_sync_pair(Some(rekey_invite), false, None, true).expect("force re-key");
    let stored = std::fs::read(
        root_b
            .join(".ledgerful/sync/peers")
            .join(format!("{id_a}.pub")),
    )
    .unwrap();
    assert_eq!(stored.as_slice(), other_pk.as_slice());
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn revoke_removes_from_load_peer_keys() {
    let tmp_a = tempdir().unwrap();
    let root_a = Utf8Path::from_path(tmp_a.path()).unwrap();
    setup_git_repo(tmp_a.path());
    let _ga = DirGuard::from_utf8(root_a);
    let _s = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id_a = init_device(root_a);
    let invite = format_invite_v1(&id_a, &read_pub(root_a), TEST_SECRET.as_bytes());

    let tmp_b = tempdir().unwrap();
    let root_b = Utf8Path::from_path(tmp_b.path()).unwrap();
    setup_git_repo(tmp_b.path());
    let _gb = DirGuard::from_utf8(root_b);
    init_device(root_b);
    handle_sync_pair(Some(invite), false, None, false).unwrap();

    let sync_dir = root_b.join(".ledgerful/sync");
    assert!(
        load_peer_keys(sync_dir.as_std_path())
            .unwrap()
            .contains_key(&id_a)
    );

    handle_sync_pair(None, false, Some(id_a.clone()), false).unwrap();
    assert!(
        !load_peer_keys(sync_dir.as_std_path())
            .unwrap()
            .contains_key(&id_a)
    );
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn conflicting_flags_clear_error() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _g = DirGuard::from_utf8(root);
    let _s = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    init_device(root);

    let err = handle_sync_pair(Some("LF-PAIR-1.x.y.z".into()), true, None, false)
        .expect_err("invite + list");
    assert!(
        format!("{err:#}").to_lowercase().contains("conflict"),
        "got: {err:#}"
    );

    let err =
        handle_sync_pair(None, true, Some("device-x".into()), false).expect_err("list + revoke");
    assert!(
        format!("{err:#}").to_lowercase().contains("conflict"),
        "got: {err:#}"
    );
}

#[test]
#[cfg(feature = "sync")]
fn malformed_peer_file_load_no_panic() {
    let tmp = tempdir().unwrap();
    let sync_dir = tmp.path().join("sync");
    let peers = sync_dir.join("peers");
    std::fs::create_dir_all(&peers).unwrap();
    std::fs::write(peers.join("device-short.pub"), [1u8; 8]).unwrap();
    let map = load_peer_keys(&sync_dir).expect("must not panic");
    assert!(!map.contains_key("device-short"));
}

#[test]
#[cfg(feature = "sync")]
fn temp_not_matching_pub_not_trusted() {
    let tmp = tempdir().unwrap();
    let sync_dir = tmp.path().join("sync");
    let peers = sync_dir.join("peers");
    std::fs::create_dir_all(&peers).unwrap();
    std::fs::write(peers.join(".device-x.pub.tmp"), [2u8; 32]).unwrap();
    let map = load_peer_keys(&sync_dir).unwrap();
    assert!(map.is_empty());
}

#[test]
#[cfg(feature = "sync")]
fn mac_not_old_hash_truncation() {
    let secret = b"team-secret";
    let pk = [9u8; 32];
    let device_id = "device-aabbccdd";
    let mut old_input = Vec::new();
    old_input.extend_from_slice(secret);
    old_input.extend_from_slice(&pk);
    let old = blake3::hash(&old_input);
    let invite = format_invite_v1(device_id, &pk, secret);
    let parsed = parse_invite(&invite).unwrap();
    assert_ne!(&old.as_bytes()[..16], &parsed.tag);
}

#[test]
#[cfg(feature = "sync")]
#[serial_test::serial(env)]
fn generate_invite_emits_lf_pair_prefix() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _g = DirGuard::from_utf8(root);
    let _s = TempEnv::set("LEDGERFUL_SYNC_SECRET", TEST_SECRET);
    let id = init_device(root);
    handle_sync_pair(None, false, None, false).expect("generate");
    // Library path asserts format; CLI prints — also verify format_invite_v1 shape.
    let invite = format_invite_v1(&id, &read_pub(root), TEST_SECRET.as_bytes());
    assert!(invite.starts_with("LF-PAIR-1."));
    assert_eq!(invite.matches('.').count(), 3);
    assert!(!invite.contains('='));
}
