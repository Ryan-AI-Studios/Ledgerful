use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::ledger::*;
use crate::state::storage::StorageManager;
use clap::ValueEnum;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;

pub fn execute_ledger_reconcile(
    tx_id: Option<String>,
    pattern: Option<String>,
    all: bool,
    reason: Option<String>,
) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    tx_mgr
        .reconcile_drift(tx_id, pattern, all, reason.unwrap_or_default())
        .map_err(|e| miette::miette!("{}", e))?;

    println!("{}", "Drift reconciled.".green());
    Ok(())
}

pub fn execute_ledger_adopt(
    pattern: Option<String>,
    all: bool,
    category: &str,
    summary: &str,
    reason: &str,
) -> Result<()> {
    let category = Category::from_str(category, true).map_err(|e| miette::miette!("{}", e))?;
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    // Collect unaudited drift entities *before* adopt to derive real file set for KG edges.
    // adopt_drift returns the tx_ids it promoted; we then gather files from each.
    let adopted_tx_ids = tx_mgr
        .adopt_drift(None, pattern, all, Some(reason.to_string()))
        .map_err(|e| miette::miette!("{}", e))?;

    if adopted_tx_ids.is_empty() {
        println!("No unaudited drift found to adopt.");
        return Ok(());
    }

    // Collect all changed files from the adopted drift transactions for KG provenance.
    let mut changed_files: Vec<String> = Vec::new();
    for adopted_id in &adopted_tx_ids {
        match tx_mgr.get_transaction_files(adopted_id) {
            Ok(mut files) => changed_files.append(&mut files),
            Err(e) => tracing::warn!(
                "ledger adopt: could not discover changed files for KG edges (adopted_tx={adopted_id}): {e}"
            ),
        }
    }
    changed_files.sort_unstable();
    changed_files.dedup();

    // Use the first adopted entity (prefixed) as the ledger transaction entity for human readability.
    // For multi-entity adoption, we use the count as a synthetic label.
    // We use a prefix to avoid conflict with the now-PENDING adopted transaction's own entity name.
    let adopt_entity = if adopted_tx_ids.len() == 1 {
        let id = &adopted_tx_ids[0];
        let entity_name = tx_mgr
            .get_transaction(id)
            .ok()
            .flatten()
            .map(|tx| tx.entity)
            .unwrap_or_else(|| id.clone());
        format!("drift_adoption:{}", entity_name)
    } else {
        format!("drift_adoption:{}_items", adopted_tx_ids.len())
    };

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category,
            entity: adopt_entity,
            planned_action: Some(summary.to_string()),
            ..Default::default()
        })
        .map_err(|e| miette::miette!("{}", e))?;

    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: summary.to_string(),
                reason: reason.to_string(),
                changed_files: Some(changed_files),
                ..Default::default()
            },
            false,
        )
        .map_err(|e| miette::miette!("{}", e))?;

    drop(tx_mgr);

    println!("{}", "Drift adopted and committed.".green());
    Ok(())
}

pub fn execute_ledger_gc(
    stale: bool,
    orphans: bool,
    ttl_hours: u64,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    // No-args UX: show usage if neither mode was selected
    if !stale && !orphans {
        println!(
            "{}",
            "Usage: ledgerful ledger gc --stale [--ttl-hours <N>] | --orphans [--force]".cyan()
        );
        println!();
        println!(
            "  {}  Remove PENDING transactions older than TTL (default: 72h)",
            "--stale".bold()
        );
        println!(
            "  {}  Remove transactions with no corresponding git commit",
            "--orphans".bold()
        );
        return Ok(());
    }

    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;

    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    if stale {
        let stale_ids = {
            let db = LedgerDb::new(tx_mgr.get_connection());
            let ttl_days = ttl_hours.div_ceil(24);
            db.get_stale_pending_transactions(ttl_days)
                .map_err(|e| miette::miette!("Failed to scan for stale transactions: {}", e))?
        };

        if stale_ids.is_empty() {
            println!("No stale PENDING transactions found.");
            return Ok(());
        }

        println!(
            "Found {} stale PENDING transaction(s) (older than {} hours).",
            stale_ids.len(),
            ttl_hours
        );

        if !force && !dry_run {
            println!(
                "{} This will mark them as ROLLED_BACK in the ledger history.",
                "WARNING".yellow().bold()
            );
            if !crate::util::term::is_interactive() {
                return Err(miette::miette!(
                    "Use --force to run GC in non-interactive shells."
                ));
            }

            print!("Proceed with cleanup? (y/N): ");
            use std::io::Write;
            std::io::stdout().flush().into_diagnostic()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).into_diagnostic()?;
            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Aborted.");
                return Ok(());
            }
        }

        let mut count = 0;
        let mut failures = 0;
        for id in stale_ids {
            if dry_run {
                println!("Would rollback PENDING transaction {}", id);
                count += 1;
                continue;
            }
            if let Err(e) = tx_mgr.rollback_change(
                id.clone(),
                "Garbage collection of stale PENDING transaction".to_string(),
            ) {
                tracing::warn!("Failed to rollback tx {}: {}", id, e);
                failures += 1;
            } else {
                count += 1;
            }
        }

        if count > 0 {
            if dry_run {
                println!(
                    "{} Dry-run completed. No transactions were modified. ({} stale transaction(s) would have been cleaned up)",
                    "DONE".green().bold(),
                    count
                );
            } else {
                println!(
                    "{} Successfully cleaned up {} stale transaction(s).",
                    "DONE".green().bold(),
                    count
                );
            }
        }

        if failures > 0 {
            if count == 0 {
                return Err(miette::miette!(
                    "GC failed to clean up any of the {} stale transaction(s). Check logs.",
                    failures
                ));
            } else {
                println!(
                    "{} Failed to clean up {} transaction(s). Check logs for details.",
                    "WARN:".yellow().bold(),
                    failures
                );
            }
        }
    }

    if orphans {
        let stale_ids = {
            let db = LedgerDb::new(tx_mgr.get_connection());
            let ttl_days = ttl_hours.div_ceil(24);
            db.get_stale_pending_transactions(ttl_days)
                .map_err(|e| miette::miette!("Failed to scan for orphans: {}", e))?
        };

        if stale_ids.is_empty() {
            println!("No orphaned transactions found.");
            return Ok(());
        }

        println!(
            "Found {} orphaned PENDING transaction(s) (older than {} hours).",
            stale_ids.len(),
            ttl_hours
        );

        if !force && !dry_run {
            println!(
                "{} This will mark them as ROLLED_BACK in the ledger history.",
                "WARNING".yellow().bold()
            );
            if !crate::util::term::is_interactive() {
                return Err(miette::miette!(
                    "Use --force to run GC in non-interactive shells."
                ));
            }

            print!("Proceed with cleanup? (y/N): ");
            use std::io::Write;
            std::io::stdout().flush().into_diagnostic()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).into_diagnostic()?;
            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Aborted.");
                return Ok(());
            }
        }

        let mut count = 0;
        let mut failures = 0;
        for id in stale_ids {
            if dry_run {
                println!("Would rollback PENDING transaction {}", id);
                count += 1;
                continue;
            }
            if let Err(e) = tx_mgr.rollback_change(
                id.clone(),
                "Garbage collection of orphaned PENDING transaction".to_string(),
            ) {
                tracing::warn!("Failed to rollback tx {}: {}", id, e);
                failures += 1;
            } else {
                count += 1;
            }
        }

        if count > 0 {
            if dry_run {
                println!(
                    "{} Dry-run completed. No transactions were modified. ({} orphaned transaction(s) would have been cleaned up)",
                    "DONE".green().bold(),
                    count
                );
            } else {
                println!(
                    "{} Successfully cleaned up {} orphaned transaction(s).",
                    "DONE".green().bold(),
                    count
                );
            }
        }

        if failures > 0 {
            if count == 0 {
                return Err(miette::miette!(
                    "GC failed to clean up any of the {} orphaned transaction(s). Check logs.",
                    failures
                ));
            } else {
                println!(
                    "{} Failed to clean up {} transaction(s). Check logs for details.",
                    "WARN:".yellow().bold(),
                    failures
                );
            }
        }
    }

    Ok(())
}

pub fn execute_ledger_hook_repair(force: bool) -> Result<()> {
    let layout = get_layout()?;
    let sidecar_path = layout.state_subdir().join("pending_hook_tx");

    println!("{}", "Ledgerful Hook Repair".bold().cyan());

    if !sidecar_path.exists() {
        println!("No pending hook sidecar found. Lifecycle is consistent.");
        return Ok(());
    }

    let sidecar_content = std::fs::read_to_string(&sidecar_path).into_diagnostic()?;
    let pending: crate::commands::hook_post_commit::PendingHookTx =
        serde_json::from_str(&sidecar_content).into_diagnostic()?;

    // Fetch HEAD commit msg
    let repo_root = layout.root.clone();
    let git_out = std::process::Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(repo_root)
        .output()
        .into_diagnostic()?;

    let current_commit_msg = String::from_utf8_lossy(&git_out.stdout).to_string();
    let cleaned_msg = crate::util::text::clean_commit_msg(&current_commit_msg);

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(cleaned_msg.as_bytes());
    let current_hash = hex::encode(hasher.finalize());

    if pending.commit_msg_hash == current_hash {
        println!("Hook sidecar is matching HEAD commit hash. No repair needed.");
        return Ok(());
    }

    println!(
        "{} Hook sidecar mismatch detected!",
        "WARNING:".yellow().bold()
    );
    println!("  Pending Tx ID: {}", pending.tx_id);
    println!(
        "  Expected commit message hash: {}",
        pending.commit_msg_hash
    );
    println!("  Current HEAD commit message hash: {}", current_hash);

    if force {
        println!("Repairing hook state (force)...");
        let storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
        let db = LedgerDb::new(storage.get_connection());
        let now = chrono::Utc::now().to_rfc3339();
        let _ = db.update_transaction_status(&pending.tx_id, "ROLLBACK", Some(&now));
        let _ = std::fs::remove_file(&sidecar_path);
        println!(
            "{} Stale sidecar removed and transaction rolled back in DB.",
            "SUCCESS:".green().bold()
        );
    } else {
        println!(
            "\nRun with --force to repair the hook state by rolling back the stale transaction and removing the sidecar."
        );
    }

    Ok(())
}
