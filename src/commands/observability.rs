use crate::commands::dx1_templates::write_openslo_template;
use crate::commands::helpers::get_layout;
use crate::observability::openslo::{ParsedSlo, parse_openslo};
use crate::output::empty::EmptyReason;
use crate::output::session_notice::{ALREADY_SHOWN_HUMAN, SessionNoticeId, apply_empty_notice};
use crate::output::table::build_premium_table;
use crate::state::cli_session::{CliSession, env_session_id};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::util::term::prompt_yes_no;
use camino::Utf8Path;
use chrono::Utc;
use clap::{Args, Subcommand};
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// 0215-A5: indexed==0 three-arm empty taxonomy (disk / unbuilt / NoMatches).
/// CleanDiff is diff-only and stays in persist `diff`.
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

const INPUTS_KINDS: [&str; 7] = [
    "AlertCondition",
    "AlertNotificationTarget",
    "AlertPolicy",
    "DataSource",
    "SLI",
    "SLO",
    "Service",
];

#[derive(Args, Debug)]
pub struct ObservabilityArgs {
    #[command(subcommand)]
    pub command: ObservabilitySubcommands,
}

#[derive(Subcommand, Debug)]
pub enum ObservabilitySubcommands {
    /// Show observability coverage for services
    Coverage {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Parse repo-root OpenSLO YAML without writing the graph
        #[arg(long)]
        preview: bool,
    },
    /// Show observability changes based on current diff (changed SLOs, metrics, alerts)
    Diff {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Parse repo-root OpenSLO YAML without writing the graph
        #[arg(long)]
        preview: bool,
    },
}

struct ParseErrorRow {
    path: String,
    reason: String,
}

struct DiskOpenSlo {
    yaml_present: bool,
    entities: Vec<(String, ParsedSlo)>,
    parse_errors: Vec<ParseErrorRow>,
}

fn relative_source_file(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() || s.contains('\\') {
        return None;
    }
    if s.starts_with('/') {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }
    Some(s.to_string())
}

fn slash_rel_from_root(root: &Utf8Path, path: &Path) -> String {
    let abs = path.to_string_lossy().replace('\\', "/");
    let root_prefix = format!(
        "{}/",
        root.as_str().replace('\\', "/").trim_end_matches('/')
    );
    abs.strip_prefix(&root_prefix)
        .unwrap_or(abs.as_str())
        .to_string()
}

fn read_openslo_dir(root: &Utf8Path) -> DiskOpenSlo {
    let dir = root.join("observability");
    let mut yaml_present = false;
    let mut entities = Vec::new();
    let mut parse_errors = Vec::new();
    let Ok(entries) = fs::read_dir(dir.as_std_path()) else {
        return DiskOpenSlo {
            yaml_present,
            entities,
            parse_errors,
        };
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("yml") && ext != Some("yaml") {
            continue;
        }
        yaml_present = true;
        let rel = slash_rel_from_root(root, &path);
        match fs::read_to_string(&path) {
            Ok(content) => match parse_openslo(&content) {
                Ok(parsed) => {
                    for entity in parsed {
                        entities.push((rel.clone(), entity));
                    }
                }
                Err(reason) => parse_errors.push(ParseErrorRow { path: rel, reason }),
            },
            Err(e) => parse_errors.push(ParseErrorRow {
                path: rel,
                reason: e.to_string(),
            }),
        }
    }
    parse_errors.sort_by(|a, b| a.path.cmp(&b.path));
    DiskOpenSlo {
        yaml_present,
        entities,
        parse_errors,
    }
}

fn preview_coverage_empty_state(disk: &DiskOpenSlo) -> (EmptyReason, String) {
    if !disk.yaml_present {
        (
            EmptyReason::NoMatches,
            "No OpenSLO YAML under observability/. Add `kind: Service` plus matching `kind: SLO`."
                .to_string(),
        )
    } else {
        (
            EmptyReason::NoMatches,
            "OpenSLO files found but no `kind: Service` documents.".to_string(),
        )
    }
}

fn preview_diff_empty_state(disk: &DiskOpenSlo) -> (EmptyReason, String) {
    if !disk.yaml_present {
        (
            EmptyReason::NoMatches,
            "No OpenSLO YAML under observability/. Add `kind: Service` plus matching `kind: SLO`."
                .to_string(),
        )
    } else {
        (
            EmptyReason::NoMatches,
            "OpenSLO files found but no SLO, metric, alert, or signal documents.".to_string(),
        )
    }
}

fn coverage_rows_from_disk(disk: &DiskOpenSlo) -> Vec<(String, i64, i64)> {
    let mut rows = Vec::new();
    for (_, entity) in &disk.entities {
        if entity.kind != "Service" {
            continue;
        }
        let mut slo_count = 0_i64;
        let mut metric_count = 0_i64;
        for (_, other) in &disk.entities {
            if other.kind != "SLO" {
                continue;
            }
            if other.service_name.as_deref() != Some(entity.name.as_str()) {
                continue;
            }
            slo_count += 1;
            metric_count += other.metrics.len() as i64;
        }
        rows.push((format!("Service: {}", entity.name), slo_count, metric_count));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

fn diff_nodes_from_disk(disk: &DiskOpenSlo) -> Vec<(String, String, String, Option<String>)> {
    let mut nodes = Vec::new();
    for (rel, entity) in &disk.entities {
        let source = relative_source_file(Some(rel.as_str()));
        match entity.kind.as_str() {
            "SLO" => {
                nodes.push((
                    entity.urn.clone(),
                    format!("SLO: {}", entity.name),
                    "slo".to_string(),
                    source.clone(),
                ));
                for metric in &entity.metrics {
                    nodes.push((
                        metric.urn.clone(),
                        format!("Metric: {}", metric.name),
                        "metric".to_string(),
                        source.clone(),
                    ));
                }
            }
            "SLI" => {
                nodes.push((
                    entity.urn.clone(),
                    format!("SLI: {}", entity.name),
                    "metric".to_string(),
                    source.clone(),
                ));
                for metric in &entity.metrics {
                    nodes.push((
                        metric.urn.clone(),
                        format!("Metric: {}", metric.name),
                        "metric".to_string(),
                        source.clone(),
                    ));
                }
            }
            "AlertPolicy" | "AlertCondition" => {
                nodes.push((
                    entity.urn.clone(),
                    format!("{}: {}", entity.kind, entity.name),
                    "alert".to_string(),
                    source,
                ));
            }
            "DataSource" => {
                nodes.push((
                    entity.urn.clone(),
                    format!("DataSource: {}", entity.name),
                    "observability_signal".to_string(),
                    source,
                ));
            }
            _ => {}
        }
    }
    nodes.sort_by(|a, b| (&a.2, &a.1, &a.0).cmp(&(&b.2, &b.1, &b.0)));
    nodes
}

fn inputs_value() -> Value {
    json!({
        "directory": "observability/",
        "ingest": "ledgerful index --analyze-graph",
        "kinds": INPUTS_KINDS,
    })
}

fn parse_errors_value(errors: &[ParseErrorRow]) -> Value {
    let items: Vec<Value> = errors
        .iter()
        .map(|e| json!({ "path": e.path, "reason": e.reason }))
        .collect();
    json!(items)
}

fn emit_parse_errors_stderr(errors: &[ParseErrorRow]) {
    if errors.is_empty() {
        return;
    }
    let _ = writeln!(io::stderr(), "OpenSLO parse errors:");
    for err in errors {
        let _ = writeln!(io::stderr(), "  {}: {}", err.path, err.reason);
    }
}

fn coverage_json_map(
    rows: &[(String, i64, i64)],
    parse_errors: &[ParseErrorRow],
    empty: Option<(EmptyReason, String)>,
    preview: bool,
) -> Map<String, Value> {
    let items: Vec<Value> = rows
        .iter()
        .map(|(svc, sc, mc)| {
            json!({
                "service": svc,
                "slo_count": sc,
                "metric_count": mc,
                "health": if *sc > 0 { "covered" } else { "missing" },
            })
        })
        .collect();
    let mut map = Map::new();
    map.insert("schemaVersion".to_string(), json!(1));
    map.insert("results".to_string(), json!(items));
    map.insert("resultCount".to_string(), json!(rows.len()));
    map.insert("inputs".to_string(), inputs_value());
    map.insert("notWired".to_string(), json!(["endpoints"]));
    if let Some((reason, message)) = empty {
        map.insert("emptyReason".to_string(), json!(reason));
        map.insert("message".to_string(), json!(message));
    }
    if !parse_errors.is_empty() {
        map.insert("parseErrors".to_string(), parse_errors_value(parse_errors));
    }
    if preview {
        map.insert("preview".to_string(), json!(true));
    }
    map
}

fn sort_diff_entries(entries: &mut [Value]) {
    entries.sort_by(|a, b| {
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
}

fn diff_item(
    id: &str,
    label: &str,
    category: &str,
    changed: bool,
    source_file: Option<&str>,
) -> Value {
    let mut map = Map::new();
    map.insert("id".to_string(), json!(id));
    map.insert("label".to_string(), json!(label));
    map.insert("category".to_string(), json!(category));
    map.insert("changed".to_string(), json!(changed));
    if let Some(sf) = source_file {
        map.insert("sourceFile".to_string(), json!(sf));
    }
    Value::Object(map)
}

fn diff_json_map(
    changed: &[Value],
    unchanged_count: usize,
    indexed_count: usize,
    parse_errors: &[ParseErrorRow],
    empty: Option<(EmptyReason, String)>,
    preview: bool,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("schemaVersion".to_string(), json!(1));
    map.insert("kind".to_string(), json!("observabilityDiff"));
    map.insert("changed".to_string(), json!(changed));
    map.insert("unchanged_count".to_string(), json!(unchanged_count));
    map.insert("indexedCount".to_string(), json!(indexed_count));
    map.insert("resultCount".to_string(), json!(changed.len()));
    if let Some((reason, message)) = empty {
        map.insert("emptyReason".to_string(), json!(reason));
        map.insert("message".to_string(), json!(message));
    }
    if !parse_errors.is_empty() {
        map.insert("parseErrors".to_string(), parse_errors_value(parse_errors));
    }
    if preview {
        map.insert("preview".to_string(), json!(true));
    }
    map
}

fn coverage_title(preview: bool) -> &'static str {
    if preview {
        "Observability Coverage (preview)"
    } else {
        "Observability Coverage Summary"
    }
}

fn diff_title(preview: bool) -> &'static str {
    if preview {
        "Observability Diff (preview)"
    } else {
        "Observability Diff"
    }
}

fn partition_disk_diff(
    disk: &DiskOpenSlo,
    changed_files: &std::collections::HashSet<String>,
) -> (Vec<Value>, Vec<Value>) {
    let mut changed = Vec::new();
    let mut unchanged = Vec::new();
    for (id, label, category, source) in diff_nodes_from_disk(disk) {
        let is_changed = source
            .as_deref()
            .map(|sf| changed_files.contains(sf))
            .unwrap_or(false);
        let entry = diff_item(&id, &label, &category, is_changed, source.as_deref());
        if is_changed {
            changed.push(entry);
        } else {
            unchanged.push(entry);
        }
    }
    sort_diff_entries(&mut changed);
    sort_diff_entries(&mut unchanged);
    (changed, unchanged)
}

fn print_coverage_table(rows: &[(String, i64, i64)]) {
    let mut table = build_premium_table(["Service", "SLOs", "Metrics", "Health"]);
    for (svc, sc, mc) in rows {
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

fn persist_coverage_rows(
    cozo: &crate::state::storage_cozo::CozoStorage,
) -> Result<Vec<(String, i64, i64)>> {
    let services_res = cozo.run_script(
        "?[svc_urn, service] := *node{id: svc_urn, label: service, category: 'service'}",
    )?;
    let slo_res = cozo.run_script("?[svc_urn, count(slo_urn)] := *edge{source: slo_urn, target: svc_urn, relation: 'monitors'}, *node{id: slo_urn, category: 'slo'}")?;
    let metric_res = cozo.run_script("?[svc_urn, count(m_urn)] := *edge{source: slo_urn, target: svc_urn, relation: 'monitors'}, *edge{source: slo_urn, target: m_urn, relation: 'depends_on'}, *node{id: m_urn, category: 'metric'}")?;

    let mut slo_map = HashMap::new();
    for row in slo_res.rows {
        if let (
            Some(cozo::DataValue::Str(svc_urn)),
            Some(cozo::DataValue::Num(cozo::Num::Int(count))),
        ) = (row.first(), row.get(1))
        {
            slo_map.insert(svc_urn.clone(), *count);
        }
    }

    let mut metric_map = HashMap::new();
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
            final_rows.push((service.to_string(), slo_count, metric_count));
        }
    }
    final_rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(final_rows)
}

fn execute_coverage(layout: &Layout, json: bool, preview: bool) -> Result<()> {
    let disk = read_openslo_dir(&layout.root);
    if preview {
        let rows = coverage_rows_from_disk(&disk);
        let empty = if rows.is_empty() {
            Some(preview_coverage_empty_state(&disk))
        } else {
            None
        };
        if json {
            let map = coverage_json_map(&rows, &disk.parse_errors, empty, true);
            crate::output::json::emit(&Value::Object(map))?;
        } else {
            println!(
                "{}",
                coverage_title(true)
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            if rows.is_empty() {
                let (_, msg) = preview_coverage_empty_state(&disk);
                println!(
                    "  {}",
                    msg.if_supports_color(Stream::Stdout, |s| s.yellow())
                );
            } else {
                print_coverage_table(&rows);
            }
            emit_parse_errors_stderr(&disk.parse_errors);
        }
        return Ok(());
    }

    let storage = StorageManager::open_read_only(layout)?;
    let cozo = storage
        .cozo()
        .ok_or_else(|| miette::miette!("CozoDB not available"))?;
    let mut final_rows = persist_coverage_rows(cozo)?;
    let mut is_preview = false;
    if final_rows.is_empty() {
        let disk_rows = coverage_rows_from_disk(&disk);
        if !disk_rows.is_empty() {
            final_rows = disk_rows;
            is_preview = true;
        }
    }

    if !json && final_rows.is_empty() {
        let mut session = CliSession::load(layout, env_session_id().as_deref(), Utc::now());
        if session.is_shown(SessionNoticeId::ObservabilityEmpty.as_str()) {
            println!("{ALREADY_SHOWN_HUMAN}");
            session.mark_shown(SessionNoticeId::ObservabilityEmpty.as_str());
            session.persist();
            emit_parse_errors_stderr(&disk.parse_errors);
            return Ok(());
        }
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
            session.mark_shown(SessionNoticeId::ObservabilityEmpty.as_str());
            session.persist();
            return Ok(());
        }
        println!(
            "  {}",
            "No OpenSLO coverage data found.".if_supports_color(Stream::Stdout, |s| s.yellow())
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
        session.mark_shown(SessionNoticeId::ObservabilityEmpty.as_str());
        session.persist();
        emit_parse_errors_stderr(&disk.parse_errors);
        return Ok(());
    }

    if json {
        let empty = if final_rows.is_empty() {
            let on_disk = crate::commands::surfaces::repo_root_openslo_present(&layout.root);
            let graph_populated = crate::commands::security::graph_has_any_nodes(cozo)?;
            Some(observability_indexed_zero_empty_state(
                on_disk,
                graph_populated,
            ))
        } else {
            None
        };
        let map = coverage_json_map(&final_rows, &disk.parse_errors, empty.clone(), is_preview);
        if let Some((_, message)) = empty {
            let mut session = CliSession::load(layout, env_session_id().as_deref(), Utc::now());
            let applied = apply_empty_notice(
                &mut session,
                SessionNoticeId::ObservabilityEmpty,
                &message,
                Value::Object(map),
            );
            crate::output::json::emit(&applied.json)?;
            session.persist();
        } else {
            crate::output::json::emit(&Value::Object(map))?;
        }
    } else {
        println!(
            "{}",
            coverage_title(is_preview)
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
        );
        print_coverage_table(&final_rows);
        emit_parse_errors_stderr(&disk.parse_errors);
    }
    Ok(())
}

fn execute_diff(layout: &Layout, json: bool, preview: bool) -> Result<()> {
    let disk = read_openslo_dir(&layout.root);
    let changed_files: std::collections::HashSet<String> =
        crate::git::status::collect_changed_files_for_filter(layout)?
            .iter()
            .map(|c| crate::git::status::normalize_filter_path(&c.path))
            .collect();

    if preview {
        let (changed, unchanged) = partition_disk_diff(&disk, &changed_files);
        let indexed = changed.len() + unchanged.len();
        let empty = if changed.is_empty() {
            if indexed == 0 {
                Some(preview_diff_empty_state(&disk))
            } else {
                Some((
                    EmptyReason::CleanDiff,
                    "No observability signals impacted by current diff.".to_string(),
                ))
            }
        } else {
            None
        };
        if json {
            let map = diff_json_map(
                &changed,
                unchanged.len(),
                indexed,
                &disk.parse_errors,
                empty,
                true,
            );
            crate::output::json::emit(&Value::Object(map))?;
        } else {
            println!(
                "{}",
                diff_title(true)
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            println!("Changed files in diff: {}", changed_files.len());
            if changed.is_empty() {
                if let Some((_, msg)) = empty {
                    println!("{}", msg.if_supports_color(Stream::Stdout, |s| s.dimmed()));
                }
            } else {
                println!(
                    "\n{}",
                    format!("{} observability signal(s) impacted:", changed.len())
                        .if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().yellow().bold()))
                );
                let mut table = build_premium_table(["Category", "Label", "ID", "Source"]);
                for item in &changed {
                    table.add_row(vec![
                        item["category"].as_str().unwrap_or("").to_string(),
                        item["label"].as_str().unwrap_or("").to_string(),
                        item["id"].as_str().unwrap_or("").to_string(),
                        item["sourceFile"].as_str().unwrap_or("-").to_string(),
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
            emit_parse_errors_stderr(&disk.parse_errors);
        }
        return Ok(());
    }

    let storage = StorageManager::open_read_only(layout)?;
    let cozo = storage
        .cozo()
        .ok_or_else(|| miette::miette!("CozoDB not available"))?;

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
            let source_file: Option<String> = row.get(3).and_then(|v| {
                if let cozo::DataValue::Json(j) = v {
                    j.get("source_file")
                        .and_then(|f| f.as_str())
                        .and_then(|s| relative_source_file(Some(s)))
                } else {
                    None
                }
            });
            let is_changed = source_file
                .as_deref()
                .map(|sf| changed_files.contains(sf))
                .unwrap_or(false);
            let entry = diff_item(id, label, cat, is_changed, source_file.as_deref());
            if is_changed {
                changed.push(entry);
            } else {
                unchanged.push(entry);
            }
        }
    }

    sort_diff_entries(&mut changed);
    sort_diff_entries(&mut unchanged);

    let mut indexed = changed.len() + unchanged.len();
    let mut is_preview = false;
    if indexed == 0 {
        let (disk_changed, disk_unchanged) = partition_disk_diff(&disk, &changed_files);
        if !disk_changed.is_empty() || !disk_unchanged.is_empty() {
            changed = disk_changed;
            unchanged = disk_unchanged;
            indexed = changed.len() + unchanged.len();
            is_preview = true;
        }
    }
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
        let empty = if changed.is_empty() {
            if let Some(empty) = indexed_zero_empty {
                Some(empty)
            } else {
                Some((
                    EmptyReason::CleanDiff,
                    "No observability signals impacted by current diff.".to_string(),
                ))
            }
        } else {
            None
        };
        let map = diff_json_map(
            &changed,
            unchanged.len(),
            indexed,
            &disk.parse_errors,
            empty,
            is_preview,
        );
        crate::output::json::emit(&Value::Object(map))?;
    } else {
        println!(
            "{}",
            diff_title(is_preview)
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
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
            );
            let mut table = build_premium_table(["Category", "Label", "ID", "Source"]);
            for item in &changed {
                table.add_row(vec![
                    item["category"].as_str().unwrap_or("").to_string(),
                    item["label"].as_str().unwrap_or("").to_string(),
                    item["id"].as_str().unwrap_or("").to_string(),
                    item["sourceFile"].as_str().unwrap_or("-").to_string(),
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
        emit_parse_errors_stderr(&disk.parse_errors);
    }
    Ok(())
}

pub fn execute_observability(args: ObservabilityArgs) -> Result<()> {
    let layout = get_layout()?;
    match args.command {
        ObservabilitySubcommands::Coverage { json, preview } => {
            execute_coverage(&layout, json, preview)
        }
        ObservabilitySubcommands::Diff { json, preview } => execute_diff(&layout, json, preview),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observability_diff_table_uses_premium_framing_and_deterministic_order() {
        let mut changed = vec![
            serde_json::json!({"id": "b", "label": "Beta", "category": "metric", "sourceFile": "b.yaml"}),
            serde_json::json!({"id": "a", "label": "Alpha", "category": "metric"}),
            serde_json::json!({"id": "c", "label": "Alpha", "category": "alert"}),
        ];
        sort_diff_entries(&mut changed);

        let mut table = build_premium_table(["Category", "Label", "ID", "Source"]);
        for item in &changed {
            table.add_row(vec![
                item["category"].as_str().unwrap_or("").to_string(),
                item["label"].as_str().unwrap_or("").to_string(),
                item["id"].as_str().unwrap_or("").to_string(),
                item["sourceFile"].as_str().unwrap_or("-").to_string(),
            ]);
        }
        let rendered = table.to_string();
        assert!(
            rendered.contains('╭') || rendered.contains('+'),
            "expected premium table border (utf8 rounded or ascii +), got:\n{rendered}"
        );
        assert!(
            rendered.contains("Category")
                && rendered.contains("Label")
                && rendered.contains("ID")
                && rendered.contains("Source"),
            "expected headers, got:\n{rendered}"
        );
        let alert_pos = rendered.find("alert").unwrap_or(usize::MAX);
        let metric_alpha_pos = rendered.find("metric").unwrap_or(usize::MAX);
        let beta_pos = rendered.find("Beta").unwrap_or(usize::MAX);
        assert!(
            alert_pos < metric_alpha_pos && metric_alpha_pos < beta_pos,
            "expected deterministic order, got:\n{rendered}"
        );
    }

    #[test]
    fn relative_source_file_rejects_windows_absolute() {
        assert_eq!(
            relative_source_file(Some("observability/dogfood_slo.yaml")),
            Some("observability/dogfood_slo.yaml".to_string())
        );
        assert_eq!(relative_source_file(Some(r"C:\dev\x.yaml")), None);
        assert_eq!(relative_source_file(Some("C:/dev/x.yaml")), None);
        assert_eq!(relative_source_file(Some("/abs/x.yaml")), None);
        assert_eq!(relative_source_file(Some(r"obs\x.yaml")), None);
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

    #[test]
    fn coverage_json_map_key_order_and_always_health() {
        let rows = vec![("Service: a".to_string(), 1_i64, 1_i64)];
        let map = coverage_json_map(&rows, &[], None, false);
        let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "schemaVersion",
                "results",
                "resultCount",
                "inputs",
                "notWired"
            ]
        );
        assert_eq!(map["results"][0]["health"], "covered");
        let kinds = map["inputs"]["kinds"]
            .as_array()
            .expect("kinds")
            .iter()
            .map(|v| v.as_str().unwrap_or(""))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "AlertCondition",
                "AlertNotificationTarget",
                "AlertPolicy",
                "DataSource",
                "SLI",
                "SLO",
                "Service"
            ]
        );
    }
}
