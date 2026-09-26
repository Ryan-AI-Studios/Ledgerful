//! Pure signature-path helpers (0438).
//!
//! Classification, tally, exit-code decision, and invalid-entry enumerate.
//! Colored CLI formatters and the `verify_ledger_signatures*` orchestrators
//! stay in `crate::commands::verify`.

use crate::ledger::crypto::SignatureTrustStatus;
use crate::ledger::types::LedgerEntry;

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
pub fn sig_entry_stream(status: SignatureTrustStatus, signing_required: bool) -> SigEntryStream {
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
    status: SignatureTrustStatus,
    signing_required: bool,
    policy_invalid: bool,
) -> SigEntryClass {
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
pub mod sig_exit {
    /// All signed rows valid; no hard policy failure.
    pub const OK: i32 = 0;
    /// INVALID signature, wrong version, entity_normalized mismatch, or chain break.
    pub const INVALID_OR_CHAIN: i32 = 1;
    /// Policy: crypto-valid unknown key when trusted keys are required (reserved).
    pub const POLICY: i32 = 2;
    /// Unsigned present under require_signing or --strict-signatures.
    pub const UNSIGNED: i32 = 3;

    /// Pure exit-code decision for the signature path (0072 DoD-4 matrix).
    ///
    /// - `invalid_count` = rows that fail crypto / min_sig_version / consistency
    /// - `unsigned_fail` = unsigned rows counted only when signing is required
    /// - Chain breaks are reported via `invalid_count`-style path with exit 1
    ///   (callers set `chain_break=true`).
    ///
    /// Never returns [`POLICY`]. That const is kept for 0072 table completeness;
    /// policy exits are requested by the command orchestrator.
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

pub fn enumerate_invalid_ledger_entries(
    entries: &[LedgerEntry],
    signing_required: bool,
) -> Vec<(String, String, String)> {
    enumerate_invalid_ledger_entries_with_policy(entries, signing_required, &[], 1)
}

pub fn enumerate_invalid_ledger_entries_with_policy(
    entries: &[LedgerEntry],
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
            SignatureTrustStatus::Invalid => {
                invalid.push((
                    entry.tx_id.clone(),
                    entry.signature.clone().unwrap_or_default(),
                    entry.public_key.clone().unwrap_or_default(),
                ));
            }
            SignatureTrustStatus::Unsigned if signing_required => {
                invalid.push((entry.tx_id.clone(), String::new(), String::new()));
            }
            SignatureTrustStatus::ValidTrusted
            | SignatureTrustStatus::ValidUnknownKey
            | SignatureTrustStatus::Unsigned => {}
        }
    }
    invalid
}

#[cfg(test)]
mod sig_entry_stream_tests {
    use super::{
        SigEntryClass, SigEntryStream, class_for_sig_entry, sig_entry_stream,
        tally_signature_classes,
    };
    use crate::ledger::crypto::SignatureTrustStatus;

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

    /// DoD-2 structural safety: INVALID / required-UNSIGNED always route to
    /// RawStderr (eprintln!), so no verbosity filter can suppress them.
    #[test]
    fn hard_failures_always_raw_stderr_across_verbosity_levels() {
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
        assert!(!sig_entry_stream(SignatureTrustStatus::ValidTrusted, false).is_raw_stderr());
        assert!(!sig_entry_stream(SignatureTrustStatus::ValidUnknownKey, false).is_raw_stderr());
        assert!(!sig_entry_stream(SignatureTrustStatus::Unsigned, false).is_raw_stderr());
    }
}

#[cfg(test)]
mod sig_exit_tests {
    use super::sig_exit;

    #[test]
    fn constants_match_0072_frozen_table() {
        assert_eq!(sig_exit::OK, 0);
        assert_eq!(sig_exit::INVALID_OR_CHAIN, 1);
        assert_eq!(sig_exit::POLICY, 2);
        assert_eq!(sig_exit::UNSIGNED, 3);
    }

    #[test]
    fn decide_matrix_valid_invalid_unsigned_chain() {
        assert_eq!(sig_exit::decide_signature_exit(0, 0, false), sig_exit::OK);
        assert_eq!(
            sig_exit::decide_signature_exit(2, 0, false),
            sig_exit::INVALID_OR_CHAIN
        );
        assert_eq!(
            sig_exit::decide_signature_exit(3, 3, false),
            sig_exit::UNSIGNED
        );
        assert_eq!(
            sig_exit::decide_signature_exit(3, 1, false),
            sig_exit::INVALID_OR_CHAIN
        );
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
