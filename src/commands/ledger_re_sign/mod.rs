//! `ledgerful ledger re-sign` command barrel (0265).
//!
//! Public path stays `crate::commands::ledger_re_sign`. Dry-run must not
//! create a key store or mutate the ledger.

mod backup;
mod mutate;
mod preview;

pub use preview::enumerate_upgrade_candidates;

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::ledger::db::LedgerDb;
use crate::state::storage::StorageManager;
use backup::backup_ledger_db;
use miette::{Result, miette};
use mutate::apply_re_sign;
use owo_colors::{OwoColorize, Stream, Style};
use preview::{
    collect_candidate_preview, handle_empty_candidates, key_fingerprint, print_dry_run_listing,
    resolve_dry_run_public_key, resolve_re_sign_keys_dir,
};

pub fn execute_ledger_re_sign(
    tx: Option<String>,
    all_invalid: bool,
    all: bool,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    execute_ledger_re_sign_with_keys_dir(tx, all_invalid, all, dry_run, yes, None)
}

/// Internal entry point with an optional keys directory override.
///
/// `keys_dir_override` is used by tests so they can run in a temporary key store
/// without touching the operator's real `~/.ledgerful/keys`. When `None`, the
/// production default from [`crate::ledger::crypto::get_keys_dir`] is used.
pub fn execute_ledger_re_sign_with_keys_dir(
    tx: Option<String>,
    all_invalid: bool,
    all: bool,
    dry_run: bool,
    yes: bool,
    keys_dir_override: Option<std::path::PathBuf>,
) -> Result<()> {
    if tx.is_none() && !all_invalid && !all {
        return Err(miette!(
            "Specify --tx <id> to re-sign one transaction, --all-invalid for key-repair of invalid signatures, or --all to upgrade legacy sig_version rows (and repair invalids). Use --dry-run to preview."
        ));
    }

    let layout = get_layout()?;
    let keys_dir = resolve_re_sign_keys_dir(keys_dir_override, dry_run)?;
    let db_path = layout
        .state_subdir()
        .join("ledger.db")
        .as_std_path()
        .to_path_buf();

    // Read-only preview: open without claiming a write lock.
    let mut preview_storage = StorageManager::open_read_only_sqlite_only(&layout)?;
    let config = load_ledger_config(&layout)?;
    let preview_db = LedgerDb::new(preview_storage.get_connection());
    let entries = preview_db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette!("Failed to read ledger entries: {}", e))?;

    let signing_required = config.intent.require_signing;
    let preview = collect_candidate_preview(
        tx.as_deref(),
        all_invalid,
        all,
        &entries,
        signing_required,
        &mut preview_storage,
        &layout,
        &config,
    )?;

    if preview.candidates.is_empty() {
        return handle_empty_candidates(dry_run, preview.is_upgrade_mode);
    }

    // Determine the public key we would re-sign with, without mutating the key store
    // on dry-run. Mutation may create the store via get_or_create_keys_in.
    let new_pub_key = if dry_run {
        resolve_dry_run_public_key(&keys_dir)
    } else {
        let (_, verifying_key) = crate::ledger::crypto::get_or_create_keys_in(&keys_dir)?;
        hex::encode(verifying_key.to_bytes())
    };
    let new_pub_fp = key_fingerprint(&new_pub_key);

    if dry_run {
        print_dry_run_listing(
            &preview.candidates,
            preview.is_upgrade_mode,
            preview.upgrade_count,
            preview.invalid_count,
            &new_pub_fp,
            &preview_storage,
        );
        return Ok(());
    }

    if !yes {
        println!(
            "{} {} ledger {} will be re-signed with key {}.",
            "Ready to re-sign:"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
            preview.candidates.len(),
            if preview.candidates.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            new_pub_fp.if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        println!(
            "Pass {} to take a verified backup and proceed.",
            "--yes".if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        return Err(miette!(
            "Re-sign requires explicit confirmation. Run with --dry-run to preview, then --yes to mutate."
        ));
    }

    // Mutation path: take the write connection, create a WAL-safe backup first, then re-sign.
    let mut storage = StorageManager::init_with_layout(&layout)?;
    let backup_path = backup_ledger_db(storage.get_connection(), &db_path)?;

    let outcome = apply_re_sign(
        &mut storage,
        &keys_dir,
        &preview.candidates,
        &new_pub_key,
        &layout,
        &config,
        signing_required,
        preview.is_upgrade_mode,
    )?;

    println!(
        "{} Re-signed {} ledger {}. Backup: {}",
        "SUCCESS:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold())),
        outcome.repaired_tx_ids.len(),
        if outcome.repaired_tx_ids.len() == 1 {
            "entry"
        } else {
            "entries"
        },
        backup_path.display()
    );
    println!(
        "{} Maintenance entry recorded for tx_id {}.",
        "AUDIT:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold())),
        outcome
            .maintenance_tx_id
            .if_supports_color(Stream::Stdout, |s| s.cyan())
    );

    Ok(())
}

#[cfg(test)]
mod tests;
