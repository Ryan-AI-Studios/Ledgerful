use super::backup::{backup_ledger_db, verify_backup_integrity};
use super::execute_ledger_re_sign_with_keys_dir;
use super::mutate::build_maintenance_entry;
use super::preview::{enumerate_upgrade_candidates, key_fingerprint, resolve_re_sign_keys_dir};
use crate::commands::verify::enumerate_invalid_ledger_entries;
use crate::ledger::crypto::sign_ledger_entry_in;
use crate::ledger::db::LedgerDb;
use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry};
use miette::Result;
use rusqlite::Connection;

#[allow(dead_code)]
fn execute_ledger_re_sign(
    tx: Option<String>,
    all_invalid: bool,
    all: bool,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    execute_ledger_re_sign_with_keys_dir(tx, all_invalid, all, dry_run, yes, None)
}

fn setup_in_memory_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE transactions (
                tx_id TEXT PRIMARY KEY,
                operation_id TEXT,
                status TEXT NOT NULL,
                category TEXT NOT NULL,
                entity TEXT NOT NULL,
                entity_normalized TEXT NOT NULL,
                planned_action TEXT,
                session_id TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'CLI',
                started_at TEXT NOT NULL,
                resolved_at TEXT,
                detected_at TEXT,
                drift_count INTEGER DEFAULT 1,
                first_seen_at TEXT,
                last_seen_at TEXT,
                issue_ref TEXT,
                snapshot_id INTEGER
            );
            CREATE TABLE ledger_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                tx_id TEXT NOT NULL,
                category TEXT NOT NULL,
                entry_type TEXT NOT NULL DEFAULT 'IMPLEMENTATION',
                entity TEXT NOT NULL,
                entity_normalized TEXT NOT NULL,
                change_type TEXT NOT NULL,
                summary TEXT NOT NULL,
                reason TEXT NOT NULL,
                is_breaking INTEGER DEFAULT 0,
                committed_at TEXT NOT NULL,
                verification_status TEXT,
                verification_basis TEXT,
                outcome_notes TEXT,
                origin TEXT NOT NULL DEFAULT 'LOCAL',
                trace_id TEXT,
                signature TEXT,
                public_key TEXT,
                risk TEXT,
                related_tickets TEXT,
                author TEXT NOT NULL DEFAULT 'unknown',
                observed INTEGER,
                prev_hash TEXT,
                sig_version INTEGER NOT NULL DEFAULT 1
            );",
    )
    .unwrap();
    conn
}

fn sample_ledger_entry(
    tx_id: &str,
    signature: Option<String>,
    public_key: Option<String>,
) -> LedgerEntry {
    LedgerEntry {
        id: 0,
        tx_id: tx_id.to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "src/main.rs".to_string(),
        entity_normalized: "src/main.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "test entry".to_string(),
        reason: "test reason".to_string(),
        is_breaking: false,
        committed_at: "2024-06-01T10:00:00Z".to_string(),
        verification_status: None,
        verification_basis: None,
        outcome_notes: None,
        origin: "LOCAL".to_string(),
        trace_id: None,
        signature,
        public_key,
        risk: None,
        related_tickets: None,
        author: "test".to_string(),
        observed: None,
        prev_hash: None,
        sig_version: 1,
    }
}

#[test]
fn enumerate_invalid_entries_excludes_valid_signatures() {
    let tmp = tempfile::tempdir().unwrap();
    let keys_dir = tmp.path().join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();

    let tx_id = "tx-valid";
    let (sig, pub_key) = sign_ledger_entry_in(
        &keys_dir,
        tx_id,
        &Category::Feature.to_string(),
        "test entry",
        "test reason",
        "2024-06-01T10:00:00Z",
    )
    .unwrap();

    let entry = sample_ledger_entry(tx_id, sig, pub_key);
    let invalid = enumerate_invalid_ledger_entries(&[entry], false);
    assert!(
        invalid.is_empty(),
        "valid signature must not be listed as invalid"
    );
}

#[test]
fn enumerate_invalid_entries_includes_corrupted_signature() {
    let entry = sample_ledger_entry(
        "tx-corrupt",
        Some("deadbeef".to_string()),
        Some("0000000000000000000000000000000000000000000000000000000000000000".to_string()),
    );
    let invalid = enumerate_invalid_ledger_entries(&[entry], false);
    assert_eq!(invalid.len(), 1);
    assert_eq!(invalid[0].0, "tx-corrupt");
}

#[test]
fn update_signature_changes_stored_values() {
    let conn = setup_in_memory_db();
    let db = LedgerDb::new(&conn);
    let entry = sample_ledger_entry("tx-update", None, None);
    db.insert_ledger_entry(&entry).unwrap();

    let updated = db
        .update_ledger_entry_signature("tx-update", "new-sig", "new-pub")
        .unwrap();
    assert_eq!(updated, 1);

    let entries = db.get_ledger_entries_for_tx("tx-update").unwrap();
    assert_eq!(entries[0].signature.as_deref(), Some("new-sig"));
    assert_eq!(entries[0].public_key.as_deref(), Some("new-pub"));
}

#[test]
fn maintenance_entry_summarizes_batch() {
    let candidates = vec![
        ("tx-1".to_string(), "sig1".to_string(), "pub1".to_string()),
        ("tx-2".to_string(), "sig2".to_string(), "pub2".to_string()),
    ];
    let entry = build_maintenance_entry(
        &candidates,
        &["tx-1".to_string(), "tx-2".to_string()],
        &["pub1fp".to_string(), "pub2fp".to_string()],
        &["newsig1".to_string(), "newsig2".to_string()],
        "newpub",
        "2024-06-01T10:00:00Z",
        "tester",
        Some("oldheadhash"),
        false,
    );
    assert_eq!(entry.entry_type, EntryType::Maintenance);
    assert_eq!(entry.category, Category::Chore);
    assert!(entry.reason.contains("tx-1, tx-2"));
    assert!(entry.reason.contains("pub1fp"));
    assert!(entry.reason.contains("newpub"));
    assert!(entry.reason.contains("Key-repair"));
    assert!(
        entry
            .outcome_notes
            .as_deref()
            .unwrap_or("")
            .contains("mode=key-repair")
    );
}

#[test]
fn maintenance_entry_upgrade_mode_wording() {
    let candidates = vec![("tx-1".to_string(), "sig1".to_string(), "pub1".to_string())];
    let entry = build_maintenance_entry(
        &candidates,
        &["tx-1".to_string()],
        &["pub1fp".to_string()],
        &["newsig1".to_string()],
        "newpub",
        "2024-06-01T10:00:00Z",
        "tester",
        None,
        true,
    );
    assert!(entry.summary.contains("sig-upgrade"));
    assert!(entry.reason.contains("Signature upgrade"));
    assert!(
        entry
            .outcome_notes
            .as_deref()
            .unwrap_or("")
            .contains("mode=upgrade")
    );
}

#[test]
fn resolve_keys_dir_override_wins_on_dry_run_and_does_not_create() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().join("missing-keys-dir");
    assert!(!override_path.exists());
    let resolved = resolve_re_sign_keys_dir(Some(override_path.clone()), true).unwrap();
    assert_eq!(resolved, override_path);
    assert!(
        !override_path.exists(),
        "dry-run resolve must not create the keys directory"
    );
}

#[test]
fn upgrade_candidates_include_valid_v1() {
    let tmp = tempfile::tempdir().unwrap();
    let keys_dir = tmp.path().join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();

    let tx_id = "tx-valid-v1";
    let (sig, pub_key) = sign_ledger_entry_in(
        &keys_dir,
        tx_id,
        &Category::Feature.to_string(),
        "test entry",
        "test reason",
        "2024-06-01T10:00:00Z",
    )
    .unwrap();

    let mut entry = sample_ledger_entry(tx_id, sig, pub_key);
    entry.sig_version = 1;
    // Valid v1 is NOT invalid under enumerate_invalid
    let invalid = enumerate_invalid_ledger_entries(std::slice::from_ref(&entry), false);
    assert!(invalid.is_empty(), "valid v1 must not be invalid");

    let upgrade = enumerate_upgrade_candidates(std::slice::from_ref(&entry), false);
    assert_eq!(upgrade.len(), 1);
    assert_eq!(upgrade[0].0, tx_id);
}

#[test]
fn upgrade_candidates_exclude_current_valid_v2() {
    let tmp = tempfile::tempdir().unwrap();
    let keys_dir = tmp.path().join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();

    let tx_id = "tx-valid-v2";
    let input = crate::ledger::crypto::LedgerSignInput::for_new_commit(
        tx_id,
        Category::Feature,
        "test entry",
        "test reason",
        "2024-06-01T10:00:00Z",
        "src/main.rs",
        "src/main.rs",
        ChangeType::Modify,
        EntryType::Implementation,
        "test",
        None,
        false,
        None,
        "LOCAL",
    );
    let (sig, pub_key) = crate::ledger::crypto::sign_ledger_entry_in_v2(&keys_dir, &input).unwrap();
    let mut entry = sample_ledger_entry(tx_id, sig, pub_key);
    entry.sig_version = crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION;
    let upgrade = enumerate_upgrade_candidates(std::slice::from_ref(&entry), false);
    assert!(
        upgrade.is_empty(),
        "valid current-version entry must not be an upgrade candidate"
    );
}

#[test]
fn maintenance_entry_inlines_single_tx() {
    let candidates = vec![("tx-1".to_string(), "sig1".to_string(), "pub1".to_string())];
    let entry = build_maintenance_entry(
        &candidates,
        &["tx-1".to_string()],
        &["pub1fp".to_string()],
        &["newsig1".to_string()],
        "newpub",
        "2024-06-01T10:00:00Z",
        "tester",
        None,
        false,
    );
    assert!(entry.reason.contains("tx_id=tx-1"));
    assert!(entry.reason.contains("old_sig="));
    assert!(entry.reason.contains("new_sig="));
}

#[test]
fn backup_is_openable_and_passes_integrity_check() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("ledger.db");
    let src = Connection::open(&db_path).unwrap();
    src.execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE demo (id INTEGER PRIMARY KEY);")
        .unwrap();

    let backup = backup_ledger_db(&src, &db_path).unwrap();
    assert!(backup.exists());
    assert!(verify_backup_integrity(&backup).unwrap());
}

#[test]
fn key_fingerprint_is_first_sixteen_hex_chars() {
    assert_eq!(key_fingerprint("abcdef1234567890aaaa"), "abcdef1234567890");
}
