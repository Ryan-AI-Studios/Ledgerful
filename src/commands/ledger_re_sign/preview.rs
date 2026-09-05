use crate::commands::verify::enumerate_invalid_ledger_entries;
use crate::ledger::TransactionManager;
use crate::ledger::types::LedgerEntry;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{Result, miette};
use owo_colors::{OwoColorize, Stream, Style};
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};

/// One re-sign candidate: tx id plus stored signature and public-key hex.
pub(crate) type ReSignCandidate = (String, String, String);

pub(crate) struct CandidatePreview {
    pub candidates: Vec<ReSignCandidate>,
    pub is_upgrade_mode: bool,
    pub upgrade_count: usize,
    pub invalid_count: usize,
}

/// Resolve the key store directory for re-sign.
///
/// Dry-run uses [`keys_dir_path`] (no create). Mutation uses [`get_keys_dir`]
/// (may create). Override always wins. Satisfies DoD-3: dry-run mutates nothing.
pub(crate) fn resolve_re_sign_keys_dir(
    keys_dir_override: Option<PathBuf>,
    dry_run: bool,
) -> Result<PathBuf> {
    match keys_dir_override {
        Some(path) => Ok(path),
        None if dry_run => crate::ledger::crypto::keys_dir_path()
            .map_err(|e| miette!("Failed to resolve keys directory: {e}")),
        None => crate::ledger::crypto::get_keys_dir()
            .map_err(|e| miette!("Failed to resolve keys directory: {e}")),
    }
}

/// LOCAL entries that need a signature rewrite under `--all`:
/// `sig_version < CURRENT` **or** invalid under the existing classify policy.
///
/// Distinct from [`enumerate_invalid_ledger_entries`]: that helper hardcodes
/// `min_sig_version=1`, so valid v1 rows never appear. Upgrade candidates must
/// include them.
pub fn enumerate_upgrade_candidates(
    entries: &[LedgerEntry],
    signing_required: bool,
) -> Vec<(String, String, String)> {
    let current = crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION;
    let invalid = enumerate_invalid_ledger_entries(entries, signing_required);
    let invalid_ids: std::collections::HashSet<&str> =
        invalid.iter().map(|(id, _, _)| id.as_str()).collect();

    let mut candidates: Vec<ReSignCandidate> = Vec::new();
    for entry in entries {
        if entry.origin != "LOCAL" {
            continue;
        }
        let needs_version_upgrade = entry.sig_version < current;
        let is_invalid = invalid_ids.contains(entry.tx_id.as_str());
        if needs_version_upgrade || is_invalid {
            candidates.push((
                entry.tx_id.clone(),
                entry.signature.clone().unwrap_or_default(),
                entry.public_key.clone().unwrap_or_default(),
            ));
        }
    }
    // Deterministic order for preview and mutation.
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_candidate_preview(
    tx: Option<&str>,
    all_invalid: bool,
    all: bool,
    entries: &[LedgerEntry],
    signing_required: bool,
    preview_storage: &mut StorageManager,
    layout: &Layout,
    config: &crate::config::model::Config,
) -> Result<CandidatePreview> {
    let invalid = enumerate_invalid_ledger_entries(entries, signing_required);
    let is_upgrade_mode = all;

    // Upgrade mode (--all) uses a distinct candidate filter: valid v1 rows never
    // appear in enumerate_invalid_* (min_sig hardcodes 1).
    let candidates: Vec<ReSignCandidate> = if all {
        enumerate_upgrade_candidates(entries, signing_required)
    } else if all_invalid {
        invalid.clone()
    } else if let Some(tx_id_or_prefix) = tx {
        // Resolve the supplied prefix to a full tx_id, then keep it only if it is invalid.
        let preview_tx_mgr =
            TransactionManager::new(preview_storage, layout.root.clone().into(), config.clone());
        let resolved = preview_tx_mgr
            .resolve_tx_id(tx_id_or_prefix)
            .map_err(|e| miette!("Could not resolve transaction '{}': {}", tx_id_or_prefix, e))?;
        invalid
            .iter()
            .filter(|(id, _, _)| id == &resolved)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    // Preview counts for --all: how many are version upgrades vs already-invalid.
    let (upgrade_count, invalid_count) = if is_upgrade_mode {
        let invalid_ids: std::collections::HashSet<&str> =
            invalid.iter().map(|(id, _, _)| id.as_str()).collect();
        let mut upg = 0usize;
        let mut inv = 0usize;
        for (id, _, _) in &candidates {
            if invalid_ids.contains(id.as_str()) {
                inv += 1;
            } else {
                upg += 1;
            }
        }
        (upg, inv)
    } else {
        (0, candidates.len())
    };

    Ok(CandidatePreview {
        candidates,
        is_upgrade_mode,
        upgrade_count,
        invalid_count,
    })
}

pub(crate) fn handle_empty_candidates(dry_run: bool, is_upgrade_mode: bool) -> Result<()> {
    if dry_run {
        if is_upgrade_mode {
            println!(
                "{} No LOCAL ledger entries need upgrade or repair (sig_version already current and signatures valid).",
                "DRY RUN:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
            );
        } else {
            println!(
                "{} No invalid-signature ledger entries to re-sign.",
                "DRY RUN:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
            );
        }
        return Ok(());
    }
    if is_upgrade_mode {
        return Err(miette!(
            "No LOCAL ledger entries need upgrade or repair. Use 'ledgerful verify --signatures' to check."
        ));
    }
    Err(miette!(
        "No invalid-signature ledger entries matched the request. Use 'ledgerful verify --signatures' to check."
    ))
}

/// Public key hex for dry-run listing. Must not create or alter the key store.
pub(crate) fn resolve_dry_run_public_key(keys_dir: &Path) -> String {
    if keys_dir.exists() {
        read_public_key_hex(keys_dir).unwrap_or_else(|| "(public key unreadable)".to_string())
    } else {
        "(key-store would be created on --yes)".to_string()
    }
}

pub(crate) fn print_dry_run_listing(
    candidates: &[ReSignCandidate],
    is_upgrade_mode: bool,
    upgrade_count: usize,
    invalid_count: usize,
    new_pub_fp: &str,
    preview_storage: &StorageManager,
) {
    println!(
        "{} Would re-sign {} ledger {} with key {}:",
        "DRY RUN:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold())),
        candidates.len(),
        if candidates.len() == 1 {
            "entry"
        } else {
            "entries"
        },
        new_pub_fp.if_supports_color(Stream::Stdout, |s| s.cyan())
    );
    if is_upgrade_mode {
        println!(
            "{} Counts: {} version-upgrade candidate(s), {} invalid/unsigned candidate(s).",
            "DRY RUN:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold())),
            upgrade_count,
            invalid_count
        );
    }
    for (tx_id, old_sig, old_pub) in candidates {
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
            tx_id
                .chars()
                .take(8)
                .collect::<String>()
                .if_supports_color(Stream::Stdout, |s| s.yellow()),
            old_sig_fp.if_supports_color(Stream::Stdout, |s| s.dimmed()),
            old_pub_fp.if_supports_color(Stream::Stdout, |s| s.dimmed())
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
        "DRY RUN:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold())),
        old_head_fp.if_supports_color(Stream::Stdout, |s| s.cyan())
    );
    println!(
        "{} Mutations skipped. Pass --yes to back up the ledger and re-sign.",
        "DRY RUN:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
    );
}

pub(crate) fn key_fingerprint(hex_key: &str) -> String {
    // Use the first 16 hex characters (8 bytes) as a stable, readable fingerprint.
    // This matches the existing verify output convention (pub_key[..8]).
    hex_key.chars().take(16).collect()
}

/// Read the existing public key file as a hex string, without creating keys or
/// writing any files. Returns `None` if the public key file is missing.
fn read_public_key_hex(keys_dir: &Path) -> Option<String> {
    let pub_path = keys_dir.join("public.pem");
    if !pub_path.exists() {
        return None;
    }
    std::fs::read_to_string(&pub_path)
        .ok()
        .map(|s| s.trim().to_string())
}
