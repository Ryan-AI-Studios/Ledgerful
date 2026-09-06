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
    #[arg(value_name = "ENTITY")]
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
        None => return refuse_missing_entity(args.json),
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

fn refuse_missing_entity(json: bool) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let conn = storage.get_connection();

    let message = if knowledge_graph_is_empty(conn)? {
        "Knowledge graph is empty. Run `ledgerful index` first.".to_string()
    } else {
        let picker = if json {
            None
        } else {
            Some(format_mapped_product_picker(conn)?)
        };
        missing_entity_usage_message(picker.as_deref())
    };

    crate::output::requested_exit::request_exit(2);
    Err(miette::miette!("{}", message))
}

fn knowledge_graph_is_empty(conn: &rusqlite::Connection) -> Result<bool> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_symbols", [], |row| row.get(0))
        .into_diagnostic()?;
    Ok(count == 0)
}

fn missing_entity_usage_message(picker: Option<&str>) -> String {
    let mut lines = vec![
        "No entity specified.".to_string(),
        String::new(),
        "Usage: ledgerful tests [OPTIONS] [ENTITY]".to_string(),
        String::new(),
        "  -e, --entity <ENTITY>".to_string(),
        String::new(),
        "Show tests that validate a specific file or symbol.".to_string(),
        String::new(),
        "Examples:".to_string(),
        "  ledgerful tests src/index/languages/rust/symbols.rs".to_string(),
        "  ledgerful tests --entity src/commands/doctor/mod.rs".to_string(),
        "  ledgerful tests --entity src/commands/verify/mod.rs --json".to_string(),
    ];
    if let Some(picker) = picker {
        lines.push(String::new());
        lines.push(picker.to_string());
        lines.push(String::new());
        lines.push("Use `ledgerful tests <entity>` to see matching tests.".to_string());
    }
    lines.join("\n")
}

/// Ranked `tested_file_id` paths (entity / production-file side only).
fn format_mapped_product_picker(conn: &rusqlite::Connection) -> Result<String> {
    let mut stmt = conn
        .prepare(
            "SELECT pf.file_path, COUNT(*) as mapping_count \
             FROM test_mapping tm \
             JOIN project_files pf ON tm.tested_file_id = pf.id \
             GROUP BY pf.file_path \
             ORDER BY mapping_count DESC, pf.file_path ASC",
        )
        .into_diagnostic()?;

    let rows: Vec<(String, i64)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?;

    let survivors: Vec<(String, i64)> = rows
        .into_iter()
        .filter(|(path, _)| is_mapped_product_picker_path(path))
        .take(10)
        .collect();

    if survivors.is_empty() {
        return Ok("No mapped product files to suggest.".to_string());
    }

    let mut out = String::from("Files with indexed test mappings (top 10):");
    for (file_path, count) in survivors {
        out.push('\n');
        out.push_str(&format!("  {file_path:<50} {count} mappings"));
    }
    Ok(out)
}

/// Local picker filter only — do not reuse from hotspots (0297) or mutate env.rs.
fn is_mapped_product_picker_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    if normalized
        .split('/')
        .filter(|seg| !seg.is_empty())
        .any(|seg| matches!(seg, "vendor" | "deps_src" | "third_party"))
    {
        return false;
    }
    if crate::commands::config::env::is_test_or_example_path(path) {
        return false;
    }
    let basename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    let stem = basename.strip_suffix(".rs").unwrap_or(basename);
    stem != "test" && stem != "tests"
}

#[cfg(test)]
mod tests {
    use super::is_mapped_product_picker_path;

    #[test]
    fn test_mapping_picker_keeps_product_paths() {
        assert!(is_mapped_product_picker_path("src/lib.rs"));
        assert!(is_mapped_product_picker_path(
            "src/commands/test_mapping.rs"
        ));
        assert!(is_mapped_product_picker_path("src/index/test_mapping.rs"));
        assert!(is_mapped_product_picker_path(
            "src\\commands\\test_mapping.rs"
        ));
    }

    #[test]
    fn test_mapping_picker_drops_vendor_deps_tests_and_incrate_tests_rs() {
        assert!(!is_mapped_product_picker_path(
            "vendor/sqlite3-src/source/sqlite3.c"
        ));
        assert!(!is_mapped_product_picker_path("crates/x/vendor/y.rs"));
        assert!(!is_mapped_product_picker_path("crates\\x\\vendor\\y.rs"));
        assert!(!is_mapped_product_picker_path("third_party/foo.c"));
        assert!(!is_mapped_product_picker_path("deps_src/bar.c"));
        assert!(!is_mapped_product_picker_path(
            "tests/integration/common/mod.rs"
        ));
        assert!(!is_mapped_product_picker_path("src/foo_test.rs"));
        assert!(!is_mapped_product_picker_path("src/verify/plan/tests.rs"));
        assert!(!is_mapped_product_picker_path(
            "src/index/call_graph/tests.rs"
        ));
        assert!(!is_mapped_product_picker_path(
            "src/commands/index/semantic/tests.rs"
        ));
        assert!(!is_mapped_product_picker_path(
            "src\\verify\\plan\\tests.rs"
        ));
        assert!(!is_mapped_product_picker_path("src/test.rs"));
    }
}
