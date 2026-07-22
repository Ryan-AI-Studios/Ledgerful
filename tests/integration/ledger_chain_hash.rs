#![allow(non_snake_case)]

use ledgerful::commands::init::execute_init;
use ledgerful::commands::verify::verify_ledger_signatures_with_options;
use ledgerful::config::model::Config;
use ledgerful::ledger::db::LedgerDb;
use ledgerful::ledger::*;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use tempfile::tempdir;

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};

struct RepoSetup {
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    root: camino::Utf8PathBuf,
    db_path: std::path::PathBuf,
    #[allow(dead_code)]
    _cwd_guard: DirGuard,
    #[allow(dead_code)]
    _home_guard: TempEnv,
    #[allow(dead_code)]
    _profile_guard: TempEnv,
}

fn setup_initialized_repo() -> RepoSetup {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());
    let root_utf8 = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let cwd_guard = DirGuard::from_utf8(&root_utf8);

    // Keep all keys/state inside the temp dir so tests never touch the real
    // home directory.
    let home_guard = TempEnv::set("HOME", dir.path().to_str().unwrap());
    let profile_guard = TempEnv::set("USERPROFILE", dir.path().to_str().unwrap());

    execute_init(false, false).unwrap();

    let db_path = root_utf8
        .join(".ledgerful")
        .join("state")
        .join("ledger.db")
        .into_std_path_buf();

    // `execute_init` writes a gate-mode ledger entry. For chain tests we want a
    // clean, known fixture count, so reset the DB and chain head. The schema
    // and keys remain in place.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("DELETE FROM ledger_entries", []).unwrap();
    conn.execute("DELETE FROM chain_head", []).unwrap();
    conn.execute("DELETE FROM transactions", []).unwrap();
    drop(conn);

    RepoSetup {
        dir,
        root: root_utf8,
        db_path,
        _cwd_guard: cwd_guard,
        _home_guard: home_guard,
        _profile_guard: profile_guard,
    }
}

#[test]
#[serial(cwd, env)]
fn chain__two_sequential_commits__linear_no_fork() {
    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("a.rs"), "").unwrap();
    std::fs::write(src_dir.join("b.rs"), "").unwrap();

    // Commit two entries sequentially (different entities, different timestamps
    // and tx_ids). The first becomes the genesis; the second links to it. This
    // deterministically proves the chain stays linear, regardless of SQLite
    // write serialization.
    let entities = ["src/a.rs", "src/b.rs"];
    let mut tx_ids: Vec<String> = Vec::new();
    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for entity in entities.iter() {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: entity.to_string(),
                    planned_action: Some(format!("sequential commit for {entity}")),
                    ..Default::default()
                })
                .unwrap();
            tx_ids.push(tx_id);
        }

        for (offset, entity) in entities.iter().enumerate() {
            let tx_id = tx_ids[offset].clone();
            let committed_at = format!("2026-07-11T10:00:0{offset}Z");
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id,
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: format!("sequential commit for {entity}"),
                        reason: "test".to_string(),
                        committed_at: Some(committed_at),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
        }
    }

    let storage = StorageManager::open_read_only_sqlite_only(&root).unwrap();
    let db = LedgerDb::new(storage.get_connection());
    let entries = db.get_all_committed_ledger_entries().unwrap();
    assert_eq!(
        entries.len(),
        2,
        "two sequential commits must produce exactly two ledger entries"
    );

    let head = db.get_chain_head().unwrap().expect("chain head must exist");
    assert_eq!(head.length, 2, "chain head length must be 2");

    // Strictly linear: exactly one genesis (no prev_hash), the other links to
    // it, and no two entries share the same prev_hash.
    let mut prev_hashes: Vec<Option<String>> =
        entries.iter().map(|e| e.prev_hash.clone()).collect();
    prev_hashes.sort();
    let unique_prevs: std::collections::HashSet<_> = prev_hashes.iter().cloned().collect();
    assert_eq!(
        unique_prevs.len(),
        prev_hashes.len(),
        "no two entries may share the same prev_hash (fork)"
    );
    let genesis_count = prev_hashes.iter().filter(|p| p.is_none()).count();
    assert_eq!(genesis_count, 1, "exactly one genesis entry allowed");

    // Walk from the head backwards through prev_hash links to the genesis.
    let mut walk_hash = Some(head.latest_entry_hash.clone());
    let mut visited = 0i64;
    while let Some(hash) = walk_hash {
        let prev = entries
            .iter()
            .find(|e| {
                ledgerful::ledger::crypto::compute_entry_hash_for_entry(e)
                    .ok()
                    .as_ref()
                    == Some(&hash)
            })
            .expect("head hash must resolve to a chained entry");
        visited += 1;
        walk_hash = prev.prev_hash.clone();
    }
    assert_eq!(
        visited, head.length,
        "chain walk must cover all linked entries"
    );

    // The public API must report the chain as valid.
    let layout = Layout::new(root.as_str());
    verify_ledger_signatures_with_options(&layout, true, true, false, None).unwrap();
}

#[test]
#[serial(cwd, env)]
fn chain__downgrade_deletes_head__verify_chain_fails_closed() {
    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for (committed_at_offset, summary) in ["genesis entry", "post-genesis entry"]
            .into_iter()
            .enumerate()
        {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: "src/main.rs".to_string(),
                    planned_action: Some(summary.to_string()),
                    ..Default::default()
                })
                .unwrap();
            let committed_at = format!("2026-07-11T10:00:0{committed_at_offset}Z");
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id,
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: summary.to_string(),
                        reason: "test".to_string(),
                        committed_at: Some(committed_at),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
        }
    }

    // Simulate real downgrade attack: delete chain head but leave prev_hash
    // values intact. Because entries still reference chain state, this is the
    // adversarial case (not a benign pre-chain ledger).
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute("DELETE FROM chain_head", []).unwrap();
    drop(conn);

    let layout = Layout::new(root.as_str());
    let err = verify_ledger_signatures_with_options(&layout, true, true, false, None).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("downgrade") || msg.contains("No chain head") || msg.contains("missing"),
        "verify --chain must fail closed after downgrade, got: {msg}"
    );
}

#[test]
#[serial(cwd, env)]
fn chain__delete_all_entries_leave_head__verify_chain_fails_orphan_head() {
    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for (committed_at_offset, summary) in ["genesis entry", "post-genesis entry"]
            .into_iter()
            .enumerate()
        {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: "src/main.rs".to_string(),
                    planned_action: Some(summary.to_string()),
                    ..Default::default()
                })
                .unwrap();
            let committed_at = format!("2026-07-11T10:00:0{committed_at_offset}Z");
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id,
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: summary.to_string(),
                        reason: "test".to_string(),
                        committed_at: Some(committed_at),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
        }
    }

    // Simulate entries-wiped corruption: delete all ledger_entries but leave
    // the signed chain_head row. verify --chain must fail closed rather than
    // report the ledger as benignly empty.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute("DELETE FROM ledger_entries", []).unwrap();
    drop(conn);

    let layout = Layout::new(root.as_str());
    let err = verify_ledger_signatures_with_options(&layout, true, true, false, None).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Chain head exists but no ledger entries found"),
        "verify --chain must detect orphan chain head after entries wiped, got: {msg}"
    );
}

#[test]
#[serial(cwd, env)]
fn chain__pre_chain_entries_without_prev_hash__verify_chain_is_benign() {
    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    // Insert a pre-chain entry with no prev_hash and no chain_head row.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute(
        "INSERT INTO transactions (tx_id, status, category, entity, entity_normalized, session_id, source, started_at)
         VALUES ('tx-prechain-001', 'COMMITTED', 'FEATURE', 'src/main.rs', 'src/main.rs', 'test', 'LOCAL', '2026-07-11T10:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (tx_id, category, entry_type, entity, entity_normalized, change_type, summary, reason, is_breaking, committed_at, origin, author, observed, prev_hash)
         VALUES ('tx-prechain-001', 'FEATURE', 'IMPLEMENTATION', 'src/main.rs', 'src/main.rs', 'MODIFY', 'pre-chain entry', 'test', 0, '2026-07-11T10:00:00Z', 'LOCAL', 'test', 0, NULL)",
        [],
    )
    .unwrap();
    drop(conn);

    let layout = Layout::new(root.as_str());
    verify_ledger_signatures_with_options(&layout, false, true, false, None)
        .expect("pre-chain ledger (no prev_hash, no head) must not report downgrade");
}

#[cfg(feature = "web")]
#[test]
#[serial(cwd, env)]
fn chain__pre_chain_entries_without_prev_hash__against_export_of_same_ledger_passes() {
    use ledgerful::export::soc2::generate_soc2_export;

    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    // Insert two pre-chain entries with no prev_hash and no chain_head row.
    // This is a legacy ledger created before the chain feature existed.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute(
        "INSERT INTO transactions (tx_id, status, category, entity, entity_normalized, session_id, source, started_at)
         VALUES ('tx-prechain-001', 'COMMITTED', 'FEATURE', 'src/main.rs', 'src/main.rs', 'test', 'LOCAL', '2026-07-11T10:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO transactions (tx_id, status, category, entity, entity_normalized, session_id, source, started_at)
         VALUES ('tx-prechain-002', 'COMMITTED', 'FEATURE', 'src/main.rs', 'src/main.rs', 'test', 'LOCAL', '2026-07-11T10:00:01Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (tx_id, category, entry_type, entity, entity_normalized, change_type, summary, reason, is_breaking, committed_at, origin, author, observed, prev_hash)
         VALUES ('tx-prechain-001', 'FEATURE', 'IMPLEMENTATION', 'src/main.rs', 'src/main.rs', 'MODIFY', 'first pre-chain entry', 'test', 0, '2026-07-11T10:00:00Z', 'LOCAL', 'test', 0, NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (tx_id, category, entry_type, entity, entity_normalized, change_type, summary, reason, is_breaking, committed_at, origin, author, observed, prev_hash)
         VALUES ('tx-prechain-002', 'FEATURE', 'IMPLEMENTATION', 'src/main.rs', 'src/main.rs', 'MODIFY', 'second pre-chain entry', 'test', 0, '2026-07-11T10:00:01Z', 'LOCAL', 'test', 0, NULL)",
        [],
    )
    .unwrap();
    drop(conn);

    let layout = Layout::new(root.as_str());

    // Export synthesizes a chain head from the entries because no stored head exists.
    let export_zip = tempdir().unwrap();
    let export_path = export_zip.path().join("export.zip");
    let zip_bytes = generate_soc2_export(&layout).unwrap();
    std::fs::write(&export_path, &zip_bytes).unwrap();

    // The local ledger exactly matches the export, so --against-export should
    // pass even though there is no stored chain head.
    verify_ledger_signatures_with_options(&layout, false, true, false, Some(export_path.as_path()))
        .expect("verify --against-export must pass for pre-chain ledger matching its own export");
}

#[test]
#[serial(cwd, env)]
fn chain__delete_middle_entry__verify_chain_fails_localized() {
    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_ids: Vec<String> = Vec::new();
    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for i in 0..3 {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: "src/main.rs".to_string(),
                    planned_action: Some(format!("entry {i}")),
                    ..Default::default()
                })
                .unwrap();
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id.clone(),
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: format!("entry {i}"),
                        reason: "test".to_string(),
                        committed_at: Some(format!("2026-07-11T10:00:0{i}Z")),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
            tx_ids.push(tx_id);
        }
    }

    // Delete the middle entry.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute(
        "DELETE FROM ledger_entries WHERE tx_id = ?1",
        rusqlite::params![tx_ids[1]],
    )
    .unwrap();
    drop(conn);

    let layout = Layout::new(root.as_str());
    let err = verify_ledger_signatures_with_options(&layout, true, true, false, None).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Chain break")
            || msg.contains("Chain head mismatch")
            || msg.contains("length"),
        "verify --chain must detect deleted entry, got: {msg}"
    );
}

#[test]
#[serial(cwd, env)]
fn chain__reorder_entries__verify_chain_fails() {
    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_ids: Vec<String> = Vec::new();
    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for i in 0..3 {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: "src/main.rs".to_string(),
                    planned_action: Some(format!("entry {i}")),
                    ..Default::default()
                })
                .unwrap();
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id.clone(),
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: format!("entry {i}"),
                        reason: "test".to_string(),
                        committed_at: Some(format!("2026-07-11T10:00:0{i}Z")),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
            tx_ids.push(tx_id);
        }
    }

    // Reorder by swapping committed_at of the first two entries.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute(
        "UPDATE ledger_entries SET committed_at = ?1 WHERE tx_id = ?2",
        rusqlite::params!["2026-07-11T10:00:01Z", tx_ids[0]],
    )
    .unwrap();
    conn.execute(
        "UPDATE ledger_entries SET committed_at = ?1 WHERE tx_id = ?2",
        rusqlite::params!["2026-07-11T10:00:00Z", tx_ids[1]],
    )
    .unwrap();
    drop(conn);

    let layout = Layout::new(root.as_str());
    // Reordering invalidates the per-entry signatures (basis includes
    // committed_at), so verify the chain linkage only, not the signatures.
    let err = verify_ledger_signatures_with_options(&layout, false, true, false, None).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Chain break")
            || msg.contains("Chain head mismatch")
            || msg.contains("length"),
        "verify --chain must detect reordered entries, got: {msg}"
    );
}

#[test]
#[serial(cwd, env)]
fn chain__insert_unlinked_entry__verify_chain_fails() {
    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_ids: Vec<String> = Vec::new();
    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for i in 0..2 {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: "src/main.rs".to_string(),
                    planned_action: Some(format!("entry {i}")),
                    ..Default::default()
                })
                .unwrap();
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id.clone(),
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: format!("entry {i}"),
                        reason: "test".to_string(),
                        committed_at: Some(format!("2026-07-11T10:00:0{i}Z")),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
            tx_ids.push(tx_id);
        }
    }

    // Insert a third entry with no prev_hash after a chain already exists.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute(
        "INSERT INTO transactions (tx_id, status, category, entity, entity_normalized, session_id, source, started_at)
         VALUES (?1, 'COMMITTED', 'FEATURE', 'src/main.rs', 'src/main.rs', 'test', 'LOCAL', '2026-07-11T10:00:02Z')",
        rusqlite::params!["tx-unlinked-001"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (tx_id, category, entry_type, entity, entity_normalized, change_type, summary, reason, is_breaking, committed_at, origin, author, observed, prev_hash)
         VALUES (?1, 'FEATURE', 'IMPLEMENTATION', 'src/main.rs', 'src/main.rs', 'MODIFY', 'unlinked', 'test', 0, '2026-07-11T10:00:02Z', 'LOCAL', 'test', 0, NULL)",
        rusqlite::params!["tx-unlinked-001"],
    )
    .unwrap();
    drop(conn);

    let layout = Layout::new(root.as_str());
    let err = verify_ledger_signatures_with_options(&layout, true, true, false, None).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Chain break")
            || msg.contains("Chain head mismatch")
            || msg.contains("length"),
        "verify --chain must detect unlinked inserted entry, got: {msg}"
    );
}

#[cfg(feature = "web")]
#[test]
#[serial(cwd, env)]
fn chain__export_contains_chain_head_json__valid_head_data() {
    use ledgerful::export::soc2::generate_soc2_export;

    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    {
        let mut storage = StorageManager::init(setup.db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: "src/main.rs".to_string(),
                planned_action: Some("export chain head test".to_string()),
                ..Default::default()
            })
            .unwrap();
        // Production path signs v2 at commit (no client-supplied sig).
        tx_mgr
            .commit_change(
                tx_id,
                CommitRequest {
                    change_type: ChangeType::Modify,
                    summary: "export chain head test".to_string(),
                    reason: "test".to_string(),
                    committed_at: Some("2026-07-11T10:00:00Z".to_string()),
                    signature: None,
                    public_key: None,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
    }

    let layout = Layout::new(root.as_str());
    let zip_bytes = generate_soc2_export(&layout).unwrap();

    let file = std::io::Cursor::new(&zip_bytes);
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut head_entry = archive.by_name("chain_head.json").unwrap();
    let mut head_buf = Vec::new();
    std::io::Read::read_to_end(&mut head_entry, &mut head_buf).unwrap();
    let head: ledgerful::ledger::types::ChainHead = serde_json::from_slice(&head_buf).unwrap();

    assert!(!head.latest_entry_hash.is_empty());
    assert!(!head.genesis.is_empty());
    assert_eq!(head.length, 1);
    assert!(
        head.head_signature.as_ref().unwrap_or(&String::new()).len() > 32,
        "head_signature must be present"
    );
    assert!(
        head.head_public_key
            .as_ref()
            .unwrap_or(&String::new())
            .len()
            > 16,
        "head_public_key must be present"
    );
    assert!(ledgerful::ledger::crypto::verify_chain_head(
        &head.latest_entry_hash,
        &head.genesis,
        head.length,
        head.head_signature.as_deref().unwrap_or(""),
        head.head_public_key.as_deref().unwrap_or(""),
    ));
}

#[cfg(feature = "web")]
#[test]
#[serial(cwd, env)]
fn chain__against_export_after_rollback__detects_rollback() {
    use ledgerful::export::soc2::generate_soc2_export;

    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_ids: Vec<String> = Vec::new();
    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for i in 0..3 {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: "src/main.rs".to_string(),
                    planned_action: Some(format!("export entry {i}")),
                    ..Default::default()
                })
                .unwrap();
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id.clone(),
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: format!("export entry {i}"),
                        reason: "test".to_string(),
                        committed_at: Some(format!("2026-07-11T10:00:0{i}Z")),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
            tx_ids.push(tx_id);
        }
    }

    let layout = Layout::new(root.as_str());
    let export_zip = tempdir().unwrap();
    let export_path = export_zip.path().join("export.zip");
    let zip_bytes = generate_soc2_export(&layout).unwrap();
    std::fs::write(&export_path, &zip_bytes).unwrap();

    // Roll back the DB: delete the latest entry and re-sign a truncated head.
    // tx_ids[2]'s prev_hash is the hash of tx_ids[1], so capture it before
    // deleting the third entry and use it as the new latest hash.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    let second_hash: String = conn
        .query_row(
            "SELECT prev_hash FROM ledger_entries WHERE tx_id = ?1",
            rusqlite::params![tx_ids[2]],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "DELETE FROM ledger_entries WHERE tx_id = ?1",
        rusqlite::params![tx_ids[2]],
    )
    .unwrap();
    drop(conn);

    // Re-sign the truncated head so the local signature verifies (simulating
    // an attacker who has the signing key). The export still carries the
    // original head, so --against-export must detect the rollback by length.
    let keys = root.join(".ledgerful").join("keys");
    let (head_sig, head_pub) = ledgerful::ledger::crypto::sign_chain_head(
        keys.as_std_path(),
        &second_hash,
        "2026-07-11T10:00:00Z",
        2,
    )
    .unwrap();
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute(
        "UPDATE chain_head SET latest_entry_hash = ?1, length = 2, updated_at = '2026-07-11T10:00:01Z', head_signature = ?2, head_public_key = ?3",
        rusqlite::params![second_hash, head_sig.unwrap_or_default(), head_pub.unwrap_or_default()],
    )
    .unwrap();
    drop(conn);

    let err = verify_ledger_signatures_with_options(
        &layout,
        true,
        true,
        false,
        Some(export_path.as_path()),
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("rollback")
            || msg.contains("tail truncation")
            || msg.contains("does not match exported"),
        "verify --against-export must detect rollback, got: {msg}"
    );
}

#[cfg(feature = "web")]
#[test]
#[serial(cwd, env)]
fn chain__against_export_missing_head_with_links__fails_closed_downgrade() {
    use ledgerful::export::soc2::generate_soc2_export;

    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for i in 0..2 {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: "src/main.rs".to_string(),
                    planned_action: Some(format!("downgrade export entry {i}")),
                    ..Default::default()
                })
                .unwrap();
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id,
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: format!("downgrade export entry {i}"),
                        reason: "test".to_string(),
                        committed_at: Some(format!("2026-07-11T10:00:0{i}Z")),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
        }
    }

    let layout = Layout::new(root.as_str());
    let export_zip = tempdir().unwrap();
    let export_path = export_zip.path().join("export.zip");
    let zip_bytes = generate_soc2_export(&layout).unwrap();
    std::fs::write(&export_path, &zip_bytes).unwrap();

    // Simulate Option-A downgrade: strip the signed chain_head row while
    // leaving prev_hash links in the ledger entries intact.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute("DELETE FROM chain_head", []).unwrap();
    drop(conn);

    let err = verify_ledger_signatures_with_options(
        &layout,
        true,
        true,
        false,
        Some(export_path.as_path()),
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Chain head is missing but entries have chain links (downgrade detected)"),
        "verify --against-export must fail closed on stripped chain head, got: {msg}"
    );
}

#[cfg(feature = "web")]
#[test]
#[serial(cwd, env)]
fn chain__against_export_tail_deleted_with_head_unchanged__fails_local_truncation() {
    use ledgerful::export::soc2::generate_soc2_export;

    let _env_non_interactive = non_interactive();
    let setup = setup_initialized_repo();
    let root = setup.root.clone();
    let db_path = setup.db_path.clone();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_ids: Vec<String> = Vec::new();
    {
        let mut storage = StorageManager::init(db_path.as_path()).unwrap();
        let mut tx_mgr =
            TransactionManager::new(&mut storage, root.clone().into(), Config::default());
        for i in 0..3 {
            let tx_id = tx_mgr
                .start_change(TransactionRequest {
                    category: Category::Feature,
                    entity: "src/main.rs".to_string(),
                    planned_action: Some(format!("tail delete entry {i}")),
                    ..Default::default()
                })
                .unwrap();
            // Production path signs v2 at commit (no client-supplied sig).
            tx_mgr
                .commit_change(
                    tx_id.clone(),
                    CommitRequest {
                        change_type: ChangeType::Modify,
                        summary: format!("tail delete entry {i}"),
                        reason: "test".to_string(),
                        committed_at: Some(format!("2026-07-11T10:00:0{i}Z")),
                        signature: None,
                        public_key: None,
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
            tx_ids.push(tx_id);
        }
    }

    let layout = Layout::new(root.as_str());
    let export_zip = tempdir().unwrap();
    let export_path = export_zip.path().join("export.zip");
    let zip_bytes = generate_soc2_export(&layout).unwrap();
    std::fs::write(&export_path, &zip_bytes).unwrap();

    // Simulate local truncation attack: delete the newest entry but leave
    // chain_head untouched. The stored head still matches the export, but
    // the live chain no longer reaches it.
    let conn = rusqlite::Connection::open(db_path.as_path()).unwrap();
    conn.execute(
        "DELETE FROM ledger_entries WHERE tx_id = ?1",
        rusqlite::params![tx_ids[2]],
    )
    .unwrap();
    drop(conn);

    let err = verify_ledger_signatures_with_options(
        &layout,
        true,
        true,
        false,
        Some(export_path.as_path()),
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Chain length mismatch") || msg.contains("Chain head mismatch"),
        "verify --against-export must detect local truncation when tail is deleted but head is unchanged, got: {msg}"
    );
}
