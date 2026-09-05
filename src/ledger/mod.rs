pub mod adr;
pub mod chain_checkpoint;
pub mod chain_iter;
pub mod crypto;
pub mod db;
pub mod drift;
pub mod enforcement;
pub mod error;
pub mod federation;
pub mod mode_history;
pub mod pending_entity_overlap;
pub mod provenance;
pub mod public_export;
pub mod session;
pub mod transaction;
pub mod types;
pub mod ui;
pub mod validators;

#[cfg(feature = "export")]
pub use chain_checkpoint::load_checkpoint_head;
pub use chain_checkpoint::{CheckpointMode, compare_against_export, ordered_local_for_head};
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
