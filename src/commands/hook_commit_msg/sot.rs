use crate::config::model::Config;
use crate::ledger::TransactionManager;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::Result;

// ---------------------------------------------------------------------------
// Provenance SoT (0122): pure extract + classify — no DB inside pure helpers.
// ---------------------------------------------------------------------------

/// Ledger status of a message-extracted TX ref after DB verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerRefStatus {
    Pending,
    Committed,
    /// Present as a non-pending/non-committed status, or absent entirely.
    Missing,
}

/// Pure classification of commit-msg provenance ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceSotClass {
    /// Verified msg ref is already COMMITTED — skip all hook intent capture.
    AlreadyCommitted { tx_id: String },
    /// Link sidecar to an existing PENDING TX without `start_change`.
    LinkPending { tx_id: String },
    /// N>1 open PENDING and no verified msg ref — warn then HookFallback.
    AmbiguousMulti,
    /// Existing conventional / LLM / TUI / silent path.
    Fallback,
}

/// Pure extraction of a ledger TX ref from a raw commit message (before clean).
///
/// Rules (B2):
/// 1. Prefer line-anchored `(?i)^\s*Ledger:\s*(<uuid>)\s*$` (end of line required)
/// 2. Accept optional trailer-style `Ledger-Tx: <uuid>` on its own line
/// 3. Validate with `uuid::Uuid::parse_str` only
/// 4. Bare UUIDs in free prose → None
/// 5. Reporting/verify strings like `Ledger: 2 pending, 0 unaudited drift.` → None
pub fn extract_ledger_tx_ref(msg: &str) -> Option<String> {
    // Prefer `Ledger:` over `Ledger-Tx:` when both appear.
    if let Some(id) = scan_ledger_uuid_lines(msg, "Ledger:") {
        return Some(id);
    }
    scan_ledger_uuid_lines(msg, "Ledger-Tx:")
}

fn scan_ledger_uuid_lines(msg: &str, key: &str) -> Option<String> {
    for line in msg.lines() {
        let trimmed = line.trim();
        // Case-insensitive ASCII key prefix. Use `get` so non-ASCII lines where
        // `key.len()` is not a UTF-8 char boundary do not panic (codex P1).
        let Some(head) = trimmed.get(..key.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(key) {
            continue;
        }
        let rest = trimmed[key.len()..].trim();
        if rest.is_empty() {
            continue;
        }
        // End-of-line after UUID only — refuse "Ledger: 2 pending, …" and similar.
        if uuid::Uuid::parse_str(rest).is_ok() {
            return Some(rest.to_string());
        }
    }
    None
}

/// Pure provenance classifier (no DB).
///
/// Priority (B1 / B3):
/// 1. AlreadyCommitted — verified msg ref is COMMITTED
/// 2. LinkPending — verified msg ref is PENDING, OR exactly one open PENDING
///    globally and no conflicting/missing msg ref
/// 3. AmbiguousMulti — N>1 pending, no verified msg ref
/// 4. Fallback — everything else (incl. extracted but Missing)
///
/// `ref_status` is `Some` only when `extracted` is `Some` and the caller looked
/// the id up; when `extracted` is `None`, pass `None`.
pub fn classify_provenance_sot(
    extracted: Option<&str>,
    pending_ids: &[String],
    ref_status: Option<LedgerRefStatus>,
) -> ProvenanceSotClass {
    if let Some(tx_id) = extracted {
        match ref_status {
            Some(LedgerRefStatus::Committed) => {
                return ProvenanceSotClass::AlreadyCommitted {
                    tx_id: tx_id.to_string(),
                };
            }
            Some(LedgerRefStatus::Pending) => {
                return ProvenanceSotClass::LinkPending {
                    tx_id: tx_id.to_string(),
                };
            }
            // Missing or not supplied: do not invent skip/link from the ref.
            Some(LedgerRefStatus::Missing) | None => {
                return ProvenanceSotClass::Fallback;
            }
        }
    }

    // No verified msg ref — global single-pending rule (entity match not required).
    match pending_ids.len() {
        0 => ProvenanceSotClass::Fallback,
        1 => ProvenanceSotClass::LinkPending {
            tx_id: pending_ids[0].clone(),
        },
        _ => ProvenanceSotClass::AmbiguousMulti,
    }
}

/// DB-backed resolve of provenance SoT for the commit-msg hook.
pub(super) fn resolve_provenance_sot(
    raw_commit_msg: &str,
    layout: &Layout,
    config: &Config,
) -> Result<ProvenanceSotClass> {
    let extracted = extract_ledger_tx_ref(raw_commit_msg);
    let mut storage = StorageManager::init_with_layout(layout)?;
    let tx_mgr = TransactionManager::new(&mut storage, layout.root.clone().into(), config.clone());

    let mut pending = tx_mgr
        .get_all_pending()
        .map_err(|e| miette::miette!("Failed to list pending transactions: {}", e))?;
    // Deterministic order for single-pending pick when multiple rows ever race.
    pending.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));
    let pending_ids: Vec<String> = pending.iter().map(|t| t.tx_id.clone()).collect();

    let ref_status = if let Some(ref tx_id) = extracted {
        match tx_mgr
            .get_transaction(tx_id)
            .map_err(|e| miette::miette!("Failed to look up ledger TX {}: {}", tx_id, e))?
        {
            Some(tx) if tx.status.eq_ignore_ascii_case("COMMITTED") => {
                Some(LedgerRefStatus::Committed)
            }
            Some(tx) if tx.status.eq_ignore_ascii_case("PENDING") => Some(LedgerRefStatus::Pending),
            Some(_) => Some(LedgerRefStatus::Missing),
            None => {
                // Secondary: ledger_entries row without live transactions row.
                let entries = tx_mgr
                    .get_ledger_entries_for_tx(tx_id)
                    .map_err(|e| miette::miette!("Failed to look up ledger entries: {}", e))?;
                if entries.is_empty() {
                    Some(LedgerRefStatus::Missing)
                } else {
                    Some(LedgerRefStatus::Committed)
                }
            }
        }
    } else {
        None
    };

    Ok(classify_provenance_sot(
        extracted.as_deref(),
        &pending_ids,
        ref_status,
    ))
}
