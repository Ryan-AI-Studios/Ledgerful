use crate::cli::{HotspotArgs, HotspotIncludeScope};
use crate::git::blob::head_path_exists;
use crate::impact::budget::{
    AnalysisBudget, AnalysisCompleteness, CompletenessStop, HistoryWalkStop, HotspotProvenance,
    HotspotProvenanceSource, completeness_for_overall, completeness_for_walk, eprint_walk_stop,
    filter_for_cli_include, format_provenance_footer, is_overall_stop, poll_overall_stop,
};
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots_detailed};
use crate::impact::packet::Hotspot;
use crate::impact::temporal::{GixHistoryProvider, TemporalEngine};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::state::storage::hotspot_trends::report_is_database_busy;
use chrono::{DateTime, Utc};
use miette::{IntoDiagnostic, Result};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

/// Truncate to `limit`, wrap as the hotspots list envelope, and echo `limit`.
/// Shared by list and `--semantic` JSON printers so the two arms cannot drift.
pub(super) fn wrap_hotspots_list_json<T: Serialize>(
    items: Vec<T>,
    limit: usize,
) -> serde_json::Value {
    wrap_hotspots_list_json_with_completeness(items, limit, None, None)
}

pub(super) fn wrap_hotspots_list_json_with_completeness<T: Serialize>(
    mut items: Vec<T>,
    limit: usize,
    completeness: Option<&crate::impact::budget::AnalysisCompleteness>,
    provenance: Option<&HotspotProvenance>,
) -> serde_json::Value {
    items.truncate(limit);
    let mut output = crate::output::empty::format_json_list_envelope(items, "files");
    if let Some(map) = output.as_object_mut() {
        map.insert("limit".to_string(), serde_json::json!(limit));
        if let Some(c) = completeness
            && let Ok(v) = serde_json::to_value(c)
        {
            map.insert("completeness".to_string(), v);
        }
        if let Some(p) = provenance
            && let Ok(v) = serde_json::to_value(p)
        {
            map.insert("provenance".to_string(), v);
        }
    }
    output
}

/// Emit-time `presence` only — never a field on shared [`Hotspot`].
pub(super) fn list_hotspot_json(repo: &gix::Repository, hotspot: &Hotspot) -> serde_json::Value {
    let mut value = match serde_json::to_value(hotspot) {
        Ok(v) => v,
        Err(_) => serde_json::json!({
            "path": hotspot.path.to_string_lossy(),
            "score": hotspot.score,
            "displayScore": hotspot.display_score,
            "complexity": hotspot.complexity,
            "frequency": hotspot.frequency,
        }),
    };
    if head_path_exists(repo, &hotspot.path.to_string_lossy()) == Some(false)
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert(
            "presence".to_string(),
            serde_json::Value::String("historical".to_string()),
        );
    }
    value
}

pub(super) fn latest_hotspot_history_timestamp(storage: &StorageManager) -> Option<String> {
    storage
        .get_connection()
        .query_row("SELECT MAX(timestamp) FROM hotspot_history", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten()
}

pub(super) fn snapshot_age_secs_from(snapshot_at: &str, now: DateTime<Utc>) -> Option<u64> {
    let parsed = DateTime::parse_from_rfc3339(snapshot_at).ok()?;
    let age = now.signed_duration_since(parsed.with_timezone(&Utc));
    Some(age.num_seconds().max(0) as u64)
}

pub(super) fn live_list_provenance(
    query: &HotspotQuery,
    include: Option<HotspotIncludeScope>,
    head: Option<String>,
    snapshot_at: Option<String>,
    now: DateTime<Utc>,
) -> HotspotProvenance {
    let (snapshot_at, snapshot_age_secs) = match snapshot_at {
        Some(ts) => match snapshot_age_secs_from(&ts, now) {
            Some(age) => (Some(ts), Some(age)),
            None => (None, None),
        },
        None => (None, None),
    };
    HotspotProvenance {
        source: HotspotProvenanceSource::Live,
        commits_requested: Some(query.commits as u64),
        days_requested: query.days,
        limit: Some(query.limit as u64),
        filter: Some(filter_for_cli_include(include)),
        head,
        snapshot_at,
        snapshot_age_secs,
        ..HotspotProvenance::default()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_hotspots_list(
    args: HotspotArgs,
    storage: &StorageManager,
    repo: &gix::Repository,
    config: &crate::config::model::Config,
    layout: &Layout,
    cancel: Arc<AtomicBool>,
    overall_deadline: Option<Instant>,
    overall_secs: u64,
    json_out: Option<&mut Vec<u8>>,
) -> Result<()> {
    if let HistoryPersistDecision::Error { message } =
        history_persist_decision(&HistoryPersistView {
            args: &args,
            configured_limit: config.hotspots.limit,
            configured_max_commits: config.hotspots.max_commits,
            auto_snapshot: config.hotspots.auto_snapshot,
            walk_stop: HistoryWalkStop::Complete,
            complexity_skipped: false,
            head_present: true,
            scores: &[],
            overall_stop: false,
            cancelled: false,
        })
    {
        return Err(miette::miette!("{message}"));
    }

    if args.semantic {
        if let Some(stop) = poll_overall_stop(overall_deadline, &cancel) {
            let completeness =
                completeness_for_overall(stop, Some(overall_secs).filter(|s| *s > 0), "semantic");
            super::eprint_hotspots_overall_stop_if_budget(Some(&completeness));
            if args.json {
                let limit = args.limit.unwrap_or(config.hotspots.limit);
                let output = wrap_hotspots_list_json_with_completeness(
                    Vec::<serde_json::Value>::new(),
                    limit,
                    Some(&completeness),
                    None,
                );
                return super::write_json(&output, json_out);
            }
            return Ok(());
        }
        let cozo = storage
            .cozo()
            .ok_or_else(|| miette::miette!("CozoDB storage not initialized"))?;

        if !args.json {
            println!("Analyzing semantic similarity hotspots (duplication)...");
        }

        let (matches, semantic_stop) = crate::semantic::hotspots::find_semantic_hotspots(
            cozo,
            layout.root.as_std_path(),
            0.85,
            overall_deadline,
            &cancel,
        )?;
        let completeness = semantic_stop.map(|stop| {
            completeness_for_overall(
                match stop {
                    crate::semantic::hotspots::SemanticStop::Budget => CompletenessStop::Budget,
                    crate::semantic::hotspots::SemanticStop::Cancelled => {
                        CompletenessStop::Cancelled
                    }
                },
                Some(overall_secs).filter(|s| *s > 0),
                "semantic",
            )
        });
        super::eprint_hotspots_overall_stop_if_budget(completeness.as_ref());

        if args.json {
            let limit = args.limit.unwrap_or(config.hotspots.limit);
            // find_semantic_hotspots ignores --limit; wrap_hotspots_list_json
            // truncates the already-computed Vec so echoing `limit` matches
            // the serialized `files` (no extra scan).
            let output = if completeness.is_some() {
                wrap_hotspots_list_json_with_completeness(
                    matches,
                    limit,
                    completeness.as_ref(),
                    None,
                )
            } else {
                wrap_hotspots_list_json(matches, limit)
            };
            return super::write_json(&output, json_out);
        }
        if semantic_stop.is_none() || !matches.is_empty() {
            crate::output::human::print_semantic_hotspots(&matches);
        }
        return Ok(());
    }

    if let Some(stop) = poll_overall_stop(overall_deadline, &cancel) {
        let completeness =
            completeness_for_overall(stop, Some(overall_secs).filter(|s| *s > 0), "git");
        super::eprint_hotspots_overall_stop_if_budget(Some(&completeness));
        if args.json {
            let limit = args.limit.unwrap_or(config.hotspots.limit);
            let output = wrap_hotspots_list_json_with_completeness(
                Vec::<serde_json::Value>::new(),
                limit,
                Some(&completeness),
                None,
            );
            return super::write_json(&output, json_out);
        }
        return Ok(());
    }

    let history_provider = GixHistoryProvider::new(repo);
    let (exclude_test_paths, exclude_docs_paths, docs_frequency_lane, exclude_vendor_paths) =
        match args.include {
            Some(HotspotIncludeScope::Tests) => (false, false, false, false),
            Some(HotspotIncludeScope::Docs) => (false, false, true, false),
            Some(HotspotIncludeScope::Vendor) => (true, true, false, false),
            None => (true, true, false, true),
        };
    let query = HotspotQuery {
        limit: args.limit.unwrap_or(config.hotspots.limit),
        commits: args.commits.unwrap_or(config.hotspots.max_commits),
        days: args.days.map(|d| d as u64),
        decay_half_life: config.hotspots.decay_half_life,
        dir_filter: args.entity.clone(),
        centrality: args.centrality,
        exclude_test_paths,
        exclude_docs_paths,
        docs_frequency_lane,
        exclude_vendor_paths,
        budget: Some(AnalysisBudget::capped_by_overall(
            config.hotspots.history_budget_secs,
            overall_deadline,
            cancel.clone(),
        )),
        skip_unindexed_complexity_fallback: overall_deadline.is_some(),
        ..Default::default()
    };

    let calculated = calculate_hotspots_detailed(storage, &history_provider, &query)?;
    let hotspots = calculated.hotspots;
    let completeness = list_completeness_after_walk(
        calculated.walk_stop,
        query.commits as u64,
        calculated.commits_walked as u64,
        query.days,
        filter_for_cli_include(args.include),
        calculated.head.clone(),
        config.hotspots.history_budget_secs,
        overall_deadline,
        overall_secs,
        &cancel,
    );
    super::eprint_hotspots_overall_stop_if_budget(completeness.as_ref());
    if !completeness.as_ref().is_some_and(is_overall_stop) {
        eprint_walk_stop(
            calculated.walk_stop,
            calculated.commits_walked,
            query.commits,
        );
    }

    let snapshot_stop = poll_overall_stop(overall_deadline, &cancel);
    let gate = post_walk_snapshot_gate(snapshot_stop, completeness.as_ref());
    if gate.eprint_budget {
        super::eprint_hotspots_overall_stop();
    }
    let scores: Vec<f32> = hotspots.iter().map(|hotspot| hotspot.score).collect();
    let mut written: Option<StorageManager> = None;
    let decision = history_persist_decision(&HistoryPersistView {
        args: &args,
        configured_limit: config.hotspots.limit,
        configured_max_commits: config.hotspots.max_commits,
        auto_snapshot: config.hotspots.auto_snapshot,
        walk_stop: calculated.walk_stop,
        complexity_skipped: calculated.complexity_skipped,
        head_present: calculated.head.is_some(),
        scores: &scores,
        overall_stop: gate.overall_stop,
        cancelled: gate.cancelled,
    });
    match decision {
        HistoryPersistDecision::Error { message } => return Err(miette::miette!("{message}")),
        HistoryPersistDecision::SnapshotSkip { line } => {
            if !args.json {
                println!("{line}");
            }
        }
        HistoryPersistDecision::SilentSkip => {}
        HistoryPersistDecision::WriteHistory | HistoryPersistDecision::WriteHistoryAndCouplings => {
            let write_couplings =
                matches!(decision, HistoryPersistDecision::WriteHistoryAndCouplings);
            let persist_budget = AnalysisBudget::capped_by_overall(
                config.hotspots.history_budget_secs,
                overall_deadline,
                cancel.clone(),
            );
            let attempt = |target: &StorageManager| {
                persist_hotspots_and_couplings(
                    target,
                    repo,
                    &hotspots,
                    config,
                    Some(&persist_budget),
                    write_couplings,
                )
            };
            if args.snapshot {
                if let Some(outcome) = settle_persist_attempt(true, attempt(storage))?
                    && !args.json
                {
                    println!("{}", format_snapshot_persist_message(&outcome));
                }
            } else if storage.is_read_only() {
                match StorageManager::init_with_layout(layout) {
                    Ok(writable) => {
                        if settle_persist_attempt(false, attempt(&writable))?.is_some() {
                            written = Some(writable);
                        }
                    }
                    Err(err) => warn_auto_snapshot(&err),
                }
            } else {
                let _ = settle_persist_attempt(false, attempt(storage))?;
            }
        }
    }

    let snapshot_at = if args.json {
        latest_hotspot_history_timestamp(written.as_ref().unwrap_or(storage))
    } else {
        None
    };
    let provenance = live_list_provenance(
        &query,
        args.include,
        calculated.head.clone(),
        snapshot_at,
        Utc::now(),
    );

    if args.json {
        let files: Vec<serde_json::Value> = hotspots
            .iter()
            .map(|h| list_hotspot_json(repo, h))
            .collect();
        let output = wrap_hotspots_list_json_with_completeness(
            files,
            query.limit,
            completeness.as_ref(),
            Some(&provenance),
        );
        super::write_json(&output, json_out)?;
    } else if args.centrality {
        crate::output::human::print_hotspots_table_with_centrality(&hotspots);
        print_omit_footers(
            calculated.omitted_test_paths,
            calculated.omitted_docs_paths,
            calculated.omitted_vendor_paths,
            docs_frequency_lane,
        );
        println!("{}", format_provenance_footer(&provenance));
    } else {
        crate::output::human::print_hotspots_table(&hotspots);
        print_omit_footers(
            calculated.omitted_test_paths,
            calculated.omitted_docs_paths,
            calculated.omitted_vendor_paths,
            docs_frequency_lane,
        );
        println!("{}", format_provenance_footer(&provenance));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn list_completeness_after_walk(
    walk_stop: crate::impact::budget::HistoryWalkStop,
    commits_requested: u64,
    commits_walked: u64,
    days_requested: Option<u64>,
    filter: crate::impact::budget::CompletenessFilter,
    head: Option<String>,
    history_budget_secs: u64,
    overall_deadline: Option<Instant>,
    overall_secs: u64,
    cancel: &AtomicBool,
) -> Option<crate::impact::budget::AnalysisCompleteness> {
    if let Some(stop) = poll_overall_stop(overall_deadline, cancel) {
        return Some(completeness_for_overall(
            stop,
            Some(overall_secs).filter(|s| *s > 0),
            "hotspots",
        ));
    }
    completeness_for_walk(
        walk_stop,
        commits_requested,
        commits_walked,
        days_requested,
        filter,
        head,
        Some(history_budget_secs).filter(|s| *s > 0),
    )
}

fn print_omit_footers(
    omitted_tests: usize,
    omitted_docs: usize,
    omitted_vendor: usize,
    docs_lane: bool,
) {
    if !docs_lane && let Some(footer) = omitted_hotspots_footer(omitted_tests) {
        println!("{footer}");
    }
    if let Some(footer) = omitted_docs_footer(omitted_docs) {
        println!("{footer}");
    }
    if !docs_lane && let Some(footer) = omitted_vendor_hotspots_footer(omitted_vendor) {
        println!("{footer}");
    }
}

pub(super) fn omitted_hotspots_footer(omitted: usize) -> Option<String> {
    if omitted == 0 {
        None
    } else {
        Some(format!(
            "{omitted} test/example files omitted; --include tests"
        ))
    }
}

pub(super) fn omitted_docs_footer(omitted: usize) -> Option<String> {
    if omitted == 0 {
        None
    } else {
        Some(format!(
            "{omitted} documentation files omitted; --include docs"
        ))
    }
}

pub(super) fn omitted_vendor_hotspots_footer(omitted: usize) -> Option<String> {
    if omitted == 0 {
        None
    } else {
        Some(format!(
            "{omitted} vendored files omitted; --include vendor"
        ))
    }
}

/// Why temporal coupling history was or was not written on a snapshot persist.
///
/// Persist-only (0375). Distinct from unscoped-audit leave-gates (0376).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CouplingsPersistOutcome {
    Persisted,
    InsufficientHistory,
    BudgetSkipped,
    Cancelled,
}

pub(super) fn format_snapshot_persist_message(outcome: &HotspotPersistOutcome) -> &'static str {
    if outcome.history == HistoryWrite::Unchanged {
        return "Hotspot snapshot unchanged (same scores).";
    }
    match outcome.couplings {
        Some(CouplingsPersistOutcome::Persisted) | None => {
            "Hotspot and temporal coupling snapshot persisted to SQLite."
        }
        Some(CouplingsPersistOutcome::InsufficientHistory) => {
            "Hotspot snapshot persisted to SQLite (temporal coupling history skipped: repository has fewer than 10 commits)."
        }
        Some(CouplingsPersistOutcome::BudgetSkipped) => {
            "Hotspot snapshot persisted to SQLite (temporal coupling history skipped: persist budget)."
        }
        Some(CouplingsPersistOutcome::Cancelled) => {
            "Hotspot snapshot persisted to SQLite (temporal coupling history skipped: cancelled)."
        }
    }
}

const SNAPSHOT_DOCS_ERROR: &str = "--snapshot cannot be combined with --include docs (docs lane score is frequency-only; hotspot_history stores f×c)";
const SNAPSHOT_TESTS_ERROR: &str = "--snapshot cannot be combined with --include tests (the audit view is not the repository budget series)";
const SNAPSHOT_SEMANTIC_ERROR: &str = "--snapshot cannot be combined with --semantic (the semantic ranking is not the repository budget series)";
const SNAPSHOT_FAMILY_ERROR: &str = "--snapshot requires the repository budget query (default filter, configured limit and max_commits, no --days, --entity, --since, or --all-parents)";
const SKIP_CANCELLED: &str = "Hotspot snapshot skipped: cancelled.";
const SKIP_OVERALL: &str = "Hotspot snapshot skipped: overall budget.";
const SKIP_HISTORY: &str = "Hotspot snapshot skipped: history budget.";
const SKIP_COMPLEXITY: &str = "Hotspot snapshot skipped: complexity was not scored.";
const SKIP_EMPTY: &str = "Hotspot snapshot skipped: no ranked files.";
const AUTO_SNAPSHOT_LOCKED_WARNING: &str =
    "[Ledgerful] WARNING: hotspot snapshot was not recorded (database locked).";
const AUTO_SNAPSHOT_WARNING: &str = "[Ledgerful] WARNING: hotspot snapshot was not recorded.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryWrite {
    Inserted,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HotspotPersistOutcome {
    pub history: HistoryWrite,
    pub couplings: Option<CouplingsPersistOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryPersistDecision {
    SilentSkip,
    SnapshotSkip { line: &'static str },
    Error { message: &'static str },
    WriteHistory,
    WriteHistoryAndCouplings,
}

pub(super) struct HistoryPersistView<'a> {
    pub args: &'a HotspotArgs,
    pub configured_limit: usize,
    pub configured_max_commits: usize,
    pub auto_snapshot: bool,
    pub walk_stop: HistoryWalkStop,
    pub complexity_skipped: bool,
    pub head_present: bool,
    pub scores: &'a [f32],
    pub overall_stop: bool,
    pub cancelled: bool,
}

fn scores_unscored(scores: &[f32]) -> bool {
    !scores.is_empty()
        && scores
            .iter()
            .all(|score| *score == 0.0 || !score.is_finite())
}

fn query_flags_are_canonical(view: &HistoryPersistView<'_>) -> bool {
    let args = view.args;
    let include_ok = args.include.is_none()
        || (args.snapshot && matches!(args.include, Some(HotspotIncludeScope::Vendor)));
    let limit_ok = args
        .limit
        .is_none_or(|limit| limit == view.configured_limit);
    let commits_ok = args
        .commits
        .is_none_or(|commits| commits == view.configured_max_commits);
    include_ok
        && !args.semantic
        && args.entity.is_none()
        && args.days.is_none()
        && args.since.is_none()
        && !args.all_parents
        && limit_ok
        && commits_ok
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PostWalkSnapshotGate {
    pub overall_stop: bool,
    pub cancelled: bool,
    pub eprint_budget: bool,
}

pub(super) fn post_walk_snapshot_gate(
    snapshot_stop: Option<CompletenessStop>,
    completeness: Option<&AnalysisCompleteness>,
) -> PostWalkSnapshotGate {
    let cancelled = snapshot_stop == Some(CompletenessStop::Cancelled)
        || completeness.is_some_and(|stop| stop.stop == CompletenessStop::Cancelled);
    let already_overall = completeness.is_some_and(is_overall_stop);
    let late_budget = snapshot_stop == Some(CompletenessStop::Budget);
    PostWalkSnapshotGate {
        overall_stop: already_overall || late_budget,
        cancelled,
        eprint_budget: late_budget && !already_overall && !cancelled,
    }
}

pub(super) fn history_persist_decision(view: &HistoryPersistView<'_>) -> HistoryPersistDecision {
    if view.args.snapshot {
        if matches!(view.args.include, Some(HotspotIncludeScope::Docs)) {
            return HistoryPersistDecision::Error {
                message: SNAPSHOT_DOCS_ERROR,
            };
        }
        if matches!(view.args.include, Some(HotspotIncludeScope::Tests)) {
            return HistoryPersistDecision::Error {
                message: SNAPSHOT_TESTS_ERROR,
            };
        }
        if view.args.semantic {
            return HistoryPersistDecision::Error {
                message: SNAPSHOT_SEMANTIC_ERROR,
            };
        }
        if !query_flags_are_canonical(view) {
            return HistoryPersistDecision::Error {
                message: SNAPSHOT_FAMILY_ERROR,
            };
        }
        if view.cancelled {
            return HistoryPersistDecision::SnapshotSkip {
                line: SKIP_CANCELLED,
            };
        }
        if view.overall_stop {
            return HistoryPersistDecision::SnapshotSkip { line: SKIP_OVERALL };
        }
        if view.walk_stop == HistoryWalkStop::Budget {
            return HistoryPersistDecision::SnapshotSkip { line: SKIP_HISTORY };
        }
        if view.walk_stop == HistoryWalkStop::Cancelled {
            return HistoryPersistDecision::SnapshotSkip {
                line: SKIP_CANCELLED,
            };
        }
        if view.walk_stop == HistoryWalkStop::Complete && view.scores.is_empty() {
            return HistoryPersistDecision::SnapshotSkip { line: SKIP_EMPTY };
        }
        if view.walk_stop == HistoryWalkStop::Complete
            && (view.complexity_skipped || scores_unscored(view.scores))
        {
            return HistoryPersistDecision::SnapshotSkip {
                line: SKIP_COMPLEXITY,
            };
        }
        if !view.head_present {
            return HistoryPersistDecision::SilentSkip;
        }
        return HistoryPersistDecision::WriteHistoryAndCouplings;
    }

    if !query_flags_are_canonical(view)
        || view.cancelled
        || view.overall_stop
        || view.walk_stop != HistoryWalkStop::Complete
        || view.complexity_skipped
        || view.scores.is_empty()
        || scores_unscored(view.scores)
        || !view.head_present
        || !view.auto_snapshot
    {
        return HistoryPersistDecision::SilentSkip;
    }
    HistoryPersistDecision::WriteHistory
}

pub(super) fn auto_snapshot_warning_line(err: &miette::Report) -> &'static str {
    if report_is_database_busy(err).is_some() {
        AUTO_SNAPSHOT_LOCKED_WARNING
    } else {
        AUTO_SNAPSHOT_WARNING
    }
}

fn warn_auto_snapshot(err: &miette::Report) {
    tracing::debug!("hotspot snapshot was not recorded: {err}");
    eprintln!("{}", auto_snapshot_warning_line(err));
}

/// Automatic persist failure warns and continues. Explicit `--snapshot` returns the error.
pub(super) fn settle_persist_attempt(
    snapshot: bool,
    result: Result<HotspotPersistOutcome>,
) -> Result<Option<HotspotPersistOutcome>> {
    match result {
        Ok(outcome) => Ok(Some(outcome)),
        Err(err) if !snapshot => {
            warn_auto_snapshot(&err);
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

enum PreparedCouplings {
    HistoryOnly,
    WithOutcome {
        outcome: CouplingsPersistOutcome,
        rows: Vec<crate::impact::packet::TemporalCoupling>,
    },
}

fn prepare_couplings(
    repo: &gix::Repository,
    config: &crate::config::model::Config,
    budget: Option<&AnalysisBudget>,
    write_couplings: bool,
) -> Result<PreparedCouplings> {
    if !write_couplings {
        return Ok(PreparedCouplings::HistoryOnly);
    }
    let history_provider = GixHistoryProvider::new(repo);
    let engine = TemporalEngine::new(history_provider, config.temporal.clone());
    match engine.calculate_couplings_budgeted(budget) {
        Ok(couplings) => {
            let outcome = match budget.and_then(|b| b.should_stop()) {
                Some(HistoryWalkStop::Cancelled) => CouplingsPersistOutcome::Cancelled,
                Some(HistoryWalkStop::Budget) => CouplingsPersistOutcome::BudgetSkipped,
                Some(HistoryWalkStop::Complete) | None => CouplingsPersistOutcome::Persisted,
            };
            let rows = if outcome == CouplingsPersistOutcome::Persisted {
                couplings
            } else {
                Vec::new()
            };
            Ok(PreparedCouplings::WithOutcome { outcome, rows })
        }
        Err(crate::git::GitError::InsufficientHistory { .. }) => {
            Ok(PreparedCouplings::WithOutcome {
                outcome: CouplingsPersistOutcome::InsufficientHistory,
                rows: Vec::new(),
            })
        }
        Err(err) => Err(miette::miette!(
            "Failed to calculate temporal couplings: {}",
            err
        )),
    }
}

fn sqlite_to_report(err: rusqlite::Error) -> miette::Report {
    match Err::<(), rusqlite::Error>(err).into_diagnostic() {
        Err(report) => report,
        Ok(()) => miette::miette!("sqlite error"),
    }
}

fn latest_history_tuples(conn: &rusqlite::Connection) -> Result<Vec<(String, f64)>> {
    let mut stmt = conn
        .prepare(
            "SELECT file_path, score FROM hotspot_history \
             WHERE timestamp = (SELECT MAX(timestamp) FROM hotspot_history) \
             ORDER BY file_path",
        )
        .into_diagnostic()?;
    let mapped = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .into_diagnostic()?;
    let mut tuples = Vec::new();
    for row in mapped {
        tuples.push(row.into_diagnostic()?);
    }
    Ok(tuples)
}

fn current_history_tuples(hotspots: &[Hotspot]) -> Vec<(String, f64)> {
    let mut tuples: Vec<(String, f64)> = hotspots
        .iter()
        .map(|hotspot| {
            (
                hotspot.path.to_string_lossy().to_string(),
                hotspot.score as f64,
            )
        })
        .collect();
    tuples.sort_by(|left, right| left.0.cmp(&right.0));
    tuples
}

fn write_snapshot_rows(
    conn: &rusqlite::Connection,
    hotspots: &[Hotspot],
    prepared: &PreparedCouplings,
) -> Result<HotspotPersistOutcome> {
    let previous = latest_history_tuples(conn)?;
    let current = current_history_tuples(hotspots);
    if !previous.is_empty() && previous == current {
        return Ok(HotspotPersistOutcome {
            history: HistoryWrite::Unchanged,
            couplings: None,
        });
    }

    let timestamp = Utc::now().to_rfc3339();
    let snapshot_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM snapshots ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();

    for hotspot in hotspots {
        conn.execute(
            "INSERT INTO hotspot_history (snapshot_id, file_path, score, display_score, complexity, frequency, centrality, timestamp) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                snapshot_id,
                hotspot.path.to_string_lossy().to_string(),
                hotspot.score as f64,
                hotspot.display_score,
                hotspot.complexity,
                hotspot.frequency,
                hotspot.centrality.map(|centrality| centrality as i64),
                timestamp
            ],
        )
        .into_diagnostic()?;
    }

    let couplings = match prepared {
        PreparedCouplings::HistoryOnly => None,
        PreparedCouplings::WithOutcome { outcome, rows } => {
            if *outcome == CouplingsPersistOutcome::Persisted {
                for coupling in rows {
                    conn.execute(
                        "INSERT INTO temporal_coupling_history (snapshot_id, file_a, file_b, score, timestamp) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![
                            snapshot_id,
                            coupling.file_a.to_string_lossy().to_string(),
                            coupling.file_b.to_string_lossy().to_string(),
                            coupling.score,
                            timestamp
                        ],
                    )
                    .into_diagnostic()?;
                }
            }
            Some(*outcome)
        }
    };

    Ok(HotspotPersistOutcome {
        history: HistoryWrite::Inserted,
        couplings,
    })
}

fn persist_owned_attempts(
    conn: &rusqlite::Connection,
    hotspots: &[Hotspot],
    prepared: &PreparedCouplings,
) -> Result<HotspotPersistOutcome> {
    for attempt in 0..=3 {
        let began = match conn.execute_batch("BEGIN IMMEDIATE;") {
            Ok(()) => true,
            Err(err) => {
                let report = sqlite_to_report(err);
                if report_is_database_busy(&report).is_some() && attempt < 3 {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                return Err(report);
            }
        };
        match write_snapshot_rows(conn, hotspots, prepared) {
            Ok(outcome) => match conn.execute_batch("COMMIT;") {
                Ok(()) => return Ok(outcome),
                Err(err) => {
                    if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                        return Err(sqlite_to_report(rollback_err));
                    }
                    let report = sqlite_to_report(err);
                    if report_is_database_busy(&report).is_some() && attempt < 3 {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    return Err(report);
                }
            },
            Err(err) => {
                if began && let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                    return Err(sqlite_to_report(rollback_err));
                }
                if report_is_database_busy(&err).is_some() && attempt < 3 {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                return Err(err);
            }
        }
    }
    Err(miette::miette!(
        "hotspot snapshot was not recorded (database locked)."
    ))
}

fn persist_in_owned_transaction(
    conn: &rusqlite::Connection,
    hotspots: &[Hotspot],
    prepared: &PreparedCouplings,
) -> Result<HotspotPersistOutcome> {
    conn.execute_batch("PRAGMA busy_timeout = 0;")
        .into_diagnostic()?;
    let work = persist_owned_attempts(conn, hotspots, prepared);
    let restored = conn.execute_batch("PRAGMA busy_timeout = 5000;");
    match (work, restored) {
        (Err(err), _) => Err(err),
        (Ok(_), Err(err)) => Err(err).into_diagnostic(),
        (Ok(outcome), Ok(())) => Ok(outcome),
    }
}

/// Persists hotspot history and, when `write_couplings` is set, temporal
/// couplings. Couplings are calculated before an owned transaction. A coupling
/// error other than `InsufficientHistory` inserts nothing. Dedup compares
/// sorted `(path, score as f64)` with the latest `hotspot_history` timestamp.
/// An owned transaction uses `busy_timeout = 0`, retries three times, and
/// restores `busy_timeout = 5000`. A caller that already holds a transaction
/// (`is_autocommit()` false) is not begun, committed, or rolled back here.
pub(super) fn persist_hotspots_and_couplings(
    storage: &StorageManager,
    repo: &gix::Repository,
    hotspots: &[Hotspot],
    config: &crate::config::model::Config,
    budget: Option<&AnalysisBudget>,
    write_couplings: bool,
) -> Result<HotspotPersistOutcome> {
    let prepared = prepare_couplings(repo, config, budget, write_couplings)?;
    let conn = storage.get_connection();
    if conn.is_autocommit() {
        persist_in_owned_transaction(conn, hotspots, &prepared)
    } else {
        write_snapshot_rows(conn, hotspots, &prepared)
    }
}
