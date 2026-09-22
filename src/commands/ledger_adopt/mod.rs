//! Explicit legacy-row adoption. Extra-genesis rows stay outside the signed segment.

mod backup;
mod manifest;

use crate::ledger::crypto::{
    LedgerSignInput, compute_entry_hash_for_entry, keys_dir_path, sign_chain_head,
    sign_ledger_entry_in_v2, verify_ledger_entry_signature,
};
use crate::ledger::db::LedgerDb;
use crate::ledger::types::{Category, ChainHead, ChangeType, EntryType, LedgerEntry, Transaction};
use crate::state::layout::{Layout, get_layout};
use crate::state::storage::StorageManager;
use backup::backup_for_adoption;
use manifest::{
    HISTORICAL_CONTINUITY, RecoveryManifest, attestation_reason, build_manifest, diagnose,
    eligible_rows, from_json, to_json,
};
use miette::{Result, miette};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use manifest::HISTORICAL_CONTINUITY as ADOPTION_CONTINUITY;

#[derive(Debug, Clone)]
pub enum RecoveryCommand {
    Plan {
        output: PathBuf,
        json: bool,
    },
    Apply {
        manifest: PathBuf,
        yes: bool,
        json: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionDecision {
    pub accepted: bool,
    pub adopted_count: usize,
    pub unresolved_count: usize,
    pub manifest_digest: String,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanJson {
    schema_version: u32,
    kind: &'static str,
    manifest_digest: String,
    adopted_count: usize,
    historical_continuity: &'static str,
    output: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResultJson {
    schema_version: u32,
    kind: &'static str,
    status: &'static str,
    tx_id: String,
    manifest_digest: String,
    adopted_count: usize,
    unresolved_count: usize,
    historical_continuity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_digest: Option<String>,
}

pub fn execute_ledger_recovery(command: RecoveryCommand) -> Result<()> {
    match command {
        RecoveryCommand::Plan { output, json } => execute_plan(&output, json),
        RecoveryCommand::Apply {
            manifest,
            yes,
            json,
        } => execute_apply(&manifest, yes, json),
    }
}

pub fn decide_adoption(
    conn: &rusqlite::Connection,
    entries: &[LedgerEntry],
    head: Option<&ChainHead>,
) -> AdoptionDecision {
    let stored = match load_manifest_row(conn) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return AdoptionDecision {
                accepted: false,
                adopted_count: 0,
                unresolved_count: 1,
                manifest_digest: String::new(),
                message: "no adoption manifest is stored".to_string(),
            };
        }
        Err(err) => {
            return AdoptionDecision {
                accepted: false,
                adopted_count: 0,
                unresolved_count: 1,
                manifest_digest: String::new(),
                message: err.to_string(),
            };
        }
    };
    let diagnosis = diagnose(entries, head);
    let unresolved = diagnosis
        .candidate_tx_ids
        .iter()
        .filter(|id| stored.adopted.iter().all(|row| row.tx_id != **id))
        .count();
    let live = match eligible_rows(&diagnosis) {
        Ok(rows) => rows,
        Err(err) => {
            return AdoptionDecision {
                accepted: false,
                adopted_count: stored.adopted.len(),
                unresolved_count: unresolved.max(diagnosis.extra_genesis_count).max(1),
                manifest_digest: stored.manifest_digest,
                message: err.to_string(),
            };
        }
    };
    let live_manifest = match build_manifest(&stored.repo_identity, &stored.stored_head, live) {
        Ok(manifest) => manifest,
        Err(err) => {
            return AdoptionDecision {
                accepted: false,
                adopted_count: stored.adopted.len(),
                unresolved_count: unresolved.max(1),
                manifest_digest: stored.manifest_digest,
                message: err.to_string(),
            };
        }
    };
    if live_manifest.manifest_digest != stored.manifest_digest {
        return AdoptionDecision {
            accepted: false,
            adopted_count: stored.adopted.len(),
            unresolved_count: unresolved.max(1),
            manifest_digest: stored.manifest_digest,
            message: "stored adoption manifest does not match the current rows".to_string(),
        };
    }
    let tx_id = adoption_tx_id(&stored.manifest_digest);
    let maintenance = entries.iter().find(|entry| entry.tx_id == tx_id);
    let Some(maintenance) = maintenance else {
        return AdoptionDecision {
            accepted: false,
            adopted_count: stored.adopted.len(),
            unresolved_count: unresolved.max(1),
            manifest_digest: stored.manifest_digest,
            message: format!("adoption maintenance entry {tx_id} is missing"),
        };
    };
    if !verify_ledger_entry_signature(maintenance) {
        return AdoptionDecision {
            accepted: false,
            adopted_count: stored.adopted.len(),
            unresolved_count: unresolved.max(1),
            manifest_digest: stored.manifest_digest,
            message: "adoption maintenance signature is not valid".to_string(),
        };
    }
    if maintenance.prev_hash.as_deref() != Some(stored.stored_head.as_str()) {
        return AdoptionDecision {
            accepted: false,
            adopted_count: stored.adopted.len(),
            unresolved_count: unresolved.max(1),
            manifest_digest: stored.manifest_digest,
            message: "adoption maintenance prev_hash is not the original head".to_string(),
        };
    }
    AdoptionDecision {
        accepted: unresolved == 0,
        adopted_count: stored.adopted.len(),
        unresolved_count: unresolved,
        manifest_digest: stored.manifest_digest,
        message: format!("historical continuity {HISTORICAL_CONTINUITY}"),
    }
}

fn execute_plan(output: &Path, json: bool) -> Result<()> {
    refuse_ledgerful_path(output)?;
    let layout = get_layout()?;
    let repo = origin_url(layout.root.as_std_path())?;
    let manifest = read_only_manifest(&layout, &repo)?;
    let body = to_json(&manifest)?;
    write_new(output, &body)?;
    let envelope = PlanJson {
        schema_version: 1,
        kind: "ledgerAdoptionPlan",
        manifest_digest: manifest.manifest_digest.clone(),
        adopted_count: manifest.adopted.len(),
        historical_continuity: HISTORICAL_CONTINUITY,
        output: output.display().to_string(),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).map_err(|e| miette!("plan json: {e}"))?
        );
    } else {
        println!(
            "Wrote recovery manifest to {}. historical continuity {HISTORICAL_CONTINUITY}.",
            output.display()
        );
    }
    Ok(())
}

fn execute_apply(manifest_path: &Path, yes: bool, json: bool) -> Result<()> {
    let body = std::fs::read_to_string(manifest_path)
        .map_err(|e| miette!("read manifest {}: {e}", manifest_path.display()))?;
    let file_manifest = from_json(&body)?;
    let layout = get_layout()?;
    let repo = origin_url(layout.root.as_std_path())?;
    if repo != file_manifest.repo_identity {
        return Err(miette!(
            "recovery manifest repoIdentity does not match origin {repo}"
        ));
    }
    let tx_id = adoption_tx_id(&file_manifest.manifest_digest);
    let preview = StorageManager::open_read_only_sqlite_only(&layout)?;
    if manifest_exists(preview.get_connection(), &file_manifest.manifest_digest)? {
        return finish_already_applied(&tx_id, &file_manifest, json);
    }
    let live = read_only_manifest(&layout, &repo)?;
    if live.manifest_digest != file_manifest.manifest_digest {
        return Err(miette!(
            "recovery manifest does not match the current ledger rows"
        ));
    }
    if !yes {
        println!(
            "Recovery manifest {} matches {} rows. Pass --yes to back up and append the attestation. historical continuity {HISTORICAL_CONTINUITY}.",
            live.manifest_digest,
            live.adopted.len()
        );
        return Ok(());
    }

    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init_with_layout(&layout)?;
    let checked = backup_for_adoption(storage.get_connection(), db_path.as_std_path())
        .map_err(|err| miette!("Adoption did not back up or change the ledger.\n{err}"))?;
    let keys_dir = keys_dir_path().map_err(|err| {
        miette!(
            "Adoption backed up the database and did not change the ledger.\nkeys directory: {err}\nbackup={}",
            checked.path.display()
        )
    })?;
    let outcome = apply_attestation(&mut storage, &keys_dir, &live, &tx_id).map_err(|err| {
        miette!(
            "Adoption backed up the database and did not change the ledger.\n{err}\nbackup={}",
            checked.path.display()
        )
    })?;
    let envelope = ResultJson {
        schema_version: 1,
        kind: "ledgerAdoptionResult",
        status: "applied",
        tx_id: outcome,
        manifest_digest: live.manifest_digest.clone(),
        adopted_count: live.adopted.len(),
        unresolved_count: 0,
        historical_continuity: HISTORICAL_CONTINUITY,
        backup_path: Some(checked.path.display().to_string()),
        backup_digest: Some(checked.digest),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).map_err(|e| miette!("result json: {e}"))?
        );
    } else {
        println!(
            "Adopted {} legacy rows as {}. historical continuity {HISTORICAL_CONTINUITY}.",
            live.adopted.len(),
            envelope.tx_id
        );
    }
    Ok(())
}

fn finish_already_applied(tx_id: &str, manifest: &RecoveryManifest, json: bool) -> Result<()> {
    let envelope = ResultJson {
        schema_version: 1,
        kind: "ledgerAdoptionResult",
        status: "alreadyApplied",
        tx_id: tx_id.to_string(),
        manifest_digest: manifest.manifest_digest.clone(),
        adopted_count: manifest.adopted.len(),
        unresolved_count: 0,
        historical_continuity: HISTORICAL_CONTINUITY,
        backup_path: None,
        backup_digest: None,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).map_err(|e| miette!("result json: {e}"))?
        );
    } else {
        println!("Already applied {tx_id}. historical continuity {HISTORICAL_CONTINUITY}.");
    }
    Ok(())
}

fn read_only_manifest(layout: &Layout, repo: &str) -> Result<RecoveryManifest> {
    let storage = StorageManager::open_read_only_sqlite_only(layout)?;
    let db = LedgerDb::new(storage.get_connection());
    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette!("Failed to read ledger entries: {e}"))?;
    let head = db
        .get_chain_head()
        .map_err(|e| miette!("Failed to read chain head: {e}"))?;
    let diagnosis = diagnose(&entries, head.as_ref());
    let adopted = eligible_rows(&diagnosis)?;
    let stored_head = head
        .as_ref()
        .map(|item| item.latest_entry_hash.clone())
        .ok_or_else(|| miette!("adoption refused: stored chain head is missing"))?;
    build_manifest(repo, &stored_head, adopted)
}

fn apply_attestation(
    storage: &mut StorageManager,
    keys_dir: &Path,
    manifest: &RecoveryManifest,
    tx_id: &str,
) -> Result<String> {
    let sqlite_tx = storage
        .get_connection_mut()
        .unchecked_transaction()
        .map_err(|e| miette!("begin adoption transaction: {e}"))?;
    let result = (|| {
        let db = LedgerDb::new(&sqlite_tx);
        let head = db
            .get_chain_head()
            .map_err(|e| miette!("Failed to read chain head: {e}"))?
            .ok_or_else(|| miette!("adoption refused: stored chain head is missing"))?;
        if head.latest_entry_hash != manifest.stored_head {
            return Err(miette!(
                "Re-sign plan is stale: chain head changed before the attestation was written"
            ));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let reason = attestation_reason(
            &manifest.manifest_digest,
            manifest.adopted.len(),
            &manifest.stored_head,
        );
        let transaction = Transaction {
            tx_id: tx_id.to_string(),
            operation_id: None,
            status: "COMMITTED".to_string(),
            category: Category::Security,
            entity: "ledger".to_string(),
            entity_normalized: "ledger".to_string(),
            planned_action: Some("Adopt legacy ledger rows".to_string()),
            session_id: crate::ledger::session::get_session_id().to_string(),
            source: "CLI".to_string(),
            started_at: now.clone(),
            resolved_at: Some(now.clone()),
            detected_at: None,
            drift_count: 0,
            first_seen_at: None,
            last_seen_at: None,
            issue_ref: None,
            snapshot_id: None,
        };
        db.insert_transaction(&transaction)
            .map_err(|e| miette!("Failed to insert adoption transaction: {e}"))?;
        let mut entry = LedgerEntry {
            id: 0,
            tx_id: tx_id.to_string(),
            category: Category::Security,
            entry_type: EntryType::Maintenance,
            entity: "ledger".to_string(),
            entity_normalized: "ledger".to_string(),
            change_type: ChangeType::Modify,
            summary: "Adopt legacy ledger rows".to_string(),
            reason,
            is_breaking: false,
            committed_at: now.clone(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            related_tickets: None,
            author: "ledgerful".to_string(),
            observed: None,
            prev_hash: Some(manifest.stored_head.clone()),
            sig_version: crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION,
        };
        let (sig, pub_key) =
            sign_ledger_entry_in_v2(keys_dir, &LedgerSignInput::from_entry(&entry))
                .map_err(|e| miette!("Failed to sign adoption attestation: {e}"))?;
        entry.signature = sig;
        entry.public_key = pub_key;
        let new_hash = compute_entry_hash_for_entry(&entry)
            .map_err(|e| miette!("Failed to hash adoption attestation: {e}"))?;
        db.insert_ledger_entry(&entry)
            .map_err(|e| miette!("Failed to insert adoption attestation: {e}"))?;
        sqlite_tx
            .execute(
                "INSERT INTO ledger_recovery_manifest (
                    manifest_digest, manifest_json, repo_identity, stored_head, adopted_count, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    manifest.manifest_digest,
                    to_json(manifest)?,
                    manifest.repo_identity,
                    manifest.stored_head,
                    manifest.adopted.len() as i64,
                    now,
                ],
            )
            .map_err(|e| miette!("Failed to store adoption manifest: {e}"))?;
        let new_length = head.length + 1;
        let (head_sig, head_pub) = sign_chain_head(keys_dir, &new_hash, &head.genesis, new_length)
            .map_err(|e| miette!("Failed to sign chain head: {e}"))?;
        let new_head = ChainHead {
            latest_entry_hash: new_hash,
            genesis: head.genesis.clone(),
            length: new_length,
            head_signature: head_sig,
            head_public_key: head_pub,
            updated_at: now,
        };
        let updated = db
            .update_chain_head(&new_head, Some(&head))
            .map_err(|e| miette!("Failed to update chain head: {e}"))?;
        if !updated {
            return Err(miette!(
                "Re-sign plan is stale: chain head changed before the attestation was written"
            ));
        }
        Ok(tx_id.to_string())
    })();
    match result {
        Ok(tx_id) => {
            sqlite_tx
                .commit()
                .map_err(|e| miette!("Failed to commit adoption: {e}"))?;
            Ok(tx_id)
        }
        Err(err) => Err(err),
    }
}

fn load_manifest_row(conn: &rusqlite::Connection) -> Result<Option<RecoveryManifest>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT manifest_json FROM ledger_recovery_manifest ORDER BY created_at, manifest_digest LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| {
            if matches!(err, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(err)
            }
        })
        .map_err(|e| miette!("read adoption manifest: {e}"))?;
    match body {
        Some(json) => Ok(Some(from_json(&json)?)),
        None => Ok(None),
    }
}

fn manifest_exists(conn: &rusqlite::Connection, digest: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_recovery_manifest WHERE manifest_digest = ?1",
            [digest],
            |row| row.get(0),
        )
        .map_err(|e| miette!("read adoption manifest: {e}"))?;
    Ok(count > 0)
}

fn adoption_tx_id(digest: &str) -> String {
    format!("adopt-{digest}")
}

fn origin_url(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| miette!("git remote get-url origin: {e}"))?;
    if !output.status.success() {
        return Err(miette!("adoption refused: repository has no origin remote"));
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        return Err(miette!("adoption refused: repository has no origin remote"));
    }
    Ok(url)
}

fn refuse_ledgerful_path(path: &Path) -> Result<()> {
    if path.components().any(|c| c.as_os_str() == ".ledgerful") {
        return Err(miette!(
            "refusing to write a recovery manifest under .ledgerful"
        ));
    }
    Ok(())
}

fn write_new(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| miette!("create {}: {e}", path.display()))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                miette!("output file already exists: {}", path.display())
            } else {
                miette!("write {}: {e}", path.display())
            }
        })?;
    file.write_all(body.as_bytes())
        .map_err(|e| miette!("write {}: {e}", path.display()))?;
    Ok(())
}
