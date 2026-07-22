use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::commands::verify::enumerate_invalid_ledger_entries;
use crate::ledger::TransactionManager;
use crate::ledger::db::LedgerDb;
use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry};
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::{Result, miette};
use owo_colors::OwoColorize;
use rusqlite::OptionalExtension;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn execute_ledger_re_sign(
    tx: Option<String>,
    all_invalid: bool,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    execute_ledger_re_sign_with_keys_dir(tx, all_invalid, dry_run, yes, None)
}

/// Internal entry point with an optional keys directory override.
///
/// `keys_dir_override` is used by tests so they can run in a temporary key store
/// without touching the operator's real `~/.ledgerful/keys`. When `None`, the
/// production default from [`crate::ledger::crypto::get_keys_dir`] is used.
pub fn execute_ledger_re_sign_with_keys_dir(
    tx: Option<String>,
    all_invalid: bool,
    dry_run: bool,
    yes: bool,
    keys_dir_override: Option<std::path::PathBuf>,
) -> Result<()> {
    if tx.is_none() && !all_invalid {
        return Err(miette!(
            "Specify either --tx <id> to re-sign one transaction, or --all-invalid to re-sign every invalid signature. Use --dry-run to preview."
        ));
    }

    let layout = get_layout()?;
    let keys_dir = keys_dir_override
        .clone()
        .map(Ok)
        .unwrap_or_else(crate::ledger::crypto::get_keys_dir)?;
    let db_path = layout
        .state_subdir()
        .join("ledger.db")
        .as_std_path()
        .to_path_buf();

    // Read-only preview: open without claiming a write lock.
    let mut preview_storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let config = load_ledger_config(&layout)?;
    let preview_db = LedgerDb::new(preview_storage.get_connection());
    let entries = preview_db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette!("Failed to read ledger entries: {}", e))?;

    let signing_required = config.intent.require_signing;
    let invalid = enumerate_invalid_ledger_entries(&entries, signing_required);

    let candidates: Vec<(String, String, String)> = if all_invalid {
        invalid.clone()
    } else if let Some(ref tx_id_or_prefix) = tx {
        // Resolve the supplied prefix to a full tx_id, then keep it only if it is invalid.
        let preview_tx_mgr = TransactionManager::new(
            &mut preview_storage,
            layout.root.clone().into(),
            config.clone(),
        );
        let resolved = preview_tx_mgr
            .resolve_tx_id(tx_id_or_prefix)
            .map_err(|e| miette!("Could not resolve transaction '{}': {}", tx_id_or_prefix, e))?;
        invalid
            .into_iter()
            .filter(|(id, _, _)| id == &resolved)
            .collect()
    } else {
        Vec::new()
    };

    if candidates.is_empty() {
        if dry_run {
            println!(
                "{} No invalid-signature ledger entries to re-sign.",
                "DRY RUN:".cyan().bold()
            );
            return Ok(());
        }
        return Err(miette!(
            "No invalid-signature ledger entries matched the request. Use 'ledgerful verify --signatures' to check."
        ));
    }

    // Determine the public key we would re-sign with, without mutating the key store.
    // In dry-run mode we must not create or alter files; only read the existing public key.
    // When no key store exists, we report that the mutation would create one on --yes.
    let new_pub_key = if dry_run {
        if keys_dir.exists() {
            read_public_key_hex(&keys_dir).unwrap_or_else(|| "(public key unreadable)".to_string())
        } else {
            "(key-store would be created on --yes)".to_string()
        }
    } else {
        let (_, verifying_key) = crate::ledger::crypto::get_or_create_keys_in(&keys_dir)?;
        hex::encode(verifying_key.to_bytes())
    };
    let new_pub_fp = key_fingerprint(&new_pub_key);

    if dry_run {
        println!(
            "{} Would re-sign {} ledger {} with key {}:",
            "DRY RUN:".cyan().bold(),
            candidates.len(),
            if candidates.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            new_pub_fp.cyan()
        );
        for (tx_id, old_sig, old_pub) in &candidates {
            let old_sig_fp = if old_sig.is_empty() {
                "(none)".to_string()
            } else {
                key_fingerprint(old_sig)
            };
            let old_pub_fp = if old_pub.is_empty() {
                "(none)".to_string()
            } else {
                key_fingerprint(old_pub)
            };
            println!(
                "  TX {}  old_sig={}  old_pub={}",
                tx_id.chars().take(8).collect::<String>().yellow(),
                old_sig_fp.dimmed(),
                old_pub_fp.dimmed()
            );
        }
        let old_head_fp = preview_storage
            .get_connection()
            .query_row(
                "SELECT latest_entry_hash FROM chain_head WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .map(|h| key_fingerprint(&h))
            .unwrap_or_else(|| "(none)".to_string());
        println!(
            "\n{} Chain segment break preview: old head {} -> new head (computed on --yes).",
            "DRY RUN:".cyan().bold(),
            old_head_fp.cyan()
        );
        println!(
            "{} Mutations skipped. Pass --yes to back up the ledger and re-sign.",
            "DRY RUN:".cyan().bold()
        );
        return Ok(());
    }

    if !yes {
        println!(
            "{} {} ledger {} will be re-signed with key {}.",
            "Ready to re-sign:".yellow().bold(),
            candidates.len(),
            if candidates.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            new_pub_fp.cyan()
        );
        println!(
            "Pass {} to take a verified backup and proceed.",
            "--yes".cyan()
        );
        return Err(miette!(
            "Re-sign requires explicit confirmation. Run with --dry-run to preview, then --yes to mutate."
        ));
    }

    // Mutation path: take the write connection, create a WAL-safe backup first, then re-sign.
    let mut storage = StorageManager::init(&db_path)?;
    let backup_path = backup_ledger_db(storage.get_connection(), &db_path)?;

    let author = current_actor(&layout);
    let now = Utc::now().to_rfc3339();

    let mut repaired_tx_ids: Vec<String> = Vec::with_capacity(candidates.len());
    let mut old_keys: Vec<String> = Vec::with_capacity(candidates.len());
    let mut new_sigs: Vec<String> = Vec::with_capacity(candidates.len());

    let sqlite_tx = storage
        .get_connection_mut()
        .transaction()
        .map_err(|e| miette!("Failed to begin re-sign transaction: {}", e))?;

    let old_head_opt: Option<crate::ledger::types::ChainHead> = {
        let db = LedgerDb::new(&sqlite_tx);
        db.get_chain_head()
            .map_err(|e| miette!("Failed to read chain head: {}", e))?
    };
    let old_head_hash = old_head_opt.as_ref().map(|h| h.latest_entry_hash.as_str());

    {
        let db = LedgerDb::new(&sqlite_tx);
        for (tx_id, _old_sig, _old_pub) in &candidates {
            // Re-read the entry row so we sign the exact committed payload fields.
            let entry_rows = db
                .get_ledger_entries_for_tx(tx_id)
                .map_err(|e| miette!("Failed to read entry for {}: {}", tx_id, e))?;
            let entry = entry_rows
                .into_iter()
                .next()
                .ok_or_else(|| miette!("Ledger entry for {} disappeared during re-sign", tx_id))?;

            let mut sign_input = crate::ledger::crypto::LedgerSignInput::from_entry(&entry);
            sign_input.sig_version = crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION;
            let (new_sig_opt, new_pub_opt) =
                crate::ledger::crypto::sign_ledger_entry_in_v2(&keys_dir, &sign_input)
                    .map_err(|e| miette!("Signing failed for {}: {}", tx_id, e))?;

            let new_sig = new_sig_opt.ok_or_else(|| {
                miette!(
                    "sign_ledger_entry_in_v2 returned no signature for {}",
                    tx_id
                )
            })?;
            let new_pub = new_pub_opt.ok_or_else(|| {
                miette!(
                    "sign_ledger_entry_in_v2 returned no public key for {}",
                    tx_id
                )
            })?;

            let updated = db
                .update_ledger_entry_signature(&entry.tx_id, &new_sig, &new_pub)
                .map_err(|e| miette!("Failed to update signature for {}: {}", tx_id, e))?;
            if updated == 0 {
                return Err(miette!(
                    "No ledger entry row updated for {} (already invalid state?)",
                    tx_id
                ));
            }

            repaired_tx_ids.push(entry.tx_id.clone());
            old_keys.push(
                entry
                    .public_key
                    .clone()
                    .as_deref()
                    .map(key_fingerprint)
                    .unwrap_or_else(|| "(none)".to_string()),
            );
            new_sigs.push(key_fingerprint(&new_sig));
        }
    }

    // Rebuild chain links for the entries whose signatures changed. Because each
    // entry hash depends on the previous head, a single re-signed entry forces
    // a new chain suffix from the genesis point forward.
    let (new_chain_length, new_genesis, new_tail_hash) = {
        let db = LedgerDb::new(&sqlite_tx);
        let mut entries = db
            .get_all_committed_ledger_entries()
            .map_err(|e| miette!("Failed to read ledger entries for chain rebuild: {}", e))?;
        entries.sort_by(|a, b| {
            a.committed_at
                .cmp(&b.committed_at)
                .then_with(|| a.tx_id.cmp(&b.tx_id))
        });

        // Re-sign creates a fresh chain segment from the earliest existing entry
        // through the new maintenance tail. The old genesis is intentionally
        // replaced because the signatures (and therefore entry hashes) changed,
        // so any entries committed before the previous genesis boundary are now
        // part of the re-established chain.
        let genesis = entries
            .first()
            .map(|e| e.committed_at.clone())
            .unwrap_or_else(|| now.clone());
        let mut chain_length: i64 = 0;
        let mut prev_hash: Option<String> = None;
        for entry in &entries {
            let prev = prev_hash.as_deref().unwrap_or("");
            if prev.is_empty() {
                db.update_ledger_entry_prev_hash(&entry.tx_id, None)
                    .map_err(|e| {
                        miette!(
                            "Failed to clear genesis prev_hash for {}: {}",
                            entry.tx_id,
                            e
                        )
                    })?;
            } else {
                db.update_ledger_entry_prev_hash(&entry.tx_id, Some(prev))
                    .map_err(|e| {
                        miette!("Failed to update prev_hash for {}: {}", entry.tx_id, e)
                    })?;
            }
            chain_length += 1;
            // Re-read after signature update so sig_version/signature are current.
            let refreshed = db
                .get_ledger_entries_for_tx(&entry.tx_id)
                .map_err(|e| miette!("Failed to re-read entry {}: {}", entry.tx_id, e))?
                .into_iter()
                .next()
                .unwrap_or_else(|| entry.clone());
            let mut for_hash = refreshed;
            for_hash.prev_hash = if prev.is_empty() {
                None
            } else {
                Some(prev.to_string())
            };
            prev_hash = Some(crate::ledger::crypto::compute_entry_hash_for_entry(
                &for_hash,
            ));
        }
        (chain_length, genesis, prev_hash)
    };

    // Exactly one MAINTENANCE entry documents the whole batch / single repair
    // and serves as the new chain head, linking the old head to the new head.
    let maintenance_entry = build_maintenance_entry(
        &candidates,
        &repaired_tx_ids,
        &old_keys,
        &new_sigs,
        &new_pub_key,
        &now,
        &author,
        old_head_hash,
    );

    let maintenance_tx_id = {
        let db = LedgerDb::new(&sqlite_tx);
        insert_maintenance_transaction(
            &db,
            &maintenance_entry.tx_id,
            &maintenance_entry.committed_at,
            &maintenance_entry.author,
        )?;

        // Sign the maintenance entry so it has a stable hash for the chain head
        // and so it does not itself become an invalid-signature row when signing
        // is required.
        let mut signed_maintenance_entry = maintenance_entry.clone();
        if signing_required {
            let maint_input =
                crate::ledger::crypto::LedgerSignInput::from_entry(&signed_maintenance_entry);
            let (maint_sig, maint_pub) =
                crate::ledger::crypto::sign_ledger_entry_in_v2(&keys_dir, &maint_input)
                    .map_err(|e| miette!("Failed to sign maintenance entry: {}", e))?;
            signed_maintenance_entry.signature = maint_sig;
            signed_maintenance_entry.public_key = maint_pub;
            signed_maintenance_entry.sig_version =
                crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION;
        }

        let maint_prev = new_tail_hash.as_deref().unwrap_or("");
        signed_maintenance_entry.prev_hash = if maint_prev.is_empty() {
            None
        } else {
            Some(maint_prev.to_string())
        };

        db.insert_ledger_entry(&signed_maintenance_entry)
            .map_err(|e| miette!("Failed to insert maintenance ledger entry: {}", e))?;

        let new_latest_hash =
            crate::ledger::crypto::compute_entry_hash_for_entry(&signed_maintenance_entry);

        let (head_sig, head_pub) = match crate::ledger::crypto::sign_chain_head(
            &keys_dir,
            &new_latest_hash,
            &new_genesis,
            new_chain_length + 1,
        ) {
            Ok(res) => res,
            Err(e) => {
                if signing_required {
                    return Err(miette!("Failed to sign new chain head: {}", e));
                }
                tracing::warn!(
                    "Chain head signing failed (signing not required, storing unsigned head): {}",
                    e
                );
                (None, None)
            }
        };

        let new_head = crate::ledger::types::ChainHead {
            latest_entry_hash: new_latest_hash,
            genesis: new_genesis,
            length: new_chain_length + 1,
            head_signature: head_sig,
            head_public_key: head_pub,
            updated_at: now.clone(),
        };
        let updated = db
            .update_chain_head(&new_head, old_head_opt.as_ref())
            .map_err(|e| miette!("Failed to update chain head: {}", e))?;
        if !updated {
            return Err(miette!(
                "Chain head moved during re-sign (CAS mismatch). Aborting to prevent stale head."
            ));
        }

        signed_maintenance_entry.tx_id.clone()
    };

    sqlite_tx
        .commit()
        .map_err(|e| miette!("Failed to commit re-sign transaction: {}", e))?;

    println!(
        "{} Re-signed {} ledger {}. Backup: {}",
        "SUCCESS:".green().bold(),
        repaired_tx_ids.len(),
        if repaired_tx_ids.len() == 1 {
            "entry"
        } else {
            "entries"
        },
        backup_path.display()
    );
    println!(
        "{} Maintenance entry recorded for tx_id {}.",
        "AUDIT:".blue().bold(),
        maintenance_tx_id.cyan()
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_maintenance_entry(
    candidates: &[(String, String, String)],
    repaired_tx_ids: &[String],
    old_keys: &[String],
    new_sigs: &[String],
    new_pub_key: &str,
    committed_at: &str,
    author: &str,
    old_head_hash: Option<&str>,
) -> LedgerEntry {
    let is_batch = candidates.len() > 1;
    let affected = if is_batch {
        // Sort deterministically; list all repaired tx_ids for batch provenance.
        let mut ids = repaired_tx_ids.to_vec();
        ids.sort();
        ids.join(", ")
    } else {
        let (_, old_sig, old_pub) = &candidates[0];
        let old_sig_fp = if old_sig.is_empty() {
            "(none)".to_string()
        } else {
            key_fingerprint(old_sig)
        };
        let old_pub_fp = if old_pub.is_empty() {
            "(none)".to_string()
        } else {
            key_fingerprint(old_pub)
        };
        format!(
            "tx_id={}; old_sig={}; old_pub={}; new_sig={}; new_pub={}",
            repaired_tx_ids[0],
            old_sig_fp,
            old_pub_fp,
            new_sigs[0],
            key_fingerprint(new_pub_key)
        )
    };

    let old_head_fp = old_head_hash
        .map(key_fingerprint)
        .unwrap_or_else(|| "(none)".to_string());

    let summary = if is_batch {
        format!(
            "Chain segment break: re-sign — re-signed {} ledger entries",
            candidates.len()
        )
    } else {
        "Chain segment break: re-sign — re-signed one ledger entry".to_string()
    };

    let reason = format!(
        "Key-repair / re-sign operation. Old key fingerprints: [{}]. New public key fingerprint: {}. Old chain head: {}. Affected: {}.",
        old_keys.join(", "),
        key_fingerprint(new_pub_key),
        old_head_fp,
        affected
    );

    // The ledger_entries table has a foreign-key constraint on transactions(tx_id).
    // The maintenance entry documents a batch re-sign operation, so we create a synthetic
    // COMMITTED transaction row to satisfy the schema and preserve the audit trail.
    let maintenance_tx_id = format!("resign-{}-maintenance", nanos_since_epoch());

    LedgerEntry {
        id: 0,
        tx_id: maintenance_tx_id,
        category: Category::Chore,
        entry_type: EntryType::Maintenance,
        entity: "ledger/signatures".to_string(),
        entity_normalized: "ledger/signatures".to_string(),
        change_type: ChangeType::Modify,
        summary,
        reason,
        is_breaking: false,
        committed_at: committed_at.to_string(),
        verification_status: None,
        verification_basis: None,
        outcome_notes: Some(format!(
            "re_sign_count={}; new_pub_fp={}; affected_tx_ids=[{}]",
            candidates.len(),
            key_fingerprint(new_pub_key),
            if is_batch {
                repaired_tx_ids.join(", ")
            } else {
                repaired_tx_ids[0].clone()
            }
        )),
        origin: "LOCAL".to_string(),
        trace_id: None,
        signature: None,
        public_key: None,
        risk: None,
        related_tickets: None,
        author: author.to_string(),
        observed: None,
        prev_hash: None,
        sig_version: crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION,
    }
}

fn insert_maintenance_transaction(
    db: &LedgerDb,
    tx_id: &str,
    committed_at: &str,
    _author: &str,
) -> Result<(), miette::Error> {
    let tx = crate::ledger::types::Transaction {
        tx_id: tx_id.to_string(),
        operation_id: None,
        status: "COMMITTED".to_string(),
        category: Category::Chore,
        entity: "ledger/signatures".to_string(),
        entity_normalized: "ledger/signatures".to_string(),
        planned_action: Some("Ledger signature re-sign / key-repair".to_string()),
        session_id: crate::ledger::session::get_session_id().to_string(),
        source: "CLI".to_string(),
        started_at: committed_at.to_string(),
        resolved_at: Some(committed_at.to_string()),
        detected_at: None,
        drift_count: 1,
        first_seen_at: Some(committed_at.to_string()),
        last_seen_at: Some(committed_at.to_string()),
        issue_ref: None,
        snapshot_id: None,
    };
    db.insert_transaction(&tx)
        .map_err(|e| miette!("Failed to insert maintenance transaction row: {}", e))?;
    Ok(())
}

fn key_fingerprint(hex_key: &str) -> String {
    // Use the first 16 hex characters (8 bytes) as a stable, readable fingerprint.
    // This matches the existing verify output convention (pub_key[..8]).
    hex_key.chars().take(16).collect()
}

fn current_actor(repo_root: &crate::state::layout::Layout) -> String {
    let from_git = || {
        let read = |key: &str| -> Option<String> {
            std::process::Command::new("git")
                .args(["config", key])
                .current_dir(repo_root.root.as_path())
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if s.is_empty() { None } else { Some(s) }
                    } else {
                        None
                    }
                })
        };
        read("user.name").or_else(|| read("user.email"))
    };

    from_git()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Create a WAL-safe, integrity-checked backup of the ledger database.
///
/// Uses SQLite's Online Backup API (`rusqlite::backup::Backup`) over a live connection.
/// After the backup is written, we open it read-only and run `PRAGMA integrity_check`.
/// The operation aborts with an actionable error if the backup is corrupt.
fn backup_ledger_db(src_conn: &rusqlite::Connection, db_path: &Path) -> Result<std::path::PathBuf> {
    let timestamp = nanos_since_epoch();
    let base_name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ledger.db");
    let backup_path = db_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{}.{}.bak", base_name, timestamp));

    // Online Backup API copies the live database into a new file.
    {
        let mut dst = rusqlite::Connection::open(&backup_path).map_err(|e| {
            miette!(
                "Failed to open backup database at {}: {}",
                backup_path.display(),
                e
            )
        })?;
        let backup = rusqlite::backup::Backup::new(src_conn, &mut dst)
            .map_err(|e| miette!("Failed to initialize SQLite online backup: {}", e))?;
        backup
            .step(-1)
            .map_err(|e| miette!("SQLite online backup failed: {}", e))?;
    }

    // Verify the backup is openable and passes integrity_check before any mutation.
    let integrity = verify_backup_integrity(&backup_path).map_err(|e| {
        miette!(
            "Backup integrity check failed for {}: {}",
            backup_path.display(),
            e
        )
    })?;
    if !integrity {
        return Err(miette!(
            "Backup at {} failed PRAGMA integrity_check. Refusing to mutate the ledger.",
            backup_path.display()
        ));
    }

    Ok(backup_path)
}

fn verify_backup_integrity(backup_path: &Path) -> Result<bool> {
    let conn = rusqlite::Connection::open(backup_path)
        .map_err(|e| miette!("Could not open backup for integrity check: {}", e))?;
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| miette!("integrity_check query failed: {}", e))?;
    Ok(result.eq_ignore_ascii_case("ok"))
}

fn nanos_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// Read the existing public key file as a hex string, without creating keys or
/// writing any files. Returns `None` if the public key file is missing.
fn read_public_key_hex(keys_dir: &std::path::Path) -> Option<String> {
    let pub_path = keys_dir.join("public.pem");
    if !pub_path.exists() {
        return None;
    }
    std::fs::read_to_string(&pub_path)
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::crypto::sign_ledger_entry_in;
    use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry};
    use rusqlite::Connection;
    #[allow(dead_code)]
    fn execute_ledger_re_sign(
        tx: Option<String>,
        all_invalid: bool,
        dry_run: bool,
        yes: bool,
    ) -> Result<()> {
        execute_ledger_re_sign_with_keys_dir(tx, all_invalid, dry_run, yes, None)
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
        );
        assert_eq!(entry.entry_type, EntryType::Maintenance);
        assert_eq!(entry.category, Category::Chore);
        assert!(entry.reason.contains("tx-1, tx-2"));
        assert!(entry.reason.contains("pub1fp"));
        assert!(entry.reason.contains("newpub"));
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
}
