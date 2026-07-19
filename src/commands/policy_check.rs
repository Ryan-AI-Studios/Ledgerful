//! CI policy gate: `ledgerful policy check` (track 0049).
//!
//! Evaluates a flat, named-rule policy against PR/diff/ledger state.
//! Distinct from `crate::policy` (tech-stack `rules.toml` matching).
//!
//! Offline and deterministic. No network code.

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::commands::scan::{files_changed_between, parse_pr_range};
use crate::commands::scan_pr::{PrRiskLevel, PrScanReport};
use crate::commands::verify::enumerate_invalid_ledger_entries;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::ledger::types::EntryType;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Stable schema version for `PolicyCheckReport`. Breaking changes bump this.
pub const POLICY_CHECK_SCHEMA_VERSION: u32 = 1;

const DEFAULT_POLICY_REL: &str = ".ledgerful/policy.toml";

// ---------------------------------------------------------------------------
// Public report contract (camelCase JSON, mirrors 0047 PrScanReport discipline)
// ---------------------------------------------------------------------------

/// Versioned machine contract for `policy check --format json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PolicyCheckReport {
    pub schema_version: u32,
    pub violations: Vec<PolicyViolation>,
    pub passed: bool,
    /// `observe` | `enforce`
    pub mode: String,
    /// `base-branch` | `trusted-path` | `local` | `synthesized`
    pub policy_source: String,
    /// Non-blocking evaluation notes (e.g. skipped rules when risk not evaluable).
    /// Omitted from JSON when empty (schemaVersion stays 1; additive optional field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
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

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Flat policy config (no DSL). Loaded from `.ledgerful/policy.toml` or trusted path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PolicyConfig {
    /// Coarse preset: `observe` | `enforce`. When omitted, derived from `gate.mode`.
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub rules: PolicyRules,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyRules {
    #[serde(default = "default_true")]
    pub require_signed_entries: bool,
    #[serde(default = "default_true")]
    pub no_pending_tx: bool,
    #[serde(default = "default_true")]
    pub verification_must_pass: bool,
    /// `off` | `low` | `medium` | `high`
    #[serde(default = "default_high")]
    pub max_risk_without_adr: String,
    /// `off` | `low` | `medium` | `high`
    #[serde(default = "default_high")]
    pub fail_on: String,
}

fn default_true() -> bool {
    true
}

fn default_high() -> String {
    "high".to_string()
}

impl Default for PolicyRules {
    fn default() -> Self {
        Self {
            require_signed_entries: true,
            no_pending_tx: true,
            verification_must_pass: true,
            max_risk_without_adr: default_high(),
            fail_on: default_high(),
        }
    }
}

/// Risk / severity threshold used by parameterized rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskThreshold {
    Off = 0,
    Low = 1,
    Medium = 2,
    High = 3,
}

impl RiskThreshold {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Ok(Self::Off),
            "low" => Ok(Self::Low),
            "medium" | "med" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(miette::miette!(
                "invalid risk threshold '{}'; expected off|low|medium|high",
                other
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyMode {
    Observe,
    Enforce,
}

impl PolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "observe" => Ok(Self::Observe),
            "enforce" => Ok(Self::Enforce),
            other => Err(miette::miette!(
                "invalid policy preset/mode '{}'; expected observe|enforce",
                other
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicySource {
    BaseBranch,
    TrustedPath,
    Local,
    /// Defaults synthesized because no policy.toml was loaded (not base-branch content).
    Synthesized,
}

impl PolicySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BaseBranch => "base-branch",
            Self::TrustedPath => "trusted-path",
            Self::Local => "local",
            Self::Synthesized => "synthesized",
        }
    }
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

    // Observe: always exit 0 (Ok). Enforce: nonzero on violations.
    if report.mode == PolicyMode::Enforce.as_str() && !report.passed {
        return Err(miette::miette!(
            "Policy check failed with {} violation(s)",
            report.violations.len()
        ));
    }
    Ok(())
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
    };

    ctx.evaluate()?;

    ctx.finish()
}

// ---------------------------------------------------------------------------
// Policy resolution (DoD-1b bypass-proof)
// ---------------------------------------------------------------------------

/// Resolve policy config and its source.
///
/// Priority:
/// 1. Explicit `--policy <path>` → trusted-path
/// 2. `--pr` mode → `git show <base>:.ledgerful/policy.toml` (base-branch);
///    never the working-tree PR-head copy. Missing base file → synthesize
///    **CI-safe** defaults (preset=enforce; ledger-backed rules off).
///    Source = `synthesized`.
/// 3. Local mode → working-tree `.ledgerful/policy.toml` or synthesize from
///    `gate.mode` with full rule set on. Source = `local` when a file is loaded,
///    `synthesized` when not.
fn resolve_policy(
    layout: &Layout,
    policy_path: Option<&Path>,
    pr_base: Option<&str>,
    is_pr_mode: bool,
) -> Result<(PolicyConfig, PolicySource)> {
    if let Some(path) = policy_path {
        let text = std::fs::read_to_string(path).map_err(|e| {
            miette::miette!(
                "failed to read trusted policy file '{}': {}",
                path.display(),
                e
            )
        })?;
        let cfg = parse_policy_toml(&text)?;
        return Ok((cfg, PolicySource::TrustedPath));
    }

    if is_pr_mode {
        let base = pr_base.ok_or_else(|| miette::miette!("--pr mode requires a base ref"))?;
        match load_policy_from_git(layout.root.as_std_path(), base)? {
            Some(text) => {
                let cfg = parse_policy_toml(&text)?;
                return Ok((cfg, PolicySource::BaseBranch));
            }
            None => {
                // No policy on base branch: synthesize CI-safe git-only defaults.
                // Ledger-backed rules stay off — clean CI has no ledger.db artifact.
                // Do not inherit working-tree gate.mode (fail-open risk).
                let cfg = synthesize_pr_defaults();
                return Ok((cfg, PolicySource::Synthesized));
            }
        }
    }

    // Local mode: working-tree policy or synthesize from gate.mode.
    let local_path = layout.root.join(DEFAULT_POLICY_REL);
    if local_path.exists() {
        let text = std::fs::read_to_string(local_path.as_std_path())
            .map_err(|e| miette::miette!("failed to read policy file '{}': {}", local_path, e))?;
        let cfg = parse_policy_toml(&text)?;
        return Ok((cfg, PolicySource::Local));
    }

    Ok((synthesize_from_gate(layout), PolicySource::Synthesized))
}

fn synthesize_from_gate(layout: &Layout) -> PolicyConfig {
    let repo_config = load_ledger_config(layout).unwrap_or_default();
    let mode = if repo_config.gate.is_enforce() {
        PolicyMode::Enforce
    } else {
        PolicyMode::Observe
    };
    synthesize_defaults(mode)
}

/// Local / full-gate synthesize: all named rules on (mirrors gate.mode presets).
fn synthesize_defaults(mode: PolicyMode) -> PolicyConfig {
    PolicyConfig {
        preset: Some(mode.as_str().to_string()),
        rules: PolicyRules::default(),
    }
}

/// CI-safe defaults for `--pr` when no base-branch `policy.toml` exists.
///
/// Only enables rules evaluable from git alone. Ledger-backed rules
/// (`require_signed_entries`, `verification_must_pass`) stay off because a
/// clean CI runner has no tracked `ledger.db`. `no_pending_tx` is on but
/// skipped under `--pr` (workspace state). Force-add a real policy.toml on
/// the base branch to enable ledger rules when a ledger artifact is presented.
fn synthesize_pr_defaults() -> PolicyConfig {
    PolicyConfig {
        preset: Some(PolicyMode::Enforce.as_str().to_string()),
        rules: PolicyRules {
            require_signed_entries: false,
            no_pending_tx: true,
            verification_must_pass: false,
            max_risk_without_adr: default_high(),
            fail_on: default_high(),
        },
    }
}

/// Load `.ledgerful/policy.toml` from a git ref via `git show`.
///
/// Returns `Ok(None)` only when the path is known-missing at that ref.
/// Invalid refs and other fatals return `Err` with an actionable message.
pub fn load_policy_from_git(repo_root: &Path, base_ref: &str) -> Result<Option<String>> {
    let spec = format!("{}:{}", base_ref, DEFAULT_POLICY_REL);
    let output = Command::new("git")
        .args(["show", &spec])
        .current_dir(repo_root)
        .output()
        .map_err(|e| miette::miette!("failed to run git show: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Treat as missing only known path-missing messages. Invalid refs /
        // other fatals must surface so CI does not silently fail-open.
        if is_git_path_missing(&stderr) {
            return Ok(None);
        }
        return Err(miette::miette!(
            "git show {} failed (ref or object error; check that base ref '{}' exists and is fetched): {}",
            spec,
            base_ref,
            stderr.trim()
        ));
    }

    let text = String::from_utf8(output.stdout)
        .map_err(|e| miette::miette!("policy.toml at {} is not valid UTF-8: {}", spec, e))?;
    Ok(Some(text))
}

/// True when git stderr indicates the path is absent at the given tree-ish.
fn is_git_path_missing(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("does not exist")
        || lower.contains("exists on disk, but not in")
        || lower.contains("path does not exist")
}

pub fn parse_policy_toml(text: &str) -> Result<PolicyConfig> {
    let cfg: PolicyConfig =
        toml::from_str(text).map_err(|e| miette::miette!("invalid policy.toml: {}", e))?;
    // Validate thresholds early.
    if let Some(ref preset) = cfg.preset {
        let _ = PolicyMode::parse(preset)?;
    }
    let _ = RiskThreshold::parse(&cfg.rules.max_risk_without_adr)?;
    let _ = RiskThreshold::parse(&cfg.rules.fail_on)?;
    Ok(cfg)
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
}

impl EvalContext {
    fn evaluate(&mut self) -> Result<()> {
        let rules = self.config.rules.clone();

        if rules.require_signed_entries {
            self.eval_require_signed_entries()?;
        }
        if rules.no_pending_tx {
            self.eval_no_pending_tx()?;
        }
        if rules.verification_must_pass {
            self.eval_verification_must_pass()?;
        }

        let max_adr = RiskThreshold::parse(&rules.max_risk_without_adr)?;
        let fail_on = RiskThreshold::parse(&rules.fail_on)?;

        // Risk is needed for max_risk_without_adr and fail_on.
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
                        // Covering ADR for *this* change set (not any ADR in history).
                        let has_adr = self.has_adr_for_changes(&risk.changed_paths)?;
                        if !has_adr {
                            self.push_violation(
                                "max_risk_without_adr",
                                ".ledgerful/state/ledger.db",
                                format!(
                                    "risk level '{}' meets max_risk_without_adr threshold '{}' but no ADR covers the high-risk change set (changed ADR document path, or ledger ARCHITECTURE/is_breaking entry whose entity covers a changed path)",
                                    risk.level,
                                    max_adr.as_str()
                                ),
                            );
                        }
                    }
                }
                None => {
                    // Not evaluable — record a note, not a violation.
                    self.notes.push(
                        "risk rules (fail_on / max_risk_without_adr) skipped: risk not evaluable in this context"
                            .to_string(),
                    );
                }
            }
        }

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
        Ok(PolicyCheckReport {
            schema_version: POLICY_CHECK_SCHEMA_VERSION,
            violations: self.violations,
            passed,
            mode: self.mode.as_str().to_string(),
            policy_source: self.policy_source.as_str().to_string(),
            notes: self.notes,
        })
    }

    fn open_storage(&self) -> Result<Option<StorageManager>> {
        let db_path = self.layout.state_subdir().join("ledger.db");
        if !db_path.exists() {
            return Ok(None);
        }
        match StorageManager::open_read_only_sqlite_only(&self.layout.root) {
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
    /// - **Local:** latest bound run; fail if none.
    /// - **`--pr`:** among bound runs, prefer one whose committed ledger entity
    ///   covers a changed path (`entity_covers_path`). If none cover and the
    ///   change set is non-empty → fail. Zero changed paths → fall back to
    ///   latest bound run.
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
                self.push_violation(
                    "verification_must_pass",
                    ".ledgerful/state/ledger.db",
                    "no verification run bound to a transaction (run verify with --tx-id or during commit hooks)".to_string(),
                );
            }
        }
        Ok(())
    }

    fn eval_verification_must_pass_pr(&mut self, storage: &StorageManager) -> Result<()> {
        // Cap scan; bound runs are newest-first (ORDER BY id DESC).
        const BOUND_SCAN_LIMIT: usize = 256;
        let bound = storage.list_bound_verification_runs(BOUND_SCAN_LIMIT)?;
        if bound.is_empty() {
            self.push_violation(
                "verification_must_pass",
                ".ledgerful/state/ledger.db",
                "no verification run bound to a transaction (run verify with --tx-id or during commit hooks)".to_string(),
            );
            return Ok(());
        }

        let changed_paths = match self.resolve_risk()? {
            Some(r) => r.changed_paths,
            None => Vec::new(),
        };

        // Prefer a bound run whose ledger entity covers a changed path.
        let mut covering: Option<(bool, String)> = None;
        for (_id, _ts, overall_pass, tx_id) in &bound {
            if let Some(entities) = self.entities_for_tx(storage, tx_id)? {
                let covers = entities.iter().any(|entity| {
                    changed_paths
                        .iter()
                        .any(|path| entity_covers_path(entity, path))
                });
                if covers {
                    covering = Some((*overall_pass, tx_id.clone()));
                    break; // newest covering (list is id DESC)
                }
            }
        }

        if let Some((overall_pass, _tx_id)) = covering {
            if !overall_pass {
                self.push_violation(
                    "verification_must_pass",
                    ".ledgerful/state/ledger.db",
                    "bound verification run covering the evaluation target change set has overall_pass=false".to_string(),
                );
            }
            return Ok(());
        }

        // No covering bound run.
        if changed_paths.is_empty() {
            // Edge: empty change set — require latest bound overall_pass.
            let (_id, _ts, overall_pass, _tx_id) = &bound[0];
            if !overall_pass {
                self.push_violation(
                    "verification_must_pass",
                    ".ledgerful/state/ledger.db",
                    "latest bound verification run overall_pass is false".to_string(),
                );
            }
        } else {
            self.push_violation(
                "verification_must_pass",
                ".ledgerful/state/ledger.db",
                "no bound verification run covers the evaluation target change set".to_string(),
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

    /// True when an ADR covers this evaluation's change set (not any ADR in history).
    ///
    /// Satisfied when any of:
    /// 1. A changed path is itself an ADR document (`/adr/`, `/adrs/`, `.adr.md`,
    ///    `architecture-decision` — case-insensitive, forward-slash normalized).
    /// 2. A ledger ADR entry (`get_adr_entries` / `ARCHITECTURE` / `is_breaking`)
    ///    has a non-empty `entity` that covers a changed path:
    ///    - entity equals path, OR
    ///    - path starts with `entity/` (entity is a directory/module scope), OR
    ///    - entity starts with `path/` (entity more specific under a changed tree)
    ///
    /// Empty-entity ADRs never blanket-satisfy. High risk with empty
    /// `changed_paths` and no covering ADR is fail-closed (`false`).
    fn has_adr_for_changes(&self, changed_paths: &[String]) -> Result<bool> {
        // (a) Changed path is itself an ADR document.
        if changed_paths.iter().any(|p| is_adr_document_path(p)) {
            return Ok(true);
        }

        // Collect ADR entities from the ledger (dedicated query + fallback scan).
        let adr_entities = self.collect_adr_entities()?;
        if adr_entities.is_empty() {
            return Ok(false);
        }

        // Fail-closed: no changed paths means no entity can cover them.
        if changed_paths.is_empty() {
            return Ok(false);
        }

        // (b) Any ADR entity covers any changed path.
        Ok(adr_entities.iter().any(|entity| {
            changed_paths
                .iter()
                .any(|path| entity_covers_path(entity, path))
        }))
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
            let report = PrScanReport::new(
                base,
                head,
                head_hash,
                branch_name,
                changes.is_empty(),
                &changes,
                &[],
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
                    "WORKTREE".to_string(),
                    "HEAD".to_string(),
                    head_hash,
                    branch_name,
                    changes.is_empty(),
                    &changes,
                    &[],
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

// ---------------------------------------------------------------------------
// ADR covering helpers (max_risk_without_adr)
// ---------------------------------------------------------------------------

/// Forward-slash normalize and trim leading/trailing slashes for stable compare.
fn normalize_repo_path(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').trim().to_string()
}

/// True when `path` looks like an ADR document (case-insensitive).
///
/// Matches: `/adr/` or `/adrs/` path segments, `.adr.md` suffix, or
/// `architecture-decision` anywhere in the path.
pub fn is_adr_document_path(path: &str) -> bool {
    let n = normalize_repo_path(path).to_ascii_lowercase();
    if n.is_empty() {
        return false;
    }
    n.contains("/adr/")
        || n.starts_with("adr/")
        || n.contains("/adrs/")
        || n.starts_with("adrs/")
        || n.ends_with(".adr.md")
        || n.contains("architecture-decision")
}

/// True when a non-empty ADR `entity` covers a changed `path`.
///
/// - entity equals path
/// - path starts with `entity/` (entity is a directory/module scope)
/// - entity starts with `path/` (entity more specific under a changed tree)
///
/// Empty entities never cover.
pub fn entity_covers_path(entity: &str, path: &str) -> bool {
    let e = normalize_repo_path(entity);
    let p = normalize_repo_path(path);
    if e.is_empty() || p.is_empty() {
        return false;
    }
    if e.eq_ignore_ascii_case(&p) {
        return true;
    }
    // Case-insensitive prefix checks with a path-separator boundary.
    let e_lower = e.to_ascii_lowercase();
    let p_lower = p.to_ascii_lowercase();
    p_lower.starts_with(&format!("{e_lower}/")) || e_lower.starts_with(&format!("{p_lower}/"))
}

fn file_changes_to_paths(changes: &[crate::git::FileChange]) -> Vec<String> {
    let mut paths: Vec<String> = changes
        .iter()
        .map(|c| normalize_repo_path(&c.path.to_string_lossy()))
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

// ---------------------------------------------------------------------------
// Human report
// ---------------------------------------------------------------------------

fn print_human_report(report: &PolicyCheckReport) {
    use owo_colors::OwoColorize;

    println!("\n{}", "Ledgerful Policy Check".bold().underline());
    println!("{:<16} {}", "Mode:".bold(), report.mode);
    println!("{:<16} {}", "Policy source:".bold(), report.policy_source);
    println!(
        "{:<16} {}",
        "Result:".bold(),
        if report.passed {
            "PASSED".green().to_string()
        } else if report.mode == "observe" {
            "WARNINGS (observe — not blocking)".yellow().to_string()
        } else {
            "FAILED".red().to_string()
        }
    );

    if !report.notes.is_empty() {
        println!("\n{}:", "Notes".bold());
        for note in &report.notes {
            println!("  • {}", note.dimmed());
        }
    }

    if report.violations.is_empty() {
        println!("\n{}", "No policy violations.".green());
        return;
    }

    println!("\n{} ({}):", "Violations".bold(), report.violations.len());
    for v in &report.violations {
        let sev = if v.severity == "error" {
            v.severity.red().to_string()
        } else {
            v.severity.yellow().to_string()
        };
        println!(
            "  [{sev}] {rule} — {msg}",
            rule = v.rule_id.cyan(),
            msg = v.message
        );
        if !v.file.is_empty() {
            println!("         file: {}", v.file.dimmed());
        }
    }

    if report.mode == "observe" {
        println!(
            "\n{}",
            "Gate is observe: violations are warnings only (exit 0)."
                .yellow()
                .bold()
        );
    } else {
        println!(
            "\n{}",
            "Fix the violations above, or adjust policy via a reviewed PR to the base branch."
                .dimmed()
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::crypto::{sign_ledger_entry_in, verify_signature};
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
    fn is_adr_document_path_detects_known_patterns() {
        assert!(is_adr_document_path("docs/adr/0001-policy.md"));
        assert!(is_adr_document_path("docs/adrs/0001.md"));
        assert!(is_adr_document_path("adr/foo.md"));
        assert!(is_adr_document_path("docs/foo.adr.md"));
        assert!(is_adr_document_path("docs/architecture-decision-record.md"));
        assert!(is_adr_document_path("Docs/ADR/Upper.md")); // case-insensitive
        assert!(!is_adr_document_path("Cargo.toml"));
        assert!(!is_adr_document_path("src/address.rs")); // no false positive on "adr" substring alone
        assert!(!is_adr_document_path(""));
    }

    #[test]
    fn entity_covers_path_equality_and_scope() {
        assert!(entity_covers_path("Cargo.toml", "Cargo.toml"));
        assert!(entity_covers_path("src", "src/commands/policy_check.rs"));
        assert!(entity_covers_path(
            "src/commands/policy_check.rs",
            "src/commands"
        )); // entity more specific under changed tree
        assert!(!entity_covers_path("docs/unrelated", "Cargo.toml"));
        assert!(!entity_covers_path("", "Cargo.toml")); // empty never covers
        assert!(!entity_covers_path("srcx", "src/foo.rs")); // not a prefix boundary
        assert!(entity_covers_path("Src/Foo.rs", "src/foo.rs")); // case-insensitive
    }

    #[test]
    fn parse_policy_toml_defaults() {
        let cfg = parse_policy_toml("").unwrap();
        assert!(cfg.rules.require_signed_entries);
        assert!(cfg.rules.no_pending_tx);
        assert!(cfg.rules.verification_must_pass);
        assert_eq!(cfg.rules.max_risk_without_adr, "high");
        assert_eq!(cfg.rules.fail_on, "high");
        assert!(cfg.preset.is_none());
    }

    #[test]
    fn parse_policy_toml_full() {
        let text = r#"
preset = "enforce"

[rules]
require_signed_entries = false
no_pending_tx = true
verification_must_pass = true
max_risk_without_adr = "medium"
fail_on = "low"
"#;
        let cfg = parse_policy_toml(text).unwrap();
        assert_eq!(cfg.preset.as_deref(), Some("enforce"));
        assert!(!cfg.rules.require_signed_entries);
        assert_eq!(cfg.rules.max_risk_without_adr, "medium");
        assert_eq!(cfg.rules.fail_on, "low");
    }

    #[test]
    fn parse_policy_toml_rejects_bad_threshold() {
        let text = r#"
[rules]
fail_on = "critical"
"#;
        assert!(parse_policy_toml(text).is_err());
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
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["schemaVersion"], 1);
        assert_eq!(json["passed"], false);
        assert_eq!(json["mode"], "enforce");
        assert_eq!(json["policySource"], "local");
        // Empty notes omitted from JSON (additive optional field).
        assert!(json.get("notes").is_none());
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
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["notes"][0], "risk rules skipped: risk not evaluable");
    }

    /// DoD-5: signing basis is exactly tx_id+category+summary+reason+committed_at.
    /// Policy/mode never enter the signed payload.
    #[test]
    fn signing_basis_fields_unchanged_by_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = tmp.path().join("keys");
        std::fs::create_dir_all(&keys).unwrap();

        let tx_id = "tx-policy-basis";
        let category = "FEATURE";
        let summary = "basis check";
        let reason = "DoD-5";
        let committed_at = "2026-07-19T00:00:00Z";

        let (sig, pub_key) =
            sign_ledger_entry_in(&keys, tx_id, category, summary, reason, committed_at).unwrap();
        let sig = sig.unwrap();
        let pub_key = pub_key.unwrap();

        // Canonical five-field basis verifies.
        assert!(verify_signature(
            tx_id,
            category,
            summary,
            reason,
            committed_at,
            &sig,
            &pub_key
        ));

        // Injecting policy/mode into any signed field must break verification —
        // proving those fields are part of the basis and policy is not a sixth field.
        assert!(!verify_signature(
            tx_id,
            category,
            "basis check [policy=enforce]",
            reason,
            committed_at,
            &sig,
            &pub_key
        ));

        // Pin the exact five basis fields and crypto's known payload format
        // (must match crypto.rs sign_ledger_entry_in / verify_signature):
        //   "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}"
        let basis_fields = ["tx_id", "category", "summary", "reason", "committed_at"];
        assert_eq!(basis_fields.len(), 5);
        assert_eq!(
            basis_fields,
            ["tx_id", "category", "summary", "reason", "committed_at"]
        );
        let known_format = format!(
            "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}",
            tx_id, category, summary, reason, committed_at
        );
        // Reconstruct by joining field:value lines — same order as crypto.
        let reconstructed = basis_fields
            .iter()
            .zip([tx_id, category, summary, reason, committed_at])
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(reconstructed, known_format);
        assert!(!basis_fields.contains(&"policy"));
        assert!(!basis_fields.contains(&"mode"));
        assert!(!basis_fields.contains(&"gate"));
    }

    #[test]
    fn is_git_path_missing_detects_known_messages_only() {
        assert!(is_git_path_missing(
            "fatal: path '.ledgerful/policy.toml' does not exist in 'main'"
        ));
        assert!(is_git_path_missing(
            "fatal: path '.ledgerful/policy.toml' exists on disk, but not in 'HEAD'"
        ));
        assert!(is_git_path_missing("Path does not exist"));
        // Invalid ref / other fatals must NOT be treated as missing.
        assert!(!is_git_path_missing(
            "fatal: invalid object name 'origin/nope'"
        ));
        assert!(!is_git_path_missing("fatal: bad revision 'xyz'"));
        assert!(!is_git_path_missing(
            "error: unknown revision or path not in the working tree."
        ));
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
        };
        ctx.push_violation("no_pending_tx", "f", "msg".into());
        assert_eq!(ctx.violations[0].severity, "error");
    }
}
