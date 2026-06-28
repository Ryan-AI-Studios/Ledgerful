use crate::commands::helpers::get_layout;
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use clap::Args;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;

#[derive(Args, Debug)]
pub struct EndpointsArgs {
    /// Filter by method (e.g. GET, POST)
    #[arg(short, long)]
    method: Option<String>,
    /// Filter by path pattern
    #[arg(short, long)]
    path: Option<String>,
    /// Show auth details
    #[arg(long)]
    auth: bool,
    /// Only show endpoints whose handler file was changed in the current diff
    #[arg(long)]
    changed: bool,
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

pub fn execute_endpoints(args: EndpointsArgs) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout.root)?;
    let conn = storage.get_connection();

    // Build set of changed file paths when --changed is requested.
    let changed_files: Option<std::collections::HashSet<String>> = if args.changed {
        let packet = crate::commands::impact::execute_impact_silent()?;
        let set = packet
            .changes
            .iter()
            .map(|c| c.path.to_string_lossy().replace('\\', "/"))
            .collect();
        Some(set)
    } else {
        None
    };

    let mut query = String::from(
        "SELECT ar.method, ar.path_pattern, ar.handler_symbol_name, ar.framework, \
         ar.auth_requirements, ar.owning_service, ar.consumers, pf.file_path \
         FROM api_routes ar \
         LEFT JOIN project_files pf ON ar.handler_file_id = pf.id \
         WHERE 1=1",
    );
    let mut params: Vec<String> = Vec::new();

    if let Some(m) = &args.method {
        query.push_str(" AND ar.method = ?");
        params.push(m.to_uppercase());
    }
    if let Some(p) = &args.path {
        query.push_str(" AND ar.path_pattern LIKE ?");
        params.push(format!("%{}%", p));
    }

    query.push_str(" ORDER BY path_pattern ASC");

    let mut stmt = conn.prepare(&query).into_diagnostic()?;
    let params_refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    #[allow(clippy::type_complexity)]
    let all_rows: Vec<(
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = stmt
        .query_map(&params_refs[..], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .into_diagnostic()?
        .collect::<std::result::Result<Vec<_>, _>>()
        .into_diagnostic()?;

    let all_rows_empty = all_rows.is_empty();

    // Apply --changed filter: keep only routes whose handler file was changed.
    let rows: Vec<_> = if let Some(ref cf) = changed_files {
        all_rows
            .into_iter()
            .filter(|(_, _, _, _, _, _, _, file_path)| {
                file_path
                    .as_deref()
                    .map(|fp| cf.contains(&fp.replace('\\', "/")))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        all_rows
    };

    if args.json {
        let mut results = Vec::new();
        for (method, path, handler, framework, auth, service, consumers, _) in &rows {
            results.push(serde_json::json!({
                "method": method,
                "path": path,
                "handler": handler,
                "framework": framework,
                "auth": auth.as_deref().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
                "service": service,
                "consumers": consumers.as_deref().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
            }));
        }
        let output = crate::output::empty::format_json_empty_state(results, "results", || {
            if all_rows_empty {
                (
                    crate::output::empty::EmptyReason::NoIndexedData,
                    "No endpoints indexed. Endpoints are extracted from HTTP route registrations \
                     (Axum, Express, etc.). Run `ledgerful index --incremental` if routes exist, \
                     or confirm your framework is supported."
                        .to_string(),
                )
            } else {
                (
                    crate::output::empty::EmptyReason::CleanDiff,
                    "No endpoints changed in the current diff.".to_string(),
                )
            }
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        let mut table = Table::new();
        table.set_header(vec!["Method", "Path", "Framework", "Service", "Auth"]);

        for (method, path, _handler, framework, auth_json, service, _consumers, _) in &rows {
            let auth_str = if let Some(aj) = auth_json {
                if aj == "[]" || aj == "null" {
                    "None".to_string()
                } else {
                    aj.clone()
                }
            } else {
                "Unknown".to_string()
            };

            table.add_row(vec![
                method.clone(),
                path.clone(),
                framework.clone(),
                service.clone().unwrap_or_else(|| "-".to_string()),
                auth_str,
            ]);
        }
        if rows.is_empty() {
            if all_rows_empty {
                println!(
                    "{}",
                    "  No endpoints indexed. Endpoints are extracted from HTTP route registrations \
                     (Axum, Express, etc.). Run `ledgerful index --incremental` if routes exist, \
                     or confirm your framework is supported."
                        .dimmed()
                );
            } else {
                println!("{}", "  No endpoints changed in the current diff.".dimmed());
            }
        }
        println!("{}", table);
    }

    Ok(())
}
