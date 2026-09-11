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

/// Greppable string for 0243 config-load absorb (stderr + tracing).
pub const CONFIG_LOAD_WARN: &str = "config load failed; using defaults";

/// Cooperative stop conditions for one history walk.
#[derive(Clone, Debug)]
pub struct AnalysisBudget {
    pub deadline: Option<Instant>,
    pub cancel: Arc<AtomicBool>,
}

impl AnalysisBudget {
    pub fn unlimited(cancel: Arc<AtomicBool>) -> Self {
        Self {
            deadline: None,
            cancel,
        }
    }

    /// `secs == 0` means no wall clock. Cancel is still honored.
    pub fn from_secs(secs: u64, cancel: Arc<AtomicBool>) -> Self {
        let deadline = if secs == 0 {
            None
        } else {
            Some(Instant::now() + Duration::from_secs(secs))
        };
        Self { deadline, cancel }
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
        }
    }

    pub fn should_stop(&self) -> Option<HistoryWalkStop> {
        if self.cancel.load(Ordering::Relaxed) {
            return Some(HistoryWalkStop::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Some(HistoryWalkStop::Budget);
        }
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
    pub commits_requested: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_walked: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_requested: Option<u64>,
    pub filter: CompletenessFilter,
    #[serde(skip_serializing_if = "is_false")]
    #[serde(default)]
    pub cache_hit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_secs: Option<u64>,
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
        commits_requested,
        commits_walked: Some(commits_walked),
        days_requested,
        filter,
        cache_hit: false,
        head,
        budget_secs,
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
        commits_requested,
        commits_walked: None,
        days_requested,
        filter,
        cache_hit: false,
        head: None,
        budget_secs,
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
    }
}
