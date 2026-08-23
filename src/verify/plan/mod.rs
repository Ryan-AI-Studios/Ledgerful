use crate::config::model::VerifyConfig;
use crate::impact::packet::ImpactPacket;
use crate::verify::timeouts::DEFAULT_AUTO_TIMEOUT_SECS;
use serde::{Deserialize, Serialize};

/// Controls how broadly `ledgerful verify` selects tests.
///
/// `Fast` (the pre-push hook default) uses `test_mapping` to run only the
/// tests that cover the changed files. When scoped selection is impossible
/// for mapping reasons it **refuses** (does not surprise-run full) unless
/// `--allow-full-fallback` is set. Shared infrastructure still runs full.
/// Empty change sets use a cheap fmt+clippy plan (no nextest).
///
/// `Full` (the manual `ledgerful verify` default, and CI) runs the entire
/// suite regardless of scope — the safe backstop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VerifyScope {
    /// Scoped test selection via `test_mapping` (Tier 1). Refuses when mapping
    /// cannot scope (unless `--allow-full-fallback`); full suite only for
    /// shared infrastructure or explicit allow.
    Fast,
    /// Full suite — no scoping. Always used by CI.
    #[default]
    Full,
}

impl VerifyScope {
    pub fn is_fast(self) -> bool {
        matches!(self, Self::Fast)
    }
}

impl std::fmt::Display for VerifyScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fast => write!(f, "fast"),
            Self::Full => write!(f, "full"),
        }
    }
}

impl std::str::FromStr for VerifyScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "fast" => Ok(Self::Fast),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown verify scope '{}', expected 'fast' or 'full'",
                other
            )),
        }
    }
}

mod full;
mod non_code;
mod scoped;
mod shared_infra;

pub use full::{build_plan, resolve_default_test_command, resolve_doctest_command};
pub use scoped::{build_plan_scoped, build_plan_scoped_with_options};

pub(crate) use full::build_plan_with_scope;
#[cfg(test)]
pub(crate) use full::{append_full_tier_commands, nextest_has_profile};
pub(crate) use non_code::is_non_code_cheap_path;
#[cfg(test)]
pub(crate) use scoped::{
    build_scoped_nextest_command, is_test_mapping_stale, should_attempt_mapping_repair,
    test_file_to_nextest_stem,
};
#[cfg(test)]
pub(crate) use shared_infra::touches_shared_infra;

pub(super) fn format_fallback_reason(trigger: &str, consequence: &str) -> String {
    format!("fast scope unavailable — {trigger}; {consequence}")
}

/// MappingRefuse plan: explicit `refused=true`, empty steps, greppable reason.
pub(super) fn refuse_plan(trigger: &str) -> VerificationPlan {
    refuse_plan_for_trigger(trigger)
}

/// Public constructor for MappingRefuse (e.g. missing impact packet + dirty tree).
pub fn refuse_plan_for_trigger(trigger: &str) -> VerificationPlan {
    VerificationPlan {
        source: Some(PlanSource::AutoPolicy),
        steps: vec![],
        fallback_reason: Some(format_fallback_reason(
            trigger,
            "refusing full suite (~5-8 min)",
        )),
        refused: true,
    }
}

/// EmptyChanges cheap plan: for Rust repos, fmt + clippy only (no nextest).
/// Non-Rust (or undetected) profiles get zero steps and still pass — not a
/// refuse. Daily 5 honesty without inventing a cargo toolchain.
///
/// Public so `commands/verify` can short-circuit live-clean trees (0145 B1)
/// without re-entering the packet-based classifier.
pub fn build_empty_changes_plan(
    profile: &crate::platform::repository::RepositoryProfile,
) -> VerificationPlan {
    let mut steps: Vec<VerificationStep> = Vec::new();
    if profile.rust.is_some() {
        steps.push(VerificationStep {
            command: "cargo fmt --all -- --check".to_string(),
            timeout_secs: 60,
            description: "No changes: format check (scoped tests N/A)".to_string(),
            shell: false,
        });
        steps.push(VerificationStep {
            command: "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
            timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
            description: "No changes: lints (scoped tests N/A)".to_string(),
            shell: false,
        });
    }
    VerificationPlan {
        source: Some(PlanSource::AutoPolicy),
        steps,
        fallback_reason: None,
        refused: false,
    }
}
/// Classification of `test_mapping` freshness relative to the impact packet.
///
/// Preserves the 0135 B2 asymmetric matrix; used by the 0145 freshness gate so
/// HeadMismatch can auto-repair while Empty still requires `--auto-index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingFreshness {
    /// Stems trustworthy for the fast gate.
    Ok,
    /// `test_mapping` count is 0 / table missing / query error.
    Empty,
    /// count>0, both heads present and unequal.
    HeadMismatch,
    /// count>0, packet head missing, indexed head present (conservative).
    PacketHeadMissing,
}

/// Classify `test_mapping` freshness against the impact packet's `head_hash`.
///
/// Matrix (0135 B2 — asymmetric by design):
/// | Condition | class |
/// | count==0 / missing tables / query Err | Empty |
/// | both heads present equal | Ok |
/// | both present differ | HeadMismatch |
/// | **indexed head missing** + count>0 | **Ok** (unknown ≠ force-stale) |
/// | **packet head missing** + indexed present + count>0 | PacketHeadMissing |
/// | both missing + count>0 | Ok |
pub fn classify_test_mapping_freshness(
    conn: &rusqlite::Connection,
    packet: &ImpactPacket,
) -> MappingFreshness {
    let total: i64 = conn
        .query_row("SELECT count(*) FROM test_mapping", [], |row| row.get(0))
        .unwrap_or(0);
    if total == 0 {
        return MappingFreshness::Empty;
    }

    let indexed_head: Option<String> = conn
        .query_row(
            "SELECT value FROM index_metadata WHERE key = 'head_hash'",
            [],
            |row| row.get(0),
        )
        .ok();

    match (&packet.head_hash, indexed_head.as_deref()) {
        (Some(packet_head), Some(indexed)) if packet_head != indexed => {
            MappingFreshness::HeadMismatch
        }
        (Some(_), Some(_)) => MappingFreshness::Ok,
        // Index head missing + populated mapping: allow stem query (product
        // bug we fix by writing head_hash on index finish — not force-stale).
        (Some(_), None) => MappingFreshness::Ok,
        // Packet head missing + indexed present: cannot confirm freshness.
        (None, Some(_)) => MappingFreshness::PacketHeadMissing,
        // Both missing with count>0: treat as Ok so stem query can still
        // produce ScopedOk; empty count already returned Empty above.
        (None, None) => MappingFreshness::Ok,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationStep {
    pub command: String,
    pub timeout_secs: u64,
    pub description: String,
    /// When false (default), the command is parsed into argv tokens and
    /// executed directly. When true, the command is executed through a
    /// system shell (cmd /C on Windows, sh -c on Unix). Shell execution is
    /// an explicit opt-in because it exposes shell-injection risk.
    #[serde(default)]
    pub shell: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PlanSource {
    ExplicitConfig,
    AutoPolicy,
    HistoricalRulesFallback,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationPlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PlanSource>,
    pub steps: Vec<VerificationStep>,
    /// When the requested fast scope could not be honored and the plan fell
    /// back to the full suite (SharedInfra or `--allow-full-fallback`), this
    /// records the human-readable reason so the runner can announce it before
    /// executing. Also set on MappingRefuse with "refusing full suite…".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// True when fast scope refused full execution (MappingRefuse). Explicit
    /// discriminator — do not infer refuse from `steps.is_empty()` alone
    /// (vacuous `overall_pass` trap on empty results). Default false for
    /// serde / older plan JSON.
    #[serde(default)]
    pub refused: bool,
}

impl VerificationPlan {
    /// Reorder verification steps by Bayesian failure probability (0140).
    ///
    /// Returns `matched_steps` = count of plan steps whose
    /// [`verify_step_key`](crate::verify::probability::verify_step_key) is
    /// present in `probabilities` (history hit).
    ///
    /// - **matched_steps == 0:** no sort — preserve original plan order
    ///   (true stable; do not alphabetical-only reshuffle).
    /// - **matched_steps >= 1:** multi-band sort by
    ///   `(band(key), −P, command)` where band(cargo-fmt)=0,
    ///   band(cargo-clippy)=1, band(else)=2. Unmatched steps use P=0.0 for
    ///   comparison only. Full-scope cheap `git diff --check` may move later
    ///   when its P is low — intentional Bayesian fail-fast.
    pub fn apply_probability_ordering(
        &mut self,
        probabilities: &std::collections::HashMap<String, f64>,
    ) -> usize {
        use crate::verify::probability::{verify_step_band, verify_step_key};

        let matched_steps = self
            .steps
            .iter()
            .filter(|s| probabilities.contains_key(&verify_step_key(&s.command)))
            .count();

        if matched_steps == 0 {
            return 0;
        }

        self.steps.sort_by(|a, b| {
            let key_a = verify_step_key(&a.command);
            let key_b = verify_step_key(&b.command);
            let band_a = verify_step_band(&key_a);
            let band_b = verify_step_band(&key_b);
            let prob_a = probabilities.get(&key_a).copied().unwrap_or(0.0);
            let prob_b = probabilities.get(&key_b).copied().unwrap_or(0.0);

            band_a
                .cmp(&band_b)
                // Within band: higher P first (fail-fast)
                .then(
                    prob_b
                        .partial_cmp(&prob_a)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                // Deterministic tiebreak
                .then(a.command.cmp(&b.command))
        });

        matched_steps
    }
}

pub fn build_plan_from_config(config: &VerifyConfig) -> Option<VerificationPlan> {
    if config.steps.is_empty() {
        return None;
    }

    let steps = config
        .steps
        .iter()
        .map(|step| VerificationStep {
            command: step.command.clone(),
            timeout_secs: step.timeout_secs.unwrap_or(config.default_timeout_secs),
            description: if step.description.is_empty() {
                format!("From config: {}", step.command)
            } else {
                step.description.clone()
            },
            shell: step.shell,
        })
        .collect();

    Some(VerificationPlan {
        source: Some(PlanSource::ExplicitConfig),
        steps,
        fallback_reason: None,
        refused: false,
    })
}

#[cfg(test)]
mod tests;
