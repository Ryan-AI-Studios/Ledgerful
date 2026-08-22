use crate::commands::dx1_templates::write_openslo_template;
use crate::commands::helpers::get_layout;
use crate::output::empty::EmptyReason;
use crate::output::table::build_premium_table;
use crate::state::storage::StorageManager;
use crate::util::term::prompt_yes_no;
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};

/// 0215-A5: indexed==0 three-arm empty taxonomy (disk / unbuilt / NoMatches).
/// CleanDiff is diff-only and stays in `execute_observability`.
pub(crate) fn observability_indexed_zero_empty_state(
    on_disk: bool,
    graph_populated: bool,
) -> (EmptyReason, String) {
    if on_disk {
        (
            EmptyReason::NoIndexedData,
            "OpenSLO files on disk but not in the graph. Run `ledgerful index --analyze-graph`."
                .to_string(),
        )
    } else if !graph_populated {
        (
            EmptyReason::NoIndexedData,
            "Knowledge graph has not been built yet. Run `ledgerful index --analyze-graph` first, \
             then add OpenSLO files to 'observability/' if this repo uses OpenSLO."
                .to_string(),
        )
    } else {
        (
            EmptyReason::NoMatches,
            "Knowledge graph is populated, but no SLO/metric/alert/observability_signal nodes exist. \
             This repo has no OpenSLO YAML configured — add them under 'observability/' and run \
             `ledgerful index --analyze-graph` to populate this surface."
                .to_string(),
        )
    }
}

#[derive(Args, Debug)]
pub struct ObservabilityArgs {
    #[command(subcommand)]
    pub command: ObservabilitySubcommands,
}

#[derive(Subcommand, Debug)]
pub enum ObservabilitySubcommands {
    /// Show observability coverage for services and endpoints
    Coverage {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show observability changes based on current diff (changed SLOs, metrics, alerts)
    Diff {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn execute_observability(args: ObservabilityArgs) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let cozo = storage
        .cozo()
        .ok_or_else(|| miette::miette!("CozoDB not available"))?;
    match args.command {
        ObservabilitySubcommands::Coverage { json } => {
            let services_res = cozo.run_script(
                "?[svc_urn, service] := *node{id: svc_urn, label: service, category: 'service'}",
            )?;
            let slo_res = cozo.run_script("?[svc_urn, count(slo_urn)] := *edge{source: slo_urn, target: svc_urn, relation: 'monitors'}, *node{id: slo_urn, category: 'slo'}")?;
            let metric_res = cozo.run_script("?[svc_urn, count(m_urn)] := *edge{source: slo_urn, target: svc_urn, relation: 'monitors'}, *edge{source: slo_urn, target: m_urn, relation: 'depends_on'}, *node{id: m_urn, category: 'metric'}")?;

            let mut slo_map = std::collections::HashMap::new();
            for row in slo_res.rows {
                if let (
                    Some(cozo::DataValue::Str(svc_urn)),
                    Some(cozo::DataValue::Num(cozo::Num::Int(count))),
                ) = (row.first(), row.get(1))
                {
                    slo_map.insert(svc_urn.clone(), *count);
                }
            }

            let mut metric_map = std::collections::HashMap::new();
            for row in metric_res.rows {
                if let (
                    Some(cozo::DataValue::Str(svc_urn)),
                    Some(cozo::DataValue::Num(cozo::Num::Int(count))),
                ) = (row.first(), row.get(1))
                {
                    metric_map.insert(svc_urn.clone(), *count);
                }
            }

            let mut final_rows = Vec::new();
            for row in services_res.rows {
                if let (Some(cozo::DataValue::Str(svc_urn)), Some(cozo::DataValue::Str(service))) =
                    (row.first(), row.get(1))
                {
                    let slo_count = *slo_map.get(svc_urn).unwrap_or(&0);
                    let metric_count = *metric_map.get(svc_urn).unwrap_or(&0);
                    final_rows.push((service.clone(), slo_count, metric_count));
                }
            }

            if !json && final_rows.is_empty() {
                // DX1: offer to generate a base OpenSLO SLO template (default
                // YES) before falling through to the read-only empty-state
                // guidance. Non-interactive environments (CI, piped stdin,
                // `LEDGERFUL_NON_INTERACTIVE=1`) decline without touching
                // stdin and fall through to the static messages below — read-
                // only degrade, no side effects.
                if prompt_yes_no(
                    "No OpenSLO coverage data found. Would you like to generate a base OpenSLO template? [Y/n] ",
                ) {
                    let written = write_openslo_template(&layout.root)?;
                    let display_path = written
                        .strip_prefix(&layout.root)
                        .map(|p| p.to_string())
                        .unwrap_or_else(|_| written.to_string());
                    println!(
                        "Generated a base OpenSLO SLO template at {} — edit the service/metric fields, then run ledgerful index --analyze-graph.",
                        display_path
                    );
                    return Ok(());
                }
                println!(
                    "  {}",
                    "No OpenSLO coverage data found."
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                println!(
                    "  Note: 'observability diff' lists OpenSLO SLO, metric, alert, and signal nodes from the knowledge graph, not source-code LOG patterns."
                );
                println!(
                    "  Coverage specifically requires OpenSLO YAML definitions in the 'observability/' directory."
                );
                println!(
                    "  Once added, run {} to populate.",
                    "ledgerful index --analyze-graph"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
                );
                return Ok(());
            }

            if json {
                let mut results = Vec::new();
                for (svc, sc, mc) in &final_rows {
                    results.push(serde_json::json!({
                        "service": svc,
                        "slo_count": sc,
                        "metric_count": mc,
                    }));
                }
                let output = if results.is_empty() {
                    // Probe before the empty-state closure — graph_has_any_nodes is Result.
                    let on_disk =
                        crate::commands::surfaces::repo_root_openslo_present(&layout.root);
                    let graph_populated = crate::commands::security::graph_has_any_nodes(cozo)?;
                    let (reason, message) =
                        observability_indexed_zero_empty_state(on_disk, graph_populated);
                    crate::output::empty::format_json_empty_state(results, "results", || {
                        (reason, message)
                    })
                } else {
                    crate::output::empty::format_json_empty_state(results, "results", || {
                        (EmptyReason::NoMatches, String::new())
                    })
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output).into_diagnostic()?
                );
            } else {
                println!(
                    "{}",
                    "Observability Coverage Summary"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                let mut table = build_premium_table(["Service", "SLOs", "Metrics", "Health"]);

                for (svc, sc, mc) in &final_rows {
                    let health = if *sc > 0 {
                        "COVERED"
                            .if_supports_color(Stream::Stdout, |s| s.green())
                            .to_string()
                    } else {
                        "MISSING"
                            .if_supports_color(Stream::Stdout, |s| s.red())
                            .to_string()
                    };
                    table.add_row(vec![
                        svc.to_string(),
                        sc.to_string(),
                        mc.to_string(),
                        health,
                    ]);
                }
                println!("{}", table);
            }
        }
        ObservabilitySubcommands::Diff { json } => {
            // Identify changed observability files (YAML/YML in changed diff)
            // and surface which graph nodes (SLO, metric, alert) they map to.
            // Path membership only — git status, not full impact (0146).
            let changed_files: std::collections::HashSet<String> =
                crate::git::status::collect_changed_files_for_filter(&layout)?
                    .iter()
                    .map(|c| crate::git::status::normalize_filter_path(&c.path))
                    .collect();

            let cozo = storage
                .cozo()
                .ok_or_else(|| miette::miette!("CozoDB not available"))?;

            // Query all observability graph nodes including metadata for source_file lookup
            let obs_res = cozo.run_script(
                "?[id, label, category, metadata] := *node{id, label, category, metadata}, \
                 category in ['slo', 'metric', 'alert', 'observability_signal']",
            )?;

            let mut changed = Vec::new();
            let mut unchanged = Vec::new();

            for row in obs_res.rows {
                if let (
                    Some(cozo::DataValue::Str(id)),
                    Some(cozo::DataValue::Str(label)),
                    Some(cozo::DataValue::Str(cat)),
                ) = (row.first(), row.get(1), row.get(2))
                {
                    // Match via source_file stored in metadata at index time.
                    // URN-based matching is unreliable because URNs use the entity name, not path.
                    let source_file: Option<String> = row.get(3).and_then(|v| {
                        if let cozo::DataValue::Json(j) = v {
                            j.get("source_file")
                                .and_then(|f| f.as_str())
                                .map(|s| s.replace('\\', "/"))
                        } else {
                            None
                        }
                    });
                    let is_changed = source_file
                        .as_deref()
                        .map(|sf| changed_files.contains(sf))
                        .unwrap_or(false);

                    let entry = serde_json::json!({
                        "id": id,
                        "label": label,
                        "category": cat,
                        "changed": is_changed,
                    });

                    if is_changed {
                        changed.push(entry);
                    } else {
                        unchanged.push(entry);
                    }
                }
            }

            // Deterministic ordering for both human and JSON output: CozoDB
            // row order is not guaranteed stable across runs, so sort by
            // (category, label, id) before emitting either path (M1 from
            // Claude cross-review).
            changed.sort_by(|a, b| {
                (
                    a["category"].as_str().unwrap_or(""),
                    a["label"].as_str().unwrap_or(""),
                    a["id"].as_str().unwrap_or(""),
                )
                    .cmp(&(
                        b["category"].as_str().unwrap_or(""),
                        b["label"].as_str().unwrap_or(""),
                        b["id"].as_str().unwrap_or(""),
                    ))
            });
            unchanged.sort_by(|a, b| {
                (
                    a["category"].as_str().unwrap_or(""),
                    a["label"].as_str().unwrap_or(""),
                    a["id"].as_str().unwrap_or(""),
                )
                    .cmp(&(
                        b["category"].as_str().unwrap_or(""),
                        b["label"].as_str().unwrap_or(""),
                        b["id"].as_str().unwrap_or(""),
                    ))
            });

            // indexed is collected 3-required-field rows, not obs_res.rows.len().
            let indexed = changed.len() + unchanged.len();
            let indexed_zero_empty = if changed.is_empty() && indexed == 0 {
                let on_disk = crate::commands::surfaces::repo_root_openslo_present(&layout.root);
                let graph_populated = crate::commands::security::graph_has_any_nodes(cozo)?;
                Some(observability_indexed_zero_empty_state(
                    on_disk,
                    graph_populated,
                ))
            } else {
                None
            };

            if json {
                let json_out = if changed.is_empty() {
                    let (reason, msg) = if let Some(empty) = indexed_zero_empty {
                        empty
                    } else {
                        (
                            EmptyReason::CleanDiff,
                            "No observability signals impacted by current diff.".to_string(),
                        )
                    };
                    serde_json::json!({
                        "changed": changed,
                        "unchanged_count": unchanged.len(),
                        "emptyReason": reason,
                        "message": msg,
                        "indexedCount": indexed,
                    })
                } else {
                    serde_json::json!({
                        "changed": changed,
                        "unchanged_count": unchanged.len(),
                    })
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json_out).into_diagnostic()?
                );
            } else {
                println!(
                    "{}",
                    "Observability Diff"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                println!("Changed files in diff: {}", changed_files.len());

                if changed.is_empty() {
                    if let Some((_, msg)) = indexed_zero_empty {
                        println!("{}", msg.if_supports_color(Stream::Stdout, |s| s.dimmed()));
                    } else if indexed > 0 {
                        println!(
                            "{}",
                            "No observability signals impacted by current diff."
                                .if_supports_color(Stream::Stdout, |s| s.dimmed())
                        );
                    }
                } else {
                    println!(
                        "\n{}",
                        format!("{} observability signal(s) impacted:", changed.len())
                            .if_supports_color(Stream::Stdout, |s| s
                                .style(Style::new().yellow().bold()))
                    );
                    // `changed` is already sorted deterministically above
                    // (shared by JSON and human paths), so iterate directly.
                    let sorted: Vec<&serde_json::Value> = changed.iter().collect();
                    let mut table = build_premium_table(["Category", "Label", "ID"]);
                    for item in sorted {
                        table.add_row(vec![
                            item["category"].as_str().unwrap_or("").to_string(),
                            item["label"].as_str().unwrap_or("").to_string(),
                            item["id"].as_str().unwrap_or("").to_string(),
                        ]);
                    }
                    println!("{}", table);
                }
                if indexed > 0 {
                    println!(
                        "\n{} other observability signal(s) not impacted.",
                        unchanged.len()
                    );
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observability_diff_table_uses_premium_framing_and_deterministic_order() {
        let mut changed = vec![
            serde_json::json!({"id": "b", "label": "Beta", "category": "metric"}),
            serde_json::json!({"id": "a", "label": "Alpha", "category": "metric"}),
            serde_json::json!({"id": "c", "label": "Alpha", "category": "alert"}),
        ];
        // Sort using the same comparator used in production.
        changed.sort_by(|a, b| {
            let a_key = (
                a["category"].as_str().unwrap_or(""),
                a["label"].as_str().unwrap_or(""),
                a["id"].as_str().unwrap_or(""),
            );
            let b_key = (
                b["category"].as_str().unwrap_or(""),
                b["label"].as_str().unwrap_or(""),
                b["id"].as_str().unwrap_or(""),
            );
            a_key.cmp(&b_key)
        });

        let mut table = build_premium_table(["Category", "Label", "ID"]);
        for item in &changed {
            table.add_row(vec![
                item["category"].as_str().unwrap_or("").to_string(),
                item["label"].as_str().unwrap_or("").to_string(),
                item["id"].as_str().unwrap_or("").to_string(),
            ]);
        }
        let rendered = table.to_string();
        assert!(
            rendered.contains('╭') || rendered.contains('+'),
            "expected premium table border (utf8 rounded or ascii +), got:\n{rendered}"
        );
        assert!(
            rendered.contains("Category") && rendered.contains("Label") && rendered.contains("ID"),
            "expected headers, got:\n{rendered}"
        );
        // Deterministic: alert Alpha should come before metric Alpha, then Beta.
        let alert_pos = rendered.find("alert").unwrap_or(usize::MAX);
        let metric_alpha_pos = rendered.find("metric").unwrap_or(usize::MAX);
        let beta_pos = rendered.find("Beta").unwrap_or(usize::MAX);
        assert!(
            alert_pos < metric_alpha_pos && metric_alpha_pos < beta_pos,
            "expected deterministic order, got:\n{rendered}"
        );
    }

    #[test]
    fn observability_indexed_zero_empty_state_three_arms() {
        let (reason, msg) = observability_indexed_zero_empty_state(true, true);
        assert_eq!(reason, EmptyReason::NoIndexedData);
        assert!(
            msg.contains("index --analyze-graph"),
            "disk-present arm must name analyze-graph, got: {msg}"
        );
        assert!(
            !msg.to_lowercase().contains("add observability"),
            "disk-present arm must not say add observability/, got: {msg}"
        );

        let (reason, msg) = observability_indexed_zero_empty_state(true, false);
        assert_eq!(
            reason,
            EmptyReason::NoIndexedData,
            "disk-first wins even when the graph is unpopulated"
        );
        assert!(
            !msg.to_lowercase().contains("add observability"),
            "disk-present unbuilt graph must not say add observability/, got: {msg}"
        );

        let (reason, msg) = observability_indexed_zero_empty_state(false, false);
        assert_eq!(reason, EmptyReason::NoIndexedData);
        assert!(
            msg.contains("index --analyze-graph"),
            "unbuilt-graph arm must name analyze-graph, got: {msg}"
        );
        assert!(
            msg.contains("observability/"),
            "unbuilt-graph arm still mentions observability/ after analyze-graph, got: {msg}"
        );

        let (reason, msg) = observability_indexed_zero_empty_state(false, true);
        assert_eq!(reason, EmptyReason::NoMatches);
        assert!(
            msg.contains("observability/"),
            "populated no-YAML arm must name observability/, got: {msg}"
        );
        assert!(
            msg.contains("index --analyze-graph"),
            "populated no-YAML arm still names ingest after add, got: {msg}"
        );
    }
}
