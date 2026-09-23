//! Cooperative history-walk budget (0308).
//!
//! Wall-clock and Ctrl-C are checked **inside** `get_history_budgeted`, not
//! only at enrichment-provider boundaries (0034). `--timeout 0` / config `0`
//! / env `0` disables the wall clock; cancel still works.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::cli::HotspotIncludeScope;

/// Built-in default when CLI, env, and config are all unset.
pub const DEFAULT_HISTORY_BUDGET_SECS: u64 = 45;

/// Env override (step 2 of spec §3.2). Unparseable values warn and fall through.
pub const HISTORY_BUDGET_ENV: &str = "LEDGERFUL_HISTORY_BUDGET_SECS";

/// Prospective / explicit `--timeout` overall emit budget (0347). Distinct from history.
pub const DEFAULT_PROSPECTIVE_BUDGET_SECS: u64 = 25;

/// Env override for the overall emit budget. Unparseable values warn and fall through.
pub const PROSPECTIVE_BUDGET_ENV: &str = "LEDGERFUL_PROSPECTIVE_BUDGET_SECS";

/// Greppable stderr token when the overall emit deadline fires (0347). Never stdout.
pub const PROSPECTIVE_BUDGET_WARN: &str = "prospective analysis stopped: overall budget";

/// Review overall emit budget (0348). Distinct from history and prospective.
pub const DEFAULT_REVIEW_BUDGET_SECS: u64 = 25;

/// Env override for the review overall emit budget. Unparseable values warn and fall through.
pub const REVIEW_BUDGET_ENV: &str = "LEDGERFUL_REVIEW_BUDGET_SECS";

/// Greppable stderr token when a review overall emit deadline fires (0348). Never stdout.
pub const REVIEW_BUDGET_WARN: &str = "review stopped: overall budget";

/// Hotspots list/explain overall emit budget (0349). Distinct from history, prospective, and review.
pub const DEFAULT_HOTSPOTS_OVERALL_BUDGET_SECS: u64 = 25;

/// Env override for the hotspots overall emit budget. Unparseable values warn and fall through.
pub const HOTSPOTS_OVERALL_BUDGET_ENV: &str = "LEDGERFUL_HOTSPOTS_OVERALL_BUDGET_SECS";

/// Greppable stderr token when a hotspots overall emit deadline fires (0349). Never stdout.
pub const HOTSPOTS_BUDGET_WARN: &str = "hotspots stopped: overall budget";

/// Unscoped audit overall emit budget (0350). Distinct from history, prospective, review, and hotspots.
pub const DEFAULT_AUDIT_OVERALL_BUDGET_SECS: u64 = 25;

/// Env override for the unscoped-audit overall emit budget. Unparseable values warn and fall through.
pub const AUDIT_OVERALL_BUDGET_ENV: &str = "LEDGERFUL_AUDIT_OVERALL_BUDGET_SECS";

/// Greppable stderr token when an unscoped-audit overall emit deadline fires (0350). Never stdout.
pub const AUDIT_BUDGET_WARN: &str = "audit stopped: overall budget";

/// `deploy impact` overall emit budget (0358). Distinct from prospective / review / hotspots / audit.
pub const DEFAULT_DEPLOY_OVERALL_BUDGET_SECS: u64 = 25;

/// Env override for the deploy-impact overall emit budget. Unparseable values warn and fall through.
pub const DEPLOY_OVERALL_BUDGET_ENV: &str = "LEDGERFUL_DEPLOY_OVERALL_BUDGET_SECS";

/// Greppable stderr token when a deploy-impact overall emit deadline fires (0358). Never stdout.
pub const DEPLOY_BUDGET_WARN: &str = "deploy impact stopped: overall budget";

/// `bridge export --hotspots` overall emit budget (0394). Distinct from
/// history / prospective / review / hotspots / audit / deploy.
pub const DEFAULT_BRIDGE_EXPORT_OVERALL_BUDGET_SECS: u64 = 25;

/// Env override for the bridge-export overall emit budget. Unparseable values
/// warn and fall through.
pub const BRIDGE_EXPORT_OVERALL_BUDGET_ENV: &str = "LEDGERFUL_BRIDGE_EXPORT_OVERALL_BUDGET_SECS";

/// Greppable stderr token when a bridge-export overall emit deadline fires (0394).
/// Never stdout. Printed from `bridge/export.rs`, not the orchestrator.
pub const BRIDGE_EXPORT_BUDGET_WARN: &str = "bridge export stopped: overall budget";

/// Greppable string for 0243 config-load absorb (stderr + tracing).
pub const CONFIG_LOAD_WARN: &str = "config load failed; using defaults";

/// Cooperative stop conditions for one history walk.
#[derive(Clone, Debug)]
pub struct AnalysisBudget {
    pub deadline: Option<Instant>,
    pub cancel: Arc<AtomicBool>,
    /// Effective wall seconds that produced `deadline` (`None` when unlimited).
    pub budget_secs: Option<u64>,
}

impl AnalysisBudget {
    pub fn unlimited(cancel: Arc<AtomicBool>) -> Self {
        Self {
            deadline: None,
            cancel,
            budget_secs: None,
        }
    }

    /// `secs == 0` means no wall clock. Cancel is still honored.
    pub fn from_secs(secs: u64, cancel: Arc<AtomicBool>) -> Self {
        let deadline = if secs == 0 {
            None
        } else {
            Some(Instant::now() + Duration::from_secs(secs))
        };
        Self {
            deadline,
            cancel,
            budget_secs: (secs > 0).then_some(secs),
        }
    }

    /// Cap a history-walk budget by an overall emit Instant (0347).
    /// History `0` is unlimited except for a present overall Instant.
    pub fn capped_by_overall(
        history_secs: u64,
        overall: Option<Instant>,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        let now = Instant::now();
        let history_deadline = (history_secs > 0).then(|| now + Duration::from_secs(history_secs));
        let deadline = match (history_deadline, overall) {
            (None, None) => None,
            (Some(h), None) => Some(h),
            (None, Some(o)) => Some(o),
            (Some(h), Some(o)) => Some(h.min(o)),
        };
        let budget_secs = deadline.map(|d| {
            d.saturating_duration_since(now)
                .as_secs()
                .max(u64::from(d > now))
        });
        Self {
            deadline,
            cancel,
            budget_secs,
        }
    }

    /// Injected near-zero / already-expired deadline for tests (DoD-1).
    pub fn expired(cancel: Arc<AtomicBool>) -> Self {
        Self {
            deadline: Some(
                Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
            ),
            cancel,
            budget_secs: Some(0),
        }
    }

    pub fn should_stop(&self) -> Option<HistoryWalkStop> {
        match poll_overall_stop(self.deadline, &self.cancel) {
            Some(CompletenessStop::Cancelled) => Some(HistoryWalkStop::Cancelled),
            Some(CompletenessStop::Budget) => Some(HistoryWalkStop::Budget),
            Some(CompletenessStop::Error) | None => None,
        }
    }
}

/// Cooperative overall-emit poll (0389). Cancel wins over budget.
/// `None` means continue. 0390–0392 must reuse this helper — do not fork
/// a third Instant checker. Pass **`opts.overall_deadline`**, never the
/// 0034 `EnrichmentContext.deadline` federation backstop.
///
/// Review (`analysis_mode == "range"`) must not print
/// [`PROSPECTIVE_BUDGET_WARN`]; see [`overall_stop_stderr_token`].
pub fn poll_overall_stop(
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Option<CompletenessStop> {
    if cancel.load(Ordering::Relaxed) {
        Some(CompletenessStop::Cancelled)
    } else if overall_deadline_fired(deadline) {
        Some(CompletenessStop::Budget)
    } else {
        None
    }
}

/// Why a walk returned early or finished the requested window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryWalkStop {
    #[default]
    Complete,
    Budget,
    Cancelled,
}

impl HistoryWalkStop {
    pub fn as_completeness_stop(self) -> Option<CompletenessStop> {
        match self {
            HistoryWalkStop::Complete => None,
            HistoryWalkStop::Budget => Some(CompletenessStop::Budget),
            HistoryWalkStop::Cancelled => Some(CompletenessStop::Cancelled),
        }
    }
}

/// CLI / packet honesty object. Omit the whole object when the walk completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisCompleteness {
    pub stop: CompletenessStop,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits_requested: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_walked: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_requested: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<CompletenessFilter>,
    #[serde(skip_serializing_if = "is_false")]
    #[serde(default)]
    pub cache_hit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_secs: Option<u64>,
    /// `overall` when the emit deadline fired. Omit on history-only 0308 objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<CompletenessScope>,
    /// Stable provider slug (`federated`, `hotspots`, …). Omit when complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

/// Emit-deadline vs history-walk completeness (0347). History-only objects omit this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompletenessScope {
    Overall,
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompletenessStop {
    Budget,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompletenessFilter {
    Default,
    Tests,
    Docs,
    Vendor,
    Unfiltered,
    Session,
}

impl CompletenessFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Tests => "tests",
            Self::Docs => "docs",
            Self::Vendor => "vendor",
            Self::Unfiltered => "unfiltered",
            Self::Session => "session",
        }
    }
}

/// Query window / source label (0309). Always present on CLI live list,
/// session `hotspots`, and trend JSON. Distinct from omit-when-complete
/// [`AnalysisCompleteness`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HotspotProvenance {
    pub source: HotspotProvenanceSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_requested: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_requested: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<CompletenessFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_age_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_recorded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recorded_at: Option<String>,
    /// Human footer only (trend `days` is top-level JSON, not this key).
    #[serde(skip)]
    pub footer_days: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum HotspotProvenanceSource {
    #[default]
    Live,
    Trends,
}

/// Exact human footer templates (0309 spec §3.3).
pub fn format_provenance_footer(p: &HotspotProvenance) -> String {
    match p.source {
        HotspotProvenanceSource::Live => {
            let commits = p.commits_requested.unwrap_or(0);
            let days = p
                .days_requested
                .map(|n| format!(" · {n} days"))
                .unwrap_or_default();
            let filter = p
                .filter
                .map(CompletenessFilter::as_str)
                .unwrap_or("default");
            format!("Window: {commits} commits{days} · Filter: {filter} · Source: live")
        }
        HotspotProvenanceSource::Trends => {
            let days = p.footer_days.or(p.days_requested).unwrap_or(0);
            if p.delta_unit.is_some() {
                let limit = p.limit.unwrap_or(0);
                format!(
                    "Window: {days} days · Limit: {limit} · Source: trends · Delta: displayScore"
                )
            } else {
                format!("Window: {days} days · Source: trends")
            }
        }
    }
}

/// CLI > env > config.toml > 45. Unparseable env warns and falls through.
pub fn resolve_history_budget_secs(cli_timeout: Option<u64>, config_secs: u64) -> u64 {
    if let Some(cli) = cli_timeout {
        return cli;
    }
    match std::env::var(HISTORY_BUDGET_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    value = %raw,
                    "{HISTORY_BUDGET_ENV} is not a valid u64; falling through to config"
                );
                config_secs
            }
        },
        _ => config_secs,
    }
}

/// Resolve once into `Config.hotspots.history_budget_secs` (runtime only).
pub fn apply_resolved_history_budget(
    config: &mut crate::config::model::Config,
    cli_timeout: Option<u64>,
) {
    config.hotspots.history_budget_secs =
        resolve_history_budget_secs(cli_timeout, config.hotspots.history_budget_secs);
}

/// CLI > env > config.toml > 25. Unparseable env warns and falls through.
pub fn resolve_prospective_budget_secs(cli_timeout: Option<u64>, config_secs: u64) -> u64 {
    if let Some(cli) = cli_timeout {
        return cli;
    }
    match std::env::var(PROSPECTIVE_BUDGET_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    value = %raw,
                    "{PROSPECTIVE_BUDGET_ENV} is not a valid u64; falling through to config"
                );
                config_secs
            }
        },
        _ => config_secs,
    }
}

/// Resolve once into `Config.impact.prospective_budget_secs` (runtime only).
pub fn apply_resolved_prospective_budget(
    config: &mut crate::config::model::Config,
    cli_timeout: Option<u64>,
) {
    config.impact.prospective_budget_secs =
        resolve_prospective_budget_secs(cli_timeout, config.impact.prospective_budget_secs);
}

/// CLI > env > config.toml > 25. Unparseable env warns and falls through.
/// Distinct from [`resolve_prospective_budget_secs`] and [`resolve_review_budget_secs`].
pub fn resolve_hotspots_overall_budget_secs(cli_timeout: Option<u64>, config_secs: u64) -> u64 {
    if let Some(cli) = cli_timeout {
        return cli;
    }
    match std::env::var(HOTSPOTS_OVERALL_BUDGET_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    value = %raw,
                    "{HOTSPOTS_OVERALL_BUDGET_ENV} is not a valid u64; falling through to config"
                );
                config_secs
            }
        },
        _ => config_secs,
    }
}

/// True when an overall Instant has already elapsed.
pub fn overall_deadline_fired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|d| Instant::now() >= d)
}

/// CLI > env > config.toml > 25. Unparseable env warns and falls through.
/// Distinct from prospective / review / hotspots overall resolvers.
/// CLI > env > config.toml > 25. Unparseable env warns and falls through.
/// Distinct from prospective / review / hotspots / audit / deploy resolvers.
pub fn resolve_bridge_export_overall_budget_secs(
    cli_timeout: Option<u64>,
    config_secs: u64,
) -> u64 {
    if let Some(cli) = cli_timeout {
        return cli;
    }
    match std::env::var(BRIDGE_EXPORT_OVERALL_BUDGET_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    value = %raw,
                    "{BRIDGE_EXPORT_OVERALL_BUDGET_ENV} is not a valid u64; falling through to config"
                );
                config_secs
            }
        },
        _ => config_secs,
    }
}

pub fn resolve_audit_overall_budget_secs(cli_timeout: Option<u64>, config_secs: u64) -> u64 {
    if let Some(cli) = cli_timeout {
        return cli;
    }
    match std::env::var(AUDIT_OVERALL_BUDGET_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    value = %raw,
                    "{AUDIT_OVERALL_BUDGET_ENV} is not a valid u64; falling through to config"
                );
                config_secs
            }
        },
        _ => config_secs,
    }
}

/// CLI > env > config.toml > 25. Unparseable env warns and falls through.
/// Distinct from [`resolve_prospective_budget_secs`] and [`resolve_review_budget_secs`].
pub fn resolve_deploy_overall_budget_secs(cli_timeout: Option<u64>, config_secs: u64) -> u64 {
    if let Some(cli) = cli_timeout {
        return cli;
    }
    match std::env::var(DEPLOY_OVERALL_BUDGET_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    value = %raw,
                    "{DEPLOY_OVERALL_BUDGET_ENV} is not a valid u64; falling through to config"
                );
                config_secs
            }
        },
        _ => config_secs,
    }
}

/// CLI > env > config.toml > 25. Unparseable env warns and falls through.
/// Distinct from [`resolve_prospective_budget_secs`].
pub fn resolve_review_budget_secs(cli_timeout: Option<u64>, config_secs: u64) -> u64 {
    if let Some(cli) = cli_timeout {
        return cli;
    }
    match std::env::var(REVIEW_BUDGET_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    value = %raw,
                    "{REVIEW_BUDGET_ENV} is not a valid u64; falling through to config"
                );
                config_secs
            }
        },
        _ => config_secs,
    }
}

/// Stderr token for an overall budget stop. `range` (review) is owned by
/// `emit_review` — the orchestrator must not print the prospective token.
pub fn overall_stop_stderr_token(analysis_mode: &str) -> Option<&'static str> {
    if analysis_mode == "range" {
        None
    } else {
        Some(PROSPECTIVE_BUDGET_WARN)
    }
}

/// Overall seconds for this run: `prospective`, `working_tree`, and `base_ref`
/// resolve via CLI > env > config > 25. Unknown modes keep bare `cli_timeout`.
/// `Some(0)` disables the wall clock.
pub fn overall_budget_secs_for_mode(
    analysis_mode: &str,
    cli_timeout: Option<u64>,
    config_secs: u64,
) -> Option<u64> {
    if matches!(analysis_mode, "prospective" | "working_tree" | "base_ref") {
        Some(resolve_prospective_budget_secs(cli_timeout, config_secs))
    } else {
        cli_timeout
    }
}

/// Install Ctrl-C. Tolerates `MultipleHandlers` (ctrlc 3.5). Tests must not call this.
pub fn install_cancel_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&flag);
    let _ = ctrlc::set_handler(move || {
        signal.store(true, Ordering::SeqCst);
    });
    flag
}

pub fn completeness_for_walk(
    stop: HistoryWalkStop,
    commits_requested: u64,
    commits_walked: u64,
    days_requested: Option<u64>,
    filter: CompletenessFilter,
    head: Option<String>,
    budget_secs: Option<u64>,
) -> Option<AnalysisCompleteness> {
    let stop = stop.as_completeness_stop()?;
    Some(AnalysisCompleteness {
        stop,
        commits_requested: Some(commits_requested),
        commits_walked: Some(commits_walked),
        days_requested,
        filter: Some(filter),
        cache_hit: false,
        head,
        budget_secs,
        scope: None,
        stage: None,
    })
}

pub fn completeness_for_error(
    commits_requested: u64,
    days_requested: Option<u64>,
    filter: CompletenessFilter,
    budget_secs: Option<u64>,
) -> AnalysisCompleteness {
    AnalysisCompleteness {
        stop: CompletenessStop::Error,
        commits_requested: Some(commits_requested),
        commits_walked: None,
        days_requested,
        filter: Some(filter),
        cache_hit: false,
        head: None,
        budget_secs,
        scope: None,
        stage: None,
    }
}

/// Overall emit-deadline completeness (0347). Omits history-walk keys.
pub fn completeness_for_overall(
    stop: CompletenessStop,
    budget_secs: Option<u64>,
    stage: &str,
) -> AnalysisCompleteness {
    AnalysisCompleteness {
        stop,
        commits_requested: None,
        commits_walked: None,
        days_requested: None,
        filter: None,
        cache_hit: false,
        head: None,
        budget_secs,
        scope: Some(CompletenessScope::Overall),
        stage: Some(stage.to_string()),
    }
}

/// True when the packet's completeness is an overall emit stop (skip durable persist).
pub fn is_overall_stop(c: &AnalysisCompleteness) -> bool {
    c.scope == Some(CompletenessScope::Overall)
}

/// Skip durable `save_packet` / `latest-impact.json` when the overall analysis
/// stopped **or** the cooperative cancel flag is set (0374).
///
/// Persist-only. Distinct from unscoped-audit leave-gates (**0376**).
pub fn should_skip_persist(
    completeness: Option<&AnalysisCompleteness>,
    cancel: &AtomicBool,
) -> bool {
    completeness.is_some_and(is_overall_stop) || cancel.load(Ordering::Relaxed)
}

/// Stable `stage` slugs for builtin enrichment providers (0347). Not `name()` prose.
pub fn stage_slug_for_provider(name: &str) -> &'static str {
    match name {
        "Federated Intelligence Enrichment Provider" => "federated",
        "API Enrichment Provider" => "api",
        "Data Model Enrichment Provider" => "data_models",
        "Contract Matching Enrichment Provider" => "contracts",
        "CI Gate Enrichment Provider" => "ci_gates",
        "Infrastructure Enrichment Provider" => "infrastructure",
        "Environment Enrichment Provider" => "environment",
        "Observability Enrichment Provider" => "observability",
        "Coupling Enrichment Provider" => "coupling",
        "Deployment Enrichment Provider" => "deploy",
        "CI Self-Awareness Enrichment Provider" => "ci_self_awareness",
        "CIPredictorProvider" => "ci_predictor",
        "Hotspot Enrichment Provider" => "hotspots",
        "Engineering Coverage Enrichment Provider" => "coverage",
        "Service Enrichment Provider" => "services",
        "Runtime Usage Enrichment Provider" => "runtime_usage",
        "Signature Delta Enrichment Provider" => "signature_delta",
        "DeadCode" => "dead_code",
        "KnowledgeGraph" => "kg",
        "ADR" => "adr",
        "Knowledge Enrichment Provider" => "knowledge",
        _ => "enrichment",
    }
}

pub fn filter_for_cli_include(include: Option<HotspotIncludeScope>) -> CompletenessFilter {
    match include {
        None => CompletenessFilter::Default,
        Some(HotspotIncludeScope::Tests) => CompletenessFilter::Tests,
        Some(HotspotIncludeScope::Docs) => CompletenessFilter::Docs,
        Some(HotspotIncludeScope::Vendor) => CompletenessFilter::Vendor,
    }
}

pub fn warn_history_truncated(
    surface: &str,
    stop: HistoryWalkStop,
    walked: usize,
    requested: usize,
) {
    if matches!(stop, HistoryWalkStop::Budget | HistoryWalkStop::Cancelled) {
        tracing::warn!(
            surface,
            ?stop,
            walked,
            requested,
            "history walk truncated; array/envelope stays unchanged"
        );
    }
}

pub fn eprint_walk_stop(stop: HistoryWalkStop, walked: usize, requested: usize) {
    if matches!(stop, HistoryWalkStop::Budget | HistoryWalkStop::Cancelled) {
        let kind = match stop {
            HistoryWalkStop::Budget => "budget",
            HistoryWalkStop::Cancelled => "cancelled",
            HistoryWalkStop::Complete => return,
        };
        eprintln!("history walk stopped ({kind}): walked {walked} of {requested} commits");
    }
}

#[cfg(test)]
pub mod test_hooks {
    use std::cell::Cell;

    thread_local! {
        static WALK_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    pub fn reset_walk_count() {
        WALK_COUNT.with(|c| c.set(0));
    }

    pub fn walk_count() -> usize {
        WALK_COUNT.with(|c| c.get())
    }

    pub fn incr_walk_count() {
        WALK_COUNT.with(|c| c.set(c.get().saturating_add(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    #[test]
    fn timeout_zero_disables_wall_budget() {
        let cancel = Arc::new(AtomicBool::new(false));
        let budget = AnalysisBudget::from_secs(0, Arc::clone(&cancel));
        assert!(budget.deadline.is_none());
        assert!(budget.should_stop().is_none());
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(budget.should_stop(), Some(HistoryWalkStop::Cancelled));
    }

    #[test]
    #[serial_test::serial(env)]
    fn history_budget_precedence_cli_env_config() {
        let _clear = TempEnv::remove(HISTORY_BUDGET_ENV);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        assert_eq!(resolve_history_budget_secs(None, 0), 0);
        let _env = TempEnv::set(HISTORY_BUDGET_ENV, "12");
        assert_eq!(resolve_history_budget_secs(None, 45), 12);
        assert_eq!(resolve_history_budget_secs(Some(3), 45), 3);
        drop(_env);
        let _bad = TempEnv::set(HISTORY_BUDGET_ENV, "nope");
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
    }

    #[test]
    fn completeness_omits_walked_on_error() {
        let c = completeness_for_error(500, None, CompletenessFilter::Default, Some(45));
        let v = serde_json::to_value(&c).expect("json");
        assert_eq!(v["stop"], "error");
        assert!(v.get("commitsWalked").is_none());
        assert!(v.get("cacheHit").is_none());
        assert!(v.get("scope").is_none());
        assert_eq!(v["filter"], "default");
        assert_eq!(v["commitsRequested"], 500);
    }

    #[test]
    #[allow(non_snake_case)]
    fn history_completeness__omits_scope() {
        completeness_omits_walked_on_error();
    }

    #[test]
    #[allow(non_snake_case)]
    fn poll_overall_stop__none_when_future() {
        let cancel = AtomicBool::new(false);
        let deadline = Some(Instant::now() + Duration::from_secs(30));
        assert!(poll_overall_stop(deadline, &cancel).is_none());
        assert!(poll_overall_stop(None, &cancel).is_none());
    }

    #[test]
    #[allow(non_snake_case)]
    fn poll_overall_stop__budget_when_elapsed() {
        let cancel = AtomicBool::new(false);
        let deadline = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
        );
        assert_eq!(
            poll_overall_stop(deadline, &cancel),
            Some(CompletenessStop::Budget)
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn poll_overall_stop__cancel_wins() {
        let cancel = AtomicBool::new(true);
        let deadline = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
        );
        assert_eq!(
            poll_overall_stop(deadline, &cancel),
            Some(CompletenessStop::Cancelled)
        );
        let future = Some(Instant::now() + Duration::from_secs(30));
        assert_eq!(
            poll_overall_stop(future, &cancel),
            Some(CompletenessStop::Cancelled)
        );
    }

    #[test]
    fn completeness_for_overall_omits_filter_and_commits() {
        let c = completeness_for_overall(CompletenessStop::Budget, Some(25), "federated");
        let v = serde_json::to_value(&c).expect("json");
        assert_eq!(v["stop"], "budget");
        assert_eq!(v["scope"], "overall");
        assert_eq!(v["stage"], "federated");
        assert_eq!(v["budgetSecs"], 25);
        assert!(v.get("filter").is_none());
        assert!(v.get("commitsRequested").is_none());
        assert!(v.get("commitsWalked").is_none());
        assert!(is_overall_stop(&c));
    }

    #[test]
    #[allow(non_snake_case)]
    fn completeness_for_overall__omits_filter_and_commits_requested() {
        completeness_for_overall_omits_filter_and_commits();
    }

    #[test]
    #[allow(non_snake_case)]
    fn should_skip_persist__overall_or_cancel__true_otherwise_false() {
        let overall = completeness_for_overall(CompletenessStop::Budget, Some(8), "federated");
        let walk = completeness_for_walk(
            HistoryWalkStop::Cancelled,
            500,
            12,
            None,
            CompletenessFilter::Unfiltered,
            None,
            None,
        );
        let idle = AtomicBool::new(false);
        let cancelled = AtomicBool::new(true);
        assert!(should_skip_persist(Some(&overall), &idle));
        assert!(should_skip_persist(None, &cancelled));
        assert!(should_skip_persist(walk.as_ref(), &cancelled));
        assert!(!should_skip_persist(walk.as_ref(), &idle));
        assert!(!should_skip_persist(None, &idle));
        assert!(!is_overall_stop(walk.as_ref().expect("walk completeness")));
    }

    #[test]
    #[allow(non_snake_case)]
    fn working_tree_impact__omitted_timeout__uses_prospective_default() {
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", None, 25),
            Some(25)
        );
        assert_eq!(
            overall_budget_secs_for_mode("prospective", None, 25),
            Some(25)
        );
        assert_eq!(overall_budget_secs_for_mode("base_ref", None, 25), Some(25));
    }

    #[test]
    fn stage_slug_for_provider_pins_locked_tokens() {
        assert_eq!(
            stage_slug_for_provider("Federated Intelligence Enrichment Provider"),
            "federated"
        );
        assert_eq!(
            stage_slug_for_provider("Hotspot Enrichment Provider"),
            "hotspots"
        );
        assert_eq!(
            stage_slug_for_provider("Coupling Enrichment Provider"),
            "coupling"
        );
        assert_eq!(stage_slug_for_provider("KnowledgeGraph"), "kg");
        assert_eq!(stage_slug_for_provider("FlagSpy"), "enrichment");
        let contract = include_str!("../../docs/agent-output-contract.md");
        for slug in [
            "federated",
            "api",
            "data_models",
            "contracts",
            "ci_gates",
            "infrastructure",
            "environment",
            "observability",
            "coupling",
            "deploy",
            "ci_self_awareness",
            "ci_predictor",
            "hotspots",
            "coverage",
            "services",
            "runtime_usage",
            "signature_delta",
            "dead_code",
            "kg",
            "adr",
            "knowledge",
            "enrichment",
            "analysis",
        ] {
            assert!(
                contract.contains(&format!("`{slug}`")),
                "agent-output-contract must name stage slug {slug}"
            );
        }
    }

    #[test]
    fn capped_by_overall_uses_min() {
        let cancel = Arc::new(AtomicBool::new(false));
        let overall = Instant::now() + Duration::from_secs(25);
        let b = AnalysisBudget::capped_by_overall(45, Some(overall), Arc::clone(&cancel));
        let secs = b.budget_secs.expect("capped secs");
        assert!(secs <= 25, "capped history must be ≤ overall, got {secs}");
        let unlimited_history =
            AnalysisBudget::capped_by_overall(0, Some(overall), Arc::clone(&cancel));
        assert!(unlimited_history.budget_secs.is_some());
        let no_overall = AnalysisBudget::capped_by_overall(45, None, cancel);
        assert_eq!(no_overall.budget_secs, Some(45));
    }

    #[test]
    #[serial_test::serial(env)]
    fn prospective_budget_precedence_cli_env_config() {
        let _clear = TempEnv::remove(PROSPECTIVE_BUDGET_ENV);
        assert_eq!(resolve_prospective_budget_secs(None, 25), 25);
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", None, 25),
            Some(25)
        );
        assert_eq!(
            overall_budget_secs_for_mode("prospective", None, 25),
            Some(25)
        );
        let _env = TempEnv::set(PROSPECTIVE_BUDGET_ENV, "12");
        assert_eq!(resolve_prospective_budget_secs(None, 25), 12);
        assert_eq!(resolve_prospective_budget_secs(Some(3), 25), 3);
        drop(_env);
        let _bad = TempEnv::set(PROSPECTIVE_BUDGET_ENV, "nope");
        assert_eq!(resolve_prospective_budget_secs(None, 25), 25);
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", Some(8), 25),
            Some(8)
        );
    }

    #[test]
    #[allow(non_snake_case)]
    #[serial_test::serial(env)]
    fn overall_budget_secs_for_mode__working_tree_and_base_ref_precedence() {
        let _clear = TempEnv::remove(PROSPECTIVE_BUDGET_ENV);
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", None, 25),
            Some(25)
        );
        assert_eq!(overall_budget_secs_for_mode("base_ref", None, 25), Some(25));
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", None, 40),
            Some(40)
        );
        assert_eq!(overall_budget_secs_for_mode("base_ref", None, 40), Some(40));
        let _env = TempEnv::set(PROSPECTIVE_BUDGET_ENV, "12");
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", None, 25),
            Some(12)
        );
        assert_eq!(overall_budget_secs_for_mode("base_ref", None, 25), Some(12));
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", Some(3), 25),
            Some(3)
        );
        assert_eq!(
            overall_budget_secs_for_mode("base_ref", Some(3), 25),
            Some(3)
        );
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", Some(0), 25),
            Some(0)
        );
        assert_eq!(
            overall_budget_secs_for_mode("base_ref", Some(0), 25),
            Some(0)
        );
        drop(_env);
        let _bad = TempEnv::set(PROSPECTIVE_BUDGET_ENV, "nope");
        assert_eq!(
            overall_budget_secs_for_mode("working_tree", None, 25),
            Some(25)
        );
        assert_eq!(overall_budget_secs_for_mode("base_ref", None, 25), Some(25));
        assert_eq!(
            overall_budget_secs_for_mode("range", None, 25),
            None,
            "unknown modes stay bare cli_timeout"
        );
    }

    #[test]
    fn skip_unindexed_complexity_fallback_still_follows_overall_deadline() {
        let src = include_str!("enrichment/hotspots.rs");
        assert!(
            src.contains("skip_unindexed_complexity_fallback: context.overall_deadline.is_some()"),
            "hang cure: Instant present must skip the unindexed symbols fallback"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    #[serial_test::serial(env)]
    fn resolve_hotspots_overall_budget_secs__cli_timeout_does_not_write_history_budget() {
        let _clear = TempEnv::remove(HOTSPOTS_OVERALL_BUDGET_ENV);
        let _hist = TempEnv::remove(HISTORY_BUDGET_ENV);
        assert_eq!(resolve_hotspots_overall_budget_secs(None, 25), 25);
        assert_eq!(resolve_hotspots_overall_budget_secs(Some(0), 25), 0);
        assert_eq!(resolve_hotspots_overall_budget_secs(Some(5), 25), 5);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        let _env = TempEnv::set(HOTSPOTS_OVERALL_BUDGET_ENV, "12");
        assert_eq!(resolve_hotspots_overall_budget_secs(None, 25), 12);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        drop(_env);
        let _bad = TempEnv::set(HOTSPOTS_OVERALL_BUDGET_ENV, "nope");
        assert_eq!(resolve_hotspots_overall_budget_secs(None, 25), 25);
    }

    #[test]
    #[allow(non_snake_case)]
    #[serial_test::serial(env)]
    fn resolve_audit_overall_budget_secs__cli_timeout_does_not_write_history_budget() {
        let _clear = TempEnv::remove(AUDIT_OVERALL_BUDGET_ENV);
        let _hist = TempEnv::remove(HISTORY_BUDGET_ENV);
        assert_eq!(resolve_audit_overall_budget_secs(None, 25), 25);
        assert_eq!(resolve_audit_overall_budget_secs(Some(0), 25), 0);
        assert_eq!(resolve_audit_overall_budget_secs(Some(5), 25), 5);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        let _env = TempEnv::set(AUDIT_OVERALL_BUDGET_ENV, "12");
        assert_eq!(resolve_audit_overall_budget_secs(None, 25), 12);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        drop(_env);
        let _bad = TempEnv::set(AUDIT_OVERALL_BUDGET_ENV, "nope");
        assert_eq!(resolve_audit_overall_budget_secs(None, 25), 25);
        assert_eq!(AUDIT_BUDGET_WARN, "audit stopped: overall budget");
        assert_eq!(DEFAULT_AUDIT_OVERALL_BUDGET_SECS, 25);
    }

    #[test]
    #[allow(non_snake_case)]
    #[serial_test::serial(env)]
    fn resolve_bridge_export_overall_budget_secs__cli_timeout_does_not_write_history_budget() {
        let _clear = TempEnv::remove(BRIDGE_EXPORT_OVERALL_BUDGET_ENV);
        let _hist = TempEnv::remove(HISTORY_BUDGET_ENV);
        assert_eq!(resolve_bridge_export_overall_budget_secs(None, 25), 25);
        assert_eq!(resolve_bridge_export_overall_budget_secs(Some(0), 25), 0);
        assert_eq!(resolve_bridge_export_overall_budget_secs(Some(5), 25), 5);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        let _env = TempEnv::set(BRIDGE_EXPORT_OVERALL_BUDGET_ENV, "12");
        assert_eq!(resolve_bridge_export_overall_budget_secs(None, 25), 12);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        drop(_env);
        let _bad = TempEnv::set(BRIDGE_EXPORT_OVERALL_BUDGET_ENV, "nope");
        assert_eq!(resolve_bridge_export_overall_budget_secs(None, 25), 25);
        assert_eq!(
            BRIDGE_EXPORT_BUDGET_WARN,
            "bridge export stopped: overall budget"
        );
        assert_ne!(BRIDGE_EXPORT_BUDGET_WARN, PROSPECTIVE_BUDGET_WARN);
        assert_eq!(DEFAULT_BRIDGE_EXPORT_OVERALL_BUDGET_SECS, 25);
        assert_eq!(
            overall_stop_stderr_token("working_tree"),
            Some(PROSPECTIVE_BUDGET_WARN)
        );
        assert_eq!(
            overall_stop_stderr_token("bridge_export"),
            Some(PROSPECTIVE_BUDGET_WARN)
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn hotspots_completeness_for_overall__stage_in_hotspots_coupling_git_storage_semantic() {
        for stage in ["hotspots", "coupling", "git", "storage", "semantic"] {
            let c = completeness_for_overall(CompletenessStop::Budget, Some(25), stage);
            let v = serde_json::to_value(&c).expect("json");
            assert_eq!(v["scope"], "overall");
            assert_eq!(v["stage"], stage);
            assert_eq!(v["stop"], "budget");
            assert_eq!(v["budgetSecs"], 25);
            assert!(v.get("filter").is_none());
        }
        assert_eq!(HOTSPOTS_BUDGET_WARN, "hotspots stopped: overall budget");
        assert!(!overall_deadline_fired(None));
        assert!(overall_deadline_fired(Some(
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now)
        )));
    }

    #[test]
    #[allow(non_snake_case)]
    #[serial_test::serial(env)]
    fn resolve_deploy_overall_budget_secs__cli_timeout_does_not_write_history_budget() {
        let _clear = TempEnv::remove(DEPLOY_OVERALL_BUDGET_ENV);
        let _hist = TempEnv::remove(HISTORY_BUDGET_ENV);
        assert_eq!(resolve_deploy_overall_budget_secs(None, 25), 25);
        assert_eq!(resolve_deploy_overall_budget_secs(Some(0), 25), 0);
        assert_eq!(resolve_deploy_overall_budget_secs(Some(5), 25), 5);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        let _env = TempEnv::set(DEPLOY_OVERALL_BUDGET_ENV, "12");
        assert_eq!(resolve_deploy_overall_budget_secs(None, 25), 12);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        drop(_env);
        let _bad = TempEnv::set(DEPLOY_OVERALL_BUDGET_ENV, "nope");
        assert_eq!(resolve_deploy_overall_budget_secs(None, 25), 25);
        assert_eq!(DEPLOY_BUDGET_WARN, "deploy impact stopped: overall budget");
    }

    #[test]
    #[allow(non_snake_case)]
    #[serial_test::serial(env)]
    fn resolve_review_budget_secs__cli_timeout_does_not_write_history_budget() {
        let _clear = TempEnv::remove(REVIEW_BUDGET_ENV);
        let _hist = TempEnv::remove(HISTORY_BUDGET_ENV);
        assert_eq!(resolve_review_budget_secs(None, 25), 25);
        assert_eq!(resolve_review_budget_secs(Some(0), 25), 0);
        assert_eq!(resolve_review_budget_secs(Some(5), 25), 5);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        let _env = TempEnv::set(REVIEW_BUDGET_ENV, "12");
        assert_eq!(resolve_review_budget_secs(None, 25), 12);
        assert_eq!(resolve_history_budget_secs(None, 45), 45);
        drop(_env);
        let _bad = TempEnv::set(REVIEW_BUDGET_ENV, "nope");
        assert_eq!(resolve_review_budget_secs(None, 25), 25);
    }

    #[test]
    #[allow(non_snake_case)]
    fn apply_overall_stop__range_mode__does_not_eprintln_prospective_token() {
        assert_eq!(overall_stop_stderr_token("range"), None);
        assert_eq!(
            overall_stop_stderr_token("prospective"),
            Some(PROSPECTIVE_BUDGET_WARN)
        );
        assert_eq!(
            overall_stop_stderr_token("working_tree"),
            Some(PROSPECTIVE_BUDGET_WARN)
        );
    }

    #[test]
    fn format_provenance_footer_exact_strings() {
        let live = HotspotProvenance {
            source: HotspotProvenanceSource::Live,
            commits_requested: Some(500),
            filter: Some(CompletenessFilter::Default),
            ..HotspotProvenance::default()
        };
        assert_eq!(
            format_provenance_footer(&live),
            "Window: 500 commits · Filter: default · Source: live"
        );
        let live_days = HotspotProvenance {
            source: HotspotProvenanceSource::Live,
            commits_requested: Some(50),
            days_requested: Some(30),
            filter: Some(CompletenessFilter::Session),
            ..HotspotProvenance::default()
        };
        assert_eq!(
            format_provenance_footer(&live_days),
            "Window: 50 commits · 30 days · Filter: session · Source: live"
        );
        let summary = HotspotProvenance {
            source: HotspotProvenanceSource::Trends,
            limit: Some(20),
            delta_unit: Some("displayScore".to_string()),
            footer_days: Some(30),
            ..HotspotProvenance::default()
        };
        assert_eq!(
            format_provenance_footer(&summary),
            "Window: 30 days · Limit: 20 · Source: trends · Delta: displayScore"
        );
        let full = HotspotProvenance {
            source: HotspotProvenanceSource::Trends,
            footer_days: Some(14),
            ..HotspotProvenance::default()
        };
        assert_eq!(
            format_provenance_footer(&full),
            "Window: 14 days · Source: trends"
        );
    }
}
