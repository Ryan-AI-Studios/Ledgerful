use crate::commands::helpers::get_layout;
use crate::commands::verify::{
    MappedTest, TestMappingState, explain_test_mappings, nextest_on_path,
};
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
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let prefer_nextest = config.verify.prefer_nextest.unwrap_or(false);
    let runner_nextest = prefer_nextest && nextest_on_path();
    let storage = StorageManager::open_read_only(&layout)?;
    let conn = storage.get_connection();

    let normalized_entity =
        crate::util::path::normalize_relative_path(layout.root.as_std_path(), &entity_val)
            .unwrap_or_else(|_| entity_val.clone());

    let state = explain_test_mappings(conn, &normalized_entity);
    let freshness = mapping_freshness_row(conn, &layout);

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
                    .iter()
                    .map(|t| mapped_test_json(t, runner_nextest))
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
        let mut output = output;
        if let Some(row) = freshness
            && let Some(map) = output.as_object_mut()
            && let Ok(value) = serde_json::to_value(row)
        {
            map.insert("freshness".to_string(), value);
        }
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
                    println!("  • {}", t.display_line());
                }
            }
        }
    }

    Ok(())
}

fn mapped_test_json(mapped: &MappedTest, runner_nextest: bool) -> serde_json::Value {
    let runner = if runner_nextest {
        "nextest"
    } else {
        "cargoTest"
    };
    let stem = if runner_nextest {
        crate::verify::plan::test_file_to_nextest_stem(&mapped.file)
    } else {
        None
    };
    let mut selector = serde_json::json!({
        "runner": runner,
        "testFile": mapped.file,
        "testName": mapped.symbol,
    });
    if let Some(stem) = stem.filter(|s| !s.is_empty())
        && let Some(obj) = selector.as_object_mut()
    {
        obj.insert("stem".to_string(), serde_json::json!(stem));
    }
    serde_json::json!({
        "test": mapped.test,
        "kind": mapped.kind,
        "location": {
            "file": mapped.file,
            "symbol": mapped.symbol,
        },
        "selector": selector,
    })
}

fn mapping_freshness_row(
    conn: &rusqlite::Connection,
    layout: &crate::state::layout::Layout,
) -> Option<crate::index::surface_freshness::SurfaceFreshness> {
    use crate::git::repo::{get_head_info, open_repo};
    use crate::index::surface_freshness::{
        ClassifySurfaceFreshness, EmbeddingsProbe, classify_surface_freshness, probe_named_table,
        read_indexed_head,
    };

    let compared_head = open_repo(layout.root.as_std_path())
        .ok()
        .and_then(|repo| get_head_info(&repo).ok())
        .and_then(|(hash, _)| hash);
    let indexed_head = read_indexed_head(conn);
    classify_surface_freshness(ClassifySurfaceFreshness {
        files_stale: None,
        compared_head: compared_head.as_deref(),
        indexed_head: indexed_head.as_deref(),
        mapping: probe_named_table(conn, "test_mapping"),
        routes: probe_named_table(conn, "api_routes"),
        embeddings: EmbeddingsProbe::NotConfigured,
        permission_denied: false,
    })
    .into_iter()
    .find(|row| row.id == "mapping")
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
    use super::{is_mapped_product_picker_path, mapped_test_json};
    use crate::commands::verify::MappedTest;

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

    #[test]
    fn mapped_json_has_kind_selector_and_no_plus_or_e_flag() {
        let mapped = MappedTest {
            test: "src/index/test_mapping.rs::is_test".into(),
            kind: "SAME_FILE".into(),
            file: "src/index/test_mapping.rs".into(),
            symbol: "is_test".into(),
        };
        let json = mapped_test_json(&mapped, true);
        let text = serde_json::to_string(&json).unwrap();
        assert_eq!(json["test"], "src/index/test_mapping.rs::is_test");
        assert_eq!(json["kind"], "SAME_FILE");
        assert_eq!(json["location"]["file"], "src/index/test_mapping.rs");
        assert_eq!(json["location"]["symbol"], "is_test");
        assert_eq!(json["selector"]["runner"], "nextest");
        assert_eq!(json["selector"]["testFile"], "src/index/test_mapping.rs");
        assert_eq!(json["selector"]["testName"], "is_test");
        assert_eq!(json["selector"]["stem"], "test_mapping");
        assert!(
            !text.contains('+') && !text.contains("-E"),
            "selector must not emit + or -E: {text}"
        );
    }

    #[test]
    fn mapped_json_cargo_test_omits_stem() {
        let mapped = MappedTest {
            test: "src/lib.rs::foo".into(),
            kind: "IMPORT".into(),
            file: "src/lib.rs".into(),
            symbol: "foo".into(),
        };
        let json = mapped_test_json(&mapped, false);
        assert_eq!(json["selector"]["runner"], "cargoTest");
        assert!(json["selector"].get("stem").is_none());
        let text = serde_json::to_string(&json).unwrap();
        assert!(!text.contains('+') && !text.contains("-E"));
    }

    #[test]
    fn tests_json_freshness_is_one_object_not_array() {
        use crate::index::surface_freshness::{
            SurfaceFreshness, SurfaceFreshnessSource, SurfaceFreshnessStatus,
        };
        let row = SurfaceFreshness {
            id: "mapping".into(),
            status: SurfaceFreshnessStatus::Available,
            source: SurfaceFreshnessSource::IndexHead,
            reason: "ok".into(),
            refresh: None,
            indexed_head: None,
            compared_head: None,
        };
        let mut output = serde_json::json!({
            "schemaVersion": 1,
            "mappings": [],
            "resultCount": 0
        });
        if let Some(map) = output.as_object_mut() {
            map.insert("freshness".to_string(), serde_json::to_value(&row).unwrap());
        }
        assert!(
            output["freshness"].is_object(),
            "freshness must be one object: {output}"
        );
        assert!(
            !output["freshness"].is_array(),
            "freshness must not be an array: {output}"
        );
        assert_eq!(output["freshness"]["id"], "mapping");
    }
}
