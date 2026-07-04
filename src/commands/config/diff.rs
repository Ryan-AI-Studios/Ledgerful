use crate::commands::config::env::{
    is_ignored_env_var, is_internal_env_var, is_test_or_example_path,
};
use crate::index::env_schema::EnvSchemaExtractor;
use crate::state::layout::Layout;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// One reference-source file path backing a "missing declaration" entry,
/// used so JSON output stays complete (requirement #5's "JSON output remains
/// complete and unambiguous" convention, applied here by the same repo
/// policy CG-F33 established).
#[derive(Serialize)]
pub(crate) struct MissingDeclarationSource {
    var_name: String,
    file_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

pub fn execute_config_diff(json: bool, show_internal: bool) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let storage = crate::state::storage::StorageManager::open_read_only(&layout.root)?;
    let conn = storage.get_connection();

    let mut decl_stmt = conn
        .prepare("SELECT DISTINCT var_name FROM env_declarations")
        .into_diagnostic()?;
    let db_declared_vars: HashSet<String> = decl_stmt
        .query_map([], |row| row.get::<_, String>(0))
        .into_diagnostic()?
        .collect::<rusqlite::Result<HashSet<_>>>()
        .into_diagnostic()?;

    // TA21: explicit env-var declarations in `.ledgerful/schema.json` (or the
    // current `.ledgerful/state/schema.json`) are treated as user-intentional
    // schema entries. Internal vars that appear there are kept in the normal
    // "Declared but not referenced" section so the user gets type-enforcement
    // feedback; only implicitly-detected internal vars are moved to the
    // dedicated internal section.
    let schema_declared_vars: HashSet<String> = {
        let mut vars = HashSet::new();
        let schema_paths = [
            layout.state_subdir().join("schema.json"),
            layout.state_dir.join("schema.json"),
        ];
        for path in &schema_paths {
            if path.exists()
                && let Ok(content) =
                    crate::util::fs::read_to_string_with_encoding(path.as_std_path())
            {
                for decl in EnvSchemaExtractor::extract_from_json(&content) {
                    vars.insert(decl.var_name);
                }
            }
        }
        vars
    };

    // Join to project_files so each reference carries its source path —
    // without this join there is no way to tell whether a "missing
    // declaration" came from production code or from a test/example file.
    let mut ref_stmt = conn
        .prepare(
            "SELECT DISTINCT r.var_name, f.file_path
             FROM env_references r
             JOIN project_files f ON f.id = r.file_id",
        )
        .into_diagnostic()?;
    let ref_rows: Vec<(String, String)> = ref_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?;

    let referenced_vars: HashSet<String> = ref_rows.iter().map(|(var, _)| var.clone()).collect();

    // Group reference file paths per var, split into production vs
    // test/example sources.
    let mut prod_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut test_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (var, path) in &ref_rows {
        let bucket = if is_test_or_example_path(path) {
            &mut test_paths
        } else {
            &mut prod_paths
        };
        bucket.entry(var.clone()).or_default().push(path.clone());
    }

    let is_missing =
        |var: &String| var != "*" && !db_declared_vars.contains(var) && !is_ignored_env_var(var);

    let mut missing_declarations: Vec<MissingDeclarationSource> = prod_paths
        .iter()
        .filter(|(var, _)| is_missing(var))
        .map(|(var, paths)| MissingDeclarationSource {
            var_name: var.clone(),
            file_paths: paths.clone(),
            note: None,
        })
        .collect();
    missing_declarations.sort_by(|a, b| a.var_name.cmp(&b.var_name));

    // Split internal env vars (LEDGERFUL_* and known non-prefixed internal
    // vars) from real missing declarations. When --show-internal is passed,
    // skip the split so users can audit all references without filtering.
    let (internal_declarations, real_missing): (Vec<_>, Vec<_>) = if show_internal {
        (Vec::new(), missing_declarations)
    } else {
        missing_declarations
            .into_iter()
            .partition(|e| is_internal_env_var(&e.var_name))
    };
    let mut internal_declarations = internal_declarations;
    internal_declarations.sort_by(|a, b| a.var_name.cmp(&b.var_name));
    let missing_declarations = real_missing;

    // A var can show up referenced from both production and test/example
    // files; only list it in the test/example section if it has no
    // production reference at all (otherwise it's already covered above).
    let mut test_only_declarations: Vec<MissingDeclarationSource> = test_paths
        .iter()
        .filter(|(var, _)| is_missing(var) && !prod_paths.contains_key(*var))
        .map(|(var, paths)| MissingDeclarationSource {
            var_name: var.clone(),
            file_paths: paths.clone(),
            note: None,
        })
        .collect();
    test_only_declarations.sort_by(|a, b| a.var_name.cmp(&b.var_name));

    let mut all_unused_declarations = Vec::new();
    for d_var in &db_declared_vars {
        if !referenced_vars.contains(d_var) {
            all_unused_declarations.push(d_var.clone());
        }
    }
    all_unused_declarations.sort();

    // TA21: separate declared-but-unreferenced internal env vars from real
    // unused declarations. In --show-internal mode the split is skipped so
    // users can audit the raw list. Internal vars that are explicitly declared
    // in the config schema are kept in the normal unused section.
    let (internal_unused, real_unused): (Vec<String>, Vec<String>) = if show_internal {
        (Vec::new(), all_unused_declarations.clone())
    } else {
        all_unused_declarations
            .iter()
            .cloned()
            .partition(|var| is_internal_env_var(var) && !schema_declared_vars.contains(var))
    };
    let mut internal_unused = internal_unused;
    internal_unused.sort();
    let real_unused = {
        let mut v = real_unused;
        v.sort();
        v
    };

    // Combine missing and declared-but-unreferenced internal env vars for the
    // dedicated "Internal env vars" output. Declared-but-unreferenced entries
    // carry a note so the user can tell why they appear there.
    let mut internal_env_vars = internal_declarations;
    for var in &internal_unused {
        internal_env_vars.push(MissingDeclarationSource {
            var_name: var.clone(),
            file_paths: Vec::new(),
            note: Some("declared but not directly referenced".to_string()),
        });
    }
    internal_env_vars.sort_by(|a, b| a.var_name.cmp(&b.var_name));

    // In --show-internal mode the full unused list is exposed; otherwise only
    // the non-internal (or explicitly-schema-declared) unused vars are shown.
    let unused_to_show: &[String] = if show_internal {
        &all_unused_declarations
    } else {
        &real_unused
    };

    if json {
        let res = if show_internal {
            serde_json::json!({
                "missing_declarations": missing_declarations,
                "missing_declarations_test_or_example_only": test_only_declarations,
                "unused_declarations": all_unused_declarations,
            })
        } else {
            serde_json::json!({
                "missing_declarations": missing_declarations,
                "missing_declarations_test_or_example_only": test_only_declarations,
                "internal_env_vars": internal_env_vars,
                "unused_declarations": real_unused,
            })
        };
        println!("{}", serde_json::to_string_pretty(&res).into_diagnostic()?);
    } else {
        println!(
            "{}",
            "Configuration Diff (Declarations vs References)"
                .bold()
                .cyan()
        );

        println!(
            "\n{}",
            "⚠️  Referenced in production code but missing from declarations:"
                .yellow()
                .bold()
        );
        if missing_declarations.is_empty() {
            println!("  None");
        } else {
            for entry in &missing_declarations {
                println!("  - {}", entry.var_name.red());
            }
        }

        if !show_internal && !internal_env_vars.is_empty() {
            println!("\n{}", "Internal env vars (not configurable):".dimmed());
            for entry in &internal_env_vars {
                if let Some(note) = &entry.note {
                    println!("  - {} {}", entry.var_name.dimmed(), note.dimmed());
                } else {
                    println!("  - {}", entry.var_name.dimmed());
                }
            }
        }

        // Requirement #7: low-signal test/example-only references stay in
        // default output, just clearly separated, so the production-gap
        // section above isn't noisy but nothing is permanently hidden.
        println!(
            "\n{}",
            "Test/example-only references (lower priority):".dimmed()
        );
        if test_only_declarations.is_empty() {
            println!("  None");
        } else {
            for entry in &test_only_declarations {
                println!("  - {}", entry.var_name.dimmed());
            }
        }

        println!(
            "\n{}",
            "ℹ️  Declared but not referenced in code:".blue().bold()
        );
        if unused_to_show.is_empty() {
            println!("  None");
        } else {
            for var in unused_to_show {
                println!("  - {}", var.dimmed());
            }
        }
    }

    Ok(())
}
