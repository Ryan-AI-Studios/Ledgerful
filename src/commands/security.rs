use crate::commands::dx1_templates::write_cedar_template;
use crate::commands::helpers::get_layout;
use crate::output::table::Table;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::util::term::prompt_yes_no;
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
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
        /// Include the URN / authorization-node table (long only).
        /// Clap same-id skip hides global `--verbose`/`-v` on this subcommand;
        /// execute still keys the table off argv `--verbose` after `boundaries`
        /// so a leading `-v` cannot leak URNs.
        #[arg(long)]
        verbose: bool,
    },
}

/// Extract changed file paths via git status (ignore-filtered) as normalized paths.
///
/// Filter-only: no full impact / federation / cache rewrite (0146).
fn collect_changed_files(layout: &Layout) -> Result<HashSet<String>> {
    let changed: HashSet<String> = crate::git::status::collect_changed_files_for_filter(layout)?
        .iter()
        .map(|c| crate::git::status::normalize_filter_path(&c.path))
        .collect();
    Ok(changed)
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
///
/// 0215-A3: Cozo probe failure is a hard error, not a silent “unbuilt.”
pub(crate) fn graph_has_any_nodes(cozo: &crate::state::storage_cozo::CozoStorage) -> Result<bool> {
    let res = cozo.run_script("?[count(n)] := *node{id: n}")?;
    let populated = res
        .rows
        .first()
        .and_then(|r| r.first())
        .is_some_and(|v| matches!(v, cozo::DataValue::Num(cozo::Num::Int(i)) if *i > 0));
    Ok(populated)
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
    let changed_files = collect_changed_files(layout)?;
    let storage = StorageManager::open_read_only(layout)?;
    let cozo = storage
        .cozo()
        .ok_or_else(|| miette::miette!("CozoDB not available"))?;

    // Query all policy nodes and determine impact in-memory
    let query = "?[id, label, raw, effect, source_file] := *node{id, label, category: 'policy', metadata: meta}, \
                 raw = get(meta, 'raw'), \
                 effect = get(meta, 'effect'), \
                 source_file = get(meta, 'source_file')";
    let res = cozo.run_script(query)?;

    let mut indexed_rows = Vec::new();
    for row in res.rows {
        if let (
            Some(cozo::DataValue::Str(id)),
            Some(cozo::DataValue::Str(label)),
            Some(cozo::DataValue::Str(raw)),
            Some(cozo::DataValue::Str(effect)),
            Some(cozo::DataValue::Str(source_file)),
        ) = (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
        {
            let source_norm = source_file.as_str().replace('\\', "/");
            let is_changed = changed_files.contains(source_norm.as_str());
            indexed_rows.push((
                serde_json::json!({
                    "id": id,
                    "label": label,
                    "raw": raw,
                    "effect": effect,
                    "is_changed": is_changed,
                    "source_file": source_norm,
                }),
                is_changed,
            ));
        }
    }
    // 0208-C: indexed = rows with complete policy metadata (matches display
    // loop), not raw query row count.
    let indexed = indexed_rows.len();
    let displayed: Vec<_> = indexed_rows
        .into_iter()
        .filter(|(_, is_changed)| !changed || *is_changed)
        .map(|(item, _)| item)
        .collect();

    if displayed.is_empty() {
        let on_disk = crate::commands::surfaces::repo_root_cedar_present(&layout.root);
        let (reason, message) = if indexed > 0 && changed {
            (
                crate::output::empty::EmptyReason::CleanDiff,
                "No changed policies found in the current diff.".to_string(),
            )
        } else if on_disk {
            // 0208-B: disk first — never "Add Cedar" when files exist.
            (
                crate::output::empty::EmptyReason::NoIndexedData,
                "Cedar files on disk but not in the graph. Run `ledgerful index --analyze-graph`."
                    .to_string(),
            )
        } else if !graph_has_any_nodes(cozo)? {
            (
                crate::output::empty::EmptyReason::NoIndexedData,
                "Knowledge graph has not been built yet. Run `ledgerful index --analyze-graph` first, \
                 then add Cedar policy files to 'policies/' if this repo uses Cedar."
                    .to_string(),
            )
        } else {
            (
                crate::output::empty::EmptyReason::NoMatches,
                "Knowledge graph is populated, but no Cedar policy/principal/action/resource nodes exist. \
                 This repo has no Cedar policy files configured — add them under 'policies/' and run \
                 `ledgerful index --analyze-graph` to populate this surface."
                    .to_string(),
            )
        };
        if json {
            let mut output =
                crate::output::empty::format_json_empty_state(displayed, "impacted", || {
                    (reason, message)
                });
            if let Some(map) = output.as_object_mut() {
                map.insert("indexedCount".to_string(), serde_json::json!(indexed));
            }
            crate::output::json::emit(&output)?;
        } else {
            println!(
                "{}",
                "Security Policy Impact Analysis"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().red()))
            );
            println!(
                "{}",
                format!("  {message}").if_supports_color(Stream::Stdout, |s| s.dimmed())
            );
            if indexed > 0 && changed {
                println!(
                    "  {} of {} policies match changed files",
                    displayed
                        .len()
                        .to_string()
                        .if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().yellow().bold())),
                    indexed
                        .to_string()
                        .if_supports_color(Stream::Stdout, |s| s.bold()),
                );
            }
        }
    } else if json {
        let mut output =
            crate::output::empty::format_json_empty_state(displayed, "impacted", || {
                (crate::output::empty::EmptyReason::NoMatches, String::new())
            });
        if let Some(map) = output.as_object_mut() {
            map.insert("indexedCount".to_string(), serde_json::json!(indexed));
        }
        crate::output::json::emit(&output)?;
    } else {
        println!(
            "{}",
            "Security Policy Impact Analysis"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().red()))
        );
        let mut table = Table::new();
        table.set_header(vec!["Policy ID", "Effect", "Changed?"]);

        let mut changed_count = 0usize;
        for item in &displayed {
            let is_changed = item["is_changed"].as_bool().unwrap_or(false);
            if is_changed {
                changed_count += 1;
            }
            table.add_row(vec![
                item["id"].as_str().unwrap_or("").to_string(),
                item["effect"].as_str().unwrap_or_default().to_string(),
                if is_changed {
                    "YES"
                        .if_supports_color(Stream::Stdout, |s| {
                            s.style(Style::new().yellow().bold())
                        })
                        .to_string()
                } else {
                    "NO".to_string()
                },
            ]);
        }

        println!("{}", table);
        if changed {
            println!(
                "  {} of {} policies match changed files",
                displayed
                    .len()
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
                indexed
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
            );
        } else {
            println!(
                "  {} policies evaluated, {} changed by this diff",
                indexed
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
                changed_count
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
            );
        }
    }

    Ok(())
}

fn datavalue_as_str(v: &cozo::DataValue) -> Option<&str> {
    match v {
        cozo::DataValue::Str(s) => Some(s.as_ref()),
        _ => None,
    }
}

fn datavalue_json(v: &cozo::DataValue) -> Option<serde_json::Value> {
    match v {
        cozo::DataValue::Json(j) => serde_json::to_value(j).ok(),
        _ => None,
    }
}

fn annotations_id(meta: &serde_json::Value) -> Option<&str> {
    meta.get("annotations")
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
}

/// Shared CLI/REST assembly: resolved `@id` labels + method-safe, `/api`-preferred edges.
pub(crate) type BoundaryAssembly = (
    std::collections::BTreeMap<String, usize>,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
);

pub(crate) fn assemble_security_boundaries(
    cozo: &crate::state::storage_cozo::CozoStorage,
) -> Result<BoundaryAssembly> {
    let auth_res = cozo.run_script(
        "?[id, label, category, metadata] := *node{id, label, category, metadata}, \
         category in ['policy', 'principal', 'action', 'resource']",
    )?;
    let boundary_res = cozo.run_script(
        "?[policy_id, policy_label, relation, target_id, target_label, target_cat] := \
         *node{id: policy_id, label: policy_label, category: 'policy'}, \
         *edge{source: policy_id, target: target_id, relation: rel}, \
         *node{id: target_id, label: target_label, category: target_cat}, \
         target_cat in ['service', 'endpoint', 'config_key', 'deploy_surface', 'adr'], \
         relation = rel",
    )?;

    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut policy_actions: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    let mut auth_nodes = Vec::new();
    for row in &auth_res.rows {
        let (Some(id), Some(label), Some(cat)) = (
            row.first().and_then(datavalue_as_str),
            row.get(1).and_then(datavalue_as_str),
            row.get(2).and_then(datavalue_as_str),
        ) else {
            continue;
        };
        *counts.entry(cat.to_string()).or_insert(0) += 1;
        let meta = row.get(3).and_then(datavalue_json);
        let resolved = if cat == "policy" {
            let cedar_id = meta
                .as_ref()
                .and_then(|m| m.get("cedar_id"))
                .and_then(|v| v.as_str());
            let raw = meta
                .as_ref()
                .and_then(|m| m.get("raw"))
                .and_then(|v| v.as_str());
            let ann_id = meta.as_ref().and_then(annotations_id);
            crate::policy::cedar::policy_operator_label(label, cedar_id, ann_id, raw)
        } else {
            label.to_string()
        };
        if cat == "policy" {
            if let Some(action) = meta
                .as_ref()
                .and_then(|m| m.get("action"))
                .and_then(|v| v.as_str())
                .and_then(crate::policy::cedar::parse_cedar_action)
            {
                policy_actions.insert(id.to_string(), action);
            } else if let Some(action) = meta
                .as_ref()
                .and_then(|m| m.get("raw"))
                .and_then(|v| v.as_str())
                .and_then(crate::policy::cedar::parse_action_from_policy_raw)
            {
                policy_actions.insert(id.to_string(), action);
            }
        }
        auth_nodes.push(serde_json::json!({
            "id": id,
            "label": resolved,
            "category": cat,
        }));
    }
    auth_nodes.sort_by(|a, b| {
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

    let mut links = Vec::new();
    for row in &boundary_res.rows {
        let (Some(pid), Some(plabel), Some(rel), Some(tid), Some(tlabel), Some(tcat)) = (
            row.first().and_then(datavalue_as_str),
            row.get(1).and_then(datavalue_as_str),
            row.get(2).and_then(datavalue_as_str),
            row.get(3).and_then(datavalue_as_str),
            row.get(4).and_then(datavalue_as_str),
            row.get(5).and_then(datavalue_as_str),
        ) else {
            continue;
        };
        let policy_label = auth_nodes
            .iter()
            .find(|n| n["id"].as_str() == Some(pid))
            .and_then(|n| n["label"].as_str())
            .unwrap_or(plabel)
            .to_string();
        links.push(crate::policy::cedar::BoundaryLink {
            policy_id: pid.to_string(),
            policy_label,
            relation: rel.to_string(),
            target_id: tid.to_string(),
            target_label: tlabel.to_string(),
            target_category: tcat.to_string(),
        });
    }
    let links = crate::policy::cedar::refine_boundary_links(links, &policy_actions);
    let boundary_edges: Vec<serde_json::Value> = links
        .into_iter()
        .map(|l| {
            serde_json::json!({
                "policy_id": l.policy_id,
                "policy_label": l.policy_label,
                "relation": l.relation,
                "target_id": l.target_id,
                "target_label": l.target_label,
                "target_category": l.target_category,
            })
        })
        .collect();

    Ok((counts, auth_nodes, boundary_edges))
}

fn execute_boundaries(
    json: bool,
    verbose: bool,
    layout: &crate::state::layout::Layout,
) -> Result<()> {
    // Open the full StorageManager so we can reach both the CozoDB knowledge
    // graph (for the policy-node + graph-populated checks) and the SQLite
    // `api_routes` table (to count detected HTTP routes for the DX1 Cedar
    // template bootstrap offer).
    let storage = StorageManager::open_read_only(layout)?;
    let cozo = storage
        .cozo()
        .ok_or_else(|| miette::miette!("CozoDB not available"))?;

    let (counts, auth_nodes, boundary_edges) = assemble_security_boundaries(cozo)?;

    if json {
        let mut json_out = if auth_nodes.is_empty() {
            let (reason, message) = if graph_has_any_nodes(cozo)? {
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
        if let Some(map) = json_out.as_object_mut() {
            map.insert("pdp".to_string(), serde_json::json!(false));
        }
        crate::output::json::emit(&json_out)?;
    } else if auth_nodes.is_empty() {
        // CG-F35: empty taxonomy + DX1 prompt unchanged (no PDP one-liner).
        if graph_has_any_nodes(cozo)? {
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
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                println!(
                    "  This repo has no Cedar policy files configured. Add them under 'policies/' \
                     and run {} to populate this surface.",
                    "ledgerful index --analyze-graph"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
                );
            }
        } else {
            println!(
                "{}",
                "No security boundary data found — the knowledge graph has not been built yet."
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
            );
            println!(
                "  Run {} first, then add Cedar policy files to 'policies/' if this repo uses Cedar.",
                "ledgerful index --analyze-graph"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
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
            format!("Security Boundaries  [{summary}]")
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().green()))
        );
        println!(
            "{}",
            "not a live PDP — daemon auth is Bearer (0090)."
                .if_supports_color(Stream::Stdout, |s| s.dimmed())
        );

        println!(
            "\n{} ({} total)",
            "Cross-Surface Boundary Links (policy → protected entity):"
                .if_supports_color(Stream::Stdout, |s| s.bold()),
            boundary_edges
                .len()
                .to_string()
                .if_supports_color(Stream::Stdout, |s| s.bold()),
        );
        if boundary_edges.is_empty() {
            println!(
                "{}",
                "  No cross-surface links found. Run `ledgerful index --incremental` to refresh."
                    .if_supports_color(Stream::Stdout, |s| s.dimmed())
            );
        } else {
            let mut boundary_table = Table::new();
            boundary_table.set_header(vec!["Policy", "Relation", "Target", "Target Category"]);
            for edge in &boundary_edges {
                boundary_table.add_row(vec![
                    truncate(edge["policy_label"].as_str().unwrap_or_default(), 50),
                    edge["relation"].as_str().unwrap_or_default().to_string(),
                    truncate(edge["target_label"].as_str().unwrap_or_default(), 50),
                    edge["target_category"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                ]);
            }
            println!("{}", boundary_table);
        }

        if verbose {
            println!(
                "\n{} ({} total)",
                "Authorization Nodes (policy/principal/action/resource):"
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
                auth_nodes
                    .len()
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
            );
            let mut auth_table = Table::new();
            auth_table.set_header(vec!["Category", "Label", "ID"]);
            for node in &auth_nodes {
                auth_table.add_row(vec![
                    node["category"].as_str().unwrap_or_default().to_string(),
                    truncate(node["label"].as_str().unwrap_or_default(), 60),
                    truncate(node["id"].as_str().unwrap_or_default(), 80),
                ]);
            }
            println!("{}", auth_table);
        }
    }

    Ok(())
}

/// URN table only when `--verbose` appears after `boundaries`. Clap same-id
/// skip copies a leading `-v` onto the local field; that must not dump URNs.
fn urn_table_from_argv(parsed_verbose: bool) -> bool {
    let args: Vec<String> = std::env::args().collect();
    urn_table_from_args(parsed_verbose, &args)
}

fn urn_table_from_args(parsed_verbose: bool, args: &[String]) -> bool {
    match args.iter().position(|a| a == "boundaries") {
        Some(idx) => args.iter().skip(idx + 1).any(|a| a == "--verbose"),
        None => parsed_verbose,
    }
}

pub fn execute_security(args: SecurityArgs) -> Result<()> {
    let layout = get_layout()?;

    match args.command {
        SecuritySubcommands::Impact { changed, json } => execute_impact(changed, json, &layout),
        SecuritySubcommands::Boundaries { json, verbose } => {
            execute_boundaries(json, urn_table_from_argv(verbose), &layout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::urn_table_from_args;

    #[test]
    fn urn_table_only_when_verbose_follows_boundaries() {
        let after = ["ledgerful", "security", "boundaries", "--verbose"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(urn_table_from_args(true, &after));
        let leading_v = ["ledgerful", "-v", "security", "boundaries"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(
            !urn_table_from_args(true, &leading_v),
            "leading -v must not dump URNs"
        );
        let none = ["ledgerful", "security", "boundaries"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(!urn_table_from_args(false, &none));
        let lib_call: Vec<String> = vec!["integration".into()];
        assert!(
            urn_table_from_args(true, &lib_call),
            "library callers without argv `boundaries` keep the parsed flag"
        );
    }
}
