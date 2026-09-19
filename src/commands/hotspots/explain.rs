use crate::commands::helpers::get_layout;
use crate::config::load_config;
use crate::config::model::Config;
use crate::impact::budget::{
    AnalysisBudget, AnalysisCompleteness, HistoryWalkStop, completeness_for_overall,
    is_overall_stop, poll_overall_stop,
};
use crate::impact::hotspots::{
    HotspotInterpretation, HotspotQuery, calculate_hotspots_detailed,
    compute_hotspot_score_breakdown_from_hotspots,
};
use crate::impact::packet::{Hotspot, TemporalCoupling};
use crate::impact::temporal::{GixHistoryProvider, TemporalEngine};
use crate::state::storage::StorageManager;
use miette::Result;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// CLI presentation so the resolution logic is directly testable.
pub struct HotspotExplanation {
    pub normalized_entity: String,
    pub complexity: i32,
    pub frequency: f64,
    pub couplings: Vec<crate::impact::packet::TemporalCoupling>,
    /// Greppable reason the couplings list is omitted/untrusted. `None` means trusted.
    pub couplings_warning: Option<String>,
    pub score_breakdown: Option<crate::impact::hotspots::HotspotScoreBreakdown>,
    pub contributing_commits: Vec<String>,
    pub empty_reason: Option<String>,
    pub not_in_window_copy: Option<String>,
}

pub fn compute_hotspot_explanation(
    storage: &StorageManager,
    entity: &str,
    repo: &gix::Repository,
) -> Result<HotspotExplanation> {
    let config = load_config(&get_layout()?)?;
    Ok(
        compute_hotspot_explanation_in(storage, entity, repo, &config, None, None, None, None, 0)?
            .explanation,
    )
}

pub(crate) struct HotspotExplanationRun {
    pub explanation: HotspotExplanation,
    pub completeness: Option<AnalysisCompleteness>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_hotspot_explanation_in(
    storage: &StorageManager,
    entity: &str,
    repo: &gix::Repository,
    config: &Config,
    commits: Option<usize>,
    days: Option<u32>,
    cancel: Option<Arc<AtomicBool>>,
    overall_deadline: Option<Instant>,
    overall_secs: u64,
) -> Result<HotspotExplanationRun> {
    let repo_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("No work dir"))?;
    let normalized_entity = crate::util::path::normalize_relative_path(repo_root, entity)
        .unwrap_or_else(|_| entity.to_string());

    let conn = storage.get_connection();
    let indexed = complexity_for_entity_path(conn, &normalized_entity)?;
    let cancel = cancel.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

    if let Some(stop) = poll_overall_stop(overall_deadline, &cancel) {
        return Ok(HotspotExplanationRun {
            explanation: HotspotExplanation {
                normalized_entity,
                complexity: indexed,
                frequency: 0.0,
                couplings: Vec::new(),
                couplings_warning: Some("temporal couplings untrusted: overall budget".to_string()),
                score_breakdown: None,
                contributing_commits: Vec::new(),
                empty_reason: None,
                not_in_window_copy: None,
            },
            completeness: Some(completeness_for_overall(
                stop,
                Some(overall_secs).filter(|s| *s > 0),
                "git",
            )),
        });
    }
    let history_provider = GixHistoryProvider::new(repo);
    let query = HotspotQuery {
        exact_file: None,
        commits: commits.unwrap_or(config.hotspots.max_commits),
        days: days.map(|d| d as u64),
        decay_half_life: config.hotspots.decay_half_life,
        limit: 10000,
        budget: Some(AnalysisBudget::capped_by_overall(
            config.hotspots.history_budget_secs,
            overall_deadline,
            cancel.clone(),
        )),
        skip_unindexed_complexity_fallback: overall_deadline.is_some(),
        record_commits_for: Some(normalized_entity.replace('\\', "/")),
        ..Default::default()
    };
    let calculated = calculate_hotspots_detailed(storage, &history_provider, &query)?;
    let hotspots = calculated.hotspots;
    let entity_normalized = normalized_entity.replace('\\', "/");
    let matching = hotspots.iter().find(|h| {
        let lossy = h.path.to_string_lossy();
        lossy == normalized_entity || lossy.replace('\\', "/") == entity_normalized
    });
    let frequency = matching.map(|h| h.frequency).unwrap_or(0.0);
    let complexity = matching
        .map(|h| {
            if h.complexity > 0 {
                h.complexity
            } else {
                indexed
            }
        })
        .unwrap_or(indexed);

    let mut completeness = super::list::list_completeness_after_walk(
        calculated.walk_stop,
        query.commits as u64,
        calculated.commits_walked as u64,
        query.days,
        crate::impact::budget::CompletenessFilter::Unfiltered,
        calculated.head.clone(),
        config.hotspots.history_budget_secs,
        overall_deadline,
        overall_secs,
        &cancel,
    );

    let skip_stop = poll_overall_stop(overall_deadline, &cancel);
    let skip_couplings = skip_stop.is_some() || completeness.as_ref().is_some_and(is_overall_stop);
    let (entity_couplings, couplings_warning) = if skip_couplings {
        if let Some(stop) = skip_stop
            && completeness.as_ref().is_none_or(|c| c.scope.is_none())
        {
            completeness = Some(completeness_for_overall(
                stop,
                Some(overall_secs).filter(|s| *s > 0),
                "coupling",
            ));
        }
        (
            Vec::new(),
            Some("temporal couplings untrusted: overall budget".to_string()),
        )
    } else {
        let engine = TemporalEngine::new(history_provider, config.temporal.clone());
        let (couplings, mut warning) =
            annotate_couplings(engine.calculate_couplings_budgeted(Some(&AnalysisBudget {
                deadline: overall_deadline,
                cancel: cancel.clone(),
                budget_secs: Some(overall_secs).filter(|s| *s > 0),
            })));
        if let Some(stop) = poll_overall_stop(overall_deadline, &cancel) {
            if completeness.as_ref().is_none_or(|c| c.scope.is_none()) {
                completeness = Some(completeness_for_overall(
                    stop,
                    Some(overall_secs).filter(|s| *s > 0),
                    "coupling",
                ));
            }
            if warning.is_none() {
                warning = Some("temporal couplings untrusted: overall budget".to_string());
            }
        }
        let entity_couplings: Vec<_> = couplings
            .into_iter()
            .filter(|c| {
                c.file_a.to_string_lossy() == normalized_entity
                    || c.file_b.to_string_lossy() == normalized_entity
            })
            .collect();
        (entity_couplings, warning)
    };

    let mut scored = hotspots.clone();
    let _ = patch_restored_complexity(&mut scored, &normalized_entity, complexity);
    let score_breakdown = compute_hotspot_score_breakdown_from_hotspots(
        &scored,
        &normalized_entity,
        entity_couplings.len(),
    );

    let mut contributing_commits = calculated.contributing_commits;
    contributing_commits.truncate(5);
    let empty_reason = if calculated.walk_stop == HistoryWalkStop::Complete
        && frequency == 0.0
        && contributing_commits.is_empty()
    {
        Some("notInWindow".to_string())
    } else {
        None
    };
    let not_in_window_copy = empty_reason
        .as_ref()
        .map(|_| not_in_window_human(query.days, query.commits));

    Ok(HotspotExplanationRun {
        explanation: HotspotExplanation {
            normalized_entity,
            complexity,
            frequency,
            couplings: entity_couplings,
            couplings_warning,
            score_breakdown,
            contributing_commits,
            empty_reason,
            not_in_window_copy,
        },
        completeness,
    })
}

/// Patch walk complexity to the restored indexed value before scoring.
pub(super) fn patch_restored_complexity(
    hotspots: &mut [Hotspot],
    entity: &str,
    restored: i32,
) -> bool {
    let entity_normalized = entity.replace('\\', "/");
    if let Some(h) = hotspots.iter_mut().find(|h| {
        let lossy = h.path.to_string_lossy();
        lossy == entity || lossy.replace('\\', "/") == entity_normalized
    }) {
        h.complexity = restored;
        true
    } else {
        false
    }
}

pub(super) fn not_in_window_human(days: Option<u64>, commits: usize) -> String {
    match days {
        Some(d) => format!("not in last {d} days"),
        None => format!("not in last {commits} commits"),
    }
}

pub(super) fn short_commit_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

pub(super) fn annotate_couplings(
    result: Result<Vec<crate::impact::packet::TemporalCoupling>, crate::git::GitError>,
) -> (Vec<crate::impact::packet::TemporalCoupling>, Option<String>) {
    match result {
        Ok(couplings) => (couplings, None),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to calculate temporal couplings; omitting untrusted list"
            );
            (
                Vec::new(),
                Some(format!("temporal couplings untrusted: {e}")),
            )
        }
    }
}

/// File-level complexity from `project_files` with 0183 unique-path resolve
/// (Ambiguous refuses; NotFound uses the raw path). Nested
/// `MAX(MAX(cognitive, cyclomatic))` across symbols; NULL/no-symbols → 0.
pub(super) fn complexity_for_entity_path(
    conn: &rusqlite::Connection,
    normalized_entity: &str,
) -> Result<i32> {
    let complexity_path =
        match crate::util::path_entity::resolve_indexed_file_path(conn, normalized_entity) {
            crate::util::path_entity::IndexedFileResolve::Unique { stored_path, .. } => stored_path,
            crate::util::path_entity::IndexedFileResolve::Ambiguous { query, candidates } => {
                let total = candidates.len();
                let show = total.min(10);
                let listed = candidates[..show].join(", ");
                let mut msg = format!("{total} indexed paths match '{query}': {listed}");
                if total > 10 {
                    msg.push_str(&format!(", and {} more", total - show));
                }
                msg.push_str(". Provide a more specific path.");
                return Err(miette::miette!("{msg}"));
            }
            crate::util::path_entity::IndexedFileResolve::NotFound => normalized_entity.to_string(),
        };
    Ok(conn
        .query_row(
            "SELECT MAX(MAX(IFNULL(cognitive_complexity, 0), IFNULL(cyclomatic_complexity, 0))) \
             FROM project_symbols ps JOIN project_files pf ON ps.file_id = pf.id WHERE pf.file_path = ?1",
            [&complexity_path],
            |row| row.get(0),
        )
        .unwrap_or(0))
}

fn format_hotspot_interpretation(interpretation: HotspotInterpretation) -> &'static str {
    match interpretation {
        HotspotInterpretation::MaintenanceRisk => {
            "High complexity, low churn — this is a maintenance risk file. \
             The code is intricate but rarely modified, so bugs here are hard to detect \
             and fixes are risky. Consider adding tests or refactoring to reduce complexity."
        }
        HotspotInterpretation::ActiveChurn => {
            "Low complexity, high churn — this file changes frequently but is simple. \
             Review churn for unnecessary volatility."
        }
        HotspotInterpretation::StableHotspot => {
            "High complexity AND high churn — this is an active hotspot. \
             Prioritize refactoring and test coverage."
        }
        HotspotInterpretation::LowRisk => {
            "Low complexity and low churn — this file is low risk. No action needed."
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HotspotExplanationEnvelope {
    schema_version: u32,
    kind: String,
    entity: String,
    complexity: i32,
    frequency: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_score: Option<f64>,
    couplings: Vec<TemporalCoupling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    couplings_warning: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    contributing_commits: Vec<ContributingCommitJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    empty_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completeness: Option<AnalysisCompleteness>,
}

#[derive(Debug, Serialize)]
struct ContributingCommitJson {
    id: String,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn explanation_json_envelope(
    entity: &str,
    complexity: i32,
    frequency: f64,
    breakdown: Option<&crate::impact::hotspots::HotspotScoreBreakdown>,
    couplings: Vec<TemporalCoupling>,
    couplings_warning: Option<String>,
    completeness: Option<&AnalysisCompleteness>,
    contributing_commits: &[String],
    empty_reason: Option<&str>,
) -> serde_json::Value {
    let envelope = HotspotExplanationEnvelope {
        schema_version: 1,
        kind: "hotspotExplanation".to_string(),
        entity: entity.to_string(),
        complexity,
        frequency,
        score: breakdown.map(|b| b.base_score),
        display_score: breakdown.map(|b| b.final_score),
        couplings,
        couplings_warning,
        contributing_commits: contributing_commits
            .iter()
            .map(|id| ContributingCommitJson { id: id.clone() })
            .collect(),
        empty_reason: empty_reason.map(str::to_string),
        completeness: completeness.cloned(),
    };
    serde_json::to_value(envelope).unwrap_or(serde_json::Value::Null)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_hotspots_explain(
    storage: &StorageManager,
    entity: String,
    repo: &gix::Repository,
    config: &Config,
    commits: Option<usize>,
    days: Option<u32>,
    json: bool,
    cancel: Arc<AtomicBool>,
    overall_deadline: Option<Instant>,
    overall_secs: u64,
    json_out: Option<&mut Vec<u8>>,
) -> Result<()> {
    let run = compute_hotspot_explanation_in(
        storage,
        &entity,
        repo,
        config,
        commits,
        days,
        Some(cancel),
        overall_deadline,
        overall_secs,
    )?;
    super::eprint_hotspots_overall_stop_if_budget(run.completeness.as_ref());
    if json {
        let output = explanation_json_envelope(
            &run.explanation.normalized_entity,
            run.explanation.complexity,
            run.explanation.frequency,
            run.explanation.score_breakdown.as_ref(),
            run.explanation.couplings.clone(),
            run.explanation.couplings_warning.clone(),
            run.completeness.as_ref(),
            &run.explanation.contributing_commits,
            run.explanation.empty_reason.as_deref(),
        );
        return super::write_json(&output, json_out);
    }
    let explanation = run.explanation;
    let normalized_entity = &explanation.normalized_entity;

    println!("Hotspot Analysis: {}", normalized_entity);

    println!("\nMetrics:");
    println!("  Complexity: {}", explanation.complexity);
    if let Some(copy) = &explanation.not_in_window_copy {
        println!(
            "  Change Frequency (weighted): {:.2} ({copy})",
            explanation.frequency
        );
    } else {
        println!(
            "  Change Frequency (weighted): {:.2}",
            explanation.frequency
        );
    }
    match &explanation.couplings_warning {
        Some(warning) => println!("  Temporal Couplings: untrusted ({warning})"),
        None => println!("  Temporal Couplings: {}", explanation.couplings.len()),
    }
    if !explanation.contributing_commits.is_empty() {
        println!("\nSource commits:");
        for id in &explanation.contributing_commits {
            println!("  {}", short_commit_id(id));
        }
    }

    if let Some(breakdown) = &explanation.score_breakdown {
        println!("\nScore Breakdown:");
        println!(
            "  Normalized complexity: {} / {} = {:.4}",
            breakdown.complexity, breakdown.max_complexity, breakdown.normalized_complexity
        );
        println!(
            "  Normalized frequency: {:.2} / {:.2} = {:.4}",
            breakdown.frequency_weight, breakdown.max_frequency, breakdown.normalized_frequency
        );
        println!(
            "  Base score: {:.4} × {:.4} = {:.4}",
            breakdown.normalized_complexity, breakdown.normalized_frequency, breakdown.base_score
        );
        println!(
            "  Display score (log-normalized): {:.4}",
            breakdown.final_score
        );

        println!("\nInterpretation:");
        println!(
            "  {}",
            format_hotspot_interpretation(breakdown.interpretation)
        );
    }

    if !explanation.couplings.is_empty() {
        println!("\nTop Couplings:");
        for c in explanation.couplings.iter().take(5) {
            let other = if c.file_a.to_string_lossy() == *normalized_entity {
                &c.file_b
            } else {
                &c.file_a
            };
            println!("  {:<40} | Score: {:.2}", other.to_string_lossy(), c.score);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::annotate_couplings;

    #[test]
    fn couplings_failure_is_annotated_not_silent_zero() {
        let err = crate::git::GitError::InsufficientHistory {
            found: 3,
            required: 10,
        };
        let (list, warning) = annotate_couplings(Err(err));
        assert!(
            list.is_empty(),
            "failed couplings must be omitted, not a trusted empty list with no warning"
        );
        let warning = warning.expect("couplings failure must be annotated");
        assert!(
            warning.contains("untrusted"),
            "expected greppable untrusted warning, got {warning}"
        );
        assert!(
            warning.contains('3') && warning.contains("10"),
            "warning should include found/required: {warning}"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn not_in_window_human__days_vs_commits() {
        assert_eq!(
            super::not_in_window_human(Some(1), 500),
            "not in last 1 days"
        );
        assert_eq!(
            super::not_in_window_human(None, 20),
            "not in last 20 commits"
        );
        assert_eq!(
            super::not_in_window_human(None, 500),
            "not in last 500 commits"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn short_commit_id__min_eight() {
        assert_eq!(super::short_commit_id("abc"), "abc");
        assert_eq!(super::short_commit_id("0123456789abcdef"), "01234567");
    }

    #[test]
    #[allow(non_snake_case)]
    fn patch_restored_complexity__score_uses_restored() {
        use crate::impact::hotspots::compute_hotspot_score_breakdown_from_hotspots;
        use crate::impact::packet::Hotspot;
        use std::path::PathBuf;

        let mut hotspots = vec![Hotspot {
            path: PathBuf::from("src/a.rs"),
            score: 0.0,
            display_score: 0.0,
            complexity: 0,
            frequency: 1.0,
            centrality: None,
        }];
        assert!(super::patch_restored_complexity(
            &mut hotspots,
            "src/a.rs",
            6
        ));
        assert_eq!(hotspots[0].complexity, 6);
        let breakdown = compute_hotspot_score_breakdown_from_hotspots(&hotspots, "src/a.rs", 0)
            .expect("matching hotspot");
        assert_eq!(breakdown.complexity, 6);
        assert!(
            breakdown.base_score > 0.0,
            "restored complexity must contribute to score, got {}",
            breakdown.base_score
        );
    }
}
