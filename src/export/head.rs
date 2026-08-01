//! Thin chain-head checkpoint export (`ledgerful export head`).

use crate::ledger::types::ChainHead;
use crate::state::layout::Layout;
use miette::Result;

/// Read the live chain head and enforce unsigned-export policy.
///
/// - Missing head → hard error
/// - Unsigned + `require_signing` → hard refuse
/// - Unsigned + `!require_signing` → warn + return head
/// - Signed → return head
pub fn prepare_chain_head_export(layout: &Layout) -> Result<ChainHead> {
    let mut storage = crate::state::storage::StorageManager::init_with_layout(layout)?;
    let db = crate::ledger::db::LedgerDb::new(storage.get_connection_mut());
    let head = db
        .get_chain_head()
        .map_err(|e| miette::miette!("Failed to read chain head: {e}"))?
        .ok_or_else(|| {
            miette::miette!(
                "No chain head to export. Commit at least one ledger entry first, or use a ledger that has a chain_head row."
            )
        })?;

    let sig = head.head_signature.as_deref().unwrap_or("");
    let pub_key = head.head_public_key.as_deref().unwrap_or("");
    let signed = !sig.is_empty() && !pub_key.is_empty();
    let config = crate::config::load::load_config(layout).unwrap_or_default();

    if !signed {
        if config.intent.require_signing {
            return Err(miette::miette!(
                "Refusing to export an unsigned chain head while intent.require_signing is true. \
                 Ensure keys exist under .ledgerful/keys and new commits sign the head \
                 (or re-sign / repair signing), then retry `export head`."
            ));
        }
        tracing::warn!(
            target: "cli_summary",
            "Exported chain head is unsigned (require_signing=false); retain as an honest unsigned checkpoint."
        );
    }

    Ok(head)
}

/// Serialize a chain head to pretty JSON bytes (same shape as SOC2 zip entry).
pub fn serialize_chain_head(head: &ChainHead) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(head)
        .map_err(|e| miette::miette!("Failed to serialize chain head: {e}"))
}
