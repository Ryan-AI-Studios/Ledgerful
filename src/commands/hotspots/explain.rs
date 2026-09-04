use crate::commands::helpers::get_layout;
use crate::config::load_config;
use crate::impact::hotspots::{
    HotspotInterpretation, HotspotQuery, calculate_hotspots,
    compute_hotspot_score_breakdown_from_hotspots,
};
use crate::impact::temporal::{GixHistoryProvider, TemporalEngine};
use crate::state::storage::StorageManager;
use miette::Result;

/// CLI presentation so the resolution logic is directly testable.
pub(crate) struct HotspotExplanation {
    pub normalized_entity: String,
    pub complexity: i32,
    pub frequency: f64,
    pub couplings: Vec<crate::impact::packet::TemporalCoupling>,
    /// Greppable reason the couplings list is omitted/untrusted. `None` means trusted.
    pub couplings_warning: Option<String>,
    pub score_breakdown: Option<crate::impact::hotspots::HotspotScoreBreakdown>,
}

pub(crate) fn compute_hotspot_explanation(
    storage: &StorageManager,
    entity: &str,
    repo: &gix::Repository,
) -> Result<HotspotExplanation> {
    let repo_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("No work dir"))?;
    let normalized_entity = crate::util::path::normalize_relative_path(repo_root, entity)
        .unwrap_or_else(|_| entity.to_string());

    // Indexed complexity is the no-row fallback (0183 unique resolve / Ambiguous
    // refuse). When a git-history hotspot row exists, copy its scoring complexity
    // including zero (same matcher as score breakdown).
    let conn = storage.get_connection();
    let indexed = complexity_for_entity_path(conn, &normalized_entity)?;

    let config = load_config(&get_layout()?)?;
    let history_provider = GixHistoryProvider::new(repo);
    let query = HotspotQuery {
        exact_file: None,
        commits: config.hotspots.max_commits,
        decay_half_life: config.hotspots.decay_half_life,
        limit: 10000,
        ..Default::default()
    };
    let hotspots = calculate_hotspots(storage, &history_provider, &query)?;
    let entity_normalized = normalized_entity.replace('\\', "/");
    let matching = hotspots.iter().find(|h| {
        let lossy = h.path.to_string_lossy();
        lossy == normalized_entity || lossy.replace('\\', "/") == entity_normalized
    });
    let frequency = matching.map(|h| h.frequency).unwrap_or(0.0);
    let complexity = matching.map(|h| h.complexity).unwrap_or(indexed);

    let engine = TemporalEngine::new(history_provider, config.temporal.clone());
    let (couplings, couplings_warning) = annotate_couplings(engine.calculate_couplings());
    let entity_couplings: Vec<_> = couplings
        .into_iter()
        .filter(|c| {
            c.file_a.to_string_lossy() == normalized_entity
                || c.file_b.to_string_lossy() == normalized_entity
        })
        .collect();

    let score_breakdown = compute_hotspot_score_breakdown_from_hotspots(
        &hotspots,
        &normalized_entity,
        entity_couplings.len(),
    );

    Ok(HotspotExplanation {
        normalized_entity,
        complexity,
        frequency,
        couplings: entity_couplings,
        couplings_warning,
        score_breakdown,
    })
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

pub(super) fn execute_hotspots_explain(
    storage: &StorageManager,
    entity: String,
    repo: &gix::Repository,
) -> Result<()> {
    let explanation = compute_hotspot_explanation(storage, &entity, repo)?;
    let normalized_entity = &explanation.normalized_entity;

    println!("Hotspot Analysis: {}", normalized_entity);

    println!("\nMetrics:");
    println!("  Complexity: {}", explanation.complexity);
    println!(
        "  Change Frequency (weighted): {:.2}",
        explanation.frequency
    );
    match &explanation.couplings_warning {
        Some(warning) => println!("  Temporal Couplings: untrusted ({warning})"),
        None => println!("  Temporal Couplings: {}", explanation.couplings.len()),
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
}
