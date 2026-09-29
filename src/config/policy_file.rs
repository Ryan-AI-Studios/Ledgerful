//! `.ledgerful/policy.toml` resolution (0449).
//!
//! File load, parse, and synthesized defaults. The policy check command
//! stays in the command layer. This is not `crate::policy` (rules.toml).

use crate::config::load::load_config;
use crate::state::layout::Layout;
use miette::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

const DEFAULT_POLICY_REL: &str = ".ledgerful/policy.toml";

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
pub(crate) fn resolve_policy(
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
    let repo_config = load_config(layout).unwrap_or_default();
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
    let output = crate::git::git_command()?
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
