use crate::commands::helpers::get_layout;
use crate::config::model::ServiceInferenceState;
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use clap::Args;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;

#[derive(Args, Debug)]
pub struct ServicesDiffArgs {
    /// Show full topology
    #[arg(short, long)]
    pub full: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn execute_services_diff(
    args: ServicesDiffArgs,
    config: &crate::config::model::Config,
) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout.root)?;
    let conn = storage.get_connection();

    // Query for services and their boundaries
    let mut stmt = conn
        .prepare(
            "SELECT pf.service_name, count(pf.id), count(ar.id)
         FROM project_files pf
         LEFT JOIN api_routes ar ON pf.id = ar.handler_file_id
         WHERE pf.service_name IS NOT NULL
         GROUP BY pf.service_name",
        )
        .into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .into_diagnostic()?;

    if args.json {
        let mut results = Vec::new();
        for row in rows {
            let (name, files, routes) = row.into_diagnostic()?;
            results.push(serde_json::json!({
                "service": name,
                "file_count": files,
                "route_count": routes,
            }));
        }
        let output = crate::output::empty::format_json_empty_state(results, "results", || {
            empty_state_message(&storage, config)
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        println!("{}", "Service Boundary Summary".bold().cyan());
        let mut table = Table::new();
        table.set_header(vec!["Service", "Files", "Endpoints", "Status"]);
        let mut row_count = 0usize;

        for row in rows {
            let (name, files, routes) = row.into_diagnostic()?;
            row_count += 1;

            // Check if declared in config
            let is_declared = config.services.definitions.iter().any(|d| d.name == name);
            let status = if is_declared {
                "Declared".green().to_string()
            } else {
                "Inferred".yellow().to_string()
            };

            table.add_row(vec![
                name.bold().to_string(),
                files.to_string(),
                routes.to_string(),
                status,
            ]);
        }
        if row_count == 0 {
            let (_, msg) = empty_state_message(&storage, config);
            println!("{}", msg.dimmed());
        }
        println!("{}", table);
    }

    Ok(())
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
            (
                EmptyReason::DisabledByConfig,
                format!(
                    "  No services found. Service inference is disabled by the global \
                     `coverage.enabled = false` switch in `.ledgerful/config.toml` -- reindexing will \
                     not change this. Set `coverage.enabled = true` and `coverage.services.enabled = true` \
                     to allow inference, or declare services explicitly under `[services]`. {hint}"
                ),
            )
        }
        ServiceInferenceState::DisabledForServices => {
            let hint = config_enable_hint(&["coverage.services.enabled"]);
            (
                EmptyReason::DisabledByConfig,
                format!(
                    "  No services found. Service inference is disabled by \
                     `coverage.services.enabled = false` in `.ledgerful/config.toml` -- reindexing will \
                     not change this. Set it to `true` to allow inference, or declare services explicitly \
                     under `[services]`. {hint}"
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
