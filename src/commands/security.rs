use crate::commands::dx1_templates::write_cedar_template;
use crate::commands::helpers::get_layout;
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use crate::util::term::prompt_yes_no;
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::collections::HashSet;

#[derive(Args, Debug)]
pub struct SecurityArgs {
    #[command(subcommand)]
    pub command: SecuritySubcommands,
}

#[derive(Subcommand, Debug)]
pub enum SecuritySubcommands {
    /// Show security impact of recent changes
    Impact {
        /// Filter by changed policies only
        #[arg(long)]
        changed: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List security boundaries, roles, and policies
    Boundaries {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Extract changed file paths from the impact packet as a HashSet of normalized paths.
fn collect_changed_files() -> Result<HashSet<String>> {
    let packet = crate::commands::impact::execute_impact_silent()?;
    let changed: HashSet<String> = packet
        .changes
        .iter()
        .map(|c| c.path.to_string_lossy().replace('\\', "/"))
        .collect();
    Ok(changed)
}

/// Open CozoDB storage in read-only mode and return the Cozo engine.
fn open_cozo(root: &camino::Utf8Path) -> Result<crate::state::storage_cozo::CozoStorage> {
    let storage = StorageManager::open_read_only(root)?;
    storage
        .cozo
        .ok_or_else(|| miette::miette!("CozoDB not available"))
}

/// Truncate a string to `max_len` characters, appending "…" if it was cut.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len])
    }
}

/// CG-F35 (requirement #2): is the knowledge graph populated at all? Used to
/// distinguish "the graph was never built / `index --analyze-graph` hasn't
/// run" (a prerequisite problem — recommend indexing) from "the graph is
/// healthy but this repo has zero Cedar policy/principal/action/resource
/// nodes" (a configuration problem — recommend adding policy files, since
/// re-indexing would be a no-op). Mirrors the exact `?[count(n)] := *node{id:
/// n}` probe `doctor.rs`'s graph-state check already uses, so the two
/// surfaces agree on what "graph populated" means.
fn graph_has_any_nodes(cozo: &crate::state::storage_cozo::CozoStorage) -> bool {
    cozo.run_script("?[count(n)] := *node{id: n}")
        .ok()
        .and_then(|res| res.rows.first().cloned())
        .and_then(|r| r.first().cloned())
        .map(|v| matches!(v, cozo::DataValue::Num(cozo::Num::Int(i)) if i > 0))
        .unwrap_or(false)
}

/// Collect `(method, path_pattern)` routes from the SQLite `api_routes` table
/// (the same surface `ledgerful endpoints` queries). Used by the DX1
/// interactive bootstrap offer to decide whether a Cedar policy template can
/// be generated for the detected routes and, if so, to seed that template.
///
/// Routes are `SELECT DISTINCT`-deduped at the SQL layer (the `api_routes`
/// schema does not enforce uniqueness on `(method, path_pattern)`, so duplicate
/// rows would otherwise produce duplicate `@id`/permit clauses in the emitted
/// Cedar), then defensively deduped again in Rust (sort + dedup by
/// `(method, path)`) as a belt-and-suspenders guard against any caller or
/// future schema path that bypasses the `DISTINCT`. Output is sorted by
/// `(method, path)` for deterministic template emission.
fn collect_detected_routes(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT method, path_pattern FROM api_routes ORDER BY method, path_pattern",
        )
        .into_diagnostic()?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?;
    // Defensive dedup: sort then drop consecutive duplicates by (method, path).
    // Belt-and-suspenders in case a future caller bypasses the SQL DISTINCT.
    let mut routes = rows;
    routes.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
    routes.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    Ok(routes)
}

fn execute_impact(changed: bool, json: bool, layout: &crate::state::layout::Layout) -> Result<()> {
    let changed_files = collect_changed_files()?;
    let cozo = open_cozo(&layout.root)?;

    // Query all policy nodes and determine impact in-memory
    let query = "?[id, label, raw, effect, source_file] := *node{id, label, category: 'policy', metadata: meta}, \
                 raw = get(meta, 'raw'), \
                 effect = get(meta, 'effect'), \
                 source_file = get(meta, 'source_file')";
    let res = cozo.run_script(query)?;

    let mut impacted = Vec::new();
    for row in res.rows {
        if let (
            Some(cozo::DataValue::Str(id)),
            Some(cozo::DataValue::Str(label)),
            Some(cozo::DataValue::Str(raw)),
            Some(cozo::DataValue::Str(effect)),
            Some(cozo::DataValue::Str(source_file)),
        ) = (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
        {
            let is_impacted = changed_files.contains(source_file.as_str());

            if !changed || is_impacted {
                impacted.push(serde_json::json!({
                    "id": id,
                    "label": label,
                    "raw": raw,
                    "effect": effect,
                    "is_changed": is_impacted,
                }));
            }
        }
    }

    let total = impacted.len();
    let changed_count = impacted
        .iter()
        .filter(|i| i["is_changed"].as_bool().unwrap_or(false))
        .count();

    if json {
        let output = crate::output::empty::format_json_empty_state(
            impacted.clone(),
            "impacted",
            || {
                if total == 0 {
                    (
                    crate::output::empty::EmptyReason::NoIndexedData,
                    "No security policy data found. Add Cedar policy files to 'policies/' and run `ledgerful index --analyze-graph`.".to_string(),
                )
                } else {
                    (
                        crate::output::empty::EmptyReason::CleanDiff,
                        "No changed policies found in the current diff.".to_string(),
                    )
                }
            },
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        println!("{}", "Security Policy Impact Analysis".bold().red());
        if total == 0 {
            println!("{}", "  No security policy data found. Add Cedar policy files to 'policies/' and run `ledgerful index --analyze-graph`.".dimmed());
        } else if changed_count == 0 && changed {
            println!(
                "{}",
                "  No changed policies found in the current diff.".dimmed()
            );
        } else {
            let mut table = Table::new();
            table.set_header(vec!["Policy ID", "Effect", "Changed?"]);

            for item in &impacted {
                table.add_row(vec![
                    item["id"].as_str().unwrap_or("").to_string(),
                    item["effect"].as_str().unwrap_or_default().to_string(),
                    if item["is_changed"].as_bool().unwrap_or(false) {
                        "YES".yellow().bold().to_string()
                    } else {
                        "NO".to_string()
                    },
                ]);
            }

            println!("{}", table);
            // Summary counts
            if changed {
                println!(
                    "  {} of {} policies match changed files",
                    changed_count.to_string().yellow().bold(),
                    total.to_string().bold(),
                );
            } else {
                println!(
                    "  {} policies evaluated, {} changed by this diff",
                    total.to_string().bold(),
                    changed_count.to_string().yellow().bold(),
                );
            }
        }
    }

    Ok(())
}

fn execute_boundaries(json: bool, layout: &crate::state::layout::Layout) -> Result<()> {
    // Open the full StorageManager so we can reach both the CozoDB knowledge
    // graph (for the policy-node + graph-populated checks) and the SQLite
    // `api_routes` table (to count detected HTTP routes for the DX1 Cedar
    // template bootstrap offer).
    let storage = StorageManager::open_read_only(&layout.root)?;
    let cozo = storage
        .cozo
        .as_ref()
        .ok_or_else(|| miette::miette!("CozoDB not available"))?;

    // Query 1: policy + principal/action/resource authorisation nodes
    let auth_res = cozo.run_script(
        "?[id, label, category] := *node{id, label, category}, \
         category in ['policy', 'principal', 'action', 'resource']",
    )?;

    // Query 2: cross-surface boundary edges — policy → service/endpoint/config/deploy/adr
    let boundary_res = cozo.run_script(
        "?[policy_id, policy_label, relation, target_id, target_label, target_cat] := \
         *node{id: policy_id, label: policy_label, category: 'policy'}, \
         *edge{source: policy_id, target: target_id, relation: rel}, \
         *node{id: target_id, label: target_label, category: target_cat}, \
         target_cat in ['service', 'endpoint', 'config_key', 'deploy_surface', 'adr'], \
         relation = rel",
    )?;

    // Build category counts
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for row in &auth_res.rows {
        if let Some(cozo::DataValue::Str(cat)) = row.get(2) {
            *counts.entry(cat.to_string()).or_insert(0) += 1;
        }
    }

    if json {
        let mut auth_nodes = Vec::new();
        for row in &auth_res.rows {
            if let (
                Some(cozo::DataValue::Str(id)),
                Some(cozo::DataValue::Str(label)),
                Some(cozo::DataValue::Str(cat)),
            ) = (row.first(), row.get(1), row.get(2))
            {
                auth_nodes.push(serde_json::json!({
                    "id": id, "label": label, "category": cat,
                }));
            }
        }
        let mut boundary_edges = Vec::new();
        for row in &boundary_res.rows {
            if let (
                Some(cozo::DataValue::Str(pid)),
                Some(cozo::DataValue::Str(plabel)),
                Some(cozo::DataValue::Str(rel)),
                Some(cozo::DataValue::Str(tid)),
                Some(cozo::DataValue::Str(tlabel)),
                Some(cozo::DataValue::Str(tcat)),
            ) = (
                row.first(),
                row.get(1),
                row.get(2),
                row.get(3),
                row.get(4),
                row.get(5),
            ) {
                boundary_edges.push(serde_json::json!({
                    "policy_id": pid, "policy_label": plabel,
                    "relation": rel,
                    "target_id": tid, "target_label": tlabel, "target_category": tcat,
                }));
            }
        }
        let json_out = if auth_res.rows.is_empty() {
            let (reason, message) = if graph_has_any_nodes(cozo) {
                (
                    crate::output::empty::EmptyReason::NoMatches,
                    "Knowledge graph is populated, but no Cedar policy/principal/action/resource nodes exist. \
                     This repo has no Cedar policy files configured — add them under 'policies/' and run \
                     `ledgerful index --analyze-graph` to populate this surface.",
                )
            } else {
                (
                    crate::output::empty::EmptyReason::NoIndexedData,
                    "Knowledge graph has not been built yet. Run `ledgerful index --analyze-graph` first, \
                     then add Cedar policy files to 'policies/' if this repo uses Cedar.",
                )
            };
            serde_json::json!({
                "meta": { "counts": counts },
                "boundaries": {
                    "auth_nodes": auth_nodes,
                    "boundary_edges": boundary_edges,
                },
                "emptyReason": reason,
                "message": message
            })
        } else {
            serde_json::json!({
                "meta": { "counts": counts },
                "boundaries": {
                    "auth_nodes": auth_nodes,
                    "boundary_edges": boundary_edges,
                },
            })
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&json_out).into_diagnostic()?
        );
    } else {
        // --- Summary counts header ---
        if auth_res.rows.is_empty() {
            // CG-F35 (requirement #2): distinguish "surface available but not
            // populated" (graph built, zero Cedar nodes — a config/policy-file
            // gap) from "surface unavailable" (graph never built — an indexing
            // prerequisite gap), each with its own one-step next action,
            // matching the established taxonomy in `hotspots trend` and
            // `doctor`'s graph-state check.
            if graph_has_any_nodes(cozo) {
                // DX1: when the graph is populated but no Cedar policy data
                // exists, check whether any HTTP routes were detected. If so,
                // offer to generate a permissive Cedar template from them
                // (default YES). Non-interactive environments decline without
                // touching stdin and fall through to the existing static
                // read-only guidance (no side effects).
                let routes = collect_detected_routes(storage.get_connection())?;
                if !routes.is_empty()
                    && prompt_yes_no(&format!(
                        "No Cedar policy data found. Would you like to generate a template policy for your {} detected routes? [Y/n] ",
                        routes.len()
                    ))
                {
                    let written = write_cedar_template(&layout.root, &routes)?;
                    let display_path = written
                        .strip_prefix(&layout.root)
                        .map(|p| p.to_string())
                        .unwrap_or_else(|_| written.to_string());
                    println!(
                        "Generated {} permissive Cedar permit policies at {} — edit to scope principal/resource, then run ledgerful index --analyze-graph.",
                        routes.len(),
                        display_path
                    );
                } else {
                    println!(
                        "{}",
                        "Knowledge graph is populated, but no Cedar policy data was found."
                            .yellow()
                    );
                    println!(
                        "  This repo has no Cedar policy files configured. Add them under 'policies/' \
                         and run {} to populate this surface.",
                        "ledgerful index --analyze-graph".cyan().bold()
                    );
                }
            } else {
                println!(
                    "{}",
                    "No security boundary data found — the knowledge graph has not been built yet."
                        .yellow()
                );
                println!(
                    "  Run {} first, then add Cedar policy files to 'policies/' if this repo uses Cedar.",
                    "ledgerful index --analyze-graph".cyan().bold()
                );
            }
        } else {
            let summary = ["policy", "principal", "action", "resource"]
                .iter()
                .map(|k| format!("{} {}", counts.get(*k).copied().unwrap_or(0), k))
                .collect::<Vec<_>>()
                .join(" | ");
            println!(
                "{}",
                format!("Security Boundaries  [{}]", summary).bold().green()
            );

            // --- Auth nodes table ---
            let auth_count = auth_res.rows.len();
            println!(
                "\n{} ({} total)",
                "Authorization Nodes (policy/principal/action/resource):".bold(),
                auth_count.to_string().bold(),
            );
            let mut auth_table = Table::new();
            auth_table.set_header(vec!["Category", "Label", "ID"]);
            for row in auth_res.rows {
                if let (
                    Some(cozo::DataValue::Str(id)),
                    Some(cozo::DataValue::Str(label)),
                    Some(cozo::DataValue::Str(cat)),
                ) = (row.first(), row.get(1), row.get(2))
                {
                    auth_table.add_row(vec![
                        cat.to_string(),
                        truncate(label, 60),
                        truncate(id, 80),
                    ]);
                }
            }
            println!("{}", auth_table);

            // --- Boundary links table ---
            let boundary_count = boundary_res.rows.len();
            println!(
                "\n{} ({} total)",
                "Cross-Surface Boundary Links (policy → protected entity):".bold(),
                boundary_count.to_string().bold(),
            );
            if boundary_res.rows.is_empty() {
                println!(
                    "{}",
                    "  No cross-surface links found. Run `ledgerful index --incremental` to refresh."
                        .dimmed()
                );
            } else {
                let mut boundary_table = Table::new();
                boundary_table.set_header(vec!["Policy", "Relation", "Target", "Target Category"]);
                for row in boundary_res.rows {
                    if let (
                        Some(cozo::DataValue::Str(_pid)),
                        Some(cozo::DataValue::Str(plabel)),
                        Some(cozo::DataValue::Str(rel)),
                        Some(cozo::DataValue::Str(_tid)),
                        Some(cozo::DataValue::Str(tlabel)),
                        Some(cozo::DataValue::Str(tcat)),
                    ) = (
                        row.first(),
                        row.get(1),
                        row.get(2),
                        row.get(3),
                        row.get(4),
                        row.get(5),
                    ) {
                        boundary_table.add_row(vec![
                            truncate(plabel, 50),
                            rel.to_string(),
                            truncate(tlabel, 50),
                            tcat.to_string(),
                        ]);
                    }
                }
                println!("{}", boundary_table);
            }
        }
    }

    Ok(())
}

pub fn execute_security(args: SecurityArgs) -> Result<()> {
    let layout = get_layout()?;

    match args.command {
        SecuritySubcommands::Impact { changed, json } => execute_impact(changed, json, &layout),
        SecuritySubcommands::Boundaries { json } => execute_boundaries(json, &layout),
    }
}
