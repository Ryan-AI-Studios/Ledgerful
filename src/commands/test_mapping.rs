use crate::commands::helpers::get_layout;
use crate::commands::verify::{TestMappingState, explain_test_mappings};
use crate::state::storage::StorageManager;
use clap::Args;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream};

#[derive(Args, Debug)]
pub struct TestsForEntityArgs {
    /// Entity ID (URN, path, or symbol name)
    #[arg(short, long, conflicts_with = "pos_entity")]
    pub entity: Option<String>,
    /// Entity ID (URN, path, or symbol name) (positional fallback)
    #[arg(hide = true)]
    pub pos_entity: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn execute_tests_for_entity(args: TestsForEntityArgs) -> Result<()> {
    // Note: the mutually-exclusive case (both `entity` and `pos_entity` set) is
    // rejected by clap itself at parse time via `conflicts_with` on `entity`,
    // so it can no longer reach this handler.
    let entity_val = match args.entity.or(args.pos_entity) {
        Some(e) => e,
        None => return show_tests_empty_state(),
    };

    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let conn = storage.get_connection();

    let normalized_entity =
        crate::util::path::normalize_relative_path(layout.root.as_std_path(), &entity_val)
            .unwrap_or_else(|_| entity_val.clone());

    let state = explain_test_mappings(conn, &normalized_entity);

    if args.json {
        let output = match state {
            TestMappingState::TableMissing => crate::output::empty::format_json_empty_state(
                Vec::<String>::new(),
                "mappings",
                || {
                    (
                        crate::output::empty::EmptyReason::NoIndexedData,
                        "Test-mapping table is not present in the index. Run `ledgerful index --incremental` to build it.".to_string()
                    )
                },
            ),
            TestMappingState::TableEmpty => crate::output::empty::format_json_empty_state(
                Vec::<String>::new(),
                "mappings",
                || {
                    (
                        crate::output::empty::EmptyReason::NoIndexedData,
                        "No test mappings have been indexed yet. Run `ledgerful index --incremental` to populate them.".to_string()
                    )
                },
            ),
            TestMappingState::EntityNotIndexed => crate::output::empty::format_json_empty_state(
                Vec::<String>::new(),
                "mappings",
                || {
                    (
                        crate::output::empty::EmptyReason::MissingSourceFiles,
                        format!(
                            "'{}' is not a recognized indexed file path or symbol name. Run `ledgerful index --incremental` if it was added recently.",
                            entity_val
                        ),
                    )
                },
            ),
            TestMappingState::EntityAmbiguous { query, candidates } => {
                let total = candidates.len();
                let show = total.min(10);
                let mut listed = candidates[..show].join(", ");
                if total > 10 {
                    listed.push_str(&format!(", and {} more", total - 10));
                }
                crate::output::empty::format_json_empty_state(
                    Vec::<String>::new(),
                    "mappings",
                    || {
                        (
                            // Stable existing reason; honesty lives in the message (M5: no index remediation).
                            crate::output::empty::EmptyReason::MissingSourceFiles,
                            format!(
                                "{total} indexed paths match '{query}': {listed}. Provide a more specific path."
                            ),
                        )
                    },
                )
            }
            TestMappingState::NoMappingsForEntity { resolved_path } => {
                let display = resolved_path
                    .as_deref()
                    .unwrap_or(normalized_entity.as_str());
                crate::output::empty::format_json_empty_state(
                    Vec::<String>::new(),
                    "mappings",
                    || {
                        (
                            crate::output::empty::EmptyReason::NoMatches,
                            format!("'{display}' is indexed, but no tests currently map to it."),
                        )
                    },
                )
            }
            TestMappingState::Mapped {
                tests,
                resolved_path,
            } => {
                let mappings: Vec<_> = tests
                    .into_iter()
                    .map(|t| serde_json::json!({"test": t}))
                    .collect();
                let result_count = mappings.len();
                let mut obj = serde_json::json!({
                    "schemaVersion": 1,
                    "mappings": mappings,
                    "resultCount": result_count,
                });
                if let Some(path) = resolved_path
                    && let Some(map) = obj.as_object_mut()
                {
                    map.insert("resolvedPath".to_string(), serde_json::json!(path));
                }
                obj
            }
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        match state {
            TestMappingState::TableMissing => {
                println!(
                    "  {}",
                    "Test-mapping table is not present in the index. Run `ledgerful index --incremental` to build it.".if_supports_color(Stream::Stdout, |s| s.yellow())

                );
            }
            TestMappingState::TableEmpty => {
                println!(
                    "  {}",
                    "No test mappings have been indexed yet. Run `ledgerful index --incremental` to populate them.".if_supports_color(Stream::Stdout, |s| s.yellow())

                );
            }
            TestMappingState::EntityNotIndexed => {
                println!(
                    "  {}",
                    format!(
                        "'{}' is not a recognized indexed file path or symbol name.",
                        entity_val
                    )
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                println!(
                    "  Run `ledgerful index --incremental` if it was added or renamed recently, or confirm the path with `ledgerful search \"{}\"`.",
                    entity_val
                );
            }
            TestMappingState::EntityAmbiguous { query, candidates } => {
                let total = candidates.len();
                println!(
                    "  {}",
                    format!("{total} indexed paths match '{query}':")
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                let show = total.min(10);
                for p in candidates.iter().take(show) {
                    println!("    • {}", p);
                }
                if total > 10 {
                    println!("    … and {} more", total - 10);
                }
                // M5: no "run index --incremental" remediation for Ambiguous.
                println!("  Provide a more specific path.");
            }
            TestMappingState::NoMappingsForEntity { resolved_path } => {
                let display = resolved_path
                    .as_deref()
                    .unwrap_or(normalized_entity.as_str());
                println!(
                    "  {}",
                    format!("'{display}' is indexed, but no tests currently map to it.")
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                println!(
                    "  This may be accurate (no covering tests yet) -- use `ledgerful search \"{}\"` to confirm test coverage manually.",
                    display
                );
            }
            TestMappingState::Mapped {
                tests,
                resolved_path,
            } => {
                // M4: prefer resolved stored path in the header when available.
                let display = resolved_path.as_deref().unwrap_or(entity_val.as_str());
                println!(
                    "{} {}",
                    "Tests validating".if_supports_color(Stream::Stdout, |s| s.bold()),
                    display.if_supports_color(Stream::Stdout, |s| s.cyan())
                );
                for t in tests {
                    println!("  • {}", t);
                }
            }
        }
    }

    Ok(())
}

fn show_tests_empty_state() -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let conn = storage.get_connection();

    let mut stmt = conn
        .prepare(
            "SELECT pf.file_path, COUNT(*) as symbol_count \
             FROM project_symbols ps \
             JOIN project_files pf ON ps.file_id = pf.id \
             GROUP BY pf.file_path \
             ORDER BY symbol_count DESC, pf.file_path ASC \
             LIMIT 10",
        )
        .into_diagnostic()?;

    let rows: Vec<(String, i64)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?;

    if rows.is_empty() {
        println!("Knowledge graph is empty. Run `ledgerful index` first.");
        return Ok(());
    }

    println!("No entity specified.");
    println!();
    println!("Usage: ledgerful tests [OPTIONS] <ENTITY>");
    println!();
    println!("Show tests that validate a specific file or symbol.");
    println!();
    println!("Examples:");
    println!("  ledgerful tests src/index/languages/rust/symbols.rs");
    println!("  ledgerful tests --entity src/commands/doctor/mod.rs");
    println!("  ledgerful tests --entity src/commands/verify/mod.rs --json");
    println!();
    println!("Available entities (top 10 by symbol count):");
    for (file_path, count) in &rows {
        println!("  {:<50} {} symbols", file_path, count);
    }
    println!();
    println!("Use `ledgerful tests <entity>` to see matching tests.");

    Ok(())
}
