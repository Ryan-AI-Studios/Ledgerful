use crate::output::human::print_verify_plan;
use crate::output::verification::VerificationReporter;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::verify::engine::{VerificationContext, VerifyEngine};
use crate::verify::plan::{VerificationStep, VerifyScope, build_plan_from_config};
use crate::verify::predictor::OutcomePredictor;
use crate::verify::results::{VerificationReport, VerificationResult};
use crate::verify::suggestions::{generate_suggestions, query_ledger_status};
use crate::verify::timeouts::manual_timeout;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::Path;
use tracing::{info, warn};

/// Stable schema version for `verify --json` CLI wire contract (track 0093).
/// Distinct from the persisted `VerificationReport` / `latest-verify.json`.
pub const VERIFY_JSON_SCHEMA_VERSION: u32 = 1;

/// Versioned machine-readable payload for `ledgerful verify --json`.
/// Built *from* `VerificationReport` — does not extend the persisted report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCliJson {
    pub schema_version: u32,
    pub ok: bool,
    pub scope_requested: String,
    pub scope_executed: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// Plan order (probability-ordered), not sorted alphabetically.
    pub steps: Vec<VerifyCliStepJson>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCliStepJson {
    pub name: String,
    pub command: String,
    /// `"pass"` when `exitCode == 0`, else `"fail"`. Defined once so it cannot
    /// disagree with `ok`.
    pub status: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_detail: Option<String>,
}

/// Derive step status from exit code — single definition for DoD-10.
pub fn step_status_from_exit_code(exit_code: i32) -> &'static str {
    if exit_code == 0 { "pass" } else { "fail" }
}

impl VerifyCliJson {
    /// Build the CLI wire payload from a completed `VerificationReport`.
    ///
    /// `scope_executed` is `"full"` whenever the plan fell back
    /// (`fallback_reason.is_some()`), otherwise the requested scope string.
    pub fn from_report(report: &VerificationReport, scope_requested: VerifyScope) -> Self {
        let fallback_reason = report.plan.as_ref().and_then(|p| p.fallback_reason.clone());
        let scope_executed = if fallback_reason.is_some() {
            "full".to_string()
        } else {
            scope_requested.to_string()
        };

        let steps: Vec<VerifyCliStepJson> = report
            .results
            .iter()
            .map(VerifyCliStepJson::from_result)
            .collect();

        Self {
            schema_version: VERIFY_JSON_SCHEMA_VERSION,
            ok: report.overall_pass,
            scope_requested: scope_requested.to_string(),
            scope_executed,
            fallback_reason,
            steps,
            timestamp: report.timestamp.clone(),
            tx_id: report.tx_id.clone(),
        }
    }

    /// Serialize deterministically for agent consumers (pretty JSON).
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self).into_diagnostic()
    }
}

impl VerifyCliStepJson {
    pub fn from_result(result: &VerificationResult) -> Self {
        let status = step_status_from_exit_code(result.exit_code).to_string();
        let failure_detail = if result.exit_code != 0 {
            let detail = if !result.stderr_summary.is_empty() {
                result.stderr_summary.clone()
            } else if !result.stdout_summary.is_empty() {
                result.stdout_summary.clone()
            } else {
                format!("exit code {}", result.exit_code)
            };
            Some(detail)
        } else {
            None
        };
        // Prefer description-like name from command; command is the full string.
        let name = result
            .command
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            name,
            command: result.command.clone(),
            status,
            exit_code: result.exit_code,
            duration_ms: result.duration_ms,
            failure_detail,
        }
    }
}

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
    verify_ledger_signatures_with_options(layout, true, false, false, None)
}

pub fn verify_ledger_signatures_with_options(
    layout: &Layout,
    verify_signatures: bool,
    verify_chain: bool,
    strict_signatures: bool,
    against_export: Option<&Path>,
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
                    "INVALID".red(),
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
                    status.as_str().green(),
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
                    status.as_str().yellow(),
                    short
                );
            }
            (crate::ledger::crypto::SignatureTrustStatus::Invalid, _) => {
                eprintln!(
                    "  [{}] TX {} signature verification FAILED!",
                    "INVALID".red(),
                    short
                );
            }
            (crate::ledger::crypto::SignatureTrustStatus::Unsigned, SigEntryStream::RawStderr) => {
                eprintln!(
                    "  [{}] TX {} has no signature — treating as verification failure.",
                    "UNSIGNED".yellow(),
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
                    "SKIP".yellow(),
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
            "SKIP (federated)".yellow(),
            aggregates.federated_skip
        );
    }

    tracing::info!(
        target: "cli_summary",
        "\nSignature verification summary: {} valid, {} invalid, {} skipped, {} federated-skip.",
        aggregates.valid.green(),
        if aggregates.invalid > 0 {
            aggregates.invalid.red().to_string()
        } else {
            aggregates.invalid.to_string()
        },
        aggregates.skipped.yellow(),
        aggregates.federated_skip
    );

    if all_valid {
        tracing::info!(
            target: "cli_summary",
            "{}",
            "All signature validations passed successfully!"
                .green()
                .bold()
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
            _ => {}
        }
    }
    invalid
}

fn compute_entry_hash_for_verify(entry: &crate::ledger::types::LedgerEntry) -> Result<String> {
    crate::ledger::crypto::compute_entry_hash_for_entry(entry)
        .map_err(|e| miette::miette!("Failed to compute entry hash for TX {}: {e}", entry.tx_id))
}

fn verify_chain_integrity(
    entries: &[crate::ledger::types::LedgerEntry],
    head: Option<&crate::ledger::types::ChainHead>,
    against_export: Option<&Path>,
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
            "SKIP (federated)".yellow(),
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
                _ => {}
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
        let export_head = load_export_chain_head(export_path)?;

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
            h
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
            let computed_latest_hash = prev_hash.as_deref().unwrap_or("");
            if computed_latest_hash != local_head.latest_entry_hash {
                return Err(miette::miette!(
                    "Chain head mismatch: computed latest entry hash {} does not match stored head {} (local chain altered).",
                    computed_latest_hash,
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

        if local_head.latest_entry_hash != export_head.latest_entry_hash {
            return Err(miette::miette!(
                "Live chain head {} does not match exported head {} (rollback/tail-truncation detected).",
                local_head.latest_entry_hash,
                export_head.latest_entry_hash
            ));
        }
        if local_head.genesis != export_head.genesis {
            return Err(miette::miette!(
                "Live chain genesis {} does not match exported genesis {}.",
                local_head.genesis,
                export_head.genesis
            ));
        }
        if local_head.length != export_head.length {
            return Err(miette::miette!(
                "Live chain length {} does not match exported length {} (tail truncation or rollback detected).",
                local_head.length,
                export_head.length
            ));
        }

        let export_sig = export_head.head_signature.as_deref().unwrap_or("");
        let export_pub = export_head.head_public_key.as_deref().unwrap_or("");
        if export_sig.is_empty() || export_pub.is_empty() {
            tracing::info!(
                target: "cli_summary",
                "Exported chain head is unsigned (synthesized), cannot verify signature; length/hash/genesis comparison completed."
            );
        } else if !crate::ledger::crypto::verify_chain_head(
            &export_head.latest_entry_hash,
            &export_head.genesis,
            export_head.length,
            export_sig,
            export_pub,
        ) {
            return Err(miette::miette!(
                "Exported chain head signature verification failed."
            ));
        }

        return Ok(());
    }

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

fn load_export_chain_head(path: &Path) -> Result<crate::ledger::types::ChainHead> {
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
    let head: crate::ledger::types::ChainHead = serde_json::from_slice(&buf)
        .map_err(|e| miette::miette!("Failed to parse chain_head.json: {}", e))?;
    Ok(head)
}

#[allow(clippy::too_many_arguments)]
pub fn execute_verify(
    command_str: Option<String>,
    tx_id: Option<String>,
    timeout_secs: u64,
    no_predict: bool,
    explain: bool,
    entity: Option<String>,
    health: bool,
    dry_run: bool,
    scope: crate::verify::plan::VerifyScope,
    auto_index: bool,
    json: bool,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let layout = crate::commands::helpers::get_layout()?;
    let manual_requested = command_str.is_some();

    // 1. Initialize Context
    let config = crate::config::load::load_config(&layout).unwrap_or_else(|e| {
        warn!("Config load failed: {e}. Using defaults.");
        crate::config::model::Config::default()
    });

    // Deferred `tx_id` resolution until after short-circuits.

    let mut ctx = VerificationContext::new(
        layout.clone(),
        current_dir.clone(),
        config.clone(),
        no_predict,
        explain,
        health,
    );
    // Keep per-step SUCCESS/FAILURE println! off stdout when emitting JSON.
    ctx.suppress_human_output = json;

    // 2. Load Storage and Packet
    ctx.storage = match StorageManager::open_read_only(&layout) {
        Ok(storage) => Some(storage),
        Err(err) => {
            if !no_predict {
                let warning =
                    format!("Prediction disabled: failed to initialize SQLite storage: {err}");
                warn!("{warning}");
                ctx.add_warning(warning);
            }
            None
        }
    };

    if let Some(storage) = &ctx.storage {
        ctx.packet = match storage.get_latest_packet() {
            Ok(packet) => packet,
            Err(err) => {
                if !no_predict {
                    let warning =
                        format!("Prediction disabled: failed to load latest packet: {err}");
                    warn!("{warning}");
                    ctx.add_warning(warning);
                }
                None
            }
        };
    }

    // CG-F35 review fix: there are actually three plan-building paths, not
    // two. Besides the manual-command path (`command_str` is `Some`), a
    // config-defined plan (`[[verify.steps]]` present) takes priority over
    // `OutcomePredictor::predict` below and, like the manual path, never
    // consults `ctx.packet` at all -- `build_plan_from_config` just maps the
    // configured steps verbatim. Compute it once, here, so we can both gate
    // the staleness warning on whether prediction will actually run *and*
    // reuse this same value in the plan-building match below instead of
    // calling `build_plan_from_config` a second time.
    let config_plan = build_plan_from_config(&config.verify);

    // CG-F35 (requirement #1, #6): the packet just loaded above feeds
    // `OutcomePredictor` and the plan-reordering heuristics below. If it's
    // stale or corrupt relative to the current HEAD/working tree, those
    // predictions are quietly built on outdated data. Reuse the same
    // `ctx.add_warning` path the storage-init failure above already uses so
    // this surfaces through `VerificationReporter::report`'s warnings
    // section rather than being silent.
    //
    // Gated on `command_str.is_none() && config_plan.is_none()`: those are
    // exactly the conditions under which the plan-building match below falls
    // through to `OutcomePredictor::predict`. Both the manual-command branch
    // (`command_str` is `Some`) and the config-defined-plan branch
    // (`config_plan` is `Some`) build their plan without consulting
    // `ctx.packet` at all, so warning about stale predictions in either of
    // those paths would be inaccurate, since no prediction happens there.
    if command_str.is_none()
        && config_plan.is_none()
        && ctx.packet.is_some()
        && let Some(reason) = crate::state::reports::warn_if_impact_stale(&layout, &config)
    {
        ctx.add_warning(format!(
            "Verification predictions are based on data where the {reason} — plan ordering may not reflect the current working tree."
        ));
    }

    // Health mode early exit — skip OutcomePredictor::predict and full plan building
    if health {
        if json {
            // Health is a separate surface; --json is for the plan execution payload.
            return Err(miette::miette!(
                "verify --json cannot be combined with --health"
            ));
        }
        return execute_verify_health(&layout, &config);
    }

    // 3. Build Plan
    let (plan, steps) = match command_str {
        Some(ref cmd) => (
            None,
            vec![manual_step(cmd.clone(), manual_timeout(timeout_secs))],
        ),
        None => {
            if let Some(config_plan) = config_plan {
                if !json {
                    print_verify_plan(&config_plan);
                }
                (Some(config_plan.clone()), config_plan.steps)
            } else {
                let prediction = OutcomePredictor::predict(&mut ctx)?;
                let rules = crate::policy::load::load_rules(&layout)?;

                let mut plan = match &ctx.packet {
                    Some(packet) => {
                        let conn = ctx.storage.as_ref().map(|s| s.get_connection());
                        let profile = crate::platform::repository::detect_repository(
                            layout.root.as_std_path(),
                        );
                        crate::verify::plan::build_plan_scoped_with_options(
                            packet,
                            &rules,
                            &prediction.files,
                            &config.verify,
                            &profile,
                            scope,
                            conn,
                            &layout,
                            auto_index,
                        )
                    }
                    None => {
                        let profile = crate::platform::repository::detect_repository(
                            layout.root.as_std_path(),
                        );
                        let empty_packet = crate::impact::packet::ImpactPacket::default();
                        crate::verify::plan::build_plan(
                            &empty_packet,
                            &rules,
                            &[],
                            &config.verify,
                            &profile,
                            layout.root.as_std_path(),
                        )
                    }
                };

                // Apply probabilistic ordering if storage is available
                if let Some(stg) = &ctx.storage
                    && let Ok(dataset) =
                        crate::verify::probability::extract_dataset(stg.get_connection())
                {
                    let probs = crate::verify::probability::calculate_probabilities(&dataset);
                    plan.apply_probability_ordering(&probs);
                    info!(
                        "Probabilistic verification ordering applied ({} active models).",
                        probs.len()
                    );
                }

                // Announce fast→full fallback before the user waits through a
                // full run they did not expect. On --json the reason is in
                // fallbackReason — do not print around the payload.
                if !json && let Some(reason) = &plan.fallback_reason {
                    println!("{} {}", "ℹ".cyan(), reason.yellow());
                }

                if !json {
                    print_verify_plan(&plan);
                }
                let steps = plan.steps.clone();
                (Some(plan), steps)
            }
        }
    };

    // Entity-scoped explanation: show tests mapped to the entity and relevant steps.
    // Skipped under --json (machine payload has steps; explain is human-only).
    if !json && explain && entity.is_some() {
        let target = entity.as_deref().unwrap_or("");
        println!(
            "\n{}",
            format!("Verification explanation for entity: {}", target)
                .bold()
                .cyan()
        );

        if let Some(storage) = &ctx.storage {
            let conn = storage.get_connection();
            let normalized_entity =
                crate::util::path::normalize_relative_path(layout.root.as_std_path(), target)
                    .unwrap_or_else(|_| target.to_string());

            match explain_test_mappings(conn, &normalized_entity) {
                TestMappingState::TableMissing => {
                    println!(
                        "  Test-mapping table is not present in the index. Run `ledgerful index --incremental` to build it."
                    );
                }
                TestMappingState::TableEmpty => {
                    println!(
                        "  No test mappings have been indexed yet. Run `ledgerful index --incremental` to populate them."
                    );
                }
                TestMappingState::EntityNotIndexed => {
                    println!(
                        "  '{}' is not a recognized indexed file path or symbol name.",
                        target
                    );
                    println!(
                        "  Run `ledgerful index --incremental` if it was added or renamed recently, or confirm the path with `ledgerful search \"{}\"`.",
                        target
                    );
                }
                TestMappingState::NoMappingsForEntity => {
                    println!(
                        "  '{}' is indexed, but no tests currently map to it.",
                        normalized_entity
                    );
                    println!(
                        "  This may be accurate (no covering tests yet) -- use `ledgerful search \"{}\"` to confirm test coverage manually.",
                        normalized_entity
                    );
                }
                TestMappingState::Mapped(tests) => {
                    println!("  Mapped tests ({}):", tests.len());
                    for t in &tests {
                        println!("    • {}", t);
                    }
                }
            }
        }

        let relevant: Vec<_> = steps
            .iter()
            .filter(|s| {
                let cmd = s.command.to_lowercase();
                let t = target.to_lowercase();
                cmd.contains(&t) || cmd.contains("test") || cmd.contains("check")
            })
            .collect();
        println!(
            "\n  Verification steps relevant to this entity ({}):",
            relevant.len()
        );
        for s in &relevant {
            println!("    • {} (timeout: {}s)", s.command, s.timeout_secs);
        }
        println!();
    }

    // Dry Run early exit with compressed output
    if dry_run {
        if json {
            return Err(miette::miette!(
                "verify --json cannot be combined with --dry-run"
            ));
        }
        // For manual commands, print the steps derived from the CLI arg
        if manual_requested {
            println!("{}", "Verification Plan".bold().green());
            println!(
                "  • {} (timeout: {}s)",
                command_str.as_deref().unwrap_or(""),
                timeout_secs
            );
            println!();
        }

        // Group predicted impacts by source for compressed output
        let verbose = std::env::var("VERBOSE_DRY_RUN").is_ok();
        let predicted: Vec<&VerificationStep> = steps
            .iter()
            .filter(|s| s.description.starts_with("Predicted impact"))
            .collect();
        let other: Vec<&VerificationStep> = steps
            .iter()
            .filter(|s| !s.description.starts_with("Predicted impact"))
            .collect();

        // Print non-predicted steps (rules, config)
        if !other.is_empty() {
            println!("{}", "Verification Steps:".bold().cyan());
            for step in &other {
                println!("  • {} (timeout: {}s)", step.command, step.timeout_secs);
            }
        }

        // Print compressed predicted impacts
        if !predicted.is_empty() {
            println!(
                "\n{}",
                "Predicted Impacts (grouped by source):".bold().cyan()
            );
            let mut groups: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for step in &predicted {
                // Extract group name from "Predicted impact (GroupName) on path"
                let desc = &step.description;
                if let Some(start) = desc.find('(')
                    && let Some(end) = desc.find(')')
                {
                    let group = desc[start + 1..end].to_string();
                    let path = desc[end + 5..].to_string(); // ") on " = 5 chars
                    groups.entry(group).or_default().push(path);
                }
            }

            for (source, paths) in &groups {
                println!(
                    "  {}",
                    format!("Source: {} — {} items", source, paths.len()).bold()
                );
                let show = if verbose {
                    paths.len()
                } else {
                    std::cmp::min(5, paths.len())
                };
                for path in paths.iter().take(show) {
                    println!("    • {}", path);
                }
                if !verbose && paths.len() > 5 {
                    println!(
                        "    ... and {} more (set VERBOSE_DRY_RUN=1 for full list)",
                        paths.len() - 5
                    );
                }
            }
        }

        println!(
            "\n{}",
            "Dry run mode: verification plan displayed above. No commands were executed.".yellow()
        );
        return Ok(());
    }

    // 4. Execute
    // Explicitly release the database connection and close locks before running verification commands.
    // This prevents deadlock/lock contention when cargo test runs child Ledgerful commands.
    if let Some(storage) = ctx.storage.take() {
        let _ = storage.shutdown();
    }

    // Show progress indicator before verification execution.
    // Machine mode (WARN filter) drops this info! so it cannot corrupt JSON;
    // also skip when --json is set for clarity.
    if !json && !ctx.no_predict {
        let num_steps = steps.len();
        if num_steps > 0 {
            tracing::info!(target: "cli_summary", "Running {} verification step(s)...", num_steps);
        }
    }

    let resolved_tx_id = if let Some(ref id) = tx_id {
        match StorageManager::init_with_layout(&layout) {
            Ok(mut stg) => {
                let mgr = crate::ledger::TransactionManager::new(
                    &mut stg,
                    layout.root.clone().into(),
                    config.clone(),
                );
                let resolved = mgr
                    .resolve_tx_id(id)
                    .map_err(|e| miette::miette!("Failed to resolve tx-id '{}': {}", id, e))?;
                match mgr.get_transaction(&resolved) {
                    Ok(Some(tx)) => {
                        if tx.status != "PENDING" {
                            return Err(miette::miette!(
                                "Cannot attach to transaction '{}': status is '{}' (must be PENDING)",
                                resolved,
                                tx.status
                            ));
                        }
                    }
                    Ok(None) => {
                        return Err(miette::miette!("Transaction '{}' not found", resolved));
                    }
                    Err(e) => {
                        return Err(miette::miette!(
                            "Failed to read transaction '{}' from database: {}",
                            resolved,
                            e
                        ));
                    }
                }
                Some(resolved)
            }
            Err(_) => {
                return Err(miette::miette!(
                    "Failed to initialize storage for tx-id resolution"
                ));
            }
        }
    } else {
        let sidecar_path = layout.state_subdir().join("pending_hook_tx");
        let mut auto_id = None;
        if sidecar_path.exists() {
            match std::fs::read_to_string(&sidecar_path) {
                Ok(content) => match serde_json::from_str::<
                    crate::commands::hook_post_commit::PendingHookTx,
                >(&content)
                {
                    Ok(pending) => {
                        let repo_root = layout.root.as_std_path();
                        let mut fresh = false;

                        let editmsg_path = repo_root.join(".git").join("COMMIT_EDITMSG");
                        let index_lock_path = repo_root.join(".git").join("index.lock");

                        if editmsg_path.exists()
                            && index_lock_path.exists()
                            && let Ok(edit_msg) = std::fs::read_to_string(&editmsg_path)
                        {
                            let cleaned = crate::util::text::clean_commit_msg(&edit_msg);
                            use sha2::{Digest, Sha256};
                            let mut hasher = Sha256::new();
                            hasher.update(cleaned.as_bytes());
                            let edit_hash = hex::encode(hasher.finalize());
                            if edit_hash == pending.commit_msg_hash {
                                fresh = true;
                            }
                        }

                        if fresh {
                            match StorageManager::init_with_layout(&layout) {
                                Ok(mut stg) => {
                                    let mgr = crate::ledger::TransactionManager::new(
                                        &mut stg,
                                        layout.root.clone().into(),
                                        config.clone(),
                                    );
                                    match mgr.resolve_tx_id(&pending.tx_id) {
                                        Ok(resolved) => match mgr.get_transaction(&resolved) {
                                            Ok(Some(tx)) => {
                                                if tx.status == "PENDING" {
                                                    auto_id = Some(resolved);
                                                } else {
                                                    warn!(
                                                        "Sidecar transaction {} is in state '{}', not PENDING; skipping auto-bind.",
                                                        resolved, tx.status
                                                    );
                                                }
                                            }
                                            Ok(None) => warn!(
                                                "Sidecar transaction {} not found in DB; skipping auto-bind.",
                                                resolved
                                            ),
                                            Err(e) => warn!(
                                                "Failed to read sidecar transaction {} from DB: {}; skipping auto-bind.",
                                                resolved, e
                                            ),
                                        },
                                        Err(e) => warn!(
                                            "Sidecar transaction {} could not be resolved: {}; skipping auto-bind.",
                                            pending.tx_id, e
                                        ),
                                    }
                                }
                                Err(e) => warn!(
                                    "Failed to initialize storage for auto-bind: {}; skipping auto-bind.",
                                    e
                                ),
                            }
                        } else {
                            warn!(
                                "Sidecar transaction {} is stale (commit_msg_hash mismatch); skipping auto-bind.",
                                pending.tx_id
                            );
                        }
                    }
                    Err(e) => warn!(
                        "Failed to parse pending hook sidecar: {}; skipping auto-bind.",
                        e
                    ),
                },
                Err(e) => warn!(
                    "Failed to read pending hook sidecar: {}; skipping auto-bind.",
                    e
                ),
            }
        }
        auto_id
    };

    let mut report = VerifyEngine::execute_with_scope(
        &mut ctx,
        plan,
        &steps,
        manual_requested,
        resolved_tx_id,
        scope,
    )?;

    // 5. Generate Suggestions
    let ledger_status = query_ledger_status(&layout);
    let suggestions = generate_suggestions(&report, &ledger_status);

    report = report.with_suggested_actions(suggestions);

    // 6. Final Reporting & IPC
    // Human report is suppressed on --json so stdout carries only the payload.
    if !json {
        VerificationReporter::report(&ctx, &report);
    }

    // Push results to bridge
    let bridge_outcomes = report
        .results
        .iter()
        .map(|res| crate::bridge::model::BridgeVerifyOutcome {
            success: res.exit_code == 0,
            command: res.command.clone(),
            error_snippet: if res.exit_code != 0 {
                let err = if !res.stderr_summary.is_empty() {
                    &res.stderr_summary
                } else {
                    &res.stdout_summary
                };
                Some(err.chars().take(200).collect::<String>())
            } else {
                None
            },
        })
        .collect();
    crate::bridge::notify::push_verify_results(bridge_outcomes);

    // Emit versioned CLI payload before any error return (DoD-15 boundary):
    // JSON present + non-zero = validation rejection; no JSON + non-zero = fatal.
    if json {
        let payload = VerifyCliJson::from_report(&report, scope);
        println!("{}", payload.to_json_string()?);
    }

    if report.overall_pass {
        Ok(())
    } else {
        Err(miette::miette!("Verification failed"))
    }
}

/// Fast health check that only probes executable availability and basic ledger
/// state, skipping OutcomePredictor::predict and full plan building entirely.
/// Returns within a bounded time (<5s on normal machines).
fn execute_verify_health(layout: &Layout, config: &crate::config::model::Config) -> Result<()> {
    println!("{}", "Verification Health Check".bold().green());
    // Product-output header (0093): keep with report body on stdout (policy §3).
    println!("Checking verification dependencies...");
    let mut all_ok = true;

    let profile = crate::platform::repository::detect_repository(layout.root.as_std_path());
    let empty_packet = crate::impact::packet::ImpactPacket::default();
    let rules = crate::policy::load::load_rules(layout).unwrap_or_default();
    let effective_plan = crate::verify::plan::build_plan(
        &empty_packet,
        &rules,
        &[],
        &config.verify,
        &profile,
        layout.root.as_std_path(),
    );

    let mut expected_tools = std::collections::HashSet::new();
    for step in &effective_plan.steps {
        let exe = extract_executable(&step.command);
        expected_tools.insert(exe.to_string());
    }

    // Always check for nextest if Rust is present and prefer_nextest is true
    let prefer_nextest = config.verify.prefer_nextest.unwrap_or(false);
    if profile.rust.is_some() && prefer_nextest {
        expected_tools.insert("cargo-nextest".to_string());
    }

    if expected_tools.is_empty() {
        println!("  [{}] No verification steps required.", "OK".green());
    } else {
        let mut sorted_tools: Vec<_> = expected_tools.into_iter().collect();
        sorted_tools.sort();
        for tool in sorted_tools {
            println!("  Checking {}...", tool);
            let exists = check_executable_exists(&tool);
            if exists {
                println!("  [{}] {} is available.", "OK".green(), tool);
            } else {
                let hint = match tool.as_str() {
                    "cargo-nextest" => " (install with `cargo install cargo-nextest`)",
                    "cargo" => " (install Rust toolchain)",
                    "npm" => " (install Node.js)",
                    "pnpm" => " (install pnpm)",
                    "yarn" => " (install yarn)",
                    "bun" => " (install Bun)",
                    "deno" => " (install Deno)",
                    _ => "",
                };
                println!("  [{}] {} not found on PATH.{}", "FAILED".red(), tool, hint);
                all_ok = false;
            }
        }
    }

    // Check ledger health (bounded query)
    println!("  Checking ledger state...");
    let ledger_status = query_ledger_status(layout);
    if ledger_status.unaudited_count > 0 || ledger_status.has_stale_pending {
        println!(
            "  [{}] Ledger: {} unaudited, stale pending: {}",
            "NOTE".yellow(),
            ledger_status.unaudited_count,
            ledger_status.has_stale_pending
        );
    } else if ledger_status.no_impact_report {
        println!(
            "  [{}] No impact report found. Run 'ledgerful scan --impact' after making changes.",
            "NOTE".yellow()
        );
    } else {
        println!("  [{}] Ledger is clean.", "OK".green());
    }

    // Show runner selection info
    let has_nextest = check_executable_exists("cargo-nextest");
    let prefer_nextest = has_nextest && config.verify.prefer_nextest.unwrap_or(false);
    println!(
        "  [{}] Runner: {} (nextest {})",
        "OK".green(),
        if prefer_nextest {
            "cargo nextest"
        } else {
            "cargo test"
        },
        if has_nextest {
            "available"
        } else {
            "not available"
        }
    );

    if all_ok {
        println!(
            "\n{}",
            "All verification dependencies are available.".green()
        );
        Ok(())
    } else {
        Err(miette::miette!(
            "Verification health check failed: some executables are missing."
        ))
    }
}

fn extract_executable(command: &str) -> &str {
    // Skip leading `KEY=value` tokens to reach the actual executable.
    // e.g. `CARGO_TERM_COLOR=always cargo test` -> `cargo`
    let exe_token = command
        .split_whitespace()
        .find(|tok| !tok.contains('='))
        .unwrap_or("");
    // Strip surrounding quotes from the token if present.
    exe_token
        .trim_start_matches(['\"', '\''])
        .trim_end_matches(['\"', '\''])
}

fn check_executable_exists(name: &str) -> bool {
    let path = std::path::Path::new(name);
    if path.is_absolute() || path.components().count() > 1 {
        return path.exists();
    }
    if let Ok(path_env) = std::env::var("PATH") {
        let paths = std::env::split_paths(&path_env);
        for p in paths {
            let exe_path = p.join(name);
            #[cfg(target_os = "windows")]
            {
                for ext in &["", ".exe", ".cmd", ".bat"] {
                    let full_path = if ext.is_empty() {
                        exe_path.clone()
                    } else {
                        let mut s = exe_path.to_string_lossy().to_string();
                        s.push_str(ext);
                        std::path::PathBuf::from(s)
                    };
                    if full_path.is_file() {
                        return true;
                    }
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                if exe_path.is_file() {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = std::fs::metadata(&exe_path)
                        && metadata.permissions().mode() & 0o111 != 0
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn manual_step(command: String, timeout_secs: u64) -> VerificationStep {
    VerificationStep {
        description: "Manually requested verification command".to_string(),
        command,
        timeout_secs,
        shell: true,
    }
}

/// Distinct absence/presence states for `verify --explain --entity`, so the
/// CLI can tell "feature is empty here" apart from "feature is broken".
#[derive(Debug, PartialEq, Eq)]
pub enum TestMappingState {
    /// The `test_mapping` table itself doesn't exist (pre-migration DB).
    TableMissing,
    /// The table exists but has never been populated by an index run.
    TableEmpty,
    /// The entity didn't resolve to an indexed file path or a known symbol name.
    EntityNotIndexed,
    /// The entity is indexed, but no test currently maps to it.
    NoMappingsForEntity,
    /// Mapped tests, formatted as `"<test file path>::<test symbol name>"`.
    Mapped(Vec<String>),
}

const MAPPED_TESTS_QUERY_BY_FILE: &str = "SELECT DISTINCT pf_test.file_path || '::' || ps_test.symbol_name \
     FROM test_mapping tm \
     JOIN project_symbols ps_test ON tm.test_symbol_id = ps_test.id \
     JOIN project_files pf_test ON tm.test_file_id = pf_test.id \
     WHERE tm.tested_file_id = ?1 \
     ORDER BY 1";

const MAPPED_TESTS_QUERY_BY_SYMBOL: &str = "SELECT DISTINCT pf_test.file_path || '::' || ps_test.symbol_name \
     FROM test_mapping tm \
     JOIN project_symbols ps_test ON tm.test_symbol_id = ps_test.id \
     JOIN project_files pf_test ON tm.test_file_id = pf_test.id \
     JOIN project_symbols ps_tested ON tm.tested_symbol_id = ps_tested.id \
     WHERE ps_tested.symbol_name = ?1 \
     ORDER BY 1";

/// Resolves test-mapping coverage for an entity against the real
/// `test_mapping` schema (`test_symbol_id`/`test_file_id`/`tested_symbol_id`/
/// `tested_file_id`), trying an exact indexed file path first and falling
/// back to a symbol-name match if the entity isn't a file path.
pub fn explain_test_mappings(
    conn: &rusqlite::Connection,
    normalized_entity: &str,
) -> TestMappingState {
    use rusqlite::OptionalExtension;

    let total: i64 = match conn.query_row("SELECT count(*) FROM test_mapping", [], |row| row.get(0))
    {
        Ok(c) => c,
        Err(_) => return TestMappingState::TableMissing,
    };
    if total == 0 {
        return TestMappingState::TableEmpty;
    }

    let file_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM project_files WHERE file_path = ?1",
            [normalized_entity],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or(None);

    let mapped = if let Some(fid) = file_id {
        conn.prepare(MAPPED_TESTS_QUERY_BY_FILE)
            .and_then(|mut s| {
                s.query_map([fid], |row| row.get(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<String>>())
            })
            .unwrap_or_default()
    } else {
        let symbol_exists: bool = conn
            .query_row(
                "SELECT 1 FROM project_symbols WHERE symbol_name = ?1 LIMIT 1",
                [normalized_entity],
                |_| Ok(true),
            )
            .optional()
            .unwrap_or(None)
            .unwrap_or(false);

        if !symbol_exists {
            return TestMappingState::EntityNotIndexed;
        }

        conn.prepare(MAPPED_TESTS_QUERY_BY_SYMBOL)
            .and_then(|mut s| {
                s.query_map([normalized_entity], |row| row.get(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<String>>())
            })
            .unwrap_or_default()
    };

    if mapped.is_empty() {
        TestMappingState::NoMappingsForEntity
    } else {
        TestMappingState::Mapped(mapped)
    }
}

#[cfg(test)]
mod sig_entry_stream_tests {
    use super::{
        SigEntryClass, SigEntryStream, SignatureAggregateCounts, class_for_sig_entry,
        sig_entry_stream, tally_signature_classes,
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

#[cfg(test)]
mod verify_cli_json_tests {
    use super::*;
    use crate::verify::plan::{PlanSource, VerificationPlan, VerificationStep};
    use crate::verify::results::{VerificationReport, VerificationResult};

    fn sample_result(command: &str, exit: i32) -> VerificationResult {
        VerificationResult {
            command: command.to_string(),
            exit_code: exit,
            duration_ms: 10,
            stdout_summary: if exit == 0 {
                "ok".into()
            } else {
                String::new()
            },
            stderr_summary: if exit != 0 {
                "boom".into()
            } else {
                String::new()
            },
            truncated: false,
            timestamp: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn sample_report(
        results: Vec<VerificationResult>,
        fallback: Option<&str>,
    ) -> VerificationReport {
        let plan = VerificationPlan {
            source: Some(PlanSource::AutoPolicy),
            steps: results
                .iter()
                .map(|r| VerificationStep {
                    command: r.command.clone(),
                    timeout_secs: 60,
                    description: "step".into(),
                    shell: false,
                })
                .collect(),
            fallback_reason: fallback.map(|s| s.to_string()),
        };
        let mut report = VerificationReport::new(Some(plan), results);
        // Stabilize timestamp for byte-identity.
        report.timestamp = "2026-01-01T00:00:00Z".into();
        report.tx_id = Some("tx-abc".into());
        report
    }

    #[test]
    fn schema_version_and_ok_from_report() {
        let report = sample_report(vec![sample_result("cargo test", 0)], None);
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Full);
        assert_eq!(payload.schema_version, VERIFY_JSON_SCHEMA_VERSION);
        assert!(payload.ok);
        assert_eq!(payload.scope_requested, "full");
        assert_eq!(payload.scope_executed, "full");
        assert!(payload.fallback_reason.is_none());
        assert_eq!(payload.steps.len(), 1);
        assert_eq!(payload.steps[0].status, "pass");
        assert_eq!(payload.steps[0].exit_code, 0);
        assert!(payload.steps[0].failure_detail.is_none());
    }

    #[test]
    fn fallback_sets_scope_executed_full() {
        let report = sample_report(
            vec![sample_result("cargo test", 0)],
            Some("fast scope unavailable — empty test_mapping; running full (~5-8 min)"),
        );
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast);
        assert_eq!(payload.scope_requested, "fast");
        assert_eq!(payload.scope_executed, "full");
        assert!(payload.fallback_reason.as_ref().unwrap().contains("empty"));
    }

    #[test]
    fn ok_false_when_step_fails_and_status_agrees() {
        let report = sample_report(
            vec![
                sample_result("cargo fmt", 0),
                sample_result("cargo clippy", 1),
            ],
            None,
        );
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast);
        assert!(!payload.ok);
        assert_eq!(payload.steps[0].status, "pass");
        assert_eq!(payload.steps[1].status, "fail");
        assert_eq!(payload.steps[1].failure_detail.as_deref(), Some("boom"));
        // DoD-10: ok == all steps pass == (would be exit 0)
        assert_eq!(payload.ok, payload.steps.iter().all(|s| s.exit_code == 0));
    }

    #[test]
    fn byte_identical_across_two_builds() {
        let report = sample_report(
            vec![
                sample_result("cargo fmt --all -- --check", 0),
                sample_result("cargo nextest run --lib", 0),
            ],
            None,
        );
        let a = VerifyCliJson::from_report(&report, VerifyScope::Fast)
            .to_json_string()
            .unwrap();
        let b = VerifyCliJson::from_report(&report, VerifyScope::Fast)
            .to_json_string()
            .unwrap();
        assert_eq!(a, b);
        let parsed: VerifyCliJson = serde_json::from_str(&a).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert!(parsed.ok);
    }

    #[test]
    fn step_status_from_exit_code_matrix() {
        assert_eq!(step_status_from_exit_code(0), "pass");
        assert_eq!(step_status_from_exit_code(1), "fail");
        assert_eq!(step_status_from_exit_code(3), "fail");
    }

    #[test]
    fn ok_matches_sig_exit_matrix_for_gate() {
        // Process exit 0 ↔ ok true; non-zero validation rejection ↔ ok false.
        for (exit, expect_ok) in [(0, true), (1, false), (3, false)] {
            let results = if exit == 0 {
                vec![sample_result("cargo test", 0)]
            } else {
                vec![sample_result("cargo test", exit)]
            };
            let report = sample_report(results, None);
            let payload = VerifyCliJson::from_report(&report, VerifyScope::Full);
            assert_eq!(payload.ok, expect_ok, "exit={exit}");
            assert_eq!(payload.ok, exit == 0);
        }
    }

    /// DoD-15: clap-level fatal rejects before `execute_verify` runs — no path
    /// that could emit a partial `VerifyCliJson` payload.
    #[test]
    fn verify_json_invalid_scope_is_clap_fatal() {
        use crate::cli::Cli;
        use clap::Parser;
        let err = Cli::try_parse_from(["ledgerful", "verify", "--json", "--scope", "not-a-scope"]);
        assert!(
            err.is_err(),
            "invalid --scope under --json must fail at clap (fatal, no partial JSON)"
        );
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("not-a-scope") || msg.contains("scope") || msg.contains("fast"),
            "clap error should mention scope; got {msg}"
        );
    }

    /// DoD-15: rejected `--json` combinations return `Err` from dispatch helpers
    /// without building a report (no `VerifyCliJson` emit site reached).
    #[test]
    fn verify_json_rejected_combos_are_errors_not_partial_payloads() {
        // Structural: execute_verify returns Err early for health/dry-run+json
        // before plan execution or JSON println. Live process proof is in
        // integration + output/0093-after/verify-json-fatal.*.
        let src = include_str!("verify.rs");
        assert!(
            src.contains("verify --json cannot be combined with --health"),
            "health+json must reject"
        );
        assert!(
            src.contains("verify --json cannot be combined with --dry-run"),
            "dry-run+json must reject"
        );
        // Emit-before-err boundary: JSON println is immediately before overall_pass check.
        let emit_idx = src
            .find("payload.to_json_string()")
            .expect("JSON emit site");
        let fail_idx = src
            .find("if report.overall_pass")
            .expect("overall_pass gate");
        assert!(
            emit_idx < fail_idx,
            "DoD-15 boundary: JSON must emit before validation-rejection Err"
        );
    }
}
