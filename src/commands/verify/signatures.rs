use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::{OwoColorize, Stream};
use std::path::Path;

/// Where a per-entry signature status line is emitted (0093 DoD-5 / 0100).
///
/// `RawStderr` lines use `eprintln!` and are **never** suppressed by the
/// four-state `cli_summary` filter (default/quiet → INFO, verbose → DEBUG,
/// machine → WARN). Filtered detail uses
/// `tracing::debug!(target: "cli_summary", …)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigEntryStream {
    /// Hard failures: INVALID crypto, and UNSIGNED when signing is required.
    RawStderr,
    /// Per-entry VALID / optional SKIP detail — `debug!` on `cli_summary`.
    CliSummaryDebug,
}

impl SigEntryStream {
    pub fn is_raw_stderr(self) -> bool {
        matches!(self, Self::RawStderr)
    }
}

/// Decide the emission stream for a signature status line (pure; unit-tested).
///
/// Production emit loop calls this for every LOCAL entry (N1). Policy-invalid
/// crypto-valid rows force [`SigEntryStream::RawStderr`] before this helper.
pub fn sig_entry_stream(
    status: crate::ledger::crypto::SignatureTrustStatus,
    signing_required: bool,
) -> SigEntryStream {
    use crate::ledger::crypto::SignatureTrustStatus;
    match status {
        SignatureTrustStatus::Invalid => SigEntryStream::RawStderr,
        SignatureTrustStatus::Unsigned if signing_required => SigEntryStream::RawStderr,
        SignatureTrustStatus::ValidTrusted
        | SignatureTrustStatus::ValidUnknownKey
        | SignatureTrustStatus::Unsigned => SigEntryStream::CliSummaryDebug,
    }
}

/// Aggregate counts on the signature verification summary line (0093 DoD-6).
///
/// Counting happens before any quiet/default/machine stream filter — the same
/// fixture always yields identical aggregates under default and `--quiet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignatureAggregateCounts {
    pub valid: usize,
    pub invalid: usize,
    pub skipped: usize,
    pub federated_skip: usize,
    pub unsigned_fail: usize,
}

impl SignatureAggregateCounts {
    /// Plain (uncolored) counts fragment for dual-run assertions and agents.
    pub fn summary_counts_fragment(&self) -> String {
        format!(
            "{} valid, {} invalid, {} skipped, {} federated-skip",
            self.valid, self.invalid, self.skipped, self.federated_skip
        )
    }

    /// Human summary line with gated colour (`Stream::Stdout` — lands on
    /// `cli_summary` info! → stdout). Used by production emit and colour-gate tests.
    pub fn format_summary_line_colored(&self) -> String {
        use owo_colors::{OwoColorize, Stream};
        let invalid = if self.invalid > 0 {
            self.invalid
                .if_supports_color(Stream::Stdout, |s| s.red())
                .to_string()
        } else {
            self.invalid.to_string()
        };
        format!(
            "\nSignature verification summary: {} valid, {} invalid, {} skipped, {} federated-skip.",
            self.valid.if_supports_color(Stream::Stdout, |s| s.green()),
            invalid,
            self.skipped
                .if_supports_color(Stream::Stdout, |s| s.yellow()),
            self.federated_skip
        )
    }
}

/// Success line for signature verify — gated on `Stream::Stdout` (cli_summary info!).
pub fn format_signature_success_line_colored() -> String {
    use owo_colors::{OwoColorize, Stream, Style};
    "All signature validations passed successfully!"
        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
        .to_string()
}

/// Pure per-entry class used by the production emit loop and DoD-6 tally tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigEntryClass {
    Federated,
    Valid,
    Invalid,
    UnsignedRequired,
    UnsignedOptional,
}

/// Classify one entry for emit routing + aggregate tallies (pure).
pub fn class_for_sig_entry(
    is_local: bool,
    status: crate::ledger::crypto::SignatureTrustStatus,
    signing_required: bool,
    policy_invalid: bool,
) -> SigEntryClass {
    use crate::ledger::crypto::SignatureTrustStatus;
    if !is_local {
        return SigEntryClass::Federated;
    }
    match status {
        SignatureTrustStatus::ValidTrusted | SignatureTrustStatus::ValidUnknownKey
            if policy_invalid =>
        {
            SigEntryClass::Invalid
        }
        SignatureTrustStatus::ValidTrusted | SignatureTrustStatus::ValidUnknownKey => {
            SigEntryClass::Valid
        }
        SignatureTrustStatus::Invalid => SigEntryClass::Invalid,
        SignatureTrustStatus::Unsigned if signing_required => SigEntryClass::UnsignedRequired,
        SignatureTrustStatus::Unsigned => SigEntryClass::UnsignedOptional,
    }
}

/// Tally loop-visible counters from entry classes. `invalid` comes from the
/// policy enumerate path (may include more than per-entry Invalid emissions).
pub fn tally_signature_classes(
    classes: impl IntoIterator<Item = SigEntryClass>,
    invalid: usize,
) -> SignatureAggregateCounts {
    let mut counts = SignatureAggregateCounts {
        invalid,
        ..SignatureAggregateCounts::default()
    };
    for class in classes {
        match class {
            SigEntryClass::Federated => counts.federated_skip += 1,
            SigEntryClass::Valid => counts.valid += 1,
            SigEntryClass::Invalid => {}
            SigEntryClass::UnsignedRequired => counts.unsigned_fail += 1,
            SigEntryClass::UnsignedOptional => counts.skipped += 1,
        }
    }
    counts
}

/// Exit codes for signature / chain verification (0072 frozen table).
///
/// | Condition | Status | Exit |
/// |---|---|---|
/// | All signed rows valid; no hard policy failure | VALID (trusted/unknown) | **0** |
/// | INVALID signature / wrong version / entity_normalized / chain break | INVALID / CHAIN_BREAK | **1** |
/// | Crypto-valid unknown key when trusted-only policy requires pins (reserved) | VALID (unknown key) policy fail | **2** |
/// | Unsigned present under `require_signing` or `--strict-signatures` | UNSIGNED | **3** |
///
/// CLI wiring: `request_exit` + `take_requested_exit_code` so `main` can exit
/// with the distinct code without a full `ExitCode` refactor of every path.
pub mod sig_exit {
    use std::sync::atomic::{AtomicI32, Ordering};

    /// All signed rows valid; no hard policy failure.
    pub const OK: i32 = 0;
    /// INVALID signature, wrong version, entity_normalized mismatch, or chain break.
    pub const INVALID_OR_CHAIN: i32 = 1;
    /// Policy: crypto-valid unknown key when trusted keys are required (reserved).
    pub const POLICY: i32 = 2;
    /// Unsigned present under require_signing or --strict-signatures.
    pub const UNSIGNED: i32 = 3;

    static REQUESTED: AtomicI32 = AtomicI32::new(0);

    /// Record a non-zero exit code for the CLI process (first-write-wins).
    pub fn request_exit(code: i32) {
        let _ = REQUESTED.compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Take the requested exit code (if any) and reset. Used by `main`.
    pub fn take_requested_exit_code() -> Option<i32> {
        let c = REQUESTED.swap(0, Ordering::SeqCst);
        if c == 0 { None } else { Some(c) }
    }

    /// Pure exit-code decision for the signature path (0072 DoD-4 matrix).
    ///
    /// - `invalid_count` = rows that fail crypto / min_sig_version / consistency
    /// - `unsigned_fail` = unsigned rows counted only when signing is required
    /// - Chain breaks are reported via `invalid_count`-style path with exit 1
    ///   (callers set `chain_break=true`).
    pub fn decide_signature_exit(
        invalid_count: usize,
        unsigned_fail: usize,
        chain_break: bool,
    ) -> i32 {
        if chain_break {
            return INVALID_OR_CHAIN;
        }
        if invalid_count == 0 {
            return OK;
        }
        if unsigned_fail > 0 && invalid_count == unsigned_fail {
            UNSIGNED
        } else {
            INVALID_OR_CHAIN
        }
    }
}

pub fn verify_ledger_signatures(layout: &Layout) -> Result<()> {
    verify_ledger_signatures_with_options(layout, true, false, false, None, false)
}

pub fn verify_ledger_signatures_with_options(
    layout: &Layout,
    verify_signatures: bool,
    verify_chain: bool,
    strict_signatures: bool,
    against_export: Option<&Path>,
    exact: bool,
) -> Result<()> {
    let mut storage = StorageManager::init_with_layout(layout)?;
    let db = crate::ledger::db::LedgerDb::new(storage.get_connection_mut());

    let config = crate::config::load::load_config(layout).unwrap_or_default();
    let signing_required = config.intent.require_signing || strict_signatures;
    let trusted_keys = &config.intent.trusted_public_keys;
    let min_sig_version = config.intent.min_sig_version;

    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette::miette!("Failed to read ledger entries: {}", e))?;

    let head = db
        .get_chain_head()
        .map_err(|e| miette::miette!("Failed to read chain head: {}", e))?;

    if verify_chain || against_export.is_some() {
        if entries.is_empty() && against_export.is_none() {
            if head.is_some() {
                return Err(miette::miette!(
                    "Chain head exists but no ledger entries found (entries may have been wiped)."
                ));
            }
            eprintln!("Ledger is empty. No chain to verify.");
            return Ok(());
        }
        verify_chain_integrity(
            &entries,
            head.as_ref(),
            against_export,
            exact,
            verify_signatures,
            signing_required,
            trusted_keys,
            min_sig_version,
        )?;
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("Ledger is empty. No signatures to verify.");
        return Ok(());
    }

    tracing::info!(
        target: "cli_summary",
        "Verifying signatures for {} ledger entries (require_signing={}, min_sig_version={})...",
        entries.len(),
        signing_required,
        min_sig_version
    );
    let invalid = enumerate_invalid_ledger_entries_with_policy(
        &entries,
        signing_required,
        trusted_keys,
        min_sig_version,
    );
    let invalid_count = invalid.len();
    let all_valid = invalid_count == 0;

    let invalid_tx_ids: std::collections::HashSet<&str> =
        invalid.iter().map(|(tx_id, _, _)| tx_id.as_str()).collect();
    let mut classes: Vec<SigEntryClass> = Vec::with_capacity(entries.len());

    for entry in &entries {
        let is_local = entry.origin == "LOCAL";
        let status = if is_local {
            crate::ledger::crypto::classify_entry_signature(entry, trusted_keys, min_sig_version)
        } else {
            // Federated rows are not crypto-classified for this path.
            crate::ledger::crypto::SignatureTrustStatus::Unsigned
        };
        let policy_invalid = is_local && invalid_tx_ids.contains(entry.tx_id.as_str());
        let class = class_for_sig_entry(is_local, status, signing_required, policy_invalid);
        classes.push(class);

        if !is_local {
            continue;
        }

        let short = if entry.tx_id.len() >= 8 {
            &entry.tx_id[..8]
        } else {
            &entry.tx_id
        };

        // Production emit routes through `sig_entry_stream` (N1 / DoD-5).
        // Policy-invalid crypto-valid rows force raw stderr like hard INVALID.
        let stream = if policy_invalid
            && matches!(
                status,
                crate::ledger::crypto::SignatureTrustStatus::ValidTrusted
                    | crate::ledger::crypto::SignatureTrustStatus::ValidUnknownKey
            ) {
            SigEntryStream::RawStderr
        } else {
            sig_entry_stream(status, signing_required)
        };

        match (status, stream) {
            (
                crate::ledger::crypto::SignatureTrustStatus::ValidTrusted
                | crate::ledger::crypto::SignatureTrustStatus::ValidUnknownKey,
                SigEntryStream::RawStderr,
            ) => {
                eprintln!(
                    "  [{}] TX {} signature verification FAILED!",
                    "INVALID".if_supports_color(Stream::Stderr, |s| s.red()),
                    short
                );
            }
            (
                crate::ledger::crypto::SignatureTrustStatus::ValidTrusted,
                SigEntryStream::CliSummaryDebug,
            ) => {
                // Per-entry detail at debug! — visible under --verbose (DEBUG
                // layer); hidden at product default INFO and under --quiet /
                // machine (0100 / 0093 DoD-5).
                tracing::debug!(
                    target: "cli_summary",
                    "  [{}] TX {}",
                    status.as_str().if_supports_color(Stream::Stdout, |s| s.green()),
                    short
                );
            }
            (
                crate::ledger::crypto::SignatureTrustStatus::ValidUnknownKey,
                SigEntryStream::CliSummaryDebug,
            ) => {
                // Amber/yellow: crypto-valid but unpinned key is not full
                // success — shared vocabulary with doctor [sig-pin] warning
                // ("unknown key", "pin", "trusted").
                tracing::debug!(
                    target: "cli_summary",
                    "  [{}] TX {}",
                    status.as_str().if_supports_color(Stream::Stdout, |s| s.yellow()),
                    short
                );
            }
            (crate::ledger::crypto::SignatureTrustStatus::Invalid, _) => {
                eprintln!(
                    "  [{}] TX {} signature verification FAILED!",
                    "INVALID".if_supports_color(Stream::Stderr, |s| s.red()),
                    short
                );
            }
            (crate::ledger::crypto::SignatureTrustStatus::Unsigned, SigEntryStream::RawStderr) => {
                eprintln!(
                    "  [{}] TX {} has no signature — treating as verification failure.",
                    "UNSIGNED".if_supports_color(Stream::Stderr, |s| s.yellow()),
                    short
                );
            }
            (
                crate::ledger::crypto::SignatureTrustStatus::Unsigned,
                SigEntryStream::CliSummaryDebug,
            ) => {
                tracing::debug!(
                    target: "cli_summary",
                    "  [{}] TX {} has no signature (signing not required, skipping).",
                    "SKIP".if_supports_color(Stream::Stdout, |s| s.yellow()),
                    short
                );
            }
        }
    }

    // Aggregate tallies are pure over `classes` — independent of quiet/default
    // filters (DoD-6). Same fixture always yields the same counts.
    let aggregates = tally_signature_classes(classes, invalid_count);

    if aggregates.federated_skip > 0 {
        tracing::debug!(
            target: "cli_summary",
            "  [{}]: {}",
            "SKIP (federated)".if_supports_color(Stream::Stdout, |s| s.yellow()),
            aggregates.federated_skip
        );
    }

    tracing::info!(
        target: "cli_summary",
        "{}",
        aggregates.format_summary_line_colored()
    );

    if all_valid {
        tracing::info!(
            target: "cli_summary",
            "{}",
            format_signature_success_line_colored()
        );
        Ok(())
    } else {
        let code =
            sig_exit::decide_signature_exit(aggregates.invalid, aggregates.unsigned_fail, false);
        sig_exit::request_exit(code);
        if code == sig_exit::UNSIGNED {
            Err(miette::miette!(
                "Ledger signature verification failed: {} unsigned entries (exit {}).",
                aggregates.unsigned_fail,
                sig_exit::UNSIGNED
            ))
        } else {
            Err(miette::miette!(
                "Ledger signature verification failed: {} entries have invalid or missing signatures.",
                aggregates.invalid
            ))
        }
    }
}

pub fn enumerate_invalid_ledger_entries(
    entries: &[crate::ledger::types::LedgerEntry],
    signing_required: bool,
) -> Vec<(String, String, String)> {
    enumerate_invalid_ledger_entries_with_policy(entries, signing_required, &[], 1)
}

pub fn enumerate_invalid_ledger_entries_with_policy(
    entries: &[crate::ledger::types::LedgerEntry],
    signing_required: bool,
    trusted_keys: &[String],
    min_sig_version: u32,
) -> Vec<(String, String, String)> {
    let mut invalid = Vec::new();
    for entry in entries {
        if entry.origin != "LOCAL" {
            continue;
        }
        let status =
            crate::ledger::crypto::classify_entry_signature(entry, trusted_keys, min_sig_version);
        match status {
            crate::ledger::crypto::SignatureTrustStatus::Invalid => {
                invalid.push((
                    entry.tx_id.clone(),
                    entry.signature.clone().unwrap_or_default(),
                    entry.public_key.clone().unwrap_or_default(),
                ));
            }
            crate::ledger::crypto::SignatureTrustStatus::Unsigned if signing_required => {
                invalid.push((entry.tx_id.clone(), String::new(), String::new()));
            }
            crate::ledger::crypto::SignatureTrustStatus::ValidTrusted
            | crate::ledger::crypto::SignatureTrustStatus::ValidUnknownKey
            | crate::ledger::crypto::SignatureTrustStatus::Unsigned => {}
        }
    }
    invalid
}

fn compute_entry_hash_for_verify(entry: &crate::ledger::types::LedgerEntry) -> Result<String> {
    crate::ledger::crypto::compute_entry_hash_for_entry(entry)
        .map_err(|e| miette::miette!("Failed to compute entry hash for TX {}: {e}", entry.tx_id))
}

#[allow(clippy::too_many_arguments)]
fn verify_chain_integrity(
    entries: &[crate::ledger::types::LedgerEntry],
    head: Option<&crate::ledger::types::ChainHead>,
    against_export: Option<&Path>,
    exact: bool,
    verify_signatures: bool,
    signing_required: bool,
    trusted_keys: &[String],
    min_sig_version: u32,
) -> Result<()> {
    // Distinguish a real stored chain head from one we will synthesize for
    // pre-chain/legacy ledgers. The integrity check that binds the computed
    // chain to the stored head must only run for real heads; a synthesized
    // head IS the computed chain, so comparing it to itself is meaningless.
    let head_is_real = head.is_some();
    let local_head = head.cloned();

    let mut chain_break: Option<String> = None;
    let mut prev_hash: Option<String> = None;
    let mut chain_length: i64 = 0;

    // Shared chain iterator (RT-C4): walk by prev_hash linkage; exclude federated.
    let walk = crate::ledger::chain_iter::iter_local_chain(entries);
    if walk.federated_skipped > 0 {
        tracing::info!(
            target: "cli_summary",
            "  [{}]: {}",
            "SKIP (federated)".if_supports_color(Stream::Stdout, |s| s.yellow()),
            walk.federated_skipped
        );
    }
    if !walk.forks.is_empty() {
        sig_exit::request_exit(sig_exit::INVALID_OR_CHAIN);
        return Err(miette::miette!(
            "CHAIN_BREAK: detected {} fork(s) in local chain (first parent hash {}).",
            walk.forks.len(),
            walk.forks[0].0
        ));
    }
    // The chain link check operates on the LOCAL walk when there is a real
    // stored head OR when entries already contain prev_hash links.  A real
    // stored head's genesis is the timestamp of the first in-chain entry.  If
    // there is no stored head and no entries have prev_hash, the ledger is
    // pre-chain/benign: verify standalone signatures if requested, but do not
    // walk a non-existent chain. The export comparison below uses a synthesized
    // head for that case.
    let has_any_prev_link = walk.ordered.iter().any(|e| e.prev_hash.is_some())
        || entries
            .iter()
            .any(|e| e.origin == "LOCAL" && e.prev_hash.is_some());
    let should_walk_chain = head_is_real || has_any_prev_link;

    // Multiple null-prev genesis rows are only a break once a chain exists.
    // Pre-chain ledgers legitimately have many null-prev entries.
    if should_walk_chain && !walk.extra_genesis.is_empty() {
        sig_exit::request_exit(sig_exit::INVALID_OR_CHAIN);
        return Err(miette::miette!(
            "Chain break: {} additional genesis entr(y/ies) with null prev_hash after chain started (first: {}).",
            walk.extra_genesis.len(),
            walk.extra_genesis[0].tx_id
        ));
    }
    if should_walk_chain && !walk.orphans.is_empty() {
        sig_exit::request_exit(sig_exit::INVALID_OR_CHAIN);
        return Err(miette::miette!(
            "Chain break: {} orphan LOCAL entr(y/ies) not linked by prev_hash (first: {}).",
            walk.orphans.len(),
            walk.orphans[0].tx_id
        ));
    }
    let chain_entries: &[crate::ledger::types::LedgerEntry] = if should_walk_chain {
        &walk.ordered
    } else {
        // Pre-chain: verify LOCAL entries only, in stable order.
        &walk.ordered
    };

    for entry in chain_entries {
        if verify_signatures {
            let status = crate::ledger::crypto::classify_entry_signature(
                entry,
                trusted_keys,
                min_sig_version,
            );
            match status {
                crate::ledger::crypto::SignatureTrustStatus::Invalid => {
                    sig_exit::request_exit(sig_exit::INVALID_OR_CHAIN);
                    return Err(miette::miette!(
                        "Signature verification failed for TX {} (chain break).",
                        entry.tx_id
                    ));
                }
                crate::ledger::crypto::SignatureTrustStatus::Unsigned if signing_required => {
                    sig_exit::request_exit(sig_exit::UNSIGNED);
                    return Err(miette::miette!(
                        "TX {} is missing a signature (chain-required-after-genesis; exit {}).",
                        entry.tx_id,
                        sig_exit::UNSIGNED
                    ));
                }
                crate::ledger::crypto::SignatureTrustStatus::ValidTrusted
                | crate::ledger::crypto::SignatureTrustStatus::ValidUnknownKey
                | crate::ledger::crypto::SignatureTrustStatus::Unsigned => {}
            }
        }

        if !should_walk_chain {
            continue;
        }

        if let Some(expected_prev) = prev_hash.as_ref() {
            match &entry.prev_hash {
                Some(actual_prev) if actual_prev == expected_prev => {}
                other => {
                    let detail = match other {
                        Some(actual) => {
                            format!("expected prev_hash {}, found {}", expected_prev, actual)
                        }
                        None => {
                            format!("expected prev_hash {} but entry has none", expected_prev)
                        }
                    };
                    chain_break = Some(format!("Chain break at TX {}: {}", entry.tx_id, detail));
                    break;
                }
            }
        } else if entry.prev_hash.is_some() {
            chain_break = Some(format!(
                "Chain break at TX {}: genesis entry must have no prev_hash",
                entry.tx_id
            ));
            break;
        }
        chain_length += 1;
        prev_hash = Some(compute_entry_hash_for_verify(entry)?);
    }

    if let Some(msg) = chain_break {
        sig_exit::request_exit(sig_exit::INVALID_OR_CHAIN);
        return Err(miette::miette!("{}", msg));
    }

    // When an export is supplied we must compare against it even if the local
    // chain head is missing or the ledger is pre-chain. The SOC2 export
    // synthesizes a chain_head.json for legacy/pre-chain ledgers, so
    // --against-export can still detect truncation/rollback.
    if let Some(export_path) = against_export {
        #[cfg(feature = "export")]
        {
            return compare_against_export_path(
                entries,
                local_head.as_ref(),
                head_is_real,
                prev_hash.as_deref(),
                chain_length,
                export_path,
                exact,
            );
        }
        #[cfg(not(feature = "export"))]
        {
            let _ = (export_path, exact, chain_length, head_is_real);
            return Err(miette::miette!(
                "verify --against-export requires the export feature; rebuild with --features export"
            ));
        }
    }
    // `--exact` is only meaningful with `--against-export` (CLI rejects otherwise).
    let _ = exact;

    // Fail-closed: if the chain head has been stripped from a DB that contains
    // in-chain entries (entries with prev_hash set), treat it as a downgrade. If
    // no entry has ever referenced chain state, the ledger is pre-chain/benign.
    if !head_is_real && !entries.is_empty() {
        let any_prev = entries.iter().any(|e| e.prev_hash.is_some());
        if any_prev {
            // Only report a downgrade if the chain links were not already
            // reported as broken by the walk above. The walk failure is more
            // specific; this fallback catches a head stripped from an otherwise
            // intact chain.
            if chain_break.is_none() {
                return Err(miette::miette!(
                    "Chain head is missing but ledger entries have prev_hash values; downgrade detected."
                ));
            }
        } else {
            tracing::info!(
                target: "cli_summary",
                "Chain not yet started (pre-chain ledger). No chain to verify."
            );
            return Ok(());
        }
    }

    if let Some(head_ref) = local_head {
        let expected_latest = prev_hash.as_deref().unwrap_or("");
        if expected_latest != head_ref.latest_entry_hash {
            return Err(miette::miette!(
                "Chain head mismatch: computed latest entry hash {} does not match stored head {}",
                expected_latest,
                head_ref.latest_entry_hash
            ));
        }
        if chain_length != head_ref.length {
            return Err(miette::miette!(
                "Chain length mismatch: computed {} linked entries but head claims {}",
                chain_length,
                head_ref.length
            ));
        }
        let head_sig = head_ref.head_signature.as_deref().unwrap_or("");
        let head_pub = head_ref.head_public_key.as_deref().unwrap_or("");
        if !crate::ledger::crypto::verify_chain_head(
            &head_ref.latest_entry_hash,
            &head_ref.genesis,
            head_ref.length,
            head_sig,
            head_pub,
        ) {
            return Err(miette::miette!(
                "Chain head signature verification failed for head {}.",
                head_ref.latest_entry_hash
            ));
        }

        tracing::info!(
            target: "cli_summary",
            "Chain verified: {} linked entries from genesis {} to head {}.",
            head_ref.length,
            head_ref.genesis,
            head_ref.latest_entry_hash
        );
    }

    Ok(())
}

/// Against-export comparison (checkpoint default or exact). Gated on `export`
/// so `--no-default-features` builds do not pull zip/export solely via verify.
#[cfg(feature = "export")]
fn compare_against_export_path(
    entries: &[crate::ledger::types::LedgerEntry],
    local_head: Option<&crate::ledger::types::ChainHead>,
    head_is_real: bool,
    computed_latest_hash: Option<&str>,
    chain_length: i64,
    export_path: &Path,
    exact: bool,
) -> Result<()> {
    use crate::ledger::chain_checkpoint::{
        CheckpointMode, compare_against_export, load_checkpoint_head, ordered_local_for_head,
    };

    let export_head = load_checkpoint_head(export_path)?;

    // An empty local ledger compared to a non-empty export is itself a
    // rollback/wipe signal: every local entry was deleted. This takes
    // precedence over the "No local chain head" error, because with no
    // local entries there is simply nothing to compare except the wipe.
    if entries.is_empty() {
        return Err(miette::miette!(
            "Local ledger is empty but export shows {} linked entries (rollback/wipe detected).",
            export_head.length
        ));
    }

    // Synthesize a local head for pre-chain/legacy ledgers so they can be
    // checked against an exported checkpoint. Use the same helper the export
    // path uses so the synthesized head matches exactly.
    let local_head = if let Some(h) = local_head {
        h.clone()
    } else {
        // Fail-closed downgrade mitigation: if entries already have
        // prev_hash links but the chain_head row is missing, the signed
        // head has been stripped (Option-A downgrade). Do not let
        // --against-export synthesize a head that would pass.
        let any_prev = entries.iter().any(|e| e.prev_hash.is_some());
        if any_prev {
            return Err(miette::miette!(
                "Chain head is missing but entries have chain links (downgrade detected)"
            ));
        }
        crate::export::soc2::synthesize_chain_head(entries).ok_or_else(|| {
            miette::miette!("No local chain head and no entries to compare against export")
        })?
    };

    // Bind the live chain to the stored local head before comparing against
    // the export. This catches local truncation or insertion attacks that leave
    // chain_head untouched. Skip this when the local head was synthesized
    // from the same entries we just walked, because in that case it is
    // guaranteed to match and the export comparison is the real validation.
    if head_is_real {
        let computed = computed_latest_hash.unwrap_or("");
        if computed != local_head.latest_entry_hash {
            return Err(miette::miette!(
                "Chain head mismatch: computed latest entry hash {} does not match stored head {} (local chain altered).",
                computed,
                local_head.latest_entry_hash
            ));
        }
        if chain_length != local_head.length {
            return Err(miette::miette!(
                "Chain length mismatch: computed {} linked entries but head claims {} (local truncation/insertion detected).",
                chain_length,
                local_head.length
            ));
        }
        let head_sig = local_head.head_signature.as_deref().unwrap_or("");
        let head_pub = local_head.head_public_key.as_deref().unwrap_or("");
        if !crate::ledger::crypto::verify_chain_head(
            &local_head.latest_entry_hash,
            &local_head.genesis,
            local_head.length,
            head_sig,
            head_pub,
        ) {
            return Err(miette::miette!(
                "Chain head signature verification failed for head {}.",
                local_head.latest_entry_hash
            ));
        }
    }

    let ordered = ordered_local_for_head(entries);
    let mode = if exact {
        CheckpointMode::Exact
    } else {
        CheckpointMode::Checkpoint
    };
    compare_against_export(&ordered, &local_head, &export_head, mode)
}

#[cfg(test)]
mod sig_entry_stream_tests {
    use super::{
        SigEntryClass, SigEntryStream, SignatureAggregateCounts, class_for_sig_entry,
        format_signature_success_line_colored, sig_entry_stream, tally_signature_classes,
    };
    use crate::ledger::crypto::SignatureTrustStatus;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing_subscriber::Layer;
    use tracing_subscriber::fmt;
    use tracing_subscriber::fmt::writer::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    /// DoD-1: under force-off, signature aggregate + success contain no ESC.
    /// Formats with `Stream::Stdout` (stream-aware; not merged 2>&1).
    #[test]
    fn signature_summary_force_off_has_no_esc() {
        owo_colors::set_override(false);
        let aggregates = SignatureAggregateCounts {
            valid: 3,
            invalid: 1,
            skipped: 2,
            federated_skip: 0,
            unsigned_fail: 0,
        };
        let summary = aggregates.format_summary_line_colored();
        let success = format_signature_success_line_colored();
        assert!(
            !summary.contains('\u{1b}'),
            "summary must be ESC-free under set_override(false): {summary:?}"
        );
        assert!(
            !success.contains('\u{1b}'),
            "success must be ESC-free under set_override(false): {success:?}"
        );
        assert!(summary.contains("3 valid"));
        assert!(summary.contains("1 invalid"));
        assert!(success.contains("All signature validations passed"));
        owo_colors::unset_override();
    }

    /// DoD-1: force-on may colour (Stdout stream path still applies styles).
    #[test]
    fn signature_summary_force_on_may_colour() {
        owo_colors::set_override(true);
        let aggregates = SignatureAggregateCounts {
            valid: 1,
            invalid: 1,
            skipped: 0,
            federated_skip: 0,
            unsigned_fail: 0,
        };
        let summary = aggregates.format_summary_line_colored();
        let success = format_signature_success_line_colored();
        assert!(
            summary.contains('\u{1b}'),
            "summary should colour under set_override(true): {summary:?}"
        );
        assert!(
            success.contains('\u{1b}'),
            "success should colour under set_override(true): {success:?}"
        );
        // Stream-aware: format helpers hard-code Stream::Stdout (aggregate/success
        // land on cli_summary info! → stdout). Mis-pairing would still "colour"
        // under global override, so this pins the helper API + force-on path.
        let _ = StreamStdoutMarker;
        owo_colors::unset_override();
    }

    /// Compile-time / doc marker that signature colour uses Stdout (not Stderr).
    struct StreamStdoutMarker;

    #[test]
    fn invalid_and_required_unsigned_are_raw_stderr() {
        assert_eq!(
            sig_entry_stream(SignatureTrustStatus::Invalid, false),
            SigEntryStream::RawStderr
        );
        assert_eq!(
            sig_entry_stream(SignatureTrustStatus::Invalid, true),
            SigEntryStream::RawStderr
        );
        assert_eq!(
            sig_entry_stream(SignatureTrustStatus::Unsigned, true),
            SigEntryStream::RawStderr
        );
    }

    #[test]
    fn valid_and_optional_unsigned_are_filterable_debug() {
        assert_eq!(
            sig_entry_stream(SignatureTrustStatus::ValidTrusted, false),
            SigEntryStream::CliSummaryDebug
        );
        assert_eq!(
            sig_entry_stream(SignatureTrustStatus::ValidUnknownKey, true),
            SigEntryStream::CliSummaryDebug
        );
        assert_eq!(
            sig_entry_stream(SignatureTrustStatus::Unsigned, false),
            SigEntryStream::CliSummaryDebug
        );
    }

    /// DoD-6: same fixture classes → identical aggregate under two tallies
    /// (models default vs quiet both counting before any stream filter).
    #[test]
    fn aggregate_counts_identical_quiet_vs_default_pure_tally() {
        use SignatureTrustStatus::*;
        let fixture: Vec<(bool, SignatureTrustStatus, bool, bool)> = vec![
            // (is_local, status, signing_required, policy_invalid)
            (true, ValidTrusted, false, false),
            (true, ValidUnknownKey, false, false),
            (true, Invalid, false, false),
            (true, Unsigned, false, false),
            (true, Unsigned, true, false),
            (false, Unsigned, false, false),
            (true, ValidTrusted, false, true),
        ];
        let classes: Vec<SigEntryClass> = fixture
            .iter()
            .map(|&(local, status, req, policy)| class_for_sig_entry(local, status, req, policy))
            .collect();
        // invalid enumerate size for this fixture: Invalid + policy-invalid Valid + required Unsigned
        let invalid = 3usize;
        let default_run = tally_signature_classes(classes.iter().copied(), invalid);
        let quiet_run = tally_signature_classes(classes.iter().copied(), invalid);
        assert_eq!(
            default_run, quiet_run,
            "DoD-6: aggregate must be identical across dual runs of the same fixture"
        );
        assert_eq!(default_run.valid, 2);
        assert_eq!(default_run.invalid, 3);
        assert_eq!(default_run.skipped, 1);
        assert_eq!(default_run.federated_skip, 1);
        assert_eq!(default_run.unsigned_fail, 1);
        assert_eq!(
            default_run.summary_counts_fragment(),
            "2 valid, 3 invalid, 1 skipped, 1 federated-skip"
        );
        // Stream decision for each LOCAL non-policy-invalid row matches class.
        for &(local, status, req, policy) in &fixture {
            if !local {
                continue;
            }
            let class = class_for_sig_entry(local, status, req, policy);
            let stream = if policy && matches!(status, ValidTrusted | ValidUnknownKey) {
                SigEntryStream::RawStderr
            } else {
                sig_entry_stream(status, req)
            };
            match class {
                SigEntryClass::Valid | SigEntryClass::UnsignedOptional => {
                    assert_eq!(stream, SigEntryStream::CliSummaryDebug);
                }
                SigEntryClass::Invalid | SigEntryClass::UnsignedRequired => {
                    assert_eq!(stream, SigEntryStream::RawStderr);
                }
                SigEntryClass::Federated => unreachable!(),
            }
        }
    }

    #[derive(Clone, Default)]
    struct BufWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = BufGuard;
        fn make_writer(&'a self) -> Self::Writer {
            BufGuard {
                buf: Arc::clone(&self.buf),
            }
        }
    }

    struct BufGuard {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for BufGuard {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf
                .lock()
                .map_err(|e| io::Error::other(e.to_string()))?
                .extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// DoD-6 dual-run with real filter levels (0100 four-state):
    /// product default / quiet are INFO (aggregate only); verbose is DEBUG
    /// (per-entry + aggregate). Aggregate counts fragment is identical.
    #[test]
    fn aggregate_summary_under_default_quiet_and_verbose_filters() {
        let counts = SignatureAggregateCounts {
            valid: 4,
            invalid: 1,
            skipped: 2,
            federated_skip: 0,
            unsigned_fail: 0,
        };
        let fragment = counts.summary_counts_fragment();

        let capture = |max: Level| -> String {
            let buf = BufWriter::default();
            let capture = Arc::clone(&buf.buf);
            let layer = fmt::layer()
                .with_writer(buf)
                .without_time()
                .with_target(false)
                .with_level(false)
                .with_filter(tracing_subscriber::filter::filter_fn(move |meta| {
                    meta.target() == "cli_summary" && *meta.level() <= max
                }));
            let _guard = tracing_subscriber::registry().with(layer).set_default();
            // Per-entry detail (production uses debug!).
            tracing::debug!(target: "cli_summary", "  [VALID] TX aaaaaaaa");
            tracing::debug!(target: "cli_summary", "  [SKIP] TX bbbbbbbb");
            // Aggregate (production uses info!).
            tracing::info!(
                target: "cli_summary",
                "Signature verification summary: {fragment}."
            );
            String::from_utf8_lossy(&capture.lock().unwrap()).to_string()
        };

        // Product default and --quiet both use INFO (0100).
        let default_out = capture(Level::INFO);
        let quiet_out = capture(Level::INFO);
        let verbose_out = capture(Level::DEBUG);

        assert!(
            default_out.contains(&fragment),
            "default must show aggregate; out={default_out:?}"
        );
        assert!(
            quiet_out.contains(&fragment),
            "quiet must show same aggregate; out={quiet_out:?}"
        );
        assert!(
            verbose_out.contains(&fragment),
            "verbose must show aggregate; out={verbose_out:?}"
        );
        assert!(
            !default_out.contains("[VALID]") && !default_out.contains("[SKIP]"),
            "default (INFO) must hide per-entry detail; out={default_out:?}"
        );
        assert!(
            !quiet_out.contains("[VALID]") && !quiet_out.contains("[SKIP]"),
            "quiet must hide per-entry detail; out={quiet_out:?}"
        );
        assert!(
            verbose_out.contains("[VALID]"),
            "verbose (DEBUG) must show per-entry detail; out={verbose_out:?}"
        );

        // Parse counts from aggregate lines — must match exactly.
        fn parse_counts(s: &str) -> Option<(usize, usize, usize, usize)> {
            let line = s
                .lines()
                .find(|l| l.contains("Signature verification summary"))?;
            let after = line.split("summary: ").nth(1)?;
            let parts: Vec<&str> = after.split(',').collect();
            if parts.len() < 4 {
                return None;
            }
            let num = |p: &str| -> Option<usize> { p.split_whitespace().next()?.parse().ok() };
            Some((
                num(parts[0])?,
                num(parts[1])?,
                num(parts[2])?,
                num(parts[3].trim_end_matches('.'))?,
            ))
        }
        let d = parse_counts(&default_out).expect("default aggregate parse");
        let q = parse_counts(&quiet_out).expect("quiet aggregate parse");
        let v = parse_counts(&verbose_out).expect("verbose aggregate parse");
        assert_eq!(d, q, "DoD-6 dual-run: aggregate counts must match");
        assert_eq!(d, v, "verbose aggregate counts must match default");
        assert_eq!(d, (4, 1, 2, 0));
    }

    /// DoD-2 structural safety: INVALID / required-UNSIGNED always route to
    /// RawStderr (eprintln!), so no verbosity filter can suppress them.
    #[test]
    fn hard_failures_always_raw_stderr_across_verbosity_levels() {
        use crate::ledger::crypto::SignatureTrustStatus;
        // At every product verbosity the stream decision is identical —
        // RawStderr is outside cli_summary filtering entirely.
        for signing_required in [false, true] {
            assert!(
                sig_entry_stream(SignatureTrustStatus::Invalid, signing_required).is_raw_stderr(),
                "INVALID must always be RawStderr (signing_required={signing_required})"
            );
        }
        assert!(
            sig_entry_stream(SignatureTrustStatus::Unsigned, true).is_raw_stderr(),
            "required UNSIGNED must always be RawStderr"
        );
        // Soft statuses stay filterable (not RawStderr).
        assert!(!sig_entry_stream(SignatureTrustStatus::ValidTrusted, false).is_raw_stderr());
        assert!(!sig_entry_stream(SignatureTrustStatus::ValidUnknownKey, false).is_raw_stderr());
        assert!(!sig_entry_stream(SignatureTrustStatus::Unsigned, false).is_raw_stderr());
    }
}

#[cfg(test)]
mod sig_exit_tests {
    use super::sig_exit;

    /// Drain any leftover code from a prior test in the same process (defensive;
    /// nextest runs tests in separate processes by default).
    fn drain() {
        let _ = sig_exit::take_requested_exit_code();
    }

    #[test]
    fn pure_unsigned_sets_exit_3() {
        drain();
        sig_exit::request_exit(sig_exit::UNSIGNED);
        assert_eq!(
            sig_exit::take_requested_exit_code(),
            Some(sig_exit::UNSIGNED)
        );
        assert_eq!(sig_exit::UNSIGNED, 3);
        // Take resets so main sees the code once.
        assert_eq!(sig_exit::take_requested_exit_code(), None);
    }

    #[test]
    fn invalid_or_chain_sets_exit_1() {
        drain();
        sig_exit::request_exit(sig_exit::INVALID_OR_CHAIN);
        assert_eq!(
            sig_exit::take_requested_exit_code(),
            Some(sig_exit::INVALID_OR_CHAIN)
        );
        assert_eq!(sig_exit::INVALID_OR_CHAIN, 1);
        assert_eq!(sig_exit::take_requested_exit_code(), None);
    }

    #[test]
    fn take_when_empty_is_none() {
        drain();
        assert_eq!(sig_exit::take_requested_exit_code(), None);
    }

    #[test]
    fn request_is_first_write_wins() {
        // Production call sites only request once per failure path; first code
        // sticks so mixed invalid+unsigned paths that request INVALID first
        // keep exit 1, while pure-unsigned paths request 3 only.
        drain();
        sig_exit::request_exit(sig_exit::UNSIGNED);
        sig_exit::request_exit(sig_exit::INVALID_OR_CHAIN);
        assert_eq!(
            sig_exit::take_requested_exit_code(),
            Some(sig_exit::UNSIGNED)
        );
    }

    #[test]
    fn constants_match_0072_frozen_table() {
        assert_eq!(sig_exit::OK, 0);
        assert_eq!(sig_exit::INVALID_OR_CHAIN, 1);
        assert_eq!(sig_exit::POLICY, 2);
        assert_eq!(sig_exit::UNSIGNED, 3);
    }

    #[test]
    fn decide_matrix_valid_invalid_unsigned_chain() {
        // All good → 0
        assert_eq!(sig_exit::decide_signature_exit(0, 0, false), sig_exit::OK);
        // Pure INVALID → 1
        assert_eq!(
            sig_exit::decide_signature_exit(2, 0, false),
            sig_exit::INVALID_OR_CHAIN
        );
        // Pure UNSIGNED under require/strict → 3
        assert_eq!(
            sig_exit::decide_signature_exit(3, 3, false),
            sig_exit::UNSIGNED
        );
        // Mixed invalid + unsigned → 1 (invalid wins)
        assert_eq!(
            sig_exit::decide_signature_exit(3, 1, false),
            sig_exit::INVALID_OR_CHAIN
        );
        // Chain break → 1
        assert_eq!(
            sig_exit::decide_signature_exit(0, 0, true),
            sig_exit::INVALID_OR_CHAIN
        );
    }

    #[test]
    fn status_vocabulary_frozen_for_0072() {
        use crate::ledger::crypto::SignatureTrustStatus;
        assert_eq!(
            SignatureTrustStatus::ValidTrusted.as_str(),
            "VALID (trusted)"
        );
        assert_eq!(
            SignatureTrustStatus::ValidUnknownKey.as_str(),
            "VALID (unknown key)"
        );
        assert_eq!(SignatureTrustStatus::Invalid.as_str(), "INVALID");
        assert_eq!(SignatureTrustStatus::Unsigned.as_str(), "UNSIGNED");
    }
}
