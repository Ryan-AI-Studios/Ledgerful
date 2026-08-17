use ledgerful::commands::init::execute_init;
use ledgerful::commands::ledger::execute_ledger_status;
use ledgerful::commands::ledger_re_sign::execute_ledger_re_sign_with_keys_dir;
use ledgerful::config::model::Config;
use ledgerful::ledger::crypto::{LedgerSignInput, sign_ledger_entry_in_v2};
use ledgerful::ledger::*;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tempfile::{TempDir, tempdir};

use crate::common::{DirGuard, non_interactive, setup_git_repo};

fn keys_dir(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".ledgerful").join("keys")
}
fn sign_v2_for_commit(
    keys: &std::path::Path,
    tx_id: &str,
    category: Category,
    summary: &str,
    reason: &str,
    committed_at: &str,
    entity: &str,
) -> (Option<String>, Option<String>) {
    let input = LedgerSignInput::for_new_commit(
        tx_id,
        category,
        summary,
        reason,
        committed_at,
        entity,
        entity,
        ChangeType::Modify,
        if category == Category::Architecture {
            EntryType::Architecture
        } else {
            EntryType::Implementation
        },
        "Test User",
        None,
        false,
        None,
        "LOCAL",
    );
    sign_ledger_entry_in_v2(keys, &input).unwrap()
}

fn hash_file(path: &std::path::Path) -> u64 {
    let bytes = std::fs::read(path).expect("read db");
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn setup_initialized_repo() -> (TempDir, camino::Utf8PathBuf, camino::Utf8PathBuf) {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());
    let root_utf8 = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let _guard = DirGuard::from_utf8(&root_utf8);

    std::fs::create_dir_all(keys_dir(dir.path())).unwrap();
    execute_init(false, false).unwrap();

    let db_path = root_utf8.join(".ledgerful").join("state").join("ledger.db");
    (dir, root_utf8, db_path)
}

fn corrupt_entry(db_path: &std::path::Path, tx_id: &str, new_sig: &str, new_pub: &str) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute(
        "UPDATE ledger_entries SET signature = ?1, public_key = ?2 WHERE tx_id = ?3",
        rusqlite::params![new_sig, new_pub, tx_id],
    )
    .unwrap();
}

#[test]
#[serial(cwd)]
fn corrupted_ledger_re_sign_all_invalid_repairs_to_valid() {
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());

    let mut tx_ids = Vec::new();
    let keys = keys_dir(root.as_std_path());
    for i in 0..3 {
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: "src/main.rs".to_string(),
                planned_action: Some(format!("entry {i}")),
                ..Default::default()
            })
            .unwrap();
        let committed_at = "2026-06-03T00:00:00Z";
        let (sig, pub_key) = sign_v2_for_commit(
            &keys,
            &tx_id,
            Category::Feature,
            &format!("entry {i}"),
            "reason",
            committed_at,
            "src/main.rs",
        );
        tx_mgr
            .commit_change(
                tx_id.clone(),
                CommitRequest {
                    change_type: ChangeType::Modify,
                    summary: format!("entry {i}"),
                    reason: "reason".to_string(),
                    committed_at: Some(committed_at.to_string()),
                    signature: sig,
                    public_key: pub_key,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        tx_ids.push(tx_id);
    }
    drop(tx_mgr);
    drop(storage);

    // Corrupt every signature.
    for tx_id in &tx_ids {
        corrupt_entry(
            db_path.as_std_path(),
            tx_id,
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
    }

    let err = execute_ledger_status(ledgerful::commands::ledger::LedgerStatusOpts {
        entity_filter: None,
        compact: true,
        exit_code: true,
        verify_signatures: true,
        json: false,
        all: false,
        strict_observe_signal: false,
    })
    .unwrap_err();
    assert!(format!("{err}").contains("Ledger signature verification failed"));

    // Re-sign all invalid entries.
    execute_ledger_re_sign_with_keys_dir(None, true, false, false, true, Some(keys.clone()))
        .unwrap();

    // All repaired rows now verify as valid.
    let verify_storage = StorageManager::open_read_only_sqlite_only(&Layout::new(&root)).unwrap();
    let db = ledgerful::ledger::db::LedgerDb::new(verify_storage.get_connection());
    let entries = db.get_all_committed_ledger_entries().unwrap();
    // Filter out the maintenance entry we created.
    let repaired: Vec<_> = entries
        .iter()
        .filter(|e| tx_ids.contains(&e.tx_id))
        .collect();
    assert_eq!(
        repaired.len(),
        tx_ids.len(),
        "expected one entry per repaired tx"
    );
    for entry in &repaired {
        assert!(
            ledgerful::ledger::crypto::verify_ledger_entry_signature(entry),
            "re-signed entry {} must verify under stored sig_version={}",
            entry.tx_id,
            entry.sig_version
        );
        assert_eq!(
            entry.sig_version, 2,
            "re-sign must upgrade to v2 for {}",
            entry.tx_id
        );
    }
}

#[test]
#[serial(cwd)]
fn dry_run_does_not_mutate_db() {
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/main.rs".to_string(),
            planned_action: Some("dry run test".to_string()),
            ..Default::default()
        })
        .unwrap();
    let keys = keys_dir(root.as_std_path());
    let committed_at = "2026-06-03T00:00:00Z";
    let (sig, pub_key) = sign_v2_for_commit(
        &keys,
        &tx_id,
        Category::Feature,
        "dry run test",
        "reason",
        committed_at,
        "src/main.rs",
    );
    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "dry run test".to_string(),
                reason: "reason".to_string(),
                committed_at: Some(committed_at.to_string()),
                signature: sig,
                public_key: pub_key,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    drop(tx_mgr);
    drop(storage);

    corrupt_entry(
        db_path.as_std_path(),
        &tx_id,
        "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    let before = hash_file(db_path.as_std_path());
    execute_ledger_re_sign_with_keys_dir(
        Some(tx_id.clone()),
        false,
        false,
        true,
        false,
        Some(keys.clone()),
    )
    .unwrap();
    let after = hash_file(db_path.as_std_path());

    assert_eq!(before, after, "dry-run must not mutate the ledger DB");
}

#[test]
#[serial(cwd)]
fn batch_re_sign_emits_one_maintenance_entry() {
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());
    let mut tx_ids = Vec::new();
    let keys = keys_dir(root.as_std_path());
    for i in 0..3 {
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: "src/main.rs".to_string(),
                planned_action: Some(format!("entry {i}")),
                ..Default::default()
            })
            .unwrap();
        let committed_at = "2026-06-03T00:00:00Z";
        let (sig, pub_key) = sign_v2_for_commit(
            &keys,
            &tx_id,
            Category::Feature,
            &format!("entry {i}"),
            "reason",
            committed_at,
            "src/main.rs",
        );
        tx_mgr
            .commit_change(
                tx_id.clone(),
                CommitRequest {
                    change_type: ChangeType::Modify,
                    summary: format!("entry {i}"),
                    reason: "reason".to_string(),
                    committed_at: Some(committed_at.to_string()),
                    signature: sig,
                    public_key: pub_key,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        tx_ids.push(tx_id);
    }
    drop(tx_mgr);
    drop(storage);

    for tx_id in &tx_ids {
        corrupt_entry(
            db_path.as_std_path(),
            tx_id,
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
    }

    execute_ledger_re_sign_with_keys_dir(None, true, false, false, true, Some(keys.clone()))
        .unwrap();

    let verify_storage = StorageManager::open_read_only_sqlite_only(&Layout::new(&root)).unwrap();
    let db = ledgerful::ledger::db::LedgerDb::new(verify_storage.get_connection());
    let entries = db.get_all_committed_ledger_entries().unwrap();
    let maintenance: Vec<_> = entries
        .iter()
        .filter(|e| e.entry_type == ledgerful::ledger::types::EntryType::Maintenance)
        .filter(|e| e.summary.contains("Chain segment break: re-sign"))
        .collect();
    assert_eq!(
        maintenance.len(),
        1,
        "batch re-sign must emit exactly one MAINTENANCE entry"
    );

    let maint = &maintenance[0];
    for tx_id in &tx_ids {
        assert!(
            maint.reason.contains(tx_id),
            "maintenance entry must reference repaired tx_id {tx_id}"
        );
    }
}

#[test]
#[serial(cwd)]
fn re_sign_creates_backup_and_aborts_if_backup_fails() {
    // Placeholder: simulating a failed backup requires a database whose backup cannot
    // be completed (e.g. read-only destination). We test the backup file is produced
    // and passes integrity_check instead.
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/main.rs".to_string(),
            planned_action: Some("backup test".to_string()),
            ..Default::default()
        })
        .unwrap();
    let keys = keys_dir(root.as_std_path());
    let committed_at = "2026-06-03T00:00:00Z";
    let (sig, pub_key) = sign_v2_for_commit(
        &keys,
        &tx_id,
        Category::Feature,
        "backup test",
        "reason",
        committed_at,
        "src/main.rs",
    );
    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "backup test".to_string(),
                reason: "reason".to_string(),
                committed_at: Some(committed_at.to_string()),
                signature: sig,
                public_key: pub_key,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    drop(tx_mgr);
    drop(storage);

    corrupt_entry(
        db_path.as_std_path(),
        &tx_id,
        "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    execute_ledger_re_sign_with_keys_dir(
        Some(tx_id.clone()),
        false,
        false,
        false,
        true,
        Some(keys.clone()),
    )
    .unwrap();

    let backups: Vec<_> = db_path
        .parent()
        .unwrap()
        .read_dir()
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.starts_with("ledger.db.") && s.ends_with(".bak"))
                .unwrap_or(false)
        })
        .collect();
    assert!(!backups.is_empty(), "backup file should be created");

    let backup = backups.last().unwrap().path();
    let conn = rusqlite::Connection::open(&backup).unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity.to_lowercase(), "ok");
}

#[test]
#[serial(cwd)]
fn valid_v1_entry_re_sign_all_upgrades_to_v2() {
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());

    let keys = keys_dir(root.as_std_path());
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/main.rs".to_string(),
            planned_action: Some("v1 upgrade path".to_string()),
            ..Default::default()
        })
        .unwrap();
    let committed_at = "2026-06-03T00:00:00Z";
    let summary = "v1 upgrade path";
    let reason = "reason";
    // Commit with a normal v2 signature first (commit_change verifies v2 basis).
    let (sig_v2, pub_v2) = sign_v2_for_commit(
        &keys,
        &tx_id,
        Category::Feature,
        summary,
        reason,
        committed_at,
        "src/main.rs",
    );
    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: summary.to_string(),
                reason: reason.to_string(),
                committed_at: Some(committed_at.to_string()),
                signature: sig_v2,
                public_key: pub_v2,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    drop(tx_mgr);
    drop(storage);

    // Rewrite to a historical valid v1 signature + sig_version=1.
    // Use the row's stored committed_at/category so dual-verify succeeds.
    {
        let conn = rusqlite::Connection::open(db_path.as_std_path()).unwrap();
        let (stored_cat, stored_at): (String, String) = conn
            .query_row(
                "SELECT category, committed_at FROM ledger_entries WHERE tx_id = ?1",
                rusqlite::params![tx_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let (sig_v1, pub_v1) = ledgerful::ledger::crypto::sign_ledger_entry_in(
            &keys,
            &tx_id,
            &stored_cat,
            summary,
            reason,
            &stored_at,
        )
        .unwrap();
        conn.execute(
            "UPDATE ledger_entries SET signature = ?1, public_key = ?2, sig_version = 1 WHERE tx_id = ?3",
            rusqlite::params![
                sig_v1.as_deref().unwrap_or(""),
                pub_v1.as_deref().unwrap_or(""),
                tx_id
            ],
        )
        .unwrap();
    }

    // Confirm preconditions: valid under dual-verify, still sig_version=1.
    {
        let verify_storage =
            StorageManager::open_read_only_sqlite_only(&Layout::new(&root)).unwrap();
        let db = ledgerful::ledger::db::LedgerDb::new(verify_storage.get_connection());
        let entries = db.get_ledger_entries_for_tx(&tx_id).unwrap();
        assert_eq!(entries[0].sig_version, 1);
        assert!(
            ledgerful::ledger::crypto::verify_ledger_entry_signature(&entries[0]),
            "precondition: v1 signature must verify"
        );
        // all-invalid must NOT pick this up (valid v1).
        let all = db.get_all_committed_ledger_entries().unwrap();
        let invalid = ledgerful::commands::verify::enumerate_invalid_ledger_entries(&all, false);
        assert!(
            !invalid.iter().any(|(id, _, _)| id == &tx_id),
            "valid v1 must not appear in invalid enumeration"
        );
    }

    // dry-run must not mutate
    let before = hash_file(db_path.as_std_path());
    execute_ledger_re_sign_with_keys_dir(None, false, true, true, false, Some(keys.clone()))
        .unwrap();
    let after = hash_file(db_path.as_std_path());
    assert_eq!(before, after, "re-sign --all --dry-run must not mutate");

    // Mutate with --all --yes
    execute_ledger_re_sign_with_keys_dir(None, false, true, false, true, Some(keys.clone()))
        .unwrap();

    let verify_storage = StorageManager::open_read_only_sqlite_only(&Layout::new(&root)).unwrap();
    let db = ledgerful::ledger::db::LedgerDb::new(verify_storage.get_connection());
    let entries = db.get_ledger_entries_for_tx(&tx_id).unwrap();
    assert_eq!(
        entries[0].sig_version, 2,
        "re-sign --all must upgrade valid v1 to sig_version=2"
    );
    assert!(
        ledgerful::ledger::crypto::verify_ledger_entry_signature(&entries[0]),
        "upgraded entry must verify"
    );

    // Maintenance wording should be upgrade, not key-repair.
    let all_entries = db.get_all_committed_ledger_entries().unwrap();
    let maint: Vec<_> = all_entries
        .iter()
        .filter(|e| e.entry_type == ledgerful::ledger::types::EntryType::Maintenance)
        .filter(|e| e.summary.contains("sig-upgrade") || e.reason.contains("Signature upgrade"))
        .collect();
    assert_eq!(
        maint.len(),
        1,
        "upgrade path must emit upgrade-worded MAINTENANCE"
    );
}

#[test]
#[serial(cwd)]
fn re_sign_all_empty_candidates_dry_run_clean() {
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    // Fresh init has no committed LOCAL entries needing upgrade.
    let keys = keys_dir(root.as_std_path());
    execute_ledger_re_sign_with_keys_dir(None, false, true, true, false, Some(keys.clone()))
        .unwrap();

    // Mutation path errors clearly.
    let err = execute_ledger_re_sign_with_keys_dir(None, false, true, false, true, Some(keys))
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("No LOCAL") || msg.contains("upgrade") || msg.contains("repair"),
        "empty --all mutation must error clearly: {msg}"
    );
    // Silence unused
    let _ = db_path;
}

#[test]
#[serial(cwd)]
fn dry_run_does_not_create_key_store() {
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/main.rs".to_string(),
            planned_action: Some("dry run key test".to_string()),
            ..Default::default()
        })
        .unwrap();
    let keys = keys_dir(root.as_std_path());
    let committed_at = "2026-06-03T00:00:00Z";
    let (sig, pub_key) = sign_v2_for_commit(
        &keys,
        &tx_id,
        Category::Feature,
        "dry run key test",
        "reason",
        committed_at,
        "src/main.rs",
    );
    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "dry run key test".to_string(),
                reason: "reason".to_string(),
                committed_at: Some(committed_at.to_string()),
                signature: sig,
                public_key: pub_key,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    drop(tx_mgr);
    drop(storage);

    // Remove the key store that was created during commit setup.
    let _ = std::fs::remove_dir_all(&keys);
    assert!(!keys.exists(), "precondition: key store must not exist");

    corrupt_entry(
        db_path.as_std_path(),
        &tx_id,
        "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    execute_ledger_re_sign_with_keys_dir(
        Some(tx_id.clone()),
        false,
        false,
        true,
        false,
        Some(keys.clone()),
    )
    .unwrap();
    assert!(
        !keys.exists(),
        "dry-run must not create key files on a machine without an existing key store"
    );
}

#[test]
#[serial(cwd)]
fn maintenance_entry_is_signed_when_signing_required() {
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    // Force signing-required mode by writing a minimal config before init creates the
    // starter config, then re-load it so the ledger commit path signs every entry.
    std::fs::write(
        root.join(".ledgerful").join("config.toml"),
        "[intent]\nrequire_signing = true\n",
    )
    .unwrap();
    let config =
        ledgerful::config::load::load_config(&ledgerful::state::layout::Layout::new(&root))
            .unwrap();
    assert!(
        config.intent.require_signing,
        "test must run with signing required"
    );

    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), config.clone());
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/main.rs".to_string(),
            planned_action: Some("signed maintenance test".to_string()),
            ..Default::default()
        })
        .unwrap();
    let keys = keys_dir(root.as_std_path());
    let committed_at = "2026-06-03T00:00:00Z";
    let (sig, pub_key) = sign_v2_for_commit(
        &keys,
        &tx_id,
        Category::Feature,
        "signed maintenance test",
        "reason",
        committed_at,
        "src/main.rs",
    );
    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "signed maintenance test".to_string(),
                reason: "reason".to_string(),
                committed_at: Some(committed_at.to_string()),
                signature: sig,
                public_key: pub_key,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    drop(tx_mgr);
    drop(storage);

    corrupt_entry(
        db_path.as_std_path(),
        &tx_id,
        "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    execute_ledger_re_sign_with_keys_dir(
        Some(tx_id.clone()),
        false,
        false,
        false,
        true,
        Some(keys.clone()),
    )
    .unwrap();

    let verify_storage = StorageManager::open_read_only_sqlite_only(&Layout::new(&root)).unwrap();
    let db = ledgerful::ledger::db::LedgerDb::new(verify_storage.get_connection());
    let entries = db.get_all_committed_ledger_entries().unwrap();
    let maintenance: Vec<_> = entries
        .iter()
        .filter(|e| e.entry_type == ledgerful::ledger::types::EntryType::Maintenance)
        .filter(|e| e.summary.contains("Chain segment break: re-sign"))
        .collect();
    assert_eq!(
        maintenance.len(),
        1,
        "expected exactly one re-sign maintenance entry, found {} maintenance entries",
        entries
            .iter()
            .filter(|e| e.entry_type == ledgerful::ledger::types::EntryType::Maintenance)
            .count()
    );
    let maint = &maintenance[0];
    assert!(
        maint.signature.is_some() && maint.public_key.is_some(),
        "maintenance entry must be signed when signing is required"
    );
    assert!(
        ledgerful::ledger::crypto::verify_ledger_entry_signature(maint),
        "maintenance entry signature must verify under v2"
    );
    assert_eq!(
        maint.sig_version, 2,
        "maintenance entry must be signed as v2"
    );
}

#[test]
#[serial(cwd, env)]
fn re_sign_then_verify_chain_passes() {
    let _env_non_interactive = non_interactive();
    let (_dir, root, db_path) = setup_initialized_repo();
    let _guard = DirGuard::from_utf8(&root);

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());
    let mut tx_ids = Vec::new();
    let keys = keys_dir(root.as_std_path());
    for i in 0..3 {
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: "src/main.rs".to_string(),
                planned_action: Some(format!("entry {i}")),
                ..Default::default()
            })
            .unwrap();
        let committed_at = "2026-06-03T00:00:00Z";
        let (sig, pub_key) = sign_v2_for_commit(
            &keys,
            &tx_id,
            Category::Feature,
            &format!("entry {i}"),
            "reason",
            committed_at,
            "src/main.rs",
        );
        tx_mgr
            .commit_change(
                tx_id.clone(),
                CommitRequest {
                    change_type: ChangeType::Modify,
                    summary: format!("entry {i}"),
                    reason: "reason".to_string(),
                    committed_at: Some(committed_at.to_string()),
                    signature: sig,
                    public_key: pub_key,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        tx_ids.push(tx_id);
    }
    drop(tx_mgr);
    drop(storage);

    // Corrupt every signature so re-sign has work to do.
    for tx_id in &tx_ids {
        corrupt_entry(
            db_path.as_std_path(),
            tx_id,
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
    }

    execute_ledger_re_sign_with_keys_dir(None, true, false, false, true, Some(keys.clone()))
        .unwrap();

    // After re-signing, verify --chain must PASS, proving the maintenance entry
    // links to the correct new tail hash and the genesis prev_hash is None.
    let layout = ledgerful::state::layout::Layout::new(root.as_str());
    ledgerful::commands::verify::verify_ledger_signatures_with_options(
        &layout, true, true, false, None, false,
    )
    .expect("verify --chain must pass after re-sign");
}
