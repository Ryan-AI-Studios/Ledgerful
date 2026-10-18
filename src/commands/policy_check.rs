//! CI policy gate: `ledgerful policy check` (track 0049).
//!
//! Evaluates a flat, named-rule policy against PR/diff/ledger state.
//! Distinct from `crate::policy` (tech-stack `rules.toml` matching).
//!
//! Offline and deterministic. No network code.

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::commands::scan::{files_changed_between, parse_pr_range};
use crate::commands::scan_pr::{HistoryEnrichment, PrRiskLevel, PrScanContext, PrScanReport};
use crate::commands::verify::enumerate_invalid_ledger_entries;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::ledger::types::EntryType;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use crate::config::policy_file::{
    PolicyConfig, PolicyMode, PolicyRules, PolicySource, RiskThreshold, entity_covers_path,
    is_adr_document_path, load_policy_from_git, parse_policy_toml,
};
use crate::config::policy_file::{file_changes_to_paths, normalize_repo_path, resolve_policy};

/// Stable schema version for `PolicyCheckReport`. Breaking changes bump this.
pub const POLICY_CHECK_SCHEMA_VERSION: u32 = 1;

/// Idle `verification_must_pass` note (no bound run; not a current-change fail).
pub const VERIFICATION_MUST_PASS_IDLE_NOTE: &str = "verification_must_pass: idle evaluation target; no bound verification run (unbound verifies do not satisfy). Not a current-change fail.";

/// Human Result when synthesized defaults are idle — not a merge-gate pass (0292).
pub const SYNTHESIZED_IDLE_RESULT: &str = "IDLE (synthesized; not a merge gate)";

/// Human footer when synthesized defaults are idle (0292). Uncolored; not the green pass line.
pub const SYNTHESIZED_IDLE_FOOTER: &str =
    "No declared policy.toml; idle synthesized defaults are not a merge gate.";

/// Human Result when a required rule did not run and nothing failed (0507).
const REQUIRED_IDLE_RESULT: &str = "IDLE (required rule did not run; not a merge gate)";

/// Human footer for that required-idle Result. Uncolored.
const REQUIRED_IDLE_FOOTER: &str = "A required rule did not run. This is not a merge gate.";

// ---------------------------------------------------------------------------
// Public report contract (camelCase JSON, mirrors 0047 PrScanReport discipline)
// ---------------------------------------------------------------------------

/// Versioned machine contract for `policy check --format json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", from = "PolicyCheckReportDe")]
pub struct PolicyCheckReport {
    pub schema_version: u32,
    pub violations: Vec<PolicyViolation>,
    /// No violations. On read, a `fail` status (missing or explicit) with violations
    /// forces `false`; an explicit `pass` or `idle` keeps `passed` as written.
    pub passed: bool,
    /// `observe` | `enforce`
    pub mode: String,
    /// `base-branch` | `trusted-path` | `local` | `synthesized`
    pub policy_source: String,
    /// Non-blocking evaluation notes (e.g. skipped rules when risk not evaluable).
    /// Omitted from JSON when empty (schemaVersion stays 1; additive optional field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// True when `verification_must_pass` was an idle note (no bound run).
    /// Omitted from JSON when false (schemaVersion stays 1; additive optional field).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub idle: bool,
    /// Per-run rule counts. Integers always present. `rules[]` always emitted
    /// (may be empty on legacy deserialize). Nested camelCase; container rename
    /// does not recurse. Default zeros are the legacy/unknown sentinel.
    #[serde(default)]
    pub evaluation: PolicyEvaluation,
    /// `pass` | `fail` | `idle`. Always serialized. A missing key reads as `fail`
    /// when `violations` is non-empty and as `idle` when `violations` is empty.
    /// An explicit `status` is kept. Not `passed`, and not `evaluation.rules[].status`.
    pub status: PolicyHeadline,
}

/// Deserialize shadow. `Serialize` stays on [`PolicyCheckReport`] and always emits `status`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PolicyCheckReportDe {
    schema_version: u32,
    violations: Vec<PolicyViolation>,
    passed: bool,
    mode: String,
    policy_source: String,
    #[serde(default)]
    notes: Vec<String>,
    #[serde(default)]
    idle: bool,
    #[serde(default)]
    evaluation: PolicyEvaluation,
    /// Missing key is `None`. A present JSON null is an error, not a missing key.
    #[serde(default, deserialize_with = "status_if_present")]
    status: Option<PolicyHeadline>,
}

fn status_if_present<'de, D>(deserializer: D) -> Result<Option<PolicyHeadline>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(PolicyHeadline::deserialize(deserializer)?))
}

impl From<PolicyCheckReportDe> for PolicyCheckReport {
    fn from(raw: PolicyCheckReportDe) -> Self {
        let status = match raw.status {
            Some(status) => status,
            None if !raw.violations.is_empty() => PolicyHeadline::Fail,
            None => PolicyHeadline::Idle,
        };
        // `passed` means no violations: a `Fail` with violations cannot be passed.
        // Explicit `pass` and `idle` keep `passed` as written.
        let passed = raw.passed && !(status == PolicyHeadline::Fail && !raw.violations.is_empty());
        Self {
            schema_version: raw.schema_version,
            violations: raw.violations,
            passed,
            mode: raw.mode,
            policy_source: raw.policy_source,
            notes: raw.notes,
            idle: raw.idle,
            evaluation: raw.evaluation,
            status,
        }
    }
}

/// Count of declared [`PolicyRules`] fields. Independent of `rules.len()`.
const POLICY_RULES_DECLARED: u32 = 5;

/// Four-status policy evaluation (0325). Identity:
/// `rulesDeclared == rulesChecked + rulesIdle + rulesOff + rulesSkipped`.
/// `rulesDeclared` is [`POLICY_RULES_DECLARED`], not `rules.len()`.
/// Serde default (all zeros, empty `rules[]`) is the legacy/unknown sentinel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct PolicyEvaluation {
    pub rules_declared: u32,
    pub rules_checked: u32,
    pub rules_idle: u32,
    pub rules_off: u32,
    pub rules_skipped: u32,
    pub rules: Vec<PolicyRuleEval>,
}

impl PolicyEvaluation {
    fn from_rules(mut rules: Vec<PolicyRuleEval>) -> Self {
        rules.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
        let mut rules_checked = 0u32;
        let mut rules_idle = 0u32;
        let mut rules_off = 0u32;
        let mut rules_skipped = 0u32;
        for rule in &rules {
            match rule.status {
                PolicyRuleEvalStatus::Checked => rules_checked += 1,
                PolicyRuleEvalStatus::Idle => rules_idle += 1,
                PolicyRuleEvalStatus::Off => rules_off += 1,
                PolicyRuleEvalStatus::Skipped => rules_skipped += 1,
            }
        }
        Self {
            rules_declared: POLICY_RULES_DECLARED,
            rules_checked,
            rules_idle,
            rules_off,
            rules_skipped,
            rules,
        }
    }

    fn identity_holds(&self) -> bool {
        self.rules_declared
            == self.rules_checked + self.rules_idle + self.rules_off + self.rules_skipped
    }
}

/// One declared `PolicyRules` field and how this run treated it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRuleEval {
    pub rule_id: String,
    pub status: PolicyRuleEvalStatus,
}

/// Live evaluator dispositions. Do not collapse to `checked|idle`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PolicyRuleEvalStatus {
    Checked,
    Idle,
    Off,
    Skipped,
}

/// Whether required policy rules ran. `Default` is `Idle` so a legacy payload
/// without `status` does not deserialize as approval.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum PolicyHeadline {
    Pass,
    Fail,
    #[default]
    Idle,
}

/// Classify a finished report. Does not read notes. Empty `rules` is the
/// unknown sentinel (`Idle`), not a vacuous pass. A violation still wins.
fn policy_headline(violations_empty: bool, rules: &[PolicyRuleEval]) -> PolicyHeadline {
    if !violations_empty {
        return PolicyHeadline::Fail;
    }
    if rules.is_empty() {
        return PolicyHeadline::Idle;
    }
    let required_did_not_run = rules.iter().any(|rule| {
        rule.status == PolicyRuleEvalStatus::Idle
            || (rule.status == PolicyRuleEvalStatus::Skipped
                && matches!(rule.rule_id.as_str(), "fail_on" | "max_risk_without_adr"))
    });
    if required_did_not_run {
        PolicyHeadline::Idle
    } else {
        PolicyHeadline::Pass
    }
}

/// A single policy violation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct PolicyViolation {
    pub rule_id: String,
    pub file: String,
    pub line: Option<u32>,
    pub message: String,
    /// `error` | `warn`
    pub severity: String,
}

fn risk_level_rank(level: PrRiskLevel) -> RiskThreshold {
    match level {
        PrRiskLevel::Low => RiskThreshold::Low,
        PrRiskLevel::Medium => RiskThreshold::Medium,
        PrRiskLevel::High => RiskThreshold::High,
    }
}

/// True when `actual` meets or exceeds `threshold` (and threshold is not Off).
pub fn risk_meets_threshold(actual: PrRiskLevel, threshold: RiskThreshold) -> bool {
    if threshold == RiskThreshold::Off {
        return false;
    }
    risk_level_rank(actual) >= threshold
}

// ---------------------------------------------------------------------------
// CLI entry
// ---------------------------------------------------------------------------

/// CLI entry point for `ledgerful policy check`.
pub fn execute_policy_check(
    pr: Option<String>,
    fail_on: Option<String>,
    policy: Option<PathBuf>,
    format: Option<String>,
) -> Result<()> {
    let fmt = format.as_deref().unwrap_or("text");
    if !matches!(fmt, "json" | "text") {
        return Err(miette::miette!(
            "unsupported --format '{}'; use 'json' or 'text'",
            fmt
        ));
    }

    let report = evaluate_policy_check(pr.as_deref(), fail_on.as_deref(), policy.as_deref())?;

    if fmt == "json" {
        let json = serde_json::to_string_pretty(&report).into_diagnostic()?;
        println!("{}", json);
    } else {
        print_human_report(&report);
    }

    // Observe: always exit 0 (Ok). Enforce: nonzero on violations. `status` is not read.
    if enforcement_rejects(&report) {
        return Err(miette::miette!(
            "Policy check failed with {} violation(s)",
            report.violations.len()
        ));
    }
    Ok(())
}

fn enforcement_rejects(report: &PolicyCheckReport) -> bool {
    report.mode == PolicyMode::Enforce.as_str() && !report.passed
}

/// Evaluate policy without printing or exiting. Used by the CLI and tests.
pub fn evaluate_policy_check(
    pr: Option<&str>,
    fail_on_override: Option<&str>,
    policy_path: Option<&Path>,
) -> Result<PolicyCheckReport> {
    let layout = get_layout()?;
    let repo_config = load_ledger_config(&layout).unwrap_or_default();
    let gate_mode = PolicyMode::parse(&repo_config.gate.mode).unwrap_or(PolicyMode::Observe);

    let is_pr_mode = pr.is_some();
    let pr_base = if let Some(range) = pr {
        let (base, _head, _git_range) = parse_pr_range(range)?;
        Some(base)
    } else {
        None
    };

    let (mut config, policy_source) =
        resolve_policy(&layout, policy_path, pr_base.as_deref(), is_pr_mode)?;

    // CLI --fail-on overrides config for this run.
    if let Some(fo) = fail_on_override {
        // Validate early.
        let _ = RiskThreshold::parse(fo)?;
        config.rules.fail_on = fo.trim().to_ascii_lowercase();
    }

    // Preset resolution (CI-safe):
    // - Explicit preset wins.
    // - `--pr` with omitted preset → enforce (never fail-open via working-tree gate.mode).
    // - Local / default with omitted preset → working-tree gate.mode.
    let mode = match config.preset.as_deref() {
        Some(p) => PolicyMode::parse(p)?,
        None if is_pr_mode => PolicyMode::Enforce,
        None => gate_mode,
    };

    let mut ctx = EvalContext {
        layout: layout.clone(),
        is_pr_mode,
        pr_range: pr.map(|s| s.to_string()),
        config,
        mode,
        policy_source,
        violations: Vec::new(),
        notes: Vec::new(),
        rule_eval: Vec::new(),
    };

    ctx.evaluate()?;

    ctx.finish()
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

struct EvalContext {
    layout: Layout,
    is_pr_mode: bool,
    pr_range: Option<String>,
    config: PolicyConfig,
    mode: PolicyMode,
    policy_source: PolicySource,
    violations: Vec<PolicyViolation>,
    notes: Vec<String>,
    rule_eval: Vec<PolicyRuleEval>,
}

impl EvalContext {
    fn record(&mut self, rule_id: &str, status: PolicyRuleEvalStatus) {
        self.rule_eval.push(PolicyRuleEval {
            rule_id: rule_id.to_string(),
            status,
        });
    }

    fn evaluate(&mut self) -> Result<()> {
        let rules = self.config.rules.clone();

        if rules.require_signed_entries {
            self.eval_require_signed_entries()?;
            self.record("require_signed_entries", PolicyRuleEvalStatus::Checked);
        } else {
            self.record("require_signed_entries", PolicyRuleEvalStatus::Skipped);
        }
        if rules.no_pending_tx {
            if self.is_pr_mode {
                self.eval_no_pending_tx()?;
                self.record("no_pending_tx", PolicyRuleEvalStatus::Skipped);
            } else {
                self.eval_no_pending_tx()?;
                self.record("no_pending_tx", PolicyRuleEvalStatus::Checked);
            }
        } else {
            self.record("no_pending_tx", PolicyRuleEvalStatus::Skipped);
        }
        if rules.verification_must_pass {
            let notes_before = self.notes.len();
            self.eval_verification_must_pass()?;
            let idle = self.notes[notes_before..]
                .iter()
                .any(|n| n == VERIFICATION_MUST_PASS_IDLE_NOTE);
            self.record(
                "verification_must_pass",
                if idle {
                    PolicyRuleEvalStatus::Idle
                } else {
                    PolicyRuleEvalStatus::Checked
                },
            );
        } else {
            self.record("verification_must_pass", PolicyRuleEvalStatus::Skipped);
        }

        let max_adr = RiskThreshold::parse(&rules.max_risk_without_adr)?;
        let fail_on = RiskThreshold::parse(&rules.fail_on)?;

        // Risk is needed for max_risk_without_adr and fail_on.
        let mut risk_ran: Option<bool> = None;
        if max_adr != RiskThreshold::Off || fail_on != RiskThreshold::Off {
            match self.resolve_risk()? {
                Some(risk) => {
                    if risk_meets_threshold(risk.level, fail_on) {
                        self.push_violation(
                            "fail_on",
                            ".ledgerful/policy.toml",
                            format!(
                                "risk level '{}' meets fail_on threshold '{}'{}",
                                risk.level,
                                fail_on.as_str(),
                                if risk.reasons.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({})", risk.reasons.join("; "))
                                }
                            ),
                        );
                    }
                    if risk_meets_threshold(risk.level, max_adr) {
                        // Full change-set ADR coverage (every path, not any-path overlap).
                        if !self.has_adr_for_changes(&risk.changed_paths)? {
                            let uncovered = self.paths_uncovered_by_adr(&risk.changed_paths)?;
                            let uncovered_note = if risk.changed_paths.is_empty() {
                                "; empty change set".to_string()
                            } else {
                                format_uncovered_paths_suffix(&uncovered)
                            };
                            self.push_violation(
                                "max_risk_without_adr",
                                ".ledgerful/state/ledger.db",
                                format!(
                                    "risk level '{}' meets max_risk_without_adr threshold '{}' but ADR does not cover the full high-risk change set (every changed path must be an ADR document or covered by a ledger ARCHITECTURE/is_breaking entity){}",
                                    risk.level,
                                    max_adr.as_str(),
                                    uncovered_note
                                ),
                            );
                        }
                    }
                    risk_ran = Some(true);
                }
                None => {
                    // Not evaluable — record a note, not a violation.
                    self.notes.push(
                        "risk rules (fail_on / max_risk_without_adr) skipped: risk not evaluable in this context"
                            .to_string(),
                    );
                    risk_ran = Some(false);
                }
            }
        }

        let max_status = match (max_adr, risk_ran) {
            (RiskThreshold::Off, _) => PolicyRuleEvalStatus::Off,
            (_, Some(true)) => PolicyRuleEvalStatus::Checked,
            _ => PolicyRuleEvalStatus::Skipped,
        };
        let fail_status = match (fail_on, risk_ran) {
            (RiskThreshold::Off, _) => PolicyRuleEvalStatus::Off,
            (_, Some(true)) => PolicyRuleEvalStatus::Checked,
            _ => PolicyRuleEvalStatus::Skipped,
        };
        self.record("max_risk_without_adr", max_status);
        self.record("fail_on", fail_status);

        Ok(())
    }

    fn push_violation(&mut self, rule_id: &str, file: &str, message: String) {
        let severity = match self.mode {
            PolicyMode::Observe => "warn".to_string(),
            PolicyMode::Enforce => "error".to_string(),
        };
        self.violations.push(PolicyViolation {
            rule_id: rule_id.to_string(),
            file: file.replace('\\', "/"),
            line: None,
            message,
            severity,
        });
    }

    fn finish(mut self) -> Result<PolicyCheckReport> {
        // Deterministic sort: ruleId, file, message.
        self.violations.sort_by(|a, b| {
            a.rule_id
                .cmp(&b.rule_id)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.message.cmp(&b.message))
        });

        let passed = self.violations.is_empty();
        // Deterministic notes ordering for stable JSON.
        self.notes.sort();
        let idle = self
            .notes
            .iter()
            .any(|n| n == VERIFICATION_MUST_PASS_IDLE_NOTE);
        let evaluation = PolicyEvaluation::from_rules(self.rule_eval);
        debug_assert!(
            evaluation.identity_holds(),
            "policy evaluation identity: {evaluation:?}"
        );
        debug_assert_eq!(
            evaluation.rules.len() as u32,
            POLICY_RULES_DECLARED,
            "every PolicyRules field must be recorded: {evaluation:?}"
        );
        let status = policy_headline(passed, &evaluation.rules);
        Ok(PolicyCheckReport {
            schema_version: POLICY_CHECK_SCHEMA_VERSION,
            violations: self.violations,
            passed,
            mode: self.mode.as_str().to_string(),
            policy_source: self.policy_source.as_str().to_string(),
            notes: self.notes,
            idle,
            evaluation,
            status,
        })
    }

    fn open_storage(&self) -> Result<Option<StorageManager>> {
        let db_path = self.layout.state_subdir().join("ledger.db");
        if !db_path.exists() {
            return Ok(None);
        }
        match StorageManager::open_read_only_sqlite_only(&self.layout) {
            Ok(s) => Ok(Some(s)),
            Err(e) => Err(e),
        }
    }

    fn eval_require_signed_entries(&mut self) -> Result<()> {
        let Some(mut storage) = self.open_storage()? else {
            // Fail-closed: rule is enabled but no ledger artifact is present
            // (typical clean CI). Do not silently pass.
            self.push_violation(
                "require_signed_entries",
                ".ledgerful/state/ledger.db",
                "ledger DB not present; cannot evaluate require_signed_entries — present a ledger artifact or disable the rule in policy.toml".to_string(),
            );
            return Ok(());
        };
        let db = crate::ledger::db::LedgerDb::new(storage.get_connection_mut());
        let entries = db
            .get_all_committed_ledger_entries()
            .map_err(|e| miette::miette!("Failed to read ledger entries: {}", e))?;

        // Rule on ⇒ treat missing signatures as invalid (fail-closed).
        let invalid = enumerate_invalid_ledger_entries(&entries, true);
        for (tx_id, _sig, _key) in invalid {
            let short = if tx_id.len() >= 8 {
                &tx_id[..8]
            } else {
                &tx_id
            };
            self.push_violation(
                "require_signed_entries",
                ".ledgerful/state/ledger.db",
                format!(
                    "committed entry {} has missing or invalid signature/public_key",
                    short
                ),
            );
        }
        Ok(())
    }

    fn eval_no_pending_tx(&mut self) -> Result<()> {
        // DoD-1c: `--pr` evaluates committed range only. Pending DB txs and the
        // pending_hook_tx sidecar are workspace state that CI would not see.
        if self.is_pr_mode {
            return Ok(());
        }

        // Local mode: pending ledger transactions in the DB.
        if let Some(mut storage) = self.open_storage()? {
            let config = load_ledger_config(&self.layout).unwrap_or_default();
            let tx_mgr = crate::ledger::TransactionManager::new(
                &mut storage,
                self.layout.root.clone().into(),
                config,
            );
            let pending = tx_mgr
                .get_all_pending()
                .map_err(|e| miette::miette!("{}", e))?;
            for tx in pending {
                let short = if tx.tx_id.len() >= 8 {
                    &tx.tx_id[..8]
                } else {
                    &tx.tx_id
                };
                self.push_violation(
                    "no_pending_tx",
                    ".ledgerful/state/ledger.db",
                    format!(
                        "pending ledger transaction {} (entity={})",
                        short, tx.entity
                    ),
                );
            }
        }

        // Local mode also inspects the pending_hook_tx sidecar.
        let sidecar = self.layout.state_subdir().join("pending_hook_tx");
        if sidecar.exists() {
            self.push_violation(
                "no_pending_tx",
                ".ledgerful/state/pending_hook_tx",
                "pending_hook_tx sidecar present (uncommitted/pending workspace)".to_string(),
            );
        }
        Ok(())
    }

    /// Bound verification run for the evaluation target must overall-pass.
    ///
    /// A "bound" run has a non-null non-empty `tx_id` (from `verify --tx-id` or
    /// commit hooks). Unbound global verifies never satisfy this rule.
    ///
    /// - **Local:** latest bound run; idle + no bound → note, not violation.
    /// - **`--pr`:** every changed path must be covered by at least one
    ///   **passing** (`overall_pass=true`) bound run whose ledger entity covers
    ///   that path (`entity_covers_path`). Failed bound runs never contribute
    ///   coverage. Partial overlap (one of N paths covered) → violation. Empty
    ///   change set + no bound → idle note; empty + bound → latest overall_pass.
    fn eval_verification_must_pass(&mut self) -> Result<()> {
        let Some(storage) = self.open_storage()? else {
            self.push_violation(
                "verification_must_pass",
                ".ledgerful/state/ledger.db",
                "ledger DB not present; cannot evaluate verification_must_pass — present a ledger artifact or disable the rule in policy.toml".to_string(),
            );
            return Ok(());
        };

        if self.is_pr_mode {
            self.eval_verification_must_pass_pr(&storage)?;
        } else {
            self.eval_verification_must_pass_local(&storage)?;
        }
        Ok(())
    }

    /// Idle evaluation target for `verification_must_pass` only.
    ///
    /// Fail-closed: git errors propagate via `collect_changed_files_for_filter`
    /// (`open_repo?` / `get_repo_status?`). Do not reuse `resolve_risk`'s local
    /// `unwrap_or_default` / `Ok(None)`.
    fn evaluation_target_is_idle(&self) -> Result<bool> {
        if self.is_pr_mode {
            // Pending is workspace; `--pr` idle iff the committed change set is empty.
            if self.pr_range.is_none() {
                return Ok(false);
            }
            let changed_paths = match self.resolve_risk()? {
                Some(r) => r.changed_paths,
                None => return Ok(false),
            };
            return Ok(changed_paths.is_empty());
        }

        // Same empty-tree definition as scan / change-context: gix status
        // plus watch.ignore_patterns (target/**, .ledgerful/**, …). Raw
        // get_repo_status can list those paths on a porcelain-clean engine
        // checkout and would turn idle into a merge scare.
        let changes = crate::git::status::collect_changed_files_for_filter(&self.layout)?;
        if !changes.is_empty() {
            return Ok(false);
        }

        // Same DB + sidecar facts as `eval_no_pending_tx`.
        if let Some(mut storage) = self.open_storage()? {
            let config = load_ledger_config(&self.layout).unwrap_or_default();
            let tx_mgr = crate::ledger::TransactionManager::new(
                &mut storage,
                self.layout.root.clone().into(),
                config,
            );
            let pending = tx_mgr
                .get_all_pending()
                .map_err(|e| miette::miette!("{}", e))?;
            if !pending.is_empty() {
                return Ok(false);
            }
        }

        let sidecar = self.layout.state_subdir().join("pending_hook_tx");
        if sidecar.exists() {
            return Ok(false);
        }

        Ok(true)
    }

    fn eval_verification_must_pass_local(&mut self, storage: &StorageManager) -> Result<()> {
        match storage.get_latest_bound_verification_run()? {
            Some((_id, _ts, overall_pass, _tx_id)) => {
                if !overall_pass {
                    self.push_violation(
                        "verification_must_pass",
                        ".ledgerful/state/ledger.db",
                        "latest bound verification run overall_pass is false".to_string(),
                    );
                }
            }
            None => {
                if self.evaluation_target_is_idle()? {
                    self.notes
                        .push(VERIFICATION_MUST_PASS_IDLE_NOTE.to_string());
                } else {
                    self.push_violation(
                        "verification_must_pass",
                        ".ledgerful/state/ledger.db",
                        "no verification run bound to a transaction (run verify with --tx-id or during commit hooks)".to_string(),
                    );
                }
            }
        }
        Ok(())
    }

    fn eval_verification_must_pass_pr(&mut self, storage: &StorageManager) -> Result<()> {
        // Cap scan; bound runs are newest-first (ORDER BY id DESC).
        const BOUND_SCAN_LIMIT: usize = 256;
        let bound = storage.list_bound_verification_runs(BOUND_SCAN_LIMIT)?;

        // 0214-A2: resolve changed_paths before bound.is_empty() so an empty
        // --pr range with no bound run is an idle note, not a violation.
        let changed_paths = match self.resolve_risk()? {
            Some(r) => r.changed_paths,
            None => Vec::new(),
        };

        if bound.is_empty() {
            if self.evaluation_target_is_idle()? {
                self.notes
                    .push(VERIFICATION_MUST_PASS_IDLE_NOTE.to_string());
            } else {
                self.push_violation(
                    "verification_must_pass",
                    ".ledgerful/state/ledger.db",
                    "no verification run bound to a transaction (run verify with --tx-id or during commit hooks)".to_string(),
                );
            }
            return Ok(());
        }

        // Empty change set → fall back to latest bound overall_pass.
        if changed_paths.is_empty() {
            let (_id, _ts, overall_pass, _tx_id) = &bound[0];
            if !overall_pass {
                self.push_violation(
                    "verification_must_pass",
                    ".ledgerful/state/ledger.db",
                    "latest bound verification run overall_pass is false".to_string(),
                );
            }
            return Ok(());
        }

        // Precompute entities per bound run (newest-first). For each changed
        // path, the *newest* covering bound run is decisive: a newer failure
        // vetoes an older pass for that path (stale-pass bypass closed).
        let mut run_entities: Vec<(bool, Vec<String>)> = Vec::with_capacity(bound.len());
        for (_id, _ts, overall_pass, tx_id) in &bound {
            let entities = self.entities_for_tx(storage, tx_id)?.unwrap_or_default();
            run_entities.push((*overall_pass, entities));
        }

        let mut uncovered: Vec<String> = Vec::new();
        let mut failed_cover: Vec<String> = Vec::new();
        for path in &changed_paths {
            let mut decided = false;
            for (overall_pass, entities) in &run_entities {
                let covers = entities
                    .iter()
                    .any(|entity| entity_covers_path(entity, path));
                if !covers {
                    continue;
                }
                // Newest covering run for this path.
                if *overall_pass {
                    decided = true;
                } else {
                    failed_cover.push(path.clone());
                    decided = true;
                }
                break;
            }
            if !decided {
                uncovered.push(path.clone());
            }
        }
        uncovered.sort();
        failed_cover.sort();

        if !uncovered.is_empty() {
            self.push_violation(
                "verification_must_pass",
                ".ledgerful/state/ledger.db",
                format!(
                    "bound verification runs do not cover the full evaluation target change set{}",
                    format_uncovered_paths_suffix(&uncovered)
                ),
            );
        }
        if !failed_cover.is_empty() {
            self.push_violation(
                "verification_must_pass",
                ".ledgerful/state/ledger.db",
                format!(
                    "newest bound verification run covering path(s) has overall_pass=false{}",
                    format_uncovered_paths_suffix(&failed_cover)
                ),
            );
        }
        Ok(())
    }

    /// Entities of committed ledger entries for `tx_id` (empty → None).
    fn entities_for_tx(
        &self,
        storage: &StorageManager,
        tx_id: &str,
    ) -> Result<Option<Vec<String>>> {
        let db = crate::ledger::db::LedgerDb::new(storage.get_connection());
        let entries = db
            .get_ledger_entries_for_tx(tx_id)
            .map_err(|e| miette::miette!("Failed to read ledger entries for tx: {}", e))?;
        let mut entities: Vec<String> = entries
            .iter()
            .map(|e| normalize_repo_path(&e.entity))
            .filter(|e| !e.is_empty())
            .collect();
        entities.sort();
        entities.dedup();
        if entities.is_empty() {
            Ok(None)
        } else {
            Ok(Some(entities))
        }
    }

    /// True when ADR coverage is complete for **every** path in this evaluation's
    /// change set (not any-path overlap, and not any ADR in history).
    ///
    /// Each changed path is covered when either:
    /// 1. The path is itself an ADR document (`is_adr_document_path` — `/adr/`,
    ///    `/adrs/`, `.adr.md`, `architecture-decision`), OR
    /// 2. At least one ledger ADR entry (`get_adr_entries` / `ARCHITECTURE` /
    ///    `is_breaking`) has a non-empty `entity` that covers that path via
    ///    `entity_covers_path` (equals / parent scope / more-specific under tree).
    ///
    /// Empty-entity ADRs never blanket-satisfy. Empty `changed_paths` is
    /// fail-closed (`false`). Partial coverage (one of N paths covered) → false.
    fn has_adr_for_changes(&self, changed_paths: &[String]) -> Result<bool> {
        if changed_paths.is_empty() {
            return Ok(false);
        }
        Ok(self.paths_uncovered_by_adr(changed_paths)?.is_empty())
    }

    /// Changed paths not covered by an ADR document path or a ledger ADR entity.
    /// Sorted. Empty input yields empty (caller treats empty change set as fail).
    fn paths_uncovered_by_adr(&self, changed_paths: &[String]) -> Result<Vec<String>> {
        if changed_paths.is_empty() {
            return Ok(Vec::new());
        }
        let adr_entities = self.collect_adr_entities()?;
        let mut uncovered: Vec<String> = changed_paths
            .iter()
            .filter(|path| {
                if is_adr_document_path(path) {
                    return false;
                }
                !adr_entities
                    .iter()
                    .any(|entity| entity_covers_path(entity, path))
            })
            .cloned()
            .collect();
        uncovered.sort();
        Ok(uncovered)
    }

    /// Entities of committed ADR entries (ARCHITECTURE / is_breaking / get_adr_entries).
    /// Empty entities are dropped (they never cover).
    fn collect_adr_entities(&self) -> Result<Vec<String>> {
        let Some(mut storage) = self.open_storage()? else {
            return Ok(Vec::new());
        };
        let db = crate::ledger::db::LedgerDb::new(storage.get_connection_mut());
        let mut entities = Vec::new();

        let adrs = db
            .get_adr_entries(None)
            .map_err(|e| miette::miette!("Failed to read ADR entries: {}", e))?;
        for e in adrs {
            let ent = normalize_repo_path(&e.entity);
            if !ent.is_empty() {
                entities.push(ent);
            }
        }

        // Fallback: scan all entries for entry_type or is_breaking (in case
        // get_adr_entries is narrower than ARCHITECTURE / is_breaking).
        let entries = db
            .get_all_committed_ledger_entries()
            .map_err(|e| miette::miette!("Failed to read ledger entries: {}", e))?;
        for e in entries {
            if e.entry_type == EntryType::Architecture || e.is_breaking {
                let ent = normalize_repo_path(&e.entity);
                if !ent.is_empty() && !entities.iter().any(|x| x == &ent) {
                    entities.push(ent);
                }
            }
        }

        entities.sort();
        Ok(entities)
    }

    /// Resolve risk level and changed paths for the evaluation target.
    ///
    /// - `--pr` mode: build PrScanReport from the committed range.
    /// - Local mode: use working-tree changes if the repo is available; else None.
    fn resolve_risk(&self) -> Result<Option<ResolvedRisk>> {
        let root = self.layout.root.as_std_path();

        if let Some(ref range) = self.pr_range {
            let (base, head, git_range) = parse_pr_range(range)?;
            let changes = files_changed_between(root, &git_range, &base)?;
            let changed_paths = file_changes_to_paths(&changes);
            let repo = open_repo(root)?;
            let (head_hash, branch_name) = get_head_info(&repo)?;
            // Risk-only path: skip history walk (churn not used for risk).
            let report = PrScanReport::new(
                PrScanContext {
                    base_ref: base,
                    head_ref: head,
                    head_hash,
                    branch_name,
                    tree_clean: changes.is_empty(),
                },
                &changes,
                &[],
                &HistoryEnrichment::empty(),
            );
            return Ok(Some(ResolvedRisk {
                level: report.risk_level,
                reasons: report.risk_reasons,
                changed_paths,
            }));
        }

        // Local / default: working-tree status.
        match open_repo(root) {
            Ok(repo) => {
                let changes = get_repo_status(&repo).unwrap_or_default();
                let changed_paths = file_changes_to_paths(&changes);
                let (head_hash, branch_name) = get_head_info(&repo).unwrap_or((None, None));
                let report = PrScanReport::new(
                    PrScanContext {
                        base_ref: "WORKTREE".to_string(),
                        head_ref: "HEAD".to_string(),
                        head_hash,
                        branch_name,
                        tree_clean: changes.is_empty(),
                    },
                    &changes,
                    &[],
                    &HistoryEnrichment::empty(),
                );
                Ok(Some(ResolvedRisk {
                    level: report.risk_level,
                    reasons: report.risk_reasons,
                    changed_paths,
                }))
            }
            Err(_) => Ok(None),
        }
    }
}

/// Risk + change-set context for `fail_on` / `max_risk_without_adr`.
struct ResolvedRisk {
    level: PrRiskLevel,
    reasons: Vec<String>,
    /// Forward-slash normalized, sorted, deduped paths.
    changed_paths: Vec<String>,
}

/// Format a trailing note listing up to 5 sorted uncovered paths for violation messages.
fn format_uncovered_paths_suffix(uncovered: &[String]) -> String {
    if uncovered.is_empty() {
        return String::new();
    }
    let listed: Vec<&str> = uncovered.iter().take(5).map(String::as_str).collect();
    if uncovered.len() > 5 {
        format!("; uncovered: {}, ...", listed.join(", "))
    } else {
        format!("; uncovered: {}", listed.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Human report
// ---------------------------------------------------------------------------

fn is_synthesized_idle_pass(report: &PolicyCheckReport) -> bool {
    report.passed
        && report.policy_source == PolicySource::Synthesized.as_str()
        && report
            .notes
            .iter()
            .any(|n| n == VERIFICATION_MUST_PASS_IDLE_NOTE)
}

fn human_result_label(report: &PolicyCheckReport) -> &'static str {
    if is_synthesized_idle_pass(report) {
        SYNTHESIZED_IDLE_RESULT
    } else if report.status == PolicyHeadline::Idle {
        REQUIRED_IDLE_RESULT
    } else if report.passed {
        "PASSED"
    } else if report.mode == PolicyMode::Observe.as_str() {
        "WARNINGS (observe — not blocking)"
    } else {
        "FAILED"
    }
}

fn human_footer_line(report: &PolicyCheckReport) -> Option<&'static str> {
    if !report.violations.is_empty() {
        return None;
    }
    if is_synthesized_idle_pass(report) {
        Some(SYNTHESIZED_IDLE_FOOTER)
    } else if report.status == PolicyHeadline::Idle {
        Some(REQUIRED_IDLE_FOOTER)
    } else {
        Some("No policy violations.")
    }
}

fn human_chrome_suppressed(report: &PolicyCheckReport) -> bool {
    is_synthesized_idle_pass(report) || report.status == PolicyHeadline::Idle
}

fn print_human_report(report: &PolicyCheckReport) {
    use owo_colors::{OwoColorize, Stream, Style};

    println!(
        "\n{}",
        "Ledgerful Policy Check"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "{:<16} {}",
        "Mode:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.mode
    );
    println!(
        "{:<16} {}",
        "Policy source:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.policy_source
    );
    let label = human_result_label(report);
    let rendered = if human_chrome_suppressed(report) {
        label.to_string()
    } else if report.passed {
        label
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string()
    } else if report.mode == PolicyMode::Observe.as_str() {
        label
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string()
    } else {
        label
            .if_supports_color(Stream::Stdout, |s| s.red())
            .to_string()
    };
    println!(
        "{:<16} {}",
        "Result:".if_supports_color(Stream::Stdout, |s| s.bold()),
        rendered
    );

    if !report.notes.is_empty() {
        println!(
            "\n{}:",
            "Notes".if_supports_color(Stream::Stdout, |s| s.bold())
        );
        for note in &report.notes {
            println!(
                "  • {}",
                note.if_supports_color(Stream::Stdout, |s| s.dimmed())
            );
        }
    }

    if let Some(footer) = human_footer_line(report) {
        if human_chrome_suppressed(report) {
            println!("\n{footer}");
        } else {
            println!(
                "\n{}",
                footer.if_supports_color(Stream::Stdout, |s| s.green())
            );
        }
        return;
    }

    println!(
        "\n{} ({}):",
        "Violations".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.violations.len()
    );
    for v in &report.violations {
        let sev = if v.severity == "error" {
            v.severity
                .if_supports_color(Stream::Stdout, |s| s.red())
                .to_string()
        } else {
            v.severity
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string()
        };
        println!(
            "  [{sev}] {rule} — {msg}",
            rule = v.rule_id.if_supports_color(Stream::Stdout, |s| s.cyan()),
            msg = v.message
        );
        if !v.file.is_empty() {
            println!(
                "         file: {}",
                v.file.if_supports_color(Stream::Stdout, |s| s.dimmed())
            );
        }
    }

    if report.mode == "observe" {
        println!(
            "\n{}",
            "Gate is observe: violations are warnings only (exit 0)."
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        );
    } else {
        println!(
            "\n{}",
            "Fix the violations above, or adjust policy via a reviewed PR to the base branch."
                .if_supports_color(Stream::Stdout, |s| s.dimmed())
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    // crypto imports are local to the basis test that needs them
    use crate::state::layout::Layout;
    use camino::Utf8PathBuf;

    #[test]
    fn risk_threshold_ordering() {
        assert!(risk_meets_threshold(PrRiskLevel::High, RiskThreshold::High));
        assert!(risk_meets_threshold(
            PrRiskLevel::High,
            RiskThreshold::Medium
        ));
        assert!(risk_meets_threshold(
            PrRiskLevel::Medium,
            RiskThreshold::Low
        ));
        assert!(!risk_meets_threshold(
            PrRiskLevel::Low,
            RiskThreshold::Medium
        ));
        assert!(!risk_meets_threshold(PrRiskLevel::High, RiskThreshold::Off));
        assert!(!risk_meets_threshold(
            PrRiskLevel::Medium,
            RiskThreshold::High
        ));
    }

    #[test]
    fn violation_sort_is_deterministic() {
        let mut v = [
            PolicyViolation {
                rule_id: "fail_on".into(),
                file: "b".into(),
                line: None,
                message: "m2".into(),
                severity: "error".into(),
            },
            PolicyViolation {
                rule_id: "fail_on".into(),
                file: "a".into(),
                line: None,
                message: "m1".into(),
                severity: "error".into(),
            },
            PolicyViolation {
                rule_id: "no_pending_tx".into(),
                file: "a".into(),
                line: None,
                message: "m".into(),
                severity: "error".into(),
            },
        ];
        v.sort();
        assert_eq!(v[0].rule_id, "fail_on");
        assert_eq!(v[0].file, "a");
        assert_eq!(v[1].file, "b");
        assert_eq!(v[2].rule_id, "no_pending_tx");
    }

    #[test]
    fn json_schema_shape_and_camel_case() {
        let report = PolicyCheckReport {
            schema_version: 1,
            violations: vec![PolicyViolation {
                rule_id: "no_pending_tx".into(),
                file: ".ledgerful/state/ledger.db".into(),
                line: None,
                message: "pending".into(),
                severity: "error".into(),
            }],
            passed: false,
            mode: "enforce".into(),
            policy_source: "local".into(),
            notes: vec![],
            idle: false,
            evaluation: PolicyEvaluation::default(),
            status: PolicyHeadline::Fail,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["schemaVersion"], 1);
        assert_eq!(json["passed"], false);
        assert_eq!(json["mode"], "enforce");
        assert_eq!(json["policySource"], "local");
        assert_eq!(json["status"], "fail");
        // Empty notes omitted from JSON (additive optional field).
        assert!(json.get("notes").is_none());
        assert!(json.get("idle").is_none());
        let v = &json["violations"][0];
        assert_eq!(v["ruleId"], "no_pending_tx");
        assert_eq!(v["file"], ".ledgerful/state/ledger.db");
        assert!(v["line"].is_null());
        assert_eq!(v["message"], "pending");
        assert_eq!(v["severity"], "error");
    }

    #[test]
    fn notes_serialized_when_nonempty() {
        let report = PolicyCheckReport {
            schema_version: 1,
            violations: vec![],
            passed: true,
            mode: "observe".into(),
            policy_source: "local".into(),
            notes: vec!["risk rules skipped: risk not evaluable".into()],
            idle: false,
            evaluation: PolicyEvaluation::default(),
            status: PolicyHeadline::Idle,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["notes"][0], "risk rules skipped: risk not evaluable");
        assert_eq!(json["status"], "idle");
        assert!(json.get("idle").is_none());
    }

    fn sample_report(
        passed: bool,
        mode: &str,
        policy_source: &str,
        notes: Vec<String>,
        idle: bool,
        status: PolicyHeadline,
    ) -> PolicyCheckReport {
        let violations = if passed {
            vec![]
        } else {
            vec![PolicyViolation {
                rule_id: "no_pending_tx".into(),
                file: ".ledgerful/state/ledger.db".into(),
                line: None,
                message: "pending".into(),
                severity: "warn".into(),
            }]
        };
        PolicyCheckReport {
            schema_version: 1,
            violations,
            passed,
            mode: mode.into(),
            policy_source: policy_source.into(),
            notes,
            idle,
            evaluation: PolicyEvaluation::default(),
            status,
        }
    }

    fn rule(id: &str, status: PolicyRuleEvalStatus) -> PolicyRuleEval {
        PolicyRuleEval {
            rule_id: id.to_string(),
            status,
        }
    }

    #[test]
    fn human_result_label_synthesized_idle_pass_is_idle() {
        let idle_note = vec![VERIFICATION_MUST_PASS_IDLE_NOTE.to_string()];
        let synthesized_idle = sample_report(
            true,
            "observe",
            "synthesized",
            idle_note.clone(),
            false,
            PolicyHeadline::Idle,
        );
        assert_eq!(
            human_result_label(&synthesized_idle),
            SYNTHESIZED_IDLE_RESULT
        );

        assert_eq!(
            human_result_label(&sample_report(
                true,
                "observe",
                "local",
                idle_note.clone(),
                true,
                PolicyHeadline::Idle,
            )),
            REQUIRED_IDLE_RESULT
        );
        assert_eq!(
            human_result_label(&sample_report(
                true,
                "observe",
                "trusted-path",
                idle_note.clone(),
                true,
                PolicyHeadline::Idle,
            )),
            REQUIRED_IDLE_RESULT
        );
        assert_eq!(
            human_result_label(&sample_report(
                true,
                "enforce",
                "base-branch",
                idle_note,
                true,
                PolicyHeadline::Idle,
            )),
            REQUIRED_IDLE_RESULT
        );
        assert_eq!(
            human_result_label(&sample_report(
                false,
                "observe",
                "synthesized",
                vec![],
                false,
                PolicyHeadline::Fail,
            )),
            "WARNINGS (observe — not blocking)"
        );
        assert_eq!(
            human_result_label(&sample_report(
                false,
                "enforce",
                "synthesized",
                vec![],
                false,
                PolicyHeadline::Fail,
            )),
            "FAILED"
        );
        assert_eq!(
            human_result_label(&sample_report(
                true,
                "observe",
                "synthesized",
                vec![],
                false,
                PolicyHeadline::Pass,
            )),
            "PASSED"
        );
    }

    #[test]
    fn human_footer_line_synthesized_idle_pass() {
        let idle_note = vec![VERIFICATION_MUST_PASS_IDLE_NOTE.to_string()];
        assert_eq!(
            human_footer_line(&sample_report(
                true,
                "observe",
                "synthesized",
                idle_note.clone(),
                false,
                PolicyHeadline::Idle,
            )),
            Some(SYNTHESIZED_IDLE_FOOTER)
        );
        assert_eq!(
            human_footer_line(&sample_report(
                true,
                "observe",
                "local",
                idle_note,
                true,
                PolicyHeadline::Idle,
            )),
            Some(REQUIRED_IDLE_FOOTER)
        );
        assert_eq!(
            human_footer_line(&sample_report(
                false,
                "observe",
                "synthesized",
                vec![],
                false,
                PolicyHeadline::Fail,
            )),
            None
        );
    }

    #[test]
    fn json_idle_additive_omit_false() {
        let omitted = sample_report(
            true,
            "observe",
            "local",
            vec![],
            false,
            PolicyHeadline::Pass,
        );
        let json = serde_json::to_value(&omitted).unwrap();
        assert!(json.get("idle").is_none());
        assert_eq!(json["status"], "pass");

        let included = sample_report(
            true,
            "observe",
            "synthesized",
            vec![VERIFICATION_MUST_PASS_IDLE_NOTE.to_string()],
            true,
            PolicyHeadline::Idle,
        );
        let json_true = serde_json::to_value(&included).unwrap();
        assert_eq!(json_true["idle"], true);

        let legacy = r#"{"schemaVersion":1,"violations":[],"passed":true,"mode":"observe","policySource":"local"}"#;
        let back: PolicyCheckReport = serde_json::from_str(legacy).unwrap();
        assert!(!back.idle);
        assert!(back.passed);
        assert_eq!(back.status, PolicyHeadline::Idle);
        assert_eq!(back.policy_source, "local");
        assert_eq!(back.evaluation, PolicyEvaluation::default());
        assert_eq!(back.evaluation.rules_declared, 0);
        assert!(back.evaluation.rules.is_empty());
    }

    #[test]
    fn legacy_missing_status_follows_violations() {
        let violation_error = r#"{"ruleId":"no_pending_tx","file":".ledgerful/state/ledger.db","line":null,"message":"pending","severity":"error"}"#;
        let violation_warn = r#"{"ruleId":"no_pending_tx","file":".ledgerful/state/ledger.db","line":null,"message":"pending","severity":"warn"}"#;

        let null_status = r#"{"schemaVersion":1,"violations":[],"passed":true,"mode":"observe","policySource":"local","status":null}"#;
        assert!(serde_json::from_str::<PolicyCheckReport>(null_status).is_err());
        let unknown_status = r#"{"schemaVersion":1,"violations":[],"passed":true,"mode":"observe","policySource":"local","status":"approved"}"#;
        assert!(serde_json::from_str::<PolicyCheckReport>(unknown_status).is_err());

        let explicit_idle = format!(
            r#"{{"schemaVersion":1,"violations":[{violation_error}],"passed":false,"mode":"enforce","policySource":"local","status":"idle"}}"#
        );
        let kept_idle: PolicyCheckReport = serde_json::from_str(&explicit_idle).unwrap();
        assert_eq!(kept_idle.status, PolicyHeadline::Idle);

        let empty_violations = r#"{"schemaVersion":1,"violations":[],"passed":false,"mode":"enforce","policySource":"local"}"#;
        let empty: PolicyCheckReport = serde_json::from_str(empty_violations).unwrap();
        assert_eq!(empty.status, PolicyHeadline::Idle);

        let enforce = format!(
            r#"{{"schemaVersion":1,"violations":[{violation_error}],"passed":false,"mode":"enforce","policySource":"local"}}"#
        );
        let observe = format!(
            r#"{{"schemaVersion":1,"violations":[{violation_warn}],"passed":false,"mode":"observe","policySource":"local"}}"#
        );
        let failed: PolicyCheckReport = serde_json::from_str(&enforce).unwrap();
        let warned: PolicyCheckReport = serde_json::from_str(&observe).unwrap();
        assert_eq!(
            (failed.status, warned.status),
            (PolicyHeadline::Fail, PolicyHeadline::Fail)
        );
        assert_eq!(serde_json::to_value(&failed).unwrap()["status"], "fail");
        assert_eq!(human_result_label(&failed), "FAILED");
        assert!(!human_chrome_suppressed(&failed));
        assert!(enforcement_rejects(&failed));
        assert_eq!(
            human_result_label(&warned),
            "WARNINGS (observe — not blocking)"
        );
        assert!(!human_chrome_suppressed(&warned));
        assert!(!enforcement_rejects(&warned));
    }

    const LEGACY_VIOLATION: &str = r#"{"ruleId":"no_pending_tx","file":".ledgerful/state/ledger.db","line":null,"message":"pending","severity":"error"}"#;

    fn legacy_payload(mode: &str, passed: bool, status: Option<&str>, violations: bool) -> String {
        let violations = if violations { LEGACY_VIOLATION } else { "" };
        let status = status
            .map(|s| format!(r#","status":"{s}""#))
            .unwrap_or_default();
        format!(
            r#"{{"schemaVersion":1,"violations":[{violations}],"passed":{passed},"mode":"{mode}","policySource":"local"{status}}}"#
        )
    }

    #[rstest::rstest]
    #[case::enforce_missing_status("enforce", None, "FAILED", true)]
    #[case::observe_missing_status("observe", None, "WARNINGS (observe — not blocking)", false)]
    #[case::enforce_explicit_fail("enforce", Some("fail"), "FAILED", true)]
    #[case::observe_explicit_fail(
        "observe",
        Some("fail"),
        "WARNINGS (observe — not blocking)",
        false
    )]
    fn legacy_passed_true_with_violations_reads_failed(
        #[case] mode: &str,
        #[case] status: Option<&str>,
        #[case] label: &str,
        #[case] rejects: bool,
    ) {
        let report: PolicyCheckReport =
            serde_json::from_str(&legacy_payload(mode, true, status, true)).unwrap();
        assert_eq!(report.status, PolicyHeadline::Fail);
        assert!(!report.passed);
        assert_eq!(human_result_label(&report), label);
        assert!(!human_chrome_suppressed(&report));
        assert_eq!(enforcement_rejects(&report), rejects);

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["status"], "fail");
        assert_eq!(json["passed"], false);
    }

    #[rstest::rstest]
    #[case::explicit_pass(Some("pass"), true, true, true, "PASSED")]
    #[case::explicit_idle_with_violation(Some("idle"), true, true, true, REQUIRED_IDLE_RESULT)]
    #[case::missing_status_empty_passed_true(None, true, false, true, REQUIRED_IDLE_RESULT)]
    #[case::missing_status_empty_passed_false(None, false, false, false, REQUIRED_IDLE_RESULT)]
    fn passed_is_kept_outside_the_fail_with_violations_arm(
        #[case] status: Option<&str>,
        #[case] passed: bool,
        #[case] violations: bool,
        #[case] expect_passed: bool,
        #[case] label: &str,
    ) {
        let report: PolicyCheckReport =
            serde_json::from_str(&legacy_payload("observe", passed, status, violations)).unwrap();
        assert_eq!(report.passed, expect_passed);
        assert_eq!(human_result_label(&report), label);
    }

    #[test]
    fn legacy_missing_status_passed_true_reserializes_canonically() {
        let legacy = legacy_payload("enforce", true, None, true);
        let first: PolicyCheckReport = serde_json::from_str(&legacy).unwrap();
        let s1 = serde_json::to_string(&first).unwrap();
        assert!(s1.contains(r#""passed":false"#));
        assert!(s1.contains(r#""status":"fail""#));
        let second: PolicyCheckReport = serde_json::from_str(&s1).unwrap();
        assert_eq!(first, second);
    }

    #[rstest::rstest]
    #[case::pass(true, "observe", PolicyHeadline::Pass)]
    #[case::fail(false, "enforce", PolicyHeadline::Fail)]
    #[case::idle(true, "observe", PolicyHeadline::Idle)]
    fn sample_report_round_trips_unchanged(
        #[case] passed: bool,
        #[case] mode: &str,
        #[case] status: PolicyHeadline,
    ) {
        let report = sample_report(passed, mode, "local", vec![], false, status);
        let back: PolicyCheckReport =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(report, back);
    }

    #[test]
    fn policy_headline_fail_wins_over_idle() {
        let rules = vec![
            rule("verification_must_pass", PolicyRuleEvalStatus::Idle),
            rule("require_signed_entries", PolicyRuleEvalStatus::Checked),
            rule("no_pending_tx", PolicyRuleEvalStatus::Checked),
            rule("fail_on", PolicyRuleEvalStatus::Off),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Off),
        ];
        assert_eq!(policy_headline(false, &rules), PolicyHeadline::Fail);
    }

    #[test]
    fn policy_headline_declared_verification_idle_is_idle() {
        let rules = vec![
            rule("verification_must_pass", PolicyRuleEvalStatus::Idle),
            rule("require_signed_entries", PolicyRuleEvalStatus::Checked),
            rule("no_pending_tx", PolicyRuleEvalStatus::Off),
            rule("fail_on", PolicyRuleEvalStatus::Off),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Checked),
        ];
        assert_eq!(policy_headline(true, &rules), PolicyHeadline::Idle);
    }

    #[test]
    fn policy_headline_risk_skipped_is_idle() {
        let rules = vec![
            rule("verification_must_pass", PolicyRuleEvalStatus::Checked),
            rule("require_signed_entries", PolicyRuleEvalStatus::Checked),
            rule("no_pending_tx", PolicyRuleEvalStatus::Checked),
            rule("fail_on", PolicyRuleEvalStatus::Skipped),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Skipped),
        ];
        assert_eq!(policy_headline(true, &rules), PolicyHeadline::Idle);
    }

    #[test]
    fn policy_headline_all_checked_is_pass() {
        let rules = vec![
            rule("verification_must_pass", PolicyRuleEvalStatus::Checked),
            rule("require_signed_entries", PolicyRuleEvalStatus::Checked),
            rule("no_pending_tx", PolicyRuleEvalStatus::Checked),
            rule("fail_on", PolicyRuleEvalStatus::Checked),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Checked),
        ];
        assert_eq!(policy_headline(true, &rules), PolicyHeadline::Pass);
    }

    #[test]
    fn policy_headline_single_risk_skipped_is_idle() {
        let fail_on_skipped = vec![
            rule("verification_must_pass", PolicyRuleEvalStatus::Checked),
            rule("require_signed_entries", PolicyRuleEvalStatus::Checked),
            rule("no_pending_tx", PolicyRuleEvalStatus::Checked),
            rule("fail_on", PolicyRuleEvalStatus::Skipped),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Off),
        ];
        assert_eq!(
            policy_headline(true, &fail_on_skipped),
            PolicyHeadline::Idle
        );
        let max_risk_skipped = vec![
            rule("verification_must_pass", PolicyRuleEvalStatus::Checked),
            rule("require_signed_entries", PolicyRuleEvalStatus::Checked),
            rule("no_pending_tx", PolicyRuleEvalStatus::Checked),
            rule("fail_on", PolicyRuleEvalStatus::Off),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Skipped),
        ];
        assert_eq!(
            policy_headline(true, &max_risk_skipped),
            PolicyHeadline::Idle
        );
    }

    #[test]
    fn policy_headline_fail_wins_over_risk_skipped() {
        let rules = vec![
            rule("verification_must_pass", PolicyRuleEvalStatus::Checked),
            rule("require_signed_entries", PolicyRuleEvalStatus::Checked),
            rule("no_pending_tx", PolicyRuleEvalStatus::Checked),
            rule("fail_on", PolicyRuleEvalStatus::Skipped),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Off),
        ];
        assert_eq!(policy_headline(false, &rules), PolicyHeadline::Fail);
    }

    #[test]
    fn policy_headline_empty_rules_is_idle() {
        assert_eq!(policy_headline(true, &[]), PolicyHeadline::Idle);
        assert_eq!(policy_headline(false, &[]), PolicyHeadline::Fail);
    }

    #[test]
    fn policy_headline_disabled_and_pr_skip_are_pass() {
        let rules = vec![
            rule("require_signed_entries", PolicyRuleEvalStatus::Skipped),
            rule("no_pending_tx", PolicyRuleEvalStatus::Skipped),
            rule("verification_must_pass", PolicyRuleEvalStatus::Skipped),
            rule("fail_on", PolicyRuleEvalStatus::Off),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Off),
        ];
        assert_eq!(policy_headline(true, &rules), PolicyHeadline::Pass);
    }

    #[test]
    fn policy_headline_is_stable() {
        let rules = vec![
            rule("require_signed_entries", PolicyRuleEvalStatus::Checked),
            rule("no_pending_tx", PolicyRuleEvalStatus::Skipped),
            rule("verification_must_pass", PolicyRuleEvalStatus::Off),
            rule("fail_on", PolicyRuleEvalStatus::Checked),
            rule("max_risk_without_adr", PolicyRuleEvalStatus::Off),
        ];
        assert_eq!(policy_headline(true, &rules), policy_headline(true, &rules));
    }

    #[test]
    fn policy_headline_json_tokens_are_pass_fail_idle() {
        let pass = sample_report(
            true,
            "observe",
            "local",
            vec![],
            false,
            PolicyHeadline::Pass,
        );
        let fail = sample_report(
            false,
            "enforce",
            "local",
            vec![],
            false,
            PolicyHeadline::Fail,
        );
        let idle = sample_report(
            true,
            "observe",
            "local",
            vec![],
            false,
            PolicyHeadline::Idle,
        );
        assert_eq!(serde_json::to_value(&pass).unwrap()["status"], "pass");
        assert_eq!(serde_json::to_value(&fail).unwrap()["status"], "fail");
        assert_eq!(serde_json::to_value(&idle).unwrap()["status"], "idle");
    }

    #[test]
    fn human_chrome_suppressed_for_required_idle() {
        let note = vec![VERIFICATION_MUST_PASS_IDLE_NOTE.to_string()];
        let synthesized = sample_report(
            true,
            "observe",
            "synthesized",
            note.clone(),
            true,
            PolicyHeadline::Idle,
        );
        let declared = sample_report(true, "observe", "local", note, true, PolicyHeadline::Idle);
        let clean = sample_report(
            true,
            "observe",
            "local",
            vec![],
            false,
            PolicyHeadline::Pass,
        );
        let violation = sample_report(
            false,
            "enforce",
            "local",
            vec![],
            false,
            PolicyHeadline::Fail,
        );
        assert!(human_chrome_suppressed(&synthesized));
        assert!(human_chrome_suppressed(&declared));
        assert!(!human_chrome_suppressed(&clean));
        assert!(!human_chrome_suppressed(&violation));
    }

    #[test]
    fn enforcement_rejects_ignores_idle_status() {
        let idle_enforce = sample_report(
            true,
            "enforce",
            "local",
            vec![],
            false,
            PolicyHeadline::Idle,
        );
        let fail_enforce = sample_report(
            false,
            "enforce",
            "local",
            vec![],
            false,
            PolicyHeadline::Fail,
        );
        let fail_observe = sample_report(
            false,
            "observe",
            "local",
            vec![],
            false,
            PolicyHeadline::Fail,
        );
        assert!(!enforcement_rejects(&idle_enforce));
        assert!(enforcement_rejects(&fail_enforce));
        assert!(!enforcement_rejects(&fail_observe));
    }

    #[test]
    fn evaluation_from_rules_declared_is_independent_of_recorded_len() {
        let eval = PolicyEvaluation::from_rules(vec![PolicyRuleEval {
            rule_id: "fail_on".into(),
            status: PolicyRuleEvalStatus::Off,
        }]);
        assert_eq!(eval.rules_declared, POLICY_RULES_DECLARED);
        assert_eq!(eval.rules.len(), 1);
        assert!(
            !eval.identity_holds(),
            "under-record must fail identity: {eval:?}"
        );
    }

    #[test]
    fn evaluation_from_rules_identity_and_sort() {
        let eval = PolicyEvaluation::from_rules(vec![
            PolicyRuleEval {
                rule_id: "verification_must_pass".into(),
                status: PolicyRuleEvalStatus::Idle,
            },
            PolicyRuleEval {
                rule_id: "fail_on".into(),
                status: PolicyRuleEvalStatus::Off,
            },
            PolicyRuleEval {
                rule_id: "no_pending_tx".into(),
                status: PolicyRuleEvalStatus::Skipped,
            },
            PolicyRuleEval {
                rule_id: "require_signed_entries".into(),
                status: PolicyRuleEvalStatus::Checked,
            },
            PolicyRuleEval {
                rule_id: "max_risk_without_adr".into(),
                status: PolicyRuleEvalStatus::Off,
            },
        ]);
        assert!(eval.identity_holds());
        assert_eq!(eval.rules_declared, 5);
        assert_eq!(eval.rules_checked, 1);
        assert_eq!(eval.rules_idle, 1);
        assert_eq!(eval.rules_off, 2);
        assert_eq!(eval.rules_skipped, 1);
        let ids: Vec<&str> = eval.rules.iter().map(|r| r.rule_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "fail_on",
                "max_risk_without_adr",
                "no_pending_tx",
                "require_signed_entries",
                "verification_must_pass"
            ]
        );
        let json = serde_json::to_value(&eval).unwrap();
        assert_eq!(json["rulesDeclared"], 5);
        assert_eq!(json["rules"][0]["ruleId"], "fail_on");
        assert_eq!(json["rules"][0]["status"], "off");
        assert_eq!(json["rules"][2]["status"], "skipped");
    }

    /// DoD-5 (0072): v2 signing basis binds provenance fields; policy/mode never enter.
    #[test]
    fn signing_basis_fields_unchanged_by_policy() {
        use crate::ledger::crypto::{
            CURRENT_LEDGER_SIG_VERSION, LedgerSignInput, encode_v2_payload,
            sign_ledger_entry_in_v2, verify_entry_signature,
        };

        let tmp = tempfile::tempdir().unwrap();
        let keys = tmp.path().join("keys");
        std::fs::create_dir_all(&keys).unwrap();

        let input = LedgerSignInput {
            sig_version: CURRENT_LEDGER_SIG_VERSION,
            tx_id: "tx-policy-basis".into(),
            category: "FEATURE".into(),
            summary: "basis check".into(),
            reason: "DoD-5".into(),
            committed_at: "2026-07-19T00:00:00Z".into(),
            entity: "src/main.rs".into(),
            change_type: "MODIFY".into(),
            entry_type: "IMPLEMENTATION".into(),
            author: "Ada".into(),
            risk: "LOW".into(),
            is_breaking: false,
            related_tickets: String::new(),
            origin: "LOCAL".into(),
            entity_normalized: "src/main.rs".into(),
        };

        let (sig, pub_key) = sign_ledger_entry_in_v2(&keys, &input).unwrap();
        let sig = sig.unwrap();
        let pub_key = pub_key.unwrap();
        assert!(verify_entry_signature(&input, &sig, &pub_key));

        // Injecting policy into a signed free-text field must break verification.
        let mut mutated = input.clone();
        mutated.summary = "basis check [policy=enforce]".into();
        assert!(!verify_entry_signature(&mutated, &sig, &pub_key));

        // Frozen v2 field order (0072 phase0 codec).
        let basis_fields = [
            "sig_version",
            "tx_id",
            "category",
            "summary",
            "reason",
            "committed_at",
            "entity",
            "change_type",
            "entry_type",
            "author",
            "risk",
            "is_breaking",
            "related_tickets",
            "origin",
        ];
        assert_eq!(basis_fields.len(), 14);
        let payload = encode_v2_payload(&input).unwrap();
        let lines: Vec<&str> = payload.lines().collect();
        assert_eq!(lines.len(), 14);
        for (i, name) in basis_fields.iter().enumerate() {
            assert!(
                lines[i].starts_with(&format!("{name}:")),
                "line {i} must start with {name}:"
            );
        }
        assert!(!basis_fields.contains(&"policy"));
        assert!(!basis_fields.contains(&"mode"));
        assert!(!basis_fields.contains(&"gate"));
        assert_eq!(CURRENT_LEDGER_SIG_VERSION, 2);
    }

    #[test]
    fn observe_mode_marks_severity_warn() {
        let mut ctx = EvalContext {
            layout: Layout::new(Utf8PathBuf::from(".")),
            is_pr_mode: false,
            pr_range: None,
            config: PolicyConfig::default(),
            mode: PolicyMode::Observe,
            policy_source: PolicySource::Local,
            violations: Vec::new(),
            notes: Vec::new(),
            rule_eval: Vec::new(),
        };
        ctx.push_violation("no_pending_tx", "f", "msg".into());
        assert_eq!(ctx.violations[0].severity, "warn");
    }

    #[test]
    fn enforce_mode_marks_severity_error() {
        let mut ctx = EvalContext {
            layout: Layout::new(Utf8PathBuf::from(".")),
            is_pr_mode: false,
            pr_range: None,
            config: PolicyConfig::default(),
            mode: PolicyMode::Enforce,
            policy_source: PolicySource::Local,
            violations: Vec::new(),
            notes: Vec::new(),
            rule_eval: Vec::new(),
        };
        ctx.push_violation("no_pending_tx", "f", "msg".into());
        assert_eq!(ctx.violations[0].severity, "error");
    }
}
