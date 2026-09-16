use crate::commands::helpers::get_layout;
use crate::config::model::ServiceInferenceState;
use crate::output::session_notice::{apply_empty_notice, notice_id_for_services};
use crate::output::table::Table;
use crate::state::cli_session::{CliSession, env_session_id};
use crate::state::storage::StorageManager;
use chrono::Utc;
use clap::Args;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};

#[derive(Args, Debug, Default)]
pub struct ServicesDiffArgs {
    /// Include up to 200 slash-normalized file paths per service
    #[arg(short, long)]
    pub full: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Infer topology in memory from the current index without persisting service_name
    #[arg(long)]
    pub preview: bool,
}

const FULL_FILES_CAP: usize = 200;

#[derive(Debug, Clone)]
struct ServiceRow {
    name: String,
    file_count: i64,
    route_count: i64,
    source: &'static str,
    root: Option<String>,
    files: Vec<String>,
    files_truncated: bool,
}

pub fn execute_services_diff(
    args: ServicesDiffArgs,
    config: &crate::config::model::Config,
) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let rows = if args.preview {
        build_preview_rows(&storage, config, args.full)?
    } else {
        build_persist_rows(&storage, config, args.full)?
    };

    if args.json {
        emit_json(&storage, config, &layout, &args, &rows)?;
    } else {
        emit_human(&storage, config, &layout, &args, &rows)?;
    }
    Ok(())
}

fn inference_state_json(state: ServiceInferenceState) -> &'static str {
    match state {
        ServiceInferenceState::Enabled => "enabled",
        ServiceInferenceState::DisabledGlobally => "disabledGlobally",
        ServiceInferenceState::DisabledForServices => "disabledForServices",
    }
}

fn declared_names_clause(config: &crate::config::model::Config) -> String {
    let mut names: Vec<&str> = config
        .services
        .definitions
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        ", or declare services explicitly under `[services]`".to_string()
    } else {
        format!(
            ". {} service(s) are declared under `[services]` ({}) but files are not assigned",
            names.len(),
            names.join(", ")
        )
    }
}

fn declared_envelope(config: &crate::config::model::Config) -> Vec<serde_json::Value> {
    let mut defs: Vec<_> = config.services.definitions.iter().collect();
    defs.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.root.cmp(&b.root)));
    defs.into_iter()
        .map(|d| {
            serde_json::json!({
                "name": d.name,
                "root": d.root,
            })
        })
        .collect()
}

fn source_for(name: &str, config: &crate::config::model::Config, preview: bool) -> &'static str {
    if config.services.definitions.iter().any(|d| d.name == name) {
        "declared"
    } else if preview {
        "preview"
    } else {
        "inferred"
    }
}

fn declared_root(name: &str, config: &crate::config::model::Config) -> Option<String> {
    config
        .services
        .definitions
        .iter()
        .find(|d| d.name == name)
        .map(|d| d.root.replace('\\', "/"))
}

fn slash_dir(dir: &std::path::Path) -> String {
    dir.to_string_lossy().replace('\\', "/")
}

fn is_root_dir(dir: &str) -> bool {
    dir.is_empty() || dir == "."
}

pub(crate) fn path_matches_dir(path: &str, dir: &str) -> bool {
    let path = path.replace('\\', "/");
    if is_root_dir(dir) {
        !path.contains('/')
    } else {
        path == dir || path.starts_with(&format!("{dir}/"))
    }
}

fn partition_paths(
    services: &[crate::impact::packet::Service],
    paths: Vec<String>,
) -> std::collections::HashMap<String, Vec<String>> {
    let mut sorted: Vec<&crate::impact::packet::Service> = services.iter().collect();
    sorted.sort_by(|a, b| {
        b.directory
            .components()
            .count()
            .cmp(&a.directory.components().count())
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut assigned: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for svc in services {
        assigned.entry(svc.name.clone()).or_default();
    }
    for path in paths {
        let slash = path.replace('\\', "/");
        if let Some(svc) = sorted
            .iter()
            .find(|svc| path_matches_dir(&slash, &slash_dir(&svc.directory)))
        {
            assigned.entry(svc.name.clone()).or_default().push(slash);
        }
    }
    for files in assigned.values_mut() {
        files.sort();
        files.dedup();
    }
    assigned
}

fn cap_files(mut files: Vec<String>) -> (Vec<String>, bool) {
    files.sort();
    files.dedup();
    let truncated = files.len() > FULL_FILES_CAP;
    files.truncate(FULL_FILES_CAP);
    (files, truncated)
}

fn load_file_paths(storage: &StorageManager) -> Result<Vec<String>> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare("SELECT file_path FROM project_files")
        .into_diagnostic()?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .into_diagnostic()?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.into_diagnostic()?);
    }
    Ok(out)
}

fn load_route_sources(storage: &StorageManager) -> Result<Vec<String>> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare("SELECT route_source FROM api_routes WHERE route_source IS NOT NULL")
        .into_diagnostic()?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .into_diagnostic()?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.into_diagnostic()?);
    }
    Ok(out)
}

fn persist_files_for(storage: &StorageManager, name: &str) -> Result<(Vec<String>, bool)> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare("SELECT file_path FROM project_files WHERE service_name = ?1 ORDER BY file_path")
        .into_diagnostic()?;
    let rows = stmt
        .query_map([name], |row| row.get::<_, String>(0))
        .into_diagnostic()?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row.into_diagnostic()?.replace('\\', "/"));
    }
    Ok(cap_files(files))
}

fn build_persist_rows(
    storage: &StorageManager,
    config: &crate::config::model::Config,
    full: bool,
) -> Result<Vec<ServiceRow>> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT pf.service_name, count(pf.id), count(ar.id)
         FROM project_files pf
         LEFT JOIN api_routes ar ON pf.id = ar.handler_file_id
         WHERE pf.service_name IS NOT NULL
         GROUP BY pf.service_name
         ORDER BY pf.service_name ASC",
        )
        .into_diagnostic()?;
    let mapped = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .into_diagnostic()?;
    let mut collected = Vec::new();
    for row in mapped {
        collected.push(row.into_diagnostic()?);
    }
    drop(stmt);
    let mut rows = Vec::new();
    for (name, files, routes) in collected {
        let source = source_for(&name, config, false);
        let root = declared_root(&name, config);
        let (file_list, files_truncated) = if full {
            persist_files_for(storage, &name)?
        } else {
            (Vec::new(), false)
        };
        rows.push(ServiceRow {
            name,
            file_count: files,
            route_count: routes,
            source,
            root,
            files: file_list,
            files_truncated,
        });
    }
    Ok(rows)
}

fn build_preview_rows(
    storage: &StorageManager,
    config: &crate::config::model::Config,
    full: bool,
) -> Result<Vec<ServiceRow>> {
    let inferred = crate::index::preview_inferred_services(storage, config)?;
    let file_map = partition_paths(&inferred, load_file_paths(storage)?);
    let route_map = partition_paths(&inferred, load_route_sources(storage)?);
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for svc in &inferred {
        seen.insert(svc.name.clone());
        let files = file_map.get(&svc.name).cloned().unwrap_or_default();
        let routes = route_map.get(&svc.name).cloned().unwrap_or_default();
        let source = source_for(&svc.name, config, true);
        let root = declared_root(&svc.name, config).or_else(|| {
            let dir = slash_dir(&svc.directory);
            if dir.is_empty() { None } else { Some(dir) }
        });
        let file_count = files.len() as i64;
        let (file_list, files_truncated) = if full {
            cap_files(files)
        } else {
            (Vec::new(), false)
        };
        rows.push(ServiceRow {
            name: svc.name.clone(),
            file_count,
            route_count: routes.len() as i64,
            source,
            root,
            files: file_list,
            files_truncated,
        });
    }
    let mut extra: Vec<_> = config
        .services
        .definitions
        .iter()
        .filter(|d| !seen.contains(&d.name))
        .collect();
    extra.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.root.cmp(&b.root)));
    for def in extra {
        rows.push(ServiceRow {
            name: def.name.clone(),
            file_count: 0,
            route_count: 0,
            source: "declared",
            root: Some(def.root.replace('\\', "/")),
            files: Vec::new(),
            files_truncated: false,
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.root.cmp(&b.root)));
    Ok(rows)
}

fn row_json(row: &ServiceRow, full: bool) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("service".to_string(), serde_json::json!(row.name));
    map.insert("file_count".to_string(), serde_json::json!(row.file_count));
    map.insert(
        "route_count".to_string(),
        serde_json::json!(row.route_count),
    );
    map.insert("source".to_string(), serde_json::json!(row.source));
    if let Some(root) = &row.root {
        map.insert("root".to_string(), serde_json::json!(root));
    }
    if full && !row.files.is_empty() {
        map.insert("files".to_string(), serde_json::json!(row.files));
    }
    if full && row.files_truncated {
        map.insert("filesTruncated".to_string(), serde_json::json!(true));
    }
    serde_json::Value::Object(map)
}

fn decorate_envelope(
    mut output: serde_json::Value,
    config: &crate::config::model::Config,
    preview: bool,
) -> serde_json::Value {
    if let Some(obj) = output.as_object_mut() {
        obj.insert(
            "inferenceState".to_string(),
            serde_json::json!(inference_state_json(
                config.coverage.service_inference_state()
            )),
        );
        if preview {
            obj.insert("preview".to_string(), serde_json::json!(true));
        }
        let declared = declared_envelope(config);
        if !declared.is_empty() {
            obj.insert("declared".to_string(), serde_json::json!(declared));
        }
    }
    output
}

fn emit_json(
    storage: &StorageManager,
    config: &crate::config::model::Config,
    layout: &crate::state::layout::Layout,
    args: &ServicesDiffArgs,
    rows: &[ServiceRow],
) -> Result<()> {
    let results: Vec<serde_json::Value> = rows.iter().map(|r| row_json(r, args.full)).collect();
    let mut output = crate::output::empty::format_json_empty_state(results, "results", || {
        empty_state_message(storage, config)
    });
    output = decorate_envelope(output, config, args.preview);
    let pending_session = if !args.preview
        && output.get("emptyReason").is_some()
        && let Some(id) = notice_id_for_services(config.coverage.service_inference_state())
    {
        let (_, full) = empty_state_message(storage, config);
        let mut session = CliSession::load(layout, env_session_id().as_deref(), Utc::now());
        let applied = apply_empty_notice(&mut session, id, &full, output);
        output = applied.json;
        output = decorate_envelope(output, config, args.preview);
        Some(session)
    } else {
        None
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).into_diagnostic()?
    );
    if let Some(session) = pending_session {
        session.persist();
    }
    Ok(())
}

fn emit_human(
    storage: &StorageManager,
    config: &crate::config::model::Config,
    layout: &crate::state::layout::Layout,
    args: &ServicesDiffArgs,
    rows: &[ServiceRow],
) -> Result<()> {
    let title = human_title(args.preview);
    println!(
        "{}",
        title.if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
    );
    if rows.is_empty() {
        let (_, full) = empty_state_message(storage, config);
        let (msg, pending_session) = if !args.preview
            && let Some(id) = notice_id_for_services(config.coverage.service_inference_state())
        {
            let mut session = CliSession::load(layout, env_session_id().as_deref(), Utc::now());
            let applied = apply_empty_notice(&mut session, id, &full, serde_json::json!({}));
            (applied.human, Some(session))
        } else {
            (full, None)
        };
        println!("{}", msg.if_supports_color(Stream::Stdout, |s| s.dimmed()));
        if let Some(session) = pending_session {
            session.persist();
        }
        return Ok(());
    }

    let mut table = Table::new();
    table.set_header(vec!["Service", "Files", "Endpoints", "Status"]);
    for row in rows {
        let status = match row.source {
            "declared" => "Declared"
                .if_supports_color(Stream::Stdout, |s| s.green())
                .to_string(),
            "preview" => "Preview"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
            _ => "Inferred"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
        };
        table.add_row(vec![
            row.name
                .if_supports_color(Stream::Stdout, |s| s.bold())
                .to_string(),
            row.file_count.to_string(),
            row.route_count.to_string(),
            status,
        ]);
    }
    println!("{table}");
    if args.full {
        for row in rows {
            let root = row.root.as_deref().unwrap_or("-");
            println!("{} ({root}) - {} files:", row.name, row.file_count);
            for path in &row.files {
                println!("  - {path}");
            }
            if row.files_truncated {
                let more = (row.file_count as usize).saturating_sub(row.files.len());
                println!("  ... and {more} more files (cap {FULL_FILES_CAP})");
            }
        }
    }
    Ok(())
}

fn human_title(preview: bool) -> &'static str {
    if preview {
        "Service topology preview (not persisted)"
    } else {
        "Service Boundary Summary"
    }
}

#[cfg(test)]
pub(crate) fn render_empty_human(preview: bool, message: &str) -> String {
    format!("{}\n{message}\n", human_title(preview))
}

use crate::output::empty::{EmptyReason, config_enable_hint};

/// Builds the message shown when no services are listed, consulting the same
/// `coverage.enabled` / `coverage.services.enabled` switches the indexer uses
/// (via `CoverageConfig::service_inference_state`) so this never tells a user
/// to reindex when reindexing cannot change the outcome.
pub fn empty_state_message(
    storage: &StorageManager,
    config: &crate::config::model::Config,
) -> (EmptyReason, String) {
    match config.coverage.service_inference_state() {
        ServiceInferenceState::DisabledGlobally => {
            let hint = config_enable_hint(&["coverage.enabled", "coverage.services.enabled"]);
            let declared = declared_names_clause(config);
            (
                EmptyReason::DisabledByConfig,
                format!(
                    "  No services found. Service inference is disabled by the global \
                     `coverage.enabled = false` switch in `.ledgerful/config.toml` -- reindexing will \
                     not change this. Set `coverage.enabled = true` and `coverage.services.enabled = true` \
                     to allow inference{declared}. {hint}"
                ),
            )
        }
        ServiceInferenceState::DisabledForServices => {
            let hint = config_enable_hint(&["coverage.services.enabled"]);
            let declared = declared_names_clause(config);
            (
                EmptyReason::DisabledByConfig,
                format!(
                    "  No services found. Service inference is disabled by \
                     `coverage.services.enabled = false` in `.ledgerful/config.toml` -- reindexing will \
                     not change this. Set it to `true` to allow inference{declared}. {hint}"
                ),
            )
        }
        ServiceInferenceState::Enabled => {
            let declared = config.services.definitions.len();
            let threshold_days = config.index.stale_threshold_days;
            let stale = crate::index::staleness::check_index_staleness(storage, threshold_days);

            if declared > 0 {
                match stale {
                    Some(w) if w.is_missing => (
                        EmptyReason::NoIndexedData,
                        format!(
                            "  No services found. {declared} service(s) are declared under `[services]`, \
                         but the index has never been built. Run `ledgerful index --incremental` to \
                         assign files to them."
                        ),
                    ),
                    Some(_) => (
                        EmptyReason::StaleIndex,
                        format!(
                            "  No services found. {declared} service(s) are declared under `[services]`, \
                         but no indexed files are currently assigned to them and the index looks stale. \
                         Run `ledgerful index --incremental` to refresh, then re-check."
                        ),
                    ),
                    None => (
                        EmptyReason::NoMatches,
                        format!(
                            "  No services found. {declared} service(s) are declared under `[services]`, \
                         but no indexed files are assigned to them even though the index is current -- \
                         reindexing is unlikely to help. Check that each `root` path in `[services]` \
                         matches real files in this repo."
                        ),
                    ),
                }
            } else {
                match stale {
                    Some(w) if w.is_missing => (
                        EmptyReason::NoIndexedData,
                        "  No services found. The index has never been built. Run \
                         `ledgerful index --incremental` to infer service boundaries from file \
                         structure."
                            .to_string(),
                    ),
                    Some(_) => (
                        EmptyReason::StaleIndex,
                        "  No services found. Service inference is enabled but the index looks \
                         stale. Run `ledgerful index --incremental` to refresh, then re-check."
                            .to_string(),
                    ),
                    None => (
                        EmptyReason::NoMatches,
                        "  No services found. Service inference is enabled and the index is \
                         current, so this likely reflects a genuine absence of service boundaries \
                         -- reindexing is unlikely to help. Declare services explicitly under \
                         `[services]` if this repo has services that aren't being inferred."
                            .to_string(),
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod services_diff_unit_tests {
    use super::*;
    use crate::config::model::{Config, ServiceDefinition};
    use tempfile::tempdir;

    fn sample_def(name: &str, root: &str) -> ServiceDefinition {
        ServiceDefinition {
            name: name.to_string(),
            root: root.to_string(),
            owners: vec![],
            runtime_name: None,
            queues: vec![],
            topics: vec![],
            rpc_endpoints: vec![],
        }
    }

    #[test]
    fn services_gated_declared_names_in_empty_copy() {
        let tmp = tempdir().unwrap();
        let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
        let mut config = Config::default();
        config.coverage.enabled = false;
        config.coverage.services.enabled = true;
        config
            .services
            .definitions
            .push(sample_def("billing-api", "src/billing"));
        let (_, msg) = empty_state_message(&storage, &config);
        assert!(msg.contains("billing-api"), "got: {msg}");
        assert!(msg.contains("not change"), "got: {msg}");
        assert!(
            !msg.contains("or declare services explicitly"),
            "should not tell the operator to declare what exists: {msg}"
        );
    }

    #[test]
    fn services_human_empty_has_no_header_only_table() {
        let rendered = render_empty_human(false, "  No services found.");
        assert!(rendered.contains("Service Boundary"));
        assert!(!rendered.contains("+===="), "got: {rendered}");
        assert!(!rendered.contains("| Service |"), "got: {rendered}");
        let preview = render_empty_human(true, "  No services found.");
        assert!(preview.to_lowercase().contains("preview"));
        assert!(preview.contains("not persisted"));
        assert!(!preview.contains("{"));
    }

    #[test]
    fn services_path_matches_root_and_nested() {
        assert!(path_matches_dir("lib.rs", "."));
        assert!(path_matches_dir("lib.rs", ""));
        assert!(!path_matches_dir("src/lib.rs", "."));
        assert!(path_matches_dir("src/billing/mod.rs", "src/billing"));
        assert!(path_matches_dir("src/billing", "src/billing"));
        assert!(!path_matches_dir("src/billing-extra/mod.rs", "src/billing"));
        assert!(path_matches_dir("src/billing/mod.rs", "src"));
    }

    #[test]
    fn services_full_caps_files_at_200_and_sets_truncated() {
        let files: Vec<String> = (0..250).map(|i| format!("src/f{i:03}.rs")).collect();
        let (capped, truncated) = cap_files(files);
        assert!(truncated);
        assert_eq!(capped.len(), 200);
        assert_eq!(capped[0], "src/f000.rs");
    }

    #[test]
    fn services_preview_does_not_update_service_name() {
        let tmp = tempdir().unwrap();
        let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO project_files (file_path, last_indexed_at, service_name)
                 VALUES ('src/lib.rs', '2026-09-16T00:00:00Z', 'existing')",
                [],
            )
            .unwrap();
        let mut config = Config::default();
        config.coverage.enabled = true;
        config.coverage.services.enabled = true;
        config
            .services
            .definitions
            .push(sample_def("billing-api", "src/billing"));
        let snapshot = |st: &StorageManager| -> (i64, String) {
            let conn = st.get_connection();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM project_files WHERE service_name IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let names: String = conn
                .query_row(
                    "SELECT GROUP_CONCAT(service_name) FROM project_files WHERE service_name IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            (count, names)
        };
        let before = snapshot(&storage);
        let inferred = crate::index::preview_inferred_services(&storage, &config).unwrap();
        assert!(
            !inferred.is_empty(),
            "declared service must appear in preview"
        );
        let after = snapshot(&storage);
        assert_eq!(before, after, "preview must not rewrite service_name");
        assert_eq!(before.0, 1);
        assert_eq!(before.1, "existing");
    }
}
