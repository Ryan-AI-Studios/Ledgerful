use ledgerful::commands::init::execute_init;
use ledgerful::commands::ledger_adopt::{
    RecoveryCommand, decide_adoption, execute_ledger_recovery,
};
use ledgerful::commands::verify::verify_ledger_signatures_with_options_and_adoption;
use ledgerful::ledger::chain_checkpoint::{
    CheckpointMode, CheckpointResultKind, classify_against_export, ordered_local_for_head_with_head,
};
use ledgerful::ledger::crypto::{LedgerSignInput, sign_ledger_entry_in_v2};
use ledgerful::ledger::types::{Category, ChangeType, EntryType};
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use rusqlite::Connection;
use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};

fn origin() -> &'static str {
    "https://example.invalid/ledgerful.git"
}

struct Repo {
    root: camino::Utf8PathBuf,
    db: camino::Utf8PathBuf,
    _dir: tempfile::TempDir,
}

fn setup() -> Repo {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());
    Command::new("git")
        .args(["remote", "add", "origin", origin()])
        .current_dir(dir.path())
        .status()
        .unwrap();
    let root = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let _guard = DirGuard::from_utf8(&root);
    fs::create_dir_all(dir.path().join(".ledgerful").join("keys")).unwrap();
    execute_init(false, false).unwrap();
    let db = root.join(".ledgerful").join("state").join("ledger.db");
    Repo {
        root,
        db,
        _dir: dir,
    }
}

fn sign_row(keys: &Path, tx_id: &str, at: &str) -> (String, String) {
    let input = LedgerSignInput::for_new_commit(
        tx_id,
        Category::Feature,
        "summary",
        "reason",
        at,
        "src/a.rs",
        "src/a.rs",
        ChangeType::Modify,
        EntryType::Implementation,
        "Test User",
        None,
        false,
        None,
        "LOCAL",
    );
    let (sig, pk) = sign_ledger_entry_in_v2(keys, &input).unwrap();
    (sig.unwrap(), pk.unwrap())
}

fn insert_extra(db: &Path, keys: &Path, tx_id: &str, at: &str) {
    let (sig, pk) = sign_row(keys, tx_id, at);
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "INSERT INTO transactions (
            tx_id, status, category, entity, entity_normalized, session_id, source, started_at
         ) VALUES (?1, 'COMMITTED', 'FEATURE', 'src/a.rs', 'src/a.rs', 'test', 'test', ?2)",
        rusqlite::params![tx_id, at],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (
            tx_id, category, entry_type, entity, entity_normalized, change_type,
            summary, reason, is_breaking, committed_at, origin, author, signature, public_key,
            sig_version
         ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', 'src/a.rs', 'src/a.rs', 'MODIFY',
            'summary', 'reason', 0, ?2, 'LOCAL', 'Test User', ?3, ?4, 2)",
        rusqlite::params![tx_id, at, sig, pk],
    )
    .unwrap();
}

fn layout(repo: &Repo) -> Layout {
    Layout::new(repo.root.as_str())
}

fn plan(repo: &Repo) -> std::path::PathBuf {
    let output = repo.root.join("recovery.json");
    let _guard = DirGuard::from_utf8(&repo.root);
    execute_ledger_recovery(RecoveryCommand::Plan {
        output: output.as_std_path().to_path_buf(),
        json: true,
    })
    .unwrap();
    output.as_std_path().to_path_buf()
}

#[test]
#[serial(cwd, env)]
fn adopt_plan_is_read_only() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    insert_extra(
        repo.db.as_std_path(),
        &home.path().join(".ledgerful").join("keys"),
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    // Keys may not exist yet. Sign with a directory we create.
    let keys = repo.root.join(".ledgerful").join("keys");
    let _ = keys;
    let before = fs::read(repo.db.as_std_path()).unwrap();
    let output = plan(&repo);
    let after = fs::read(repo.db.as_std_path()).unwrap();
    assert_eq!(before, after);
    let body = fs::read_to_string(&output).unwrap();
    assert!(body.contains("ledgerRecoveryManifest"));
    assert!(!body.contains("publicKey"));
    assert!(!body.contains("signature"));
    assert!(body.contains(origin()));
    assert!(!body.contains(":\\"));
}

#[test]
#[serial(cwd, env)]
fn adopt_plan_rejects_invalid_signature() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let conn = Connection::open(repo.db.as_std_path()).unwrap();
    conn.execute(
        "INSERT INTO transactions (
            tx_id, status, category, entity, entity_normalized, session_id, source, started_at
         ) VALUES ('tx-bad', 'COMMITTED', 'FEATURE', 'a', 'a', 'test', 'test', '2020-01-02T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (
            tx_id, category, entry_type, entity, entity_normalized, change_type,
            summary, reason, is_breaking, committed_at, origin, author, signature, public_key, sig_version
         ) VALUES ('tx-bad', 'FEATURE', 'IMPLEMENTATION', 'a', 'a', 'MODIFY', 's', 'r', 0,
            '2020-01-02T00:00:00Z', 'LOCAL', 't', 'aa', 'bb', 1)",
        [],
    )
    .unwrap();
    let err = execute_ledger_recovery(RecoveryCommand::Plan {
        output: repo.root.join("nope.json").into(),
        json: false,
    })
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("adoption refused"), "{msg}");
}

#[test]
#[serial(cwd, env)]
fn adopt_plan_rejects_fork() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    let head = Connection::open(repo.db.as_std_path())
        .unwrap()
        .query_row(
            "SELECT latest_entry_hash FROM chain_head WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    for (id, at) in [
        ("tx-f1", "2020-02-01T00:00:00Z"),
        ("tx-f2", "2020-02-02T00:00:00Z"),
    ] {
        let (sig, pk) = sign_row(&keys, id, at);
        let conn = Connection::open(repo.db.as_std_path()).unwrap();
        conn.execute(
            "INSERT INTO transactions (
                tx_id, status, category, entity, entity_normalized, session_id, source, started_at
             ) VALUES (?1, 'COMMITTED', 'FEATURE', 'src/a.rs', 'src/a.rs', 'test', 'test', ?2)",
            rusqlite::params![id, at],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ledger_entries (
                tx_id, category, entry_type, entity, entity_normalized, change_type,
                summary, reason, is_breaking, committed_at, origin, author, signature, public_key,
                sig_version, prev_hash
             ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', 'src/a.rs', 'src/a.rs', 'MODIFY',
                'summary', 'reason', 0, ?2, 'LOCAL', 'Test User', ?3, ?4, 2, ?5)",
            rusqlite::params![id, at, sig, pk, head],
        )
        .unwrap();
    }
    let err = execute_ledger_recovery(RecoveryCommand::Plan {
        output: repo.root.as_std_path().join("fork.json"),
        json: false,
    })
    .unwrap_err();
    assert!(format!("{err}").contains("fork"), "{}", err);
}

#[test]
#[serial(cwd, env)]
fn adopt_apply_preserves_original_rows() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    insert_extra(
        repo.db.as_std_path(),
        &keys,
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let before_prev: Option<String> = Connection::open(repo.db.as_std_path())
        .unwrap()
        .query_row(
            "SELECT prev_hash FROM ledger_entries WHERE tx_id = 'tx-extra'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(before_prev.is_none());
    let manifest = plan(&repo);
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest: manifest.clone(),
        yes: true,
        json: true,
    })
    .unwrap();
    let after_prev: Option<String> = Connection::open(repo.db.as_std_path())
        .unwrap()
        .query_row(
            "SELECT prev_hash FROM ledger_entries WHERE tx_id = 'tx-extra'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(after_prev.is_none());
    let count: i64 = Connection::open(repo.db.as_std_path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM ledger_recovery_manifest", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
#[serial(cwd, env)]
fn adopt_second_apply_is_already_applied() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    insert_extra(
        repo.db.as_std_path(),
        &keys,
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let manifest = plan(&repo);
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest: manifest.clone(),
        yes: true,
        json: false,
    })
    .unwrap();
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: true,
    })
    .unwrap();
    let count: i64 = Connection::open(repo.db.as_std_path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries WHERE tx_id LIKE 'adopt-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
#[serial(cwd, env)]
fn adopt_verify_strict_still_fails() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    insert_extra(
        repo.db.as_std_path(),
        &keys,
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let manifest = plan(&repo);
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap();
    let err = verify_ledger_signatures_with_options_and_adoption(
        &layout(&repo),
        true,
        true,
        false,
        None,
        false,
        false,
        false,
    )
    .unwrap_err();
    assert!(format!("{err}").contains("additional genesis"), "{err}");
}

#[test]
#[serial(cwd, env)]
fn adopt_verify_accept_adoption_labels_unestablished() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    insert_extra(
        repo.db.as_std_path(),
        &keys,
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let manifest = plan(&repo);
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap();
    verify_ledger_signatures_with_options_and_adoption(
        &layout(&repo),
        true,
        true,
        false,
        None,
        false,
        true,
        true,
    )
    .expect("accept-adoption");
    let storage = StorageManager::init_with_layout(&layout(&repo)).unwrap();
    let db = ledgerful::ledger::db::LedgerDb::new(storage.get_connection());
    let entries = db.get_all_committed_ledger_entries().unwrap();
    let head = db.get_chain_head().unwrap();
    let decision = decide_adoption(storage.get_connection(), &entries, head.as_ref());
    assert!(decision.accepted, "{}", decision.message);
    assert_eq!(decision.unresolved_count, 0);
    assert!(decision.message.contains("notEstablished"));
}

#[test]
#[serial(cwd, env)]
fn adopt_backup_failure_does_not_mutate() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    insert_extra(
        repo.db.as_std_path(),
        &keys,
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let manifest = plan(&repo);
    let blocker = repo.root.join("not-a-dir");
    fs::write(&blocker, b"x").unwrap();
    let _backup = TempEnv::set("LEDGERFUL_TEST_BACKUP_DIR", blocker.as_str());
    let before = fs::read(repo.db.as_std_path()).unwrap();
    let err = execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap_err();
    assert!(format!("{err}").contains("did not back up"), "{err}");
    assert_eq!(before, fs::read(repo.db.as_std_path()).unwrap());
}

#[test]
#[serial(cwd, env)]
fn adopt_does_not_create_keys_on_refusal() {
    let _env = non_interactive();
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let conn = Connection::open(repo.db.as_std_path()).unwrap();
    conn.execute(
        "INSERT INTO transactions (
            tx_id, status, category, entity, entity_normalized, session_id, source, started_at
         ) VALUES ('tx-bad', 'COMMITTED', 'FEATURE', 'a', 'a', 'test', 'test', '2020-01-02T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (
            tx_id, category, entry_type, entity, entity_normalized, change_type,
            summary, reason, is_breaking, committed_at, origin, author, sig_version
         ) VALUES ('tx-bad', 'FEATURE', 'IMPLEMENTATION', 'a', 'a', 'MODIFY', 's', 'r', 0,
            '2020-01-02T00:00:00Z', 'LOCAL', 't', 1)",
        [],
    )
    .unwrap();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let _ = execute_ledger_recovery(RecoveryCommand::Plan {
        output: repo.root.as_std_path().join("nope.json"),
        json: false,
    });
    assert!(!home.path().join(".ledgerful").join("keys").exists());
}

#[test]
#[serial(cwd, env)]
fn adopt_rejects_tampered_manifest() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    insert_extra(
        repo.db.as_std_path(),
        &keys,
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let manifest = plan(&repo);
    let mut body = fs::read_to_string(&manifest).unwrap();
    body = body.replace("tx-extra", "tx-other");
    fs::write(&manifest, body).unwrap();
    let before = fs::read(repo.db.as_std_path()).unwrap();
    let err = execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap_err();
    assert!(
        format!("{err}").contains("digest") || format!("{err}").contains("match"),
        "{err}"
    );
    assert_eq!(before, fs::read(repo.db.as_std_path()).unwrap());
}

#[test]
#[serial(cwd, env)]
fn adopt_manifest_omits_absolute_path_and_keys() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    insert_extra(
        repo.db.as_std_path(),
        &keys,
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let manifest = plan(&repo);
    let body = fs::read_to_string(manifest).unwrap();
    assert!(!body.contains("private"));
    assert!(!body.contains("BEGIN"));
    assert!(body.contains("https://example.invalid/ledgerful.git"));
}

#[test]
#[serial(cwd, env)]
fn adopt_preserves_checkpoint_extension() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let repo = setup();
    let _guard = DirGuard::from_utf8(&repo.root);
    let keys = home.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys).unwrap();
    let old_head = {
        let storage = StorageManager::init_with_layout(&layout(&repo)).unwrap();
        let db = ledgerful::ledger::db::LedgerDb::new(storage.get_connection());
        db.get_chain_head().unwrap().unwrap()
    };
    insert_extra(
        repo.db.as_std_path(),
        &keys,
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let manifest = plan(&repo);
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap();
    let storage = StorageManager::init_with_layout(&layout(&repo)).unwrap();
    let db = ledgerful::ledger::db::LedgerDb::new(storage.get_connection());
    let entries = db.get_all_committed_ledger_entries().unwrap();
    let head = db.get_chain_head().unwrap().unwrap();
    let ordered = ordered_local_for_head_with_head(&entries, Some(&head));
    let kind = classify_against_export(&ordered, &head, &old_head, CheckpointMode::Checkpoint)
        .expect("extends");
    assert_eq!(kind, CheckpointResultKind::Extends);
}

fn prepared() -> (Repo, std::path::PathBuf, DirGuard) {
    let repo = setup();
    let guard = DirGuard::from_utf8(&repo.root);
    let keys = repo.root.join(".ledgerful").join("keys");
    fs::create_dir_all(keys.as_std_path()).unwrap();
    insert_extra(
        repo.db.as_std_path(),
        keys.as_std_path(),
        "tx-extra",
        "2020-01-02T00:00:00Z",
    );
    let manifest = plan(&repo);
    (repo, manifest, guard)
}

#[test]
#[serial(cwd, env)]
fn adopt_rejects_omitted_row() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let (_repo, manifest, _guard) = prepared();
    let body = fs::read_to_string(&manifest).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
    value["adopted"] = serde_json::json!([]);
    fs::write(&manifest, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    let err = execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap_err();
    assert!(
        format!("{err}").contains("digest") || format!("{err}").contains("match"),
        "{err}"
    );
}

#[test]
#[serial(cwd, env)]
fn adopt_verify_rejects_new_extra_genesis() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let (repo, manifest, _guard) = prepared();
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap();
    let keys = repo.root.join(".ledgerful").join("keys");
    insert_extra(
        repo.db.as_std_path(),
        keys.as_std_path(),
        "tx-new",
        "2020-03-01T00:00:00Z",
    );
    let err = verify_ledger_signatures_with_options_and_adoption(
        &layout(&repo),
        true,
        true,
        false,
        None,
        false,
        false,
        true,
    )
    .unwrap_err();
    assert!(format!("{err}").contains("not established"), "{err}");
}

#[test]
#[serial(cwd, env)]
fn adopt_verify_rejects_mutated_row() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let (repo, manifest, _guard) = prepared();
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap();
    Connection::open(repo.db.as_std_path())
        .unwrap()
        .execute(
            "UPDATE ledger_entries SET summary = 'tampered' WHERE tx_id = 'tx-extra'",
            [],
        )
        .unwrap();
    let err = verify_ledger_signatures_with_options_and_adoption(
        &layout(&repo),
        true,
        true,
        false,
        None,
        false,
        false,
        true,
    )
    .unwrap_err();
    assert!(format!("{err}").contains("not established"), "{err}");
}

#[test]
#[serial(cwd, env)]
fn adopt_restore_backup_matches_preimage() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let (repo, manifest, _guard) = prepared();
    execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap();
    let state = repo.db.parent().unwrap();
    let bak = fs::read_dir(state.as_std_path())
        .unwrap()
        .filter_map(|item| item.ok())
        .find(|item| item.file_name().to_string_lossy().contains(".bak"))
        .expect("backup");
    let restored = tempdir().unwrap();
    let copy = restored.path().join("restored.db");
    fs::copy(bak.path(), &copy).unwrap();
    let conn = Connection::open(&copy).unwrap();
    let extra: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries WHERE tx_id = 'tx-extra'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let adopted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries WHERE tx_id LIKE 'adopt-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(extra, 1);
    assert_eq!(adopted, 0);
}

#[test]
#[serial(cwd, env)]
fn adopt_stale_head_aborts() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let (repo, manifest, _guard) = prepared();
    Connection::open(repo.db.as_std_path())
        .unwrap()
        .execute(
            "UPDATE chain_head SET latest_entry_hash = 'stale-head' WHERE id = 1",
            [],
        )
        .unwrap();
    let before = fs::read(repo.db.as_std_path()).unwrap();
    let err = execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap_err();
    assert!(
        format!("{err}").contains("disagrees") || format!("{err}").contains("match"),
        "{err}"
    );
    let adopted: i64 = Connection::open(repo.db.as_std_path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries WHERE tx_id LIKE 'adopt-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(adopted, 0);
    let _ = before;
}

#[test]
#[serial(cwd, env)]
fn adopt_signing_failure_leaves_rows() {
    let _env = non_interactive();
    let home = tempdir().unwrap();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().unwrap());
    let _home2 = TempEnv::set("HOME", home.path().to_str().unwrap());
    let (repo, manifest, _guard) = prepared();
    let keys = home.path().join(".ledgerful").join("keys");
    if keys.exists() {
        fs::remove_dir_all(&keys).unwrap();
    }
    fs::create_dir_all(keys.parent().unwrap()).unwrap();
    fs::write(&keys, b"not-a-directory").unwrap();
    let err = execute_ledger_recovery(RecoveryCommand::Apply {
        manifest,
        yes: true,
        json: false,
    })
    .unwrap_err();
    assert!(format!("{err}").contains("did not change"), "{err}");
    let adopted: i64 = Connection::open(repo.db.as_std_path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries WHERE tx_id LIKE 'adopt-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(adopted, 0);
}
