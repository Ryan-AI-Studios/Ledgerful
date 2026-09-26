//! Engine ledger facade: crate-safe core from `ledgerful_ledger` plus adapters
//! that still need parent modules (`transaction`, `validators`, `state`, …).

pub use ledgerful_ledger::{
    adr, chain_iter, crypto, db, enforcement, error, pending_entity_overlap, re_sign, reason,
    session, types,
};

pub mod drift;
pub mod federation;
pub mod mode_history;
pub mod provenance;
pub mod public_export;
pub mod transaction;
pub mod ui;
pub mod validators;

#[cfg(feature = "export")]
pub use chain_checkpoint::load_checkpoint_head;
pub use chain_checkpoint::{
    CheckpointMode, CheckpointResultKind, classify_against_export, compare_against_export,
    ordered_local_for_head, ordered_local_for_head_with_head,
};
pub use chain_iter::{ChainWalk, check_chain_links, iter_local_chain};
pub use crypto::{
    CURRENT_LEDGER_SIG_VERSION, CryptoError, LEDGER_SIG_VERSION_V1, LEDGER_SIG_VERSION_V2,
    LedgerSignInput, SignatureTrustStatus, SignatureVerifyError, TrustedKeyError,
    classify_entry_signature, compute_entry_hash, compute_entry_hash_for_entry,
    compute_entry_hash_versioned, content_digest_hex, derive_entity_normalized, encode_v1_payload,
    encode_v2_payload, entity_normalized_consistent, get_keys_dir, get_or_create_keys,
    get_or_create_keys_in, keys_dir_path, nfc_normalize, normalize_trusted_public_key,
    read_public_key_hex, sign_chain_head, sign_ledger_entry, sign_ledger_entry_in,
    sign_ledger_entry_in_v2, sign_ledger_entry_v2, verify_chain_head, verify_entry_signature,
    verify_entry_signature_with_result, verify_keypair_consistency, verify_ledger_entry_signature,
    verify_signature,
};
pub use db::LedgerDb;
pub use drift::DriftManager;
pub use enforcement::{
    CategoryStackMapping, CommitValidator, RuleType, TechStackRule, ValidationLevel, WatcherPattern,
};
pub use error::LedgerError;
pub use pending_entity_overlap::{
    COLLISION_GREP_PREFIX, COLLISION_PATH_CAP, CollisionHit, PendingEntityCollision,
    find_start_collisions, format_collision_report, normalize_overlap_key, pending_entity_overlaps,
};
pub use provenance::{ProvenanceAction, TokenProvenance, compute_symbol_diff};
pub use public_export::{
    ExportOptions, compute_author_pseudonym, export_public_bundle, verify_manifest_signature,
};
pub use re_sign::{ReSignBlock, ReSignPreflight, evaluate_re_sign_preflight};
pub use reason::{
    classify_reason_kind, classify_risk_source, is_trailer_only_reason, labeled_search_items,
    risk_from_category, substantive_reason_from_commit_msg,
};
pub use session::get_session_id;
pub use transaction::TransactionManager;
pub use types::{
    AdrMetadata, AdrMetadataUpdate, AdrStatus, Category, ChainHead, ChangeType, CommitRequest,
    EntryType, LedgerEntry, Transaction, TransactionRequest, VerificationBasis, VerificationStatus,
};
pub use ui::{
    LedgerStatus, breaking_icon, get_category_icon, get_change_type_icon, get_status_icon,
    with_icon,
};
pub use validators::{ValidationResult, ValidatorRunner};

/// Engine adapter: zip/JSON load stays here; chain math lives in `ledgerful_ledger`.
pub mod chain_checkpoint {
    pub use ledgerful_ledger::chain_checkpoint::*;

    #[cfg(feature = "export")]
    pub use super::chain_checkpoint_load::load_checkpoint_head;
}

#[cfg(feature = "export")]
mod chain_checkpoint_load {
    use crate::ledger::types::ChainHead;
    use miette::Result;
    use std::path::Path;

    /// Load a retained chain head from a SOC2 evidence zip (`chain_head.json`
    /// entry) or a bare JSON file of the same `ChainHead` shape.
    pub fn load_checkpoint_head(path: &Path) -> Result<ChainHead> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        match ext.as_str() {
            "zip" => load_from_zip(path),
            "json" => load_from_json_file(path),
            _ => match load_from_zip(path) {
                Ok(head) => Ok(head),
                Err(zip_err) => match load_from_json_file(path) {
                    Ok(head) => Ok(head),
                    Err(json_err) => Err(miette::miette!(
                        "Failed to load chain head from {}: not a valid zip ({}); not bare ChainHead JSON ({})",
                        path.display(),
                        zip_err,
                        json_err
                    )),
                },
            },
        }
    }

    fn load_from_zip(path: &Path) -> Result<ChainHead> {
        let file = std::fs::File::open(path)
            .map_err(|e| miette::miette!("Failed to open export zip {}: {}", path.display(), e))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| miette::miette!("Failed to read export zip {}: {}", path.display(), e))?;
        let mut entry = archive
            .by_name("chain_head.json")
            .map_err(|e| miette::miette!("Export missing chain_head.json: {}", e))?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf)
            .map_err(|e| miette::miette!("Failed to read chain_head.json from export: {}", e))?;
        let head: ChainHead = serde_json::from_slice(&buf)
            .map_err(|e| miette::miette!("Failed to parse chain_head.json: {}", e))?;
        Ok(head)
    }

    fn load_from_json_file(path: &Path) -> Result<ChainHead> {
        let buf = std::fs::read(path).map_err(|e| {
            miette::miette!("Failed to read chain head file {}: {}", path.display(), e)
        })?;
        let head: ChainHead = serde_json::from_slice(&buf).map_err(|e| {
            miette::miette!(
                "Failed to parse chain head JSON from {}: {}",
                path.display(),
                e
            )
        })?;
        Ok(head)
    }
}
