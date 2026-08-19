use crate::commands::doctor::finding::{DoctorCategory, DoctorFinding};
use crate::commands::doctor::remediation::{
    ContentHashDriftInputs, GraphAgeInputs, GraphIndexHealth, SearchDocsClassification,
    build_graph_content_stale_finding, build_graph_drift_check_failed_finding,
    build_search_empty_finding, classify_graph_index_health, classify_search_document_count,
    graph_content_stale_index_health_line, graph_current_empty_cozo_index_health_line,
    graph_current_populated_index_health_line, graph_drift_check_failed_index_health_line,
    search_empty_index_health_line, search_ok_index_health_line,
};
use crate::output::human::DoctorReport;
use crate::state::layout::Layout;
use crate::state::reports::write_clean_tree_tombstone;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::{OwoColorize, Stream};
use std::path::Path;

/// Graph / search / impact freshness probes (after network spawn).
pub(crate) fn collect_index_findings(
    storage: &StorageManager,
    layout: &Layout,
    config: &crate::config::model::Config,
    current_dir: &Path,
    report: &mut DoctorReport<'_>,
) -> Result<Vec<DoctorFinding>> {
    let mut findings = Vec::new();
    let mut total_nodes = 0;
    let mut total_edges = 0;

    // --- Graph Probe ---
    if let Some(cozo) = storage.cozo() {
        match cozo.run_script("?[count(n)] := *node{id: n}") {
            Ok(res) => {
                let node_count = res
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .and_then(|v| match v {
                        cozo::DataValue::Num(cozo::Num::Int(i)) => Some(*i),
                        _ => None,
                    })
                    .unwrap_or(0);

                let edge_res = cozo.run_script("?[count(s)] := *edge{source: s}");
                let edge_count = edge_res
                    .ok()
                    .and_then(|res| res.rows.first().cloned())
                    .and_then(|r| r.first().cloned())
                    .and_then(|v| match v {
                        cozo::DataValue::Num(cozo::Num::Int(i)) => Some(i),
                        _ => None,
                    })
                    .unwrap_or(0);

                total_nodes = node_count;
                total_edges = edge_count;

                report.native_graph_status = format!(
                    "Ready (CozoDB active, {} nodes, {} edges)",
                    node_count, edge_count
                );
            }
            Err(e) => {
                report.native_graph_status = format!(
                    "Error ({})",
                    e.if_supports_color(Stream::Stdout, |s| s.red())
                );
                findings.push(DoctorFinding::warn(
                    "graph-error",
                    DoctorCategory::Index,
                    format!("Native graph error ({e})"),
                ));
            }
        }
    } else {
        report.native_graph_status = "Not initialized".to_string();
        findings.push(DoctorFinding::info(
            "graph-not-initialized",
            DoctorCategory::Index,
            "Native graph not initialized",
        ));
    }

    // --- Index Health Probes ---
    // 1. Tantivy Search Index
    let index_path = layout.search_index_dir();
    if !index_path.exists() {
        findings.push(DoctorFinding::warn(
            "search-missing",
            DoctorCategory::Index,
            "Search index: Missing (run 'ledgerful index')",
        ));
    } else {
        let engine = crate::search::tantivy_engine::TantivySearchEngine::open_or_create(
            index_path.as_std_path(),
        );
        match engine {
            Ok(e) => {
                if let Err(err) = e.verify_index_integrity(index_path.as_std_path()) {
                    findings.push(DoctorFinding::warn(
                        "search-corrupt",
                        DoctorCategory::Index,
                        format!("Search index: Corrupt ({err}) - run 'ledgerful index --full'"),
                    ));
                } else {
                    let docs = e.document_count();
                    // 0126: pure classify — empty is a state, not OK.
                    match classify_search_document_count(docs) {
                        SearchDocsClassification::Empty => {
                            findings.push(build_search_empty_finding());
                            report
                                .index_health
                                .push(search_empty_index_health_line().to_string());
                        }
                        SearchDocsClassification::Populated { docs } => {
                            report.index_health.push(search_ok_index_health_line(docs));
                        }
                    }
                }
            }
            Err(e) => {
                findings.push(DoctorFinding::warn(
                    "search-load-failed",
                    DoctorCategory::Index,
                    format!("Search index: Load failed ({e})"),
                ));
            }
        }
    }

    // 2. Knowledge Graph Staleness (0133: age first STOP, else content-hash drift)
    // Age path: graph-empty | graph-stale only — do not run content drift (double findings + I/O).
    // Else: one count_content_hash_drift on layout.root (never bare cwd); dirty → content-stale;
    // clean → Current / empty-Cozo hint; Err → graph-drift-check-failed (never Current).
    let age_warning =
        crate::index::staleness::check_index_staleness(storage, config.index.stale_threshold_days);
    let age_inputs = age_warning.as_ref().map(|w| GraphAgeInputs {
        is_missing: w.is_missing,
        stale_files: w.stale_files,
    });
    let drift_for_classify: Option<Result<ContentHashDriftInputs, String>> = if age_inputs.is_none()
    {
        match crate::index::staleness::count_content_hash_drift(storage, layout.root.as_path()) {
            Ok(d) => Some(Ok(ContentHashDriftInputs {
                changed_or_unindexed: d.changed_or_unindexed,
            })),
            Err(e) => {
                tracing::debug!("Full graph content-hash drift check error: {e}");
                Some(Err(e.to_string()))
            }
        }
    } else {
        None
    };
    let graph_health = classify_graph_index_health(
        age_inputs.as_ref(),
        drift_for_classify,
        total_nodes,
        total_edges,
    );
    match graph_health {
        GraphIndexHealth::AgeEmpty => {
            findings.push(DoctorFinding::warn(
                "graph-empty",
                DoctorCategory::Index,
                "Graph state: Empty (never indexed)",
            ));
        }
        GraphIndexHealth::AgeStale { stale_files } => {
            findings.push(DoctorFinding::warn(
                "graph-stale",
                DoctorCategory::Index,
                format!(
                    "Graph state: STALE ({stale_files} files affected) - run 'ledgerful index'"
                ),
            ));
        }
        GraphIndexHealth::ContentStale { n } => {
            findings.push(build_graph_content_stale_finding(n));
            report
                .index_health
                .push(graph_content_stale_index_health_line(n));
        }
        GraphIndexHealth::DriftCheckFailed { truncated_err } => {
            findings.push(build_graph_drift_check_failed_finding(&truncated_err));
            report
                .index_health
                .push(graph_drift_check_failed_index_health_line().to_string());
        }
        GraphIndexHealth::CurrentPopulated => {
            report
                .index_health
                .push(graph_current_populated_index_health_line().to_string());
        }
        GraphIndexHealth::CurrentEmptyCozo => {
            report
                .index_health
                .push(graph_current_empty_cozo_index_health_line().to_string());
        }
    }

    // 3. Impact Report Freshness
    if let Ok(repo) = crate::git::repo::open_repo(current_dir)
        && let Ok((head_hash, branch_name)) = crate::git::repo::get_head_info(&repo)
    {
        let changes = crate::git::status::get_repo_status(&repo).unwrap_or_default();
        let filtered = crate::git::ignore::filter_ignored_changes(
            changes,
            &config.watch.ignore_patterns,
            true,
        )
        .unwrap_or_default();

        let snapshot = crate::git::RepoSnapshot {
            head_hash,
            branch_name,
            is_clean: filtered.is_empty(),
            changes: filtered,
        };

        let freshness = crate::state::reports::check_impact_freshness(layout, &snapshot);
        match freshness {
            crate::state::reports::ImpactFreshness::Missing => {
                findings.push(DoctorFinding::warn(
                    "impact-missing",
                    DoctorCategory::Index,
                    "Impact report: None (run 'ledgerful scan --impact')",
                ));
            }
            crate::state::reports::ImpactFreshness::CurrentClean => {
                report
                    .index_health
                    .push("Impact report: Current (Clean tree)".to_string());
            }
            crate::state::reports::ImpactFreshness::CurrentDirty => {
                report
                    .index_health
                    .push("Impact report: Current (Dirty tree packet)".to_string());
            }
            crate::state::reports::ImpactFreshness::Stale { reason } => {
                if snapshot.is_clean {
                    tracing::debug!(
                        "Auto-refreshing stale clean-tree impact report for HEAD {:?}",
                        snapshot.head_hash
                    );
                    match write_clean_tree_tombstone(
                        layout,
                        snapshot.head_hash.clone(),
                        snapshot.branch_name.clone(),
                    ) {
                        Ok(()) => {
                            tracing::debug!("Auto-refreshed impact report successfully");
                            report
                                .index_health
                                .push("Impact report: Current (Clean tree)".to_string());
                        }
                        Err(e) => {
                            tracing::debug!("Failed to auto-refresh impact report: {e}");
                            findings.push(DoctorFinding::warn(
                                "impact-stale",
                                DoctorCategory::Index,
                                format!(
                                    "Impact report: STALE ({reason}) — run 'ledgerful impact' or 'ledgerful scan --impact' to refresh"
                                ),
                            ));
                        }
                    }
                } else {
                    findings.push(DoctorFinding::warn(
                        "impact-stale",
                        DoctorCategory::Index,
                        format!(
                            "Impact report: STALE ({reason}) — run 'ledgerful impact' or 'ledgerful scan --impact' to refresh"
                        ),
                    ));
                }
            }
            crate::state::reports::ImpactFreshness::Corrupt { reason } => {
                // Impact-corrupt stays warn (not block): publish path does not require impact.
                findings.push(DoctorFinding::warn(
                    "impact-corrupt",
                    DoctorCategory::Index,
                    format!("Impact report: Corrupt ({reason})"),
                ));
            }
        }
    }

    Ok(findings)
}
