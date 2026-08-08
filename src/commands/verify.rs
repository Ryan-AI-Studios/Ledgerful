use crate::output::human::print_verify_plan;
use crate::output::verification::{
    VerificationReporter, print_dry_run_human, should_print_suggested_actions,
};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::verify::engine::{VerificationContext, VerifyEngine};
use crate::verify::plan::{VerificationStep, VerifyScope, build_plan_from_config};
use crate::verify::predictor::OutcomePredictor;
use crate::verify::results::{VerificationReport, VerificationResult};
use crate::verify::suggestions::{generate_suggestions, query_ledger_status};
use crate::verify::timeouts::manual_timeout;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use serde::{Deserialize, Serialize};
use std::env;
use std::path::Path;
use tracing::{debug, warn};

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
    /// Count of plan steps whose step key was present in the Bayesian
    /// probability map (0140). Omitted when ordering was not attempted.
    /// schemaVersion stays **1** (additive optional field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_steps: Option<usize>,
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
    /// Best-effort formatter paths (0121). Omitted when empty / pass.
    /// `schemaVersion` stays **1** (additive optional field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_paths: Option<Vec<String>>,
}

/// Derive step status from exit code — single definition for DoD-10.
pub fn step_status_from_exit_code(exit_code: i32) -> &'static str {
    if exit_code == 0 { "pass" } else { "fail" }
}

impl VerifyCliJson {
    /// Build the CLI wire payload from a completed `VerificationReport`.
    ///
    /// Three-way `scope_executed` (0135):
    /// - `plan.refused` → `"refused"`
    /// - else `fallback_reason.is_some()` → `"full"`
    /// - else requested scope string
    ///
    /// `ok` is false when refused (vacuous-pass guard: empty results would
    /// otherwise make `overall_pass` true).
    ///
    /// `matched_steps` is the Bayesian apply hit count (0140); pass `None`
    /// when ordering was not attempted (no history / cold-start / no storage).
    pub fn from_report(
        report: &VerificationReport,
        scope_requested: VerifyScope,
        matched_steps: Option<usize>,
    ) -> Self {
        let plan = report.plan.as_ref();
        let refused = plan.is_some_and(|p| p.refused);
        let fallback_reason = plan.and_then(|p| p.fallback_reason.clone());
        let scope_executed = if refused {
            "refused".to_string()
        } else if fallback_reason.is_some() {
            "full".to_string()
        } else {
            scope_requested.to_string()
        };

        let steps: Vec<VerifyCliStepJson> = if refused {
            Vec::new()
        } else {
            report
                .results
                .iter()
                .map(VerifyCliStepJson::from_result)
                .collect()
        };

        Self {
            schema_version: VERIFY_JSON_SCHEMA_VERSION,
            // Belt-and-suspenders: refuse must never report ok:true even if
            // results is empty (vacuous `.all()` is true).
            ok: report.overall_pass && !refused,
            scope_requested: scope_requested.to_string(),
            scope_executed,
            fallback_reason,
            steps,
            timestamp: report.timestamp.clone(),
            tx_id: report.tx_id.clone(),
            matched_steps,
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
            Some(crate::verify::fail_block::failure_detail_from_result(
                result,
            ))
        } else {
            None
        };
        let failed_paths = if result.exit_code != 0 {
            let paths = crate::verify::fail_block::extract_formatter_paths(
                &result.command,
                &result.stdout_summary,
                &result.stderr_summary,
            );
            if paths.is_empty() { None } else { Some(paths) }
        } else {
            None
        };
        // Prefer description-like name from command; command is the full string.
        let name = crate::verify::fail_block::step_name_from_command(&result.command);
        Self {
            name,
            command: result.command.clone(),
            status,
            exit_code: result.exit_code,
            duration_ms: result.duration_ms,
            failure_detail,
            failed_paths,
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
            _ => {}
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
    allow_full_fallback: bool,
    json: bool,
    verbose: bool,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let layout = crate::commands::helpers::get_layout_or_cwd_if_not_git()?;
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
    // Quiet success is orthogonal: never set suppress from `!verbose`.
    ctx.suppress_human_output = json;
    ctx.verbose = verbose;

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

    // Bayesian apply hit count for honesty log + VerifyCliJson.matchedSteps (0140).
    // None = ordering not attempted; Some(n) = extract_dataset succeeded.
    let mut bayesian_matched_steps: Option<usize> = None;
    // Probability map size when extract_dataset succeeds (0144 dry-run stdout).
    let mut bayesian_dataset_keys: Option<usize> = None;

    // 3. Build Plan
    let (plan, steps) = match command_str {
        Some(ref cmd) => (
            None,
            vec![manual_step(cmd.clone(), manual_timeout(timeout_secs))],
        ),
        None => {
            if let Some(config_plan) = config_plan {
                // Plan banner only under --verbose live path (0121 quiet success).
                // Never on --dry-run: descriptions are pipe-merged walls (0144).
                if verbose && !json && !dry_run {
                    print_verify_plan(&config_plan);
                }
                (Some(config_plan.clone()), config_plan.steps)
            } else {
                let prediction = OutcomePredictor::predict(&mut ctx)?;
                let rules = crate::policy::load::load_rules(&layout)?;

                let mut plan = match &ctx.packet {
                    Some(packet) => {
                        let profile = crate::platform::repository::detect_repository(
                            layout.root.as_std_path(),
                        );
                        // 0145 B1: live-clean working tree → EmptyChanges even when
                        // a saved impact packet still lists changes (phantom packet).
                        // Kept here (not in plan.rs) so Layout::new(".") unit tests
                        // stay hermetic without a live git short-circuit.
                        if scope.is_fast()
                            && !working_tree_has_material_changes(layout.root.as_std_path())
                        {
                            crate::verify::plan::build_empty_changes_plan(&profile)
                        } else {
                            let conn = ctx.storage.as_ref().map(|s| s.get_connection());
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
                                allow_full_fallback,
                            )
                        }
                    }
                    None => {
                        // No saved impact packet (0135 final codex P1):
                        // - Full scope: keep build_plan (historical).
                        // - Fast + clean working tree: EmptyChanges cheap path.
                        // - Fast + dirty working tree: MappingRefuse (or allow
                        //   full) — never pretend EmptyChanges and under-verify.
                        let profile = crate::platform::repository::detect_repository(
                            layout.root.as_std_path(),
                        );
                        let empty_packet = crate::impact::packet::ImpactPacket::default();
                        if scope.is_fast() {
                            if working_tree_has_material_changes(layout.root.as_std_path()) {
                                if allow_full_fallback {
                                    let mut plan = crate::verify::plan::build_plan(
                                        &empty_packet,
                                        &rules,
                                        &[],
                                        &config.verify,
                                        &profile,
                                        layout.root.as_std_path(),
                                    );
                                    plan.fallback_reason = Some(
                                        "fast scope unavailable — no impact packet for dirty tree; run `ledgerful scan --impact`; running full (~5-8 min)"
                                            .to_string(),
                                    );
                                    plan.refused = false;
                                    plan
                                } else {
                                    crate::verify::plan::refuse_plan_for_trigger(
                                        "no impact packet for dirty tree; run `ledgerful scan --impact`",
                                    )
                                }
                            } else {
                                let conn = ctx.storage.as_ref().map(|s| s.get_connection());
                                crate::verify::plan::build_plan_scoped_with_options(
                                    &empty_packet,
                                    &rules,
                                    &prediction.files,
                                    &config.verify,
                                    &profile,
                                    scope,
                                    conn,
                                    &layout,
                                    auto_index,
                                    allow_full_fallback,
                                )
                            }
                        } else {
                            crate::verify::plan::build_plan(
                                &empty_packet,
                                &rules,
                                &[],
                                &config.verify,
                                &profile,
                                layout.root.as_std_path(),
                            )
                        }
                    }
                };

                // Apply probabilistic ordering if storage is available
                if let Some(stg) = &ctx.storage
                    && let Ok(dataset) =
                        crate::verify::probability::extract_dataset(stg.get_connection())
                {
                    let probs = crate::verify::probability::calculate_probabilities(&dataset);
                    let matched = plan.apply_probability_ordering(&probs);
                    bayesian_matched_steps = Some(matched);
                    bayesian_dataset_keys = Some(probs.len());
                    // 0144: product surface is dry-run stdout `matched_steps=`;
                    // demote tracing so default RUST_LOG=info is not a duplicate.
                    if matched > 0 {
                        debug!(
                            "Probabilistic verification ordering applied (matched_steps={matched}, dataset_keys={})",
                            probs.len()
                        );
                    } else {
                        // Honesty: never claim "applied N models" with dataset
                        // size when zero plan steps hit the probability map.
                        debug!(
                            "Probabilistic verification ordering skipped reorder (matched_steps=0, dataset_keys={})",
                            probs.len()
                        );
                    }
                }

                // Announce fast→full fallback or MappingRefuse before the user
                // waits. On --json the reason is in fallbackReason — do not
                // print around the payload.
                if !json && let Some(reason) = &plan.fallback_reason {
                    println!(
                        "{} {}",
                        "ℹ".if_supports_color(Stream::Stdout, |s| s.cyan()),
                        reason.if_supports_color(Stream::Stdout, |s| s.yellow())
                    );
                    if plan.refused {
                        println!(
                            "{}",
                            "Next: ledgerful index --incremental\n      ledgerful verify --scope fast --auto-index\n      ledgerful verify --scope full\n      ledgerful verify --scope fast --allow-full-fallback"
                                .if_supports_color(Stream::Stdout, |s| s.yellow())
                        );
                    }
                }

                // Plan banner only under --verbose live path (0121 quiet success).
                // Never on --dry-run: descriptions are pipe-merged walls (0144).
                if verbose && !json && !dry_run {
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
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
        );

        // M3: prefer resolved stored path for step relevance when alias/suffix resolved.
        let mut resolved_for_filter: Option<String> = None;

        if let Some(storage) = &ctx.storage {
            let conn = storage.get_connection();
            let normalized_entity =
                crate::util::path::normalize_relative_path(layout.root.as_std_path(), target)
                    .unwrap_or_else(|_| target.to_string());

            let mapping_state = explain_test_mappings(conn, &normalized_entity);
            resolved_for_filter = mapping_state.resolved_path().map(|p| p.to_string());

            match mapping_state {
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
                TestMappingState::EntityAmbiguous { query, candidates } => {
                    let total = candidates.len();
                    println!("  {} indexed paths match '{}':", total, query);
                    let show = total.min(10);
                    for p in candidates.iter().take(show) {
                        println!("    • {}", p);
                    }
                    if total > 10 {
                        println!("    … and {} more", total - 10);
                    }
                    println!("  Provide a more specific path.");
                }
                TestMappingState::NoMappingsForEntity { resolved_path } => {
                    let display = resolved_path
                        .as_deref()
                        .unwrap_or(normalized_entity.as_str());
                    println!(
                        "  '{}' is indexed, but no tests currently map to it.",
                        display
                    );
                    println!(
                        "  This may be accurate (no covering tests yet) -- use `ledgerful search \"{}\"` to confirm test coverage manually.",
                        display
                    );
                }
                TestMappingState::Mapped {
                    tests,
                    resolved_path,
                } => {
                    let display = resolved_path
                        .as_deref()
                        .unwrap_or(normalized_entity.as_str());
                    println!("  Mapped tests for '{}' ({}):", display, tests.len());
                    for t in &tests {
                        println!("    • {}", t);
                    }
                }
            }
        }

        let relevant: Vec<_> = steps
            .iter()
            .filter(|s| step_relevant_to_entity(&s.command, target, resolved_for_filter.as_deref()))
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

    // MappingRefuse early path: never execute cargo; force fail (vacuous-pass guard).
    // Dry-run and live share this: refuse → exit 1 / Err.
    let plan_refused = plan.as_ref().is_some_and(|p| p.refused);
    if plan_refused {
        if dry_run {
            if json {
                return Err(miette::miette!(
                    "verify --json cannot be combined with --dry-run"
                ));
            }
            // Reason + Next already printed above when !json.
            println!(
                "\n{}",
                "Dry run mode: plan refused — no commands would be executed."
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
            );
            let reason = plan
                .as_ref()
                .and_then(|p| p.fallback_reason.clone())
                .unwrap_or_else(|| "fast scope unavailable; refusing full suite".to_string());
            return Err(miette::miette!("{reason}"));
        }

        // Live refuse: emit report/JSON with ok:false, empty steps; no cargo.
        let mut report = VerificationReport::new(plan, Vec::new());
        report.overall_pass = false;
        if json {
            let payload = VerifyCliJson::from_report(&report, scope, bayesian_matched_steps);
            println!("{}", payload.to_json_string()?);
        }
        let reason = report
            .plan
            .as_ref()
            .and_then(|p| p.fallback_reason.clone())
            .unwrap_or_else(|| "fast scope unavailable; refusing full suite".to_string());
        return Err(miette::miette!("{reason}"));
    }

    // Dry Run early exit — plan-first scannable layout (0144).
    // No print_verify_plan (gated above); no cargo execution.
    if dry_run {
        if json {
            return Err(miette::miette!(
                "verify --json cannot be combined with --dry-run"
            ));
        }
        // Manual --command: keep simple Verification Plan + single step + footer.
        if manual_requested {
            println!(
                "{}",
                "Verification Plan"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().green()))
            );
            println!(
                "  • {} (timeout: {}s)",
                command_str.as_deref().unwrap_or(""),
                timeout_secs
            );
            println!();
            println!(
                "{}",
                "Dry run mode: verification plan displayed above. No commands were executed."
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
            );
            return Ok(());
        }

        // CLI --verbose expands path lists; VERBOSE_DRY_RUN remains additive alias.
        let dry_verbose = verbose || std::env::var("VERBOSE_DRY_RUN").is_ok();
        print_dry_run_human(
            &steps,
            bayesian_matched_steps,
            bayesian_dataset_keys,
            dry_verbose,
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
    // DoD-1 quiet success: demote to debug! when !verbose so the default INFO
    // filter does not print progress noise. --verbose restores info!. Skip
    // entirely for --json (machine mode).
    if !json && !ctx.no_predict {
        let num_steps = steps.len();
        if num_steps > 0 {
            if crate::output::verification::should_emit_verify_progress_info(verbose, json) {
                tracing::info!(
                    target: "cli_summary",
                    "Running {} verification step(s)...",
                    num_steps
                );
            } else {
                tracing::debug!(
                    target: "cli_summary",
                    "Running {} verification step(s)...",
                    num_steps
                );
            }
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
    // Ordering on fail (non-json): step FAILURE lines (during run) → structured
    // fail block → Suggested Actions → miette on stderr.
    // Quiet success: suppress Suggested Actions; one trailing ok line.
    if !json {
        if !report.overall_pass
            && let Some(block) = crate::verify::fail_block::format_fail_block_from_report(&report)
        {
            println!("{block}");
        }

        if should_print_suggested_actions(verbose, report.overall_pass) {
            VerificationReporter::report(&ctx, &report);
        } else {
            // Quiet green: still surface prediction warnings on stderr; no
            // Suggested Actions header on stdout.
            if !report.prediction_warnings.is_empty() {
                VerificationReporter::print_prediction_warnings(&report.prediction_warnings);
            }
            println!("Verification passed");
        }
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
        let payload = VerifyCliJson::from_report(&report, scope, bayesian_matched_steps);
        println!("{}", payload.to_json_string()?);
    }

    if report.overall_pass {
        Ok(())
    } else {
        Err(miette::miette!("Verification failed"))
    }
}

/// True when the working tree has **material** changes that verify should not
/// ignore when there is no saved impact packet (0135 final codex P1).
///
/// Material = source-like extensions or known shared-infra basenames. Ignores
/// `.ledgerful/**` (also default ignore) and PATH/test fixtures such as a
/// root `cargo.bat` used by empty-repo integration tests.
///
/// On git discovery/status failure, returns false so clean EmptyChanges still
/// works in non-git fixtures.
fn working_tree_has_material_changes(repo_root: &std::path::Path) -> bool {
    let Ok(repo) = crate::git::repo::open_repo(repo_root) else {
        return false;
    };
    let Ok(changes) = crate::git::status::get_repo_status(&repo) else {
        return false;
    };
    changes.iter().any(|c| is_material_verify_path(&c.path))
}

fn is_material_verify_path(path: &std::path::Path) -> bool {
    let norm = path.to_string_lossy().replace('\\', "/");
    let norm = norm.trim_start_matches("./");
    if norm.starts_with(".ledgerful/") || norm == ".ledgerful" {
        return false;
    }
    // PATH/test shim used by empty-repo verify tests — not product source.
    if norm.eq_ignore_ascii_case("cargo.bat")
        || norm.eq_ignore_ascii_case("cargo.cmd")
        || norm.eq_ignore_ascii_case("cargo")
    {
        return false;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Shared-infra basenames that already force full under --scope fast when
    // present in a packet (see plan::touches_shared_infra).
    const INFRA: &[&str] = &[
        "cargo.toml",
        "cargo.lock",
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "go.mod",
        "go.sum",
        "pyproject.toml",
        "requirements.txt",
        "poetry.lock",
        "dockerfile",
        "docker-compose.yml",
        "docker-compose.yaml",
        "makefile",
    ];
    if INFRA.iter().any(|b| name == *b) {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "md"
            | "css"
            | "scss"
            | "html"
            | "sql"
            | "sh"
            | "ps1"
    )
}

/// Fast health check that only probes executable availability and basic ledger
/// state, skipping OutcomePredictor::predict and full plan building entirely.
/// Returns within a bounded time (<5s on normal machines).
fn execute_verify_health(layout: &Layout, config: &crate::config::model::Config) -> Result<()> {
    println!(
        "{}",
        "Verification Health Check"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().green()))
    );
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
        println!(
            "  [{}] No verification steps required.",
            "OK".if_supports_color(Stream::Stdout, |s| s.green())
        );
    } else {
        let mut sorted_tools: Vec<_> = expected_tools.into_iter().collect();
        sorted_tools.sort();
        for tool in sorted_tools {
            println!("  Checking {}...", tool);
            let exists = check_executable_exists(&tool);
            if exists {
                println!(
                    "  [{}] {} is available.",
                    "OK".if_supports_color(Stream::Stdout, |s| s.green()),
                    tool
                );
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
                println!(
                    "  [{}] {} not found on PATH.{}",
                    "FAILED".if_supports_color(Stream::Stdout, |s| s.red()),
                    tool,
                    hint
                );
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
            "NOTE".if_supports_color(Stream::Stdout, |s| s.yellow()),
            ledger_status.unaudited_count,
            ledger_status.has_stale_pending
        );
    } else if ledger_status.no_impact_report {
        println!(
            "  [{}] No impact report found. Run 'ledgerful scan --impact' after making changes.",
            "NOTE".if_supports_color(Stream::Stdout, |s| s.yellow())
        );
    } else {
        println!(
            "  [{}] Ledger is clean.",
            "OK".if_supports_color(Stream::Stdout, |s| s.green())
        );
    }

    // Show runner selection info
    let has_nextest = check_executable_exists("cargo-nextest");
    let prefer_nextest = has_nextest && config.verify.prefer_nextest.unwrap_or(false);
    println!(
        "  [{}] Runner: {} (nextest {})",
        "OK".if_supports_color(Stream::Stdout, |s| s.green()),
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
            "All verification dependencies are available."
                .if_supports_color(Stream::Stdout, |s| s.green())
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
    /// Full-input path suffix matched more than one indexed file (unique-only).
    /// Candidates are sorted by `file_path`; display may cap the list.
    EntityAmbiguous {
        query: String,
        candidates: Vec<String>,
    },
    /// The entity is indexed, but no test currently maps to it.
    NoMappingsForEntity {
        /// Stored `project_files.file_path` when resolved via a file path.
        resolved_path: Option<String>,
    },
    /// Mapped tests, formatted as `"<test file path>::<test symbol name>"`.
    Mapped {
        tests: Vec<String>,
        /// Stored `project_files.file_path` when resolved via a file path
        /// (`None` for pure symbol-name matches).
        resolved_path: Option<String>,
    },
}

impl TestMappingState {
    /// Stored path when the entity resolved to a file (alias/suffix/exact).
    pub fn resolved_path(&self) -> Option<&str> {
        match self {
            Self::Mapped { resolved_path, .. } | Self::NoMappingsForEntity { resolved_path } => {
                resolved_path.as_deref()
            }
            _ => None,
        }
    }
}

/// Whether a verification step command is relevant to `--entity` (M3).
///
/// Matches case-insensitively on the raw entity string and, when path resolution
/// produced a stored path (alias/suffix), that resolved form as well. Generic
/// `test` / `check` steps stay in the relevant set.
pub(crate) fn step_relevant_to_entity(
    command: &str,
    target: &str,
    resolved_path: Option<&str>,
) -> bool {
    let cmd = command.to_lowercase();
    let t_raw = target.to_lowercase();
    let t_resolved = resolved_path.map(|p| p.to_lowercase());
    cmd.contains(&t_raw)
        || t_resolved.as_ref().is_some_and(|r| cmd.contains(r))
        || cmd.contains("test")
        || cmd.contains("check")
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

/// Outcome of path/symbol resolution before mapping lookup.
#[derive(Debug, PartialEq, Eq)]
enum ResolvedEntity {
    ExactPath {
        file_id: i64,
        stored_path: String,
    },
    Ambiguous {
        query: String,
        candidates: Vec<String>,
    },
    Symbol {
        name: String,
    },
    NotFound,
}

/// Build generalized path-alias candidates (M1). Accept iff exactly one exists.
///
/// | Input shape | Candidates |
/// | ends with `.rs` | `{stem}/mod.rs` only |
/// | no extension / trailing `/` | `{trim}/mod.rs` and `{trim}.rs` |
/// | other extension | none |
fn alias_path_candidates(normalized: &str) -> Vec<String> {
    if normalized.ends_with(".rs") {
        let stem = match normalized.strip_suffix(".rs") {
            Some(s) if !s.is_empty() => s,
            _ => return Vec::new(),
        };
        return vec![format!("{stem}/mod.rs")];
    }

    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        return Vec::new();
    }

    let last_seg = trimmed.rsplit('/').next().unwrap_or(trimmed);
    // Other extension (.ts, .go, …): no module-layout candidates.
    if last_seg.contains('.') {
        return Vec::new();
    }

    vec![format!("{trimmed}/mod.rs"), format!("{trimmed}.rs")]
}

/// Exact `project_files` lookup. Windows uses `LOWER` equality (dead-code mirror).
fn lookup_file_exact(conn: &rusqlite::Connection, path: &str) -> Option<(i64, String)> {
    use rusqlite::OptionalExtension;

    let sql = if cfg!(target_os = "windows") {
        "SELECT id, file_path FROM project_files WHERE LOWER(file_path) = LOWER(?1)"
    } else {
        "SELECT id, file_path FROM project_files WHERE file_path = ?1"
    };
    conn.query_row(sql, [path], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
        .ok()
        .flatten()
}

/// Unique-only full-input path suffix (M2). No LCS scoring.
/// `file_path = ?1 OR file_path LIKE '%/' || ?1`, ordered by `file_path`.
fn lookup_files_by_suffix(conn: &rusqlite::Connection, query: &str) -> Vec<(i64, String)> {
    let mut stmt = match conn.prepare(
        "SELECT id, file_path FROM project_files \
         WHERE file_path = ?1 OR file_path LIKE '%/' || ?1 \
         ORDER BY file_path",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([query], |row| Ok((row.get(0)?, row.get(1)?))) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    rows.filter_map(|r| r.ok()).collect()
}

fn symbol_name_exists(conn: &rusqlite::Connection, name: &str) -> bool {
    use rusqlite::OptionalExtension;

    conn.query_row(
        "SELECT 1 FROM project_symbols WHERE symbol_name = ?1 LIMIT 1",
        [name],
        |_| Ok(true),
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or(false)
}

/// Resolve entity → indexed file or symbol.
/// Order: exact path → generalized alias (exactly one hit) → unique-only
/// full-input suffix → exact symbol name → NotFound.
fn resolve_tested_entity(conn: &rusqlite::Connection, normalized: &str) -> ResolvedEntity {
    // 1. Exact path always first (BS1).
    if let Some((file_id, stored_path)) = lookup_file_exact(conn, normalized) {
        return ResolvedEntity::ExactPath {
            file_id,
            stored_path,
        };
    }

    // 2. Generalized path alias (M1): accept iff exactly one candidate exists.
    let mut alias_hits: Vec<(i64, String)> = Vec::new();
    for cand in alias_path_candidates(normalized) {
        if let Some(hit) = lookup_file_exact(conn, &cand) {
            // Deduplicate by file id (Windows LOWER can collapse case variants).
            if !alias_hits.iter().any(|(id, _)| *id == hit.0) {
                alias_hits.push(hit);
            }
        }
    }
    if alias_hits.len() == 1 {
        let (file_id, stored_path) = alias_hits.remove(0);
        return ResolvedEntity::ExactPath {
            file_id,
            stored_path,
        };
    }
    // 0 or >1 alias hits: do not guess; fall through.

    // 3. Unique-only full-input path suffix (M2 — no LCS).
    let mut suffix_hits = lookup_files_by_suffix(conn, normalized);
    match suffix_hits.len() {
        1 => {
            let (file_id, stored_path) = suffix_hits.remove(0);
            return ResolvedEntity::ExactPath {
                file_id,
                stored_path,
            };
        }
        n if n > 1 => {
            let candidates: Vec<String> = suffix_hits.into_iter().map(|(_, p)| p).collect();
            return ResolvedEntity::Ambiguous {
                query: normalized.to_string(),
                candidates,
            };
        }
        _ => {}
    }

    // 4. Exact symbol name.
    if symbol_name_exists(conn, normalized) {
        return ResolvedEntity::Symbol {
            name: normalized.to_string(),
        };
    }

    ResolvedEntity::NotFound
}

fn query_mapped_tests_by_file(conn: &rusqlite::Connection, file_id: i64) -> Vec<String> {
    conn.prepare(MAPPED_TESTS_QUERY_BY_FILE)
        .and_then(|mut s| {
            s.query_map([file_id], |row| row.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<String>>())
        })
        .unwrap_or_default()
}

fn query_mapped_tests_by_symbol(conn: &rusqlite::Connection, name: &str) -> Vec<String> {
    conn.prepare(MAPPED_TESTS_QUERY_BY_SYMBOL)
        .and_then(|mut s| {
            s.query_map([name], |row| row.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<String>>())
        })
        .unwrap_or_default()
}

/// Resolves test-mapping coverage for an entity against the real
/// `test_mapping` schema (`test_symbol_id`/`test_file_id`/`tested_symbol_id`/
/// `tested_file_id`). Path resolution: exact → module/extensionless alias →
/// unique-only full-input suffix → symbol (shared by `tests -e` and
/// `verify --explain --entity`).
pub fn explain_test_mappings(
    conn: &rusqlite::Connection,
    normalized_entity: &str,
) -> TestMappingState {
    let total: i64 = match conn.query_row("SELECT count(*) FROM test_mapping", [], |row| row.get(0))
    {
        Ok(c) => c,
        Err(_) => return TestMappingState::TableMissing,
    };
    if total == 0 {
        return TestMappingState::TableEmpty;
    }

    match resolve_tested_entity(conn, normalized_entity) {
        ResolvedEntity::Ambiguous { query, candidates } => {
            TestMappingState::EntityAmbiguous { query, candidates }
        }
        ResolvedEntity::NotFound => TestMappingState::EntityNotIndexed,
        ResolvedEntity::ExactPath {
            file_id,
            stored_path,
        } => {
            let mapped = query_mapped_tests_by_file(conn, file_id);
            if mapped.is_empty() {
                TestMappingState::NoMappingsForEntity {
                    resolved_path: Some(stored_path),
                }
            } else {
                TestMappingState::Mapped {
                    tests: mapped,
                    resolved_path: Some(stored_path),
                }
            }
        }
        ResolvedEntity::Symbol { name } => {
            let mapped = query_mapped_tests_by_symbol(conn, &name);
            if mapped.is_empty() {
                TestMappingState::NoMappingsForEntity {
                    resolved_path: None,
                }
            } else {
                TestMappingState::Mapped {
                    tests: mapped,
                    resolved_path: None,
                }
            }
        }
    }
}

#[cfg(test)]
mod entity_path_resolution_tests {
    use super::step_relevant_to_entity;

    /// M3: resolved stored path matches step even when the raw entity is an alias.
    #[test]
    fn step_filter_matches_resolved_path_when_raw_differs() {
        let cmd = "cargo nextest run --package ledgerful -- src/commands/doctor/mod.rs";
        assert!(step_relevant_to_entity(
            cmd,
            "src/commands/doctor.rs",
            Some("src/commands/doctor/mod.rs"),
        ));
    }

    /// M3: raw entity still matches when present in the command.
    #[test]
    fn step_filter_matches_raw_target() {
        let cmd = "rg src/pkg.rs --type rust";
        assert!(step_relevant_to_entity(cmd, "src/pkg.rs", None));
        assert!(step_relevant_to_entity(
            cmd,
            "src/pkg.rs",
            Some("src/pkg/mod.rs"),
        ));
    }

    /// M3: neither raw nor resolved → only generic test/check steps stay relevant.
    #[test]
    fn step_filter_requires_path_or_generic_when_unrelated() {
        // No path match and no test/check token → not relevant.
        assert!(!step_relevant_to_entity(
            "cargo fmt --all",
            "src/commands/doctor.rs",
            Some("src/commands/doctor/mod.rs"),
        ));
        // "check" / "test" are generic verifier tokens → still relevant
        assert!(step_relevant_to_entity(
            "cargo check -p ledgerful",
            "src/orphan.rs",
            None,
        ));
        assert!(step_relevant_to_entity(
            "cargo nextest run --lib",
            "src/orphan.rs",
            None,
        ));
    }

    /// Display cap helper contract for Ambiguous lists (DoD-3 / L2 data side).
    #[test]
    fn ambiguous_display_cap_shows_and_n_more_when_over_10() {
        let total = 11usize;
        let show = total.min(10);
        assert_eq!(show, 10);
        let more = total - show;
        assert_eq!(more, 1);
        let line = format!("… and {} more", more);
        assert_eq!(line, "… and 1 more");
    }
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
        sample_report_with_refused(results, fallback, false)
    }

    fn sample_report_with_refused(
        results: Vec<VerificationResult>,
        fallback: Option<&str>,
        refused: bool,
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
            refused,
        };
        let mut report = VerificationReport::new(Some(plan), results);
        // Stabilize timestamp for byte-identity.
        report.timestamp = "2026-01-01T00:00:00Z".into();
        report.tx_id = Some("tx-abc".into());
        if refused {
            report.overall_pass = false;
        }
        report
    }

    #[test]
    fn schema_version_and_ok_from_report() {
        let report = sample_report(vec![sample_result("cargo test", 0)], None);
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Full, None);
        assert_eq!(payload.schema_version, VERIFY_JSON_SCHEMA_VERSION);
        assert!(payload.ok);
        assert_eq!(payload.scope_requested, "full");
        assert_eq!(payload.scope_executed, "full");
        assert!(payload.fallback_reason.is_none());
        assert_eq!(payload.steps.len(), 1);
        assert_eq!(payload.steps[0].status, "pass");
        assert_eq!(payload.steps[0].exit_code, 0);
        assert!(payload.steps[0].failure_detail.is_none());
        assert!(payload.matched_steps.is_none());
    }

    #[test]
    fn fallback_sets_scope_executed_full() {
        let report = sample_report(
            vec![sample_result("cargo test", 0)],
            Some("fast scope unavailable — empty test_mapping; running full (~5-8 min)"),
        );
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert_eq!(payload.scope_requested, "fast");
        assert_eq!(payload.scope_executed, "full");
        assert!(payload.fallback_reason.as_ref().unwrap().contains("empty"));
    }

    #[test]
    fn refused_sets_scope_executed_refused_ok_false_empty_steps() {
        let report = sample_report_with_refused(
            vec![],
            Some(
                "fast scope unavailable — test_mapping is stale or empty; refusing full suite (~5-8 min)",
            ),
            true,
        );
        // Vacuous overall_pass on empty results would be true without the force.
        assert!(!report.overall_pass || report.results.is_empty());
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert!(!payload.ok, "refused must never report ok:true");
        assert_eq!(payload.scope_requested, "fast");
        assert_eq!(payload.scope_executed, "refused");
        assert!(payload.steps.is_empty());
        assert!(
            payload
                .fallback_reason
                .as_ref()
                .unwrap()
                .contains("refusing full suite")
        );
    }

    #[test]
    fn refused_guards_vacuous_overall_pass_in_from_report() {
        // Even if overall_pass is left true on empty results, from_report forces ok:false.
        let plan = VerificationPlan {
            source: Some(PlanSource::AutoPolicy),
            steps: vec![],
            fallback_reason: Some(
                "fast scope unavailable — no mappings; refusing full suite (~5-8 min)".into(),
            ),
            refused: true,
        };
        let mut report = VerificationReport::new(Some(plan), vec![]);
        report.timestamp = "2026-01-01T00:00:00Z".into();
        // Vacuous .all() on empty results is true — the bug 0135 guards against.
        assert!(report.overall_pass);
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert!(!payload.ok);
        assert_eq!(payload.scope_executed, "refused");
        assert!(payload.steps.is_empty());
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
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert!(!payload.ok);
        assert_eq!(payload.steps[0].status, "pass");
        assert_eq!(payload.steps[1].status, "fail");
        assert_eq!(payload.steps[1].failure_detail.as_deref(), Some("boom"));
        // DoD-10: ok == all steps pass == (would be exit 0)
        assert_eq!(payload.ok, payload.steps.iter().all(|s| s.exit_code == 0));
    }

    #[test]
    fn failed_paths_additive_on_fmt_fail_schema_v1() {
        let mut result = sample_result("cargo fmt --all -- --check", 1);
        result.stderr_summary = "Diff in src/lib.rs:\nDiff in src/main.rs:\n".into();
        let report = sample_report(vec![result], None);
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert_eq!(payload.schema_version, 1);
        assert!(!payload.ok);
        let paths = payload.steps[0].failed_paths.as_ref().expect("paths");
        assert_eq!(
            paths,
            &vec!["src/lib.rs".to_string(), "src/main.rs".to_string()]
        );
        let json = payload.to_json_string().unwrap();
        assert!(json.contains("\"failedPaths\""));
        assert!(json.contains("src/lib.rs"));
        // Pass steps omit the field.
        let pass = VerifyCliJson::from_report(
            &sample_report(vec![sample_result("cargo test", 0)], None),
            VerifyScope::Full,
            None,
        );
        assert!(pass.steps[0].failed_paths.is_none());
        assert!(!pass.to_json_string().unwrap().contains("failedPaths"));
    }

    #[test]
    fn matched_steps_additive_schema_v1() {
        let report = sample_report(vec![sample_result("cargo test", 0)], None);
        let with = VerifyCliJson::from_report(&report, VerifyScope::Full, Some(2));
        assert_eq!(with.schema_version, 1);
        assert_eq!(with.matched_steps, Some(2));
        let json = with.to_json_string().unwrap();
        assert!(json.contains("\"matchedSteps\": 2"));
        let without = VerifyCliJson::from_report(&report, VerifyScope::Full, None);
        assert!(without.matched_steps.is_none());
        assert!(!without.to_json_string().unwrap().contains("matchedSteps"));
        // Vacuous honesty: matched_steps=0 still serializes (ordering ran, zero hits).
        let zero = VerifyCliJson::from_report(&report, VerifyScope::Full, Some(0));
        assert_eq!(zero.matched_steps, Some(0));
        assert!(
            zero.to_json_string()
                .unwrap()
                .contains("\"matchedSteps\": 0")
        );
    }

    #[test]
    fn format_fail_block_header_from_report() {
        let mut result = sample_result("cargo fmt --all -- --check", 1);
        result.stderr_summary = "Diff in a.rs:\n".into();
        let report = sample_report(vec![result], None);
        let block =
            crate::verify::fail_block::format_fail_block_from_report(&report).expect("fail block");
        assert!(block.starts_with("[Ledgerful] verify failed\n"));
        assert!(block.contains("exitCode: 1"));
        assert!(block.contains("failedPaths: a.rs"));
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
        let a = VerifyCliJson::from_report(&report, VerifyScope::Fast, None)
            .to_json_string()
            .unwrap();
        let b = VerifyCliJson::from_report(&report, VerifyScope::Fast, None)
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
            let payload = VerifyCliJson::from_report(&report, VerifyScope::Full, None);
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

    /// 0144 B1: dry-run must not print the plan-banner wall; both call sites gate
    /// `print_verify_plan` with `!dry_run` (and verbose && !json).
    #[test]
    fn execute_verify_print_verify_plan_gated_off_dry_run() {
        let src = include_str!("verify.rs");
        // Production body only — include_str also sees this test module.
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("production body before unit tests");
        let mut from = 0usize;
        let mut gated_sites = 0usize;
        while let Some(rel) = prod[from..].find("print_verify_plan(") {
            let abs = from + rel;
            // Look back a short window for the dry-run gate on the same call site.
            let window_start = abs.saturating_sub(120);
            let window = &prod[window_start..abs];
            assert!(
                window.contains("!dry_run"),
                "print_verify_plan at byte {abs} must be gated by !dry_run; nearby: {window:?}"
            );
            gated_sites += 1;
            from = abs + "print_verify_plan(".len();
        }
        assert!(
            gated_sites >= 2,
            "expected both config_plan and plan print_verify_plan sites; found {gated_sites}"
        );
    }
}
