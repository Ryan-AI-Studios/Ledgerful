use crate::policy::load as policy_load;
use crate::state::layout::Layout;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;

pub fn execute_config_verify(json: bool, section: Option<&str>, verbose: bool) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

    let mut success = true;
    let mut errors = Vec::new();

    if !json {
        println!("Verifying Ledgerful configuration...");
    }

    // Verify config.toml
    let config = match crate::config::load_config(&layout) {
        Ok(cfg) => {
            if !json {
                println!("  ✅ config.toml is valid");
            }
            Some(cfg)
        }
        Err(e) => {
            if !json {
                println!("  ❌ config.toml is invalid:\n    {e}");
            }
            errors.push(format!("config.toml is invalid: {e}"));
            success = false;
            None
        }
    };

    // Verify rules.toml
    match policy_load::load_rules(&layout) {
        Ok(_) => {
            if !json {
                println!("  ✅ rules.toml is valid");
            }
        }
        Err(e) => {
            if !json {
                println!("  ❌ rules.toml is invalid:\n    {e}");
            }
            errors.push(format!("rules.toml is invalid: {e}"));
            success = false;
        }
    }

    // TA14 R5: Validate provider priority list
    if let Some(cfg) = &config {
        let providers = &cfg.ask.providers.priority;
        if !providers.is_empty() {
            for (idx, entry) in providers.iter().enumerate() {
                if entry.timeout_secs.is_some_and(|t| t == 0) {
                    let msg = format!("ask.providers.priority[{}]: timeout_secs must be > 0", idx);
                    if !json {
                        println!("  ❌ {msg}");
                    }
                    errors.push(msg);
                    success = false;
                }
            }
            if !json && success {
                println!(
                    "  ✅ ask.providers.priority is valid ({} provider(s))",
                    providers.len()
                );
            }
        }
    }

    // Report config sections
    if let (true, Some(cfg)) = (success, &config) {
        match crate::commands::config_verify::render_verify_report(cfg, json, section, verbose) {
            Ok(report) => {
                if json {
                    println!("{report}");
                } else {
                    println!("\nResolved Settings:");
                    println!("{report}");
                }
            }
            Err(e) => {
                errors.push(e.to_string());
                success = false;
            }
        }
    }

    if success {
        if !json {
            println!("\nAll configurations are valid.");
        }
        Ok(())
    } else {
        if json {
            let err_json = serde_json::json!({
                "success": false,
                "errors": errors
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&err_json).unwrap_or_default()
            );
        }
        Err(miette::miette!("Configuration verification failed."))
    }
}

pub fn execute_config_view(json: bool, section: Option<String>, key: Option<String>) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let config = crate::config::load_config(&layout)?;

    let mut val = serde_json::to_value(&config)
        .map_err(|e| miette::miette!("Failed to serialize config: {e}"))?;

    // Redact secret fields (api_key, token, etc.) before any output
    crate::config::redact::redact_config_value(&mut val);

    // TA32: Inject effective verification mode into view output
    let rules = crate::policy::load::load_rules(&layout).unwrap_or_default();

    if let Some(verify_obj) = val.get_mut("verify").and_then(|v| v.as_object_mut()) {
        let effective_mode = config.verify.effective_mode();
        let mode_str = match effective_mode {
            crate::config::model::VerifyMode::Auto => "auto",
            crate::config::model::VerifyMode::Explicit => "explicit",
        };
        verify_obj.insert(
            "effective_mode".to_string(),
            serde_json::Value::String(mode_str.to_string()),
        );

        let rules_source = if rules.was_legacy_default {
            "historical-rules-fallback"
        } else if effective_mode == crate::config::model::VerifyMode::Auto {
            "auto-policy"
        } else {
            "explicit-config"
        };
        verify_obj.insert(
            "rules_source".to_string(),
            serde_json::Value::String(rules_source.to_string()),
        );
    }

    if !json && rules.was_legacy_default {
        println!(
            "{}",
            "ℹ️  Detected historical rules; using automatic fallback policy.".blue()
        );
    }

    let filtered = if let Some(sec) = &section {
        let sec_key = val
            .as_object()
            .and_then(|obj| obj.keys().find(|k| k.eq_ignore_ascii_case(sec)).cloned());
        if let Some(sk) = sec_key {
            let sec_val = &val[&sk];
            if let Some(k) = &key {
                let k_key = sec_val.as_object().and_then(|obj| {
                    obj.keys()
                        .find(|inner_k| inner_k.eq_ignore_ascii_case(k))
                        .cloned()
                });
                if let Some(kk) = k_key {
                    sec_val[&kk].clone()
                } else {
                    return Err(miette::miette!("Key '{}' not found in section '{}'", k, sk));
                }
            } else {
                sec_val.clone()
            }
        } else {
            return Err(miette::miette!("Section '{}' not found in config", sec));
        }
    } else if let Some(k) = &key {
        let top_key = val.as_object().and_then(|obj| {
            obj.keys()
                .find(|inner_k| inner_k.eq_ignore_ascii_case(k))
                .cloned()
        });
        if let Some(tk) = top_key {
            val[&tk].clone()
        } else {
            return Err(miette::miette!("Key '{}' not found in top-level config", k));
        }
    } else {
        val
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&filtered)
                .map_err(|e| miette::miette!("Failed to serialize filtered config to JSON: {e}"))?
        );
    } else {
        if filtered.is_string() {
            println!("{}", filtered.as_str().unwrap());
        } else if filtered.is_number() || filtered.is_boolean() || filtered.is_null() {
            println!("{}", filtered);
        } else {
            println!("{}", serde_json::to_string_pretty(&filtered).unwrap());
        }
    }
    Ok(())
}

pub fn execute_config_schema(json: bool) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let storage = crate::state::storage::StorageManager::open_read_only(&layout.root)?;
    let conn = storage.get_connection();

    let mut stmt = conn.prepare(
        "SELECT var_name, source_kind, required, is_secret, default_value_redacted, description, owner, environment 
         FROM env_declarations ORDER BY var_name ASC"
    ).into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok(crate::index::env_schema::EnvDeclaration {
                var_name: row.get(0)?,
                source_kind: serde_json::from_str(&format!("\"{}\"", row.get::<_, String>(1)?))
                    .unwrap_or(crate::index::env_schema::EnvSourceKind::Config),
                required: row.get::<_, i32>(2)? != 0,
                is_secret: row.get::<_, i32>(3)? != 0,
                default_value_redacted: row.get(4)?,
                description: row.get(5)?,
                owner: row.get(6)?,
                environment: row.get(7)?,
                confidence: 1.0,
            })
        })
        .into_diagnostic()?;

    if json {
        let mut results = Vec::new();
        for row in rows {
            results.push(row.into_diagnostic()?);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&results).into_diagnostic()?
        );
    } else {
        use crate::output::table::Table;
        let mut table = Table::new();
        table.set_header(vec!["Variable", "Source", "Req", "Sec", "Default", "Owner"]);

        for row in rows {
            let d = row.into_diagnostic()?;
            table.add_row(vec![
                d.var_name,
                d.source_kind.to_string(),
                if d.required { "YES" } else { "no" }.to_string(),
                if d.is_secret { "🔒" } else { "-" }.to_string(),
                d.default_value_redacted.unwrap_or_else(|| "-".to_string()),
                d.owner.unwrap_or_else(|| "-".to_string()),
            ]);
        }
        println!("{}", table);
    }

    Ok(())
}

/// CG-F35 (requirement #4): is this reference path a test or example file
/// rather than production code?
///
/// This is intentionally a narrow, purpose-built check rather than a reuse
/// of `index::topology::is_test_file` (substring matching, e.g.
/// `path.contains("test_")`). That helper was designed for soft/statistical
/// directory classification where false positives are tolerable; reused
/// here as a hard binary filter it is too loose — it would, for example,
/// classify the genuine production files `src/commands/test_mapping.rs` and
/// `src/index/test_mapping.rs` as "test/example" purely because their
/// filename contains the substring `test_`, silently downgrading any real
/// env-var declaration gap they have. That is exactly the failure mode
/// requirement #7 warns against ("real dependencies are not hidden
/// accidentally").
///
/// Instead this matches on path *segments* (split on `/` after normalizing
/// `\` to `/`) being exactly `tests`, `test`, `examples`, or `example`, or
/// the file's *basename* matching an anchored `*_test.rs` / `*_tests.rs`
/// suffix pattern — never a bare substring search over the full path. Note
/// a `test_*.rs` *prefix* pattern is deliberately excluded: it would match
/// `test_mapping.rs` (the exact production file this check must not
/// misclassify), and no convention in this codebase names a single
/// production-adjacent file that way to mean "this file is a test".
fn is_test_or_example_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let mut segments = normalized.split('/').filter(|s| !s.is_empty());

    if segments.any(|seg| matches!(seg, "tests" | "test" | "examples" | "example")) {
        return true;
    }

    let basename = normalized.rsplit('/').next().unwrap_or(&normalized);
    let stem = basename.strip_suffix(".rs").unwrap_or(basename);
    stem.ends_with("_test") || stem.ends_with("_tests")
}

fn is_ignored_env_var(var: &str) -> bool {
    // Standard OS/shell environment variables — never configurable via ledgerful.
    let ignored = [
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "TERM",
        "CI",
        "PSModulePath",
        "XDG_CACHE_HOME",
        "LOCALAPPDATA",
        "TARGET",
        "USERNAME",
        "USERPROFILE",
        // Standard Rust/Cargo ecosystem variables — convention-based, not user-facing
        // configuration for ledgerful. CARGO_* is already covered by the starts_with
        // check below; these are the non-CARGO-prefixed ones.
        "RUST_LOG",
        "RUST_BACKTRACE",
        "RUSTC_WRAPPER",
        // Standard OpenTelemetry convention variable.
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        // Standard terminal color convention variables.
        "CLICOLOR",
        "CLICOLOR_FORCE",
    ];
    ignored.contains(&var) || var.starts_with("CARGO_")
}

const INTERNAL_ENV_PREFIX: &str = "LEDGERFUL_";

/// Internal environment variables that are NOT prefixed with `LEDGERFUL_`.
/// These are convention-based or tool-internal variables that should be
/// categorized as "internal" rather than "missing from declarations".
const NON_PREFIXED_INTERNAL_ENV_VARS: &[&str] = &[
    // Verify dry-run diagnostic flag — internal to ledgerful's verify path.
    "VERBOSE_DRY_RUN",
    // Standard terminal color convention — not user-facing ledgerful config.
    "NO_COLOR",
    // Non-interactive mode convention — used by ledgerful's hook path.
    "NON_INTERACTIVE",
    // Provider API keys/models — internal to ledgerful's LLM backend selection.
    "OLLAMA_CLOUD_API_KEY",
    "OLLAMA_API_KEY",
    "OLLAMA_CLOUD_URL",
    "OLLAMA_CLOUD_MODEL",
    "OPENROUTER_API_KEY",
    "OPENROUTER_MODEL",
];

fn is_internal_env_var(var: &str) -> bool {
    var.starts_with(INTERNAL_ENV_PREFIX) || NON_PREFIXED_INTERNAL_ENV_VARS.contains(&var)
}

/// One reference-source file path backing a "missing declaration" entry,
/// used so JSON output stays complete (requirement #5's "JSON output remains
/// complete and unambiguous" convention, applied here by the same repo
/// policy CG-F33 established).
#[derive(serde::Serialize)]
struct MissingDeclarationSource {
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
    let db_declared_vars: std::collections::HashSet<String> = decl_stmt
        .query_map([], |row| row.get::<_, String>(0))
        .into_diagnostic()?
        .collect::<rusqlite::Result<std::collections::HashSet<_>>>()
        .into_diagnostic()?;

    // TA21: explicit env-var declarations in `.ledgerful/schema.json` (or the
    // current `.ledgerful/state/schema.json`) are treated as user-intentional
    // schema entries. Internal vars that appear there are kept in the normal
    // "Declared but not referenced" section so the user gets type-enforcement
    // feedback; only implicitly-detected internal vars are moved to the
    // dedicated internal section.
    let schema_declared_vars: std::collections::HashSet<String> = {
        let mut vars = std::collections::HashSet::new();
        let schema_paths = [
            layout.state_subdir().join("schema.json"),
            layout.state_dir.join("schema.json"),
        ];
        for path in &schema_paths {
            if path.exists()
                && let Ok(content) =
                    crate::util::fs::read_to_string_with_encoding(path.as_std_path())
            {
                for decl in
                    crate::index::env_schema::EnvSchemaExtractor::extract_from_json(&content)
                {
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

    let referenced_vars: std::collections::HashSet<String> =
        ref_rows.iter().map(|(var, _)| var.clone()).collect();

    // Group reference file paths per var, split into production vs
    // test/example sources.
    let mut prod_paths: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut test_paths: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
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

// ---------------------------------------------------------------------------
// config set (Track DX3)
// ---------------------------------------------------------------------------

/// Set a configuration value in `.ledgerful/config.toml` by dotted key.
///
/// `key_value` is a single `dotted.path = rhs` string (e.g.
/// `coverage.services.enabled=true`). The path is split on `.` and the
/// right-hand side is split on the FIRST `=`. Intermediate tables are created
/// if they do not exist; existing comments and formatting are preserved via
/// `toml_edit`.
///
/// RHS value inference: the right-hand side is first parsed as TOML (so
/// `true`/`42`/`3.14`/"quoted"/`[1,2]` become bool/int/float/string/array
/// respectively). If parsing fails and the RHS is a non-empty bareword with
/// no TOML-significant characters (i.e. it failed only because it is not a
/// valid TOML literal), it is stored as a string so users can write
/// `services.alias=mylabel` without quoting. Any other parse failure is
/// surfaced as a diagnostic.
pub fn execute_config_set(key_value: &str) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    execute_config_set_in(&layout, key_value)
}

/// Testable form of [`execute_config_set`] that operates against an explicit
/// [`Layout`] rather than the process current directory.
pub fn execute_config_set_in(layout: &Layout, key_value: &str) -> Result<()> {
    // Split on the FIRST `=`. Everything to the left is the dotted key path,
    // everything to the right is the TOML value literal.
    let eq_pos = key_value
        .find('=')
        .ok_or_else(|| miette::miette!("missing `=` in `key=value` argument: `{key_value}`"))?;
    let key_path_str = key_value[..eq_pos].trim();
    let rhs = key_value[eq_pos + 1..].trim();

    if key_path_str.is_empty() {
        return Err(miette::miette!("empty key path in `key=value` argument"));
    }

    let path = split_key_path(key_path_str)?;

    // TA22: special-case the `ask.providers.priority` array-of-tables path.
    if path.len() >= 3
        && path[0] == "ask"
        && path[1] == "providers"
        && (path[2] == "priority" || path[2].starts_with("priority["))
    {
        return set_provider_priority(layout, key_path_str, rhs);
    }

    if rhs.is_empty() {
        return Err(miette::miette!(
            "empty value in `key=value` argument: `{key_value}`"
        ));
    }

    let leaf_key = path[path.len() - 1].as_str();
    let parent_path: Vec<&str> = path[..path.len() - 1].iter().map(|s| s.as_str()).collect();

    let (mut doc, config_path) = load_config_doc(layout)?;

    let root = doc.as_table_mut();
    let parent = navigate_or_create(root, &parent_path)?;
    let new_item = parse_rhs(rhs)?;

    // Preserve inline comments on an existing value key. If the leaf key
    // already exists as a `toml_edit::Item::Value`, mutate its inner value in
    // place so the existing `Decor` (prefix/suffix, including inline comments
    // such as `enabled = false # keep this`) survives the edit. Only fall back
    // to `parent.insert` (which swaps the whole `Item` and drops decor) when
    // the key is missing or the existing item is not a Value (e.g. a Table),
    // matching the prior replace-whole-item behavior.
    let existing_is_value = matches!(parent.get(leaf_key), Some(toml_edit::Item::Value(_)));
    if existing_is_value
        && let Some(toml_edit::Item::Value(existing)) = parent.get_mut(leaf_key)
        && let Some(new_value) = new_item.as_value()
    {
        // Carry the existing value's decor (inline comment, spacing) onto the
        // replacement so the typed value changes while the surrounding
        // formatting is preserved.
        let preserved_decor = existing.decor().clone();
        let mut replacement = new_value.clone();
        *replacement.decor_mut() = preserved_decor;
        *existing = replacement;
    } else if !existing_is_value {
        // Missing key, or existing item is a Table/non-Value: insert/replace
        // the whole item (prior behavior).
        parent.insert(leaf_key, new_item);
    } else if new_item.as_value().is_none() {
        // Defensive fallback: existing was a Value but the parsed RHS is not
        // (currently unreachable — parse_rhs always returns an Item::Value).
        // Replace the whole item so the edit is not silently dropped. The
        // `get_mut` borrow from the `if let` chain has ended by this branch.
        parent.insert(leaf_key, new_item);
    }

    write_config_doc(&config_path, &doc)?;

    println!("Set {key_path_str} = {rhs} in {}", config_path);
    Ok(())
}

/// Entry point for the `ledgerful config unset` subcommand.
pub fn execute_config_unset(key: &str) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    execute_config_unset_in(&layout, key)
}

/// Testable form of [`execute_config_unset`] that operates against an explicit
/// [`Layout`].
pub fn execute_config_unset_in(layout: &Layout, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        return Err(miette::miette!("empty key path"));
    }

    let path = split_key_path(key)?;

    // TA32: Support both array-of-tables index removal and standard key removal.
    let (mut doc, config_path) = load_config_doc(layout)?;
    let root = doc.as_table_mut();

    match path.as_slice() {
        [prefix @ .., last] if last.ends_with(']') => {
            let open = last
                .rfind('[')
                .ok_or_else(|| miette::miette!("invalid key path `{key}`: unmatched `]`"))?;
            let index_str = &last[open + 1..last.len() - 1];
            let index: usize = index_str.parse().map_err(|_| {
                miette::miette!("invalid key path `{key}`: index must be a non-negative integer")
            })?;
            let mut prefix = prefix.to_vec();
            prefix.push(last[..open].to_string());

            let item = navigate_to_item(root, &prefix)?;
            let array = item.as_array_of_tables_mut().ok_or_else(|| {
                miette::miette!("key `{}` is not an array of tables", prefix.join("."))
            })?;

            let len = array.len();
            if index >= len {
                return Err(miette::miette!(
                    "Index {index} is out of bounds for array of length {len}. Use index {len} to append."
                ));
            }

            array.remove(index);

            if array.is_empty()
                && let Some(parent_table) = root
                    .get_mut("ask")
                    .and_then(|ask| ask.as_table_mut())
                    .and_then(|ask| ask.get_mut("providers"))
                    .and_then(|p| p.as_table_mut())
            {
                parent_table.remove("priority");
            }
        }
        [prefix @ .., last] => {
            let item = navigate_to_item(root, prefix)?;
            let table = item
                .as_table_mut()
                .ok_or_else(|| miette::miette!("key `{}` is not a table", prefix.join(".")))?;
            table.remove(last);
        }
        [] => unreachable!(),
    };

    write_config_doc(&config_path, &doc)?;
    println!("Unset {} in {}", key, config_path);
    Ok(())
}

/// Load the editable TOML document and the config file path for a `Layout`.
fn load_config_doc(layout: &Layout) -> Result<(toml_edit::DocumentMut, camino::Utf8PathBuf)> {
    layout.ensure_state_dir()?;
    let config_path = layout.config_file();

    let content = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .map_err(|e| miette::miette!("Failed to read {}: {e}", config_path))?
    } else {
        crate::config::defaults::default_config_contents()
            .map_err(|e| miette::miette!("Failed to materialize default config: {e}"))?
    };

    let doc: toml_edit::DocumentMut = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| miette::miette!("TOML parse error in {}: {e}", config_path))?;

    Ok((doc, config_path))
}

/// Serialize a TOML document back to the config file.
fn write_config_doc(config_path: &camino::Utf8PathBuf, doc: &toml_edit::DocumentMut) -> Result<()> {
    let serialized = doc_to_string(doc);
    std::fs::write(config_path, serialized)
        .map_err(|e| miette::miette!("Failed to write {}: {e}", config_path))
}

/// Serialize a TOML document to a string.
fn doc_to_string(doc: &toml_edit::DocumentMut) -> String {
    doc.to_string()
}

/// Split a dotted key path into segments, detecting `[N]` index markers that
/// are part of a segment (e.g. `ask.providers.priority[0].backend` becomes
/// `["ask", "providers", "priority[0]", "backend"]`).
fn split_key_path(key_path_str: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = key_path_str.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if current.is_empty() {
                    return Err(miette::miette!(
                        "invalid key path `{key_path_str}`: empty segment after splitting on `.`"
                    ));
                }
                segments.push(std::mem::take(&mut current));
            }
            '[' => {
                current.push(c);
                let mut depth = 1usize;
                for inner in chars.by_ref() {
                    current.push(inner);
                    match inner {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if depth != 0 {
                    return Err(miette::miette!(
                        "invalid key path `{key_path_str}`: unmatched `[`"
                    ));
                }
            }
            _ => current.push(c),
        }
    }
    if current.is_empty() {
        return Err(miette::miette!(
            "invalid key path `{key_path_str}`: empty segment after splitting on `.`"
        ));
    }
    segments.push(current);
    Ok(segments)
}

/// Walk a TOML table along `path`, creating implicit intermediate tables as
/// needed. Returns a mutable reference to the (possibly newly created) parent
/// table. If any segment along the path exists but is not a table, returns a
/// diagnostic instead of clobbering it.
fn navigate_or_create<'a>(
    table: &'a mut toml_edit::Table,
    path: &[&str],
) -> Result<&'a mut toml_edit::Table> {
    if path.is_empty() {
        return Ok(table);
    }
    let key = path[0];
    let entry = table.entry(key);
    let item = entry.or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        toml_edit::Item::Table(t)
    });
    match item {
        toml_edit::Item::Table(sub) => navigate_or_create(sub, &path[1..]),
        other => {
            let _ = other;
            Err(miette::miette!(
                "key `{key}` exists but is not a table; cannot descend into `{key}`"
            ))
        }
    }
}

/// Navigate to an existing `Item` along `path`, descending into tables.
/// Returns a mutable reference to the final item.
fn navigate_to_item<'a>(
    table: &'a mut toml_edit::Table,
    path: &[String],
) -> Result<&'a mut toml_edit::Item> {
    if path.is_empty() {
        return Err(miette::miette!("empty key path"));
    }
    if path.len() == 1 {
        let key = path[0].as_str();
        return table
            .get_mut(key)
            .ok_or_else(|| miette::miette!("key `{key}` not found"));
    }
    let key = path[0].as_str();
    let item = table
        .get_mut(key)
        .ok_or_else(|| miette::miette!("key `{key}` not found"))?;
    let sub = item.as_table_mut().ok_or_else(|| {
        miette::miette!("key `{key}` exists but is not a table; cannot descend into `{key}`")
    })?;
    navigate_to_item(sub, &path[1..])
}

/// TA22: handle `ask.providers.priority` paths.
///
/// Supports three forms:
/// - `ask.providers.priority[0].backend=ollama_cloud` — set a field inside an
///   existing or newly appended array-of-tables entry.
/// - `ask.providers.priority=ollama_cloud,gemini,local` — replace the entire
///   list with provider stubs.
/// - `ask.providers.priority=` — clear the array.
fn set_provider_priority(layout: &Layout, key_path_str: &str, rhs: &str) -> Result<()> {
    let (mut doc, config_path) = load_config_doc(layout)?;
    let root = doc.as_table_mut();

    // `ask.providers` table must exist; create it if missing.
    let providers = navigate_or_create(root, &["ask", "providers"])?;

    // Determine whether the key uses an indexed path (e.g. `priority[0].backend`).
    let priority_key = "priority";
    let raw_item = providers
        .entry(priority_key)
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    let array = raw_item.as_array_of_tables_mut().ok_or_else(|| {
        miette::miette!("ask.providers.priority exists but is not an array of tables")
    })?;

    // Detect index syntax in the original key path string.
    let indexed_field = parse_priority_index_path(key_path_str);

    // Guard against common path-typing mistakes that would otherwise fall
    // through to the shortcut/clear path and silently clobber the list:
    //   1. `ask.providers.priority.backend=…` — bare `.field` without `[N]`.
    //   2. `ask.providers.priority[0]=…` — `[N]` without a trailing `.field`.
    //   3. `ask.providers.priority[abc].backend=…` — non-numeric index.
    // `parse_priority_index_path` returns None for all three, so detect them
    // explicitly via the raw key-path suffix and surface a clear error.
    if indexed_field.is_none()
        && let Some(rest) = key_path_str.strip_prefix("ask.providers.priority")
    {
        if rest.starts_with('.') && !rest[1..].is_empty() {
            return Err(miette::miette!(
                "missing index in `{key_path_str}`; use `ask.providers.priority[N].backend=...` to set a single entry, or `ask.providers.priority=backend1,backend2` to replace the whole list"
            ));
        }
        if rest.starts_with('[') {
            // `[N]` present but parse_priority_index_path rejected it:
            // either no trailing `.field`, a non-numeric index, or an empty
            // index. Distinguish a bare `[N]` (missing field) from a malformed
            // index for a clearer message.
            let close = rest.find(']');
            let after_close = close.map(|c| &rest[c + 1..]).unwrap_or("");
            if after_close.is_empty() || !after_close.starts_with('.') {
                return Err(miette::miette!(
                    "missing field in `{key_path_str}`; use `ask.providers.priority[N].field=...` (e.g. `ask.providers.priority[0].backend=ollama_cloud`), or `ask.providers.priority=backend1,backend2` to replace the whole list"
                ));
            }
            // There is a `.field` suffix but the index didn't parse, so the
            // index itself is malformed (non-numeric or empty).
            return Err(miette::miette!(
                "invalid index in `{key_path_str}`; the index `[N]` must be a non-negative integer"
            ));
        }
    }

    if let Some((index, field)) = indexed_field {
        let len = array.len();
        if index > len {
            return Err(miette::miette!(
                "Index {index} is out of bounds for array of length {len}. Use index {len} to append."
            ));
        }

        // Append a new table if index == len.
        if index == len {
            let mut entry_table = toml_edit::Table::new();
            entry_table.insert("backend", toml_edit::value("local".to_string()));
            array.push(entry_table);
        }

        let entry = array.get_mut(index).ok_or_else(|| {
            miette::miette!("internal error: failed to access priority entry {index}")
        })?;

        // Validate the field name so we only write supported keys.
        if !is_valid_provider_entry_field(&field) {
            return Err(miette::miette!("invalid provider entry field `{field}`"));
        }

        let value_item = parse_rhs(rhs)?;
        // For `backend`, validate the provider variant immediately.
        if field == "backend" {
            let value_str = value_item
                .as_str()
                .ok_or_else(|| miette::miette!("backend must be a string"))?;
            let _ = crate::config::model::Provider::from_str_fail_fast(
                value_str,
                &format!("ask.providers.priority[{index}].backend"),
            )
            .map_err(|e| miette::miette!("{e}"))?;
        }

        entry.insert(&field, value_item);

        write_config_doc(&config_path, &doc)?;
        println!("Set {key_path_str} = {rhs} in {}", config_path);
        return validate_and_print_priority(layout);
    }

    // Non-indexed `ask.providers.priority` — full list replacement or clear.
    if rhs.is_empty() {
        array.clear();
        if array.is_empty() {
            providers.remove(priority_key);
        }
        write_config_doc(&config_path, &doc)?;
        println!("{}", build_priority_clear_confirmation());
        return Ok(());
    }

    // Comma-separated backend shortcut syntax.
    let mut new_entries = Vec::new();
    let mut backend_names: Vec<&str> = Vec::new();
    for backend_name in rhs.split(',') {
        let backend_name = backend_name.trim();
        if backend_name.is_empty() {
            continue;
        }
        let provider = crate::config::model::Provider::from_str_fail_fast(
            backend_name,
            "ask.providers.priority",
        )
        .map_err(|e| miette::miette!("{e}"))?;
        let default_entry = default_provider_entry(provider);
        new_entries.push(default_entry);
        backend_names.push(backend_name);
    }

    if new_entries.is_empty() {
        return Err(miette::miette!("at least one provider must be configured"));
    }

    array.clear();
    for entry in new_entries {
        array.push(entry);
    }

    // Build the confirmation message before serializing so the mutable borrow
    // of `array` can end before we immutably borrow `doc`.
    let confirmation = build_priority_set_confirmation(&backend_names);

    write_config_doc(&config_path, &doc)?;

    println!("{confirmation}");

    validate_and_print_priority(layout)
}

/// Parse `ask.providers.priority[N].field` from a raw key path string.
/// Returns `Some((index, field))` when an index is present, or `None` for the
/// bare `ask.providers.priority` form.
fn parse_priority_index_path(key_path_str: &str) -> Option<(usize, String)> {
    let prefix = "ask.providers.priority";
    let rest = key_path_str.strip_prefix(prefix)?;
    if rest.is_empty() {
        return None;
    }
    if !rest.starts_with('[') {
        return None;
    }
    let close = rest.find(']')?;
    let index: usize = rest[1..close].parse().ok()?;
    let after = &rest[close + 1..];
    if !after.starts_with('.') {
        return None;
    }
    let field = after[1..].to_string();
    if field.is_empty() {
        return None;
    }
    Some((index, field))
}

/// Fields allowed inside a `[[ask.providers.priority]]` entry.
fn is_valid_provider_entry_field(field: &str) -> bool {
    matches!(
        field,
        "backend" | "model" | "timeout_secs" | "api_key_env" | "base_url"
    )
}

/// Build a default `ProviderEntry` table for the shortcut syntax.
fn default_provider_entry(provider: crate::config::model::Provider) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    let backend_name = match provider {
        crate::config::model::Provider::OllamaCloud => "ollama_cloud",
        crate::config::model::Provider::Gemini => "gemini",
        crate::config::model::Provider::Local => "local",
        crate::config::model::Provider::OpenRouter => "openrouter",
    };
    table.insert("backend", toml_edit::value(backend_name));
    // TA22 R2: create stubs with default model/timeout values.
    match provider {
        crate::config::model::Provider::OllamaCloud => {
            table.insert("model", toml_edit::value("minimax-m3:cloud".to_string()));
            table.insert("timeout_secs", toml_edit::value(30i64));
        }
        crate::config::model::Provider::Gemini => {
            table.insert(
                "model",
                toml_edit::value("gemini-3.1-flash-lite".to_string()),
            );
            table.insert("timeout_secs", toml_edit::value(30i64));
        }
        crate::config::model::Provider::Local => {
            table.insert("model", toml_edit::value("local".to_string()));
            table.insert("timeout_secs", toml_edit::value(30i64));
        }
        crate::config::model::Provider::OpenRouter => {
            table.insert("model", toml_edit::value("openrouter".to_string()));
            table.insert("timeout_secs", toml_edit::value(30i64));
        }
    }
    table
}

/// Human-readable display name for a backend string.
fn provider_display_name(backend: &str) -> String {
    match crate::config::model::Provider::from_str_fail_fast(backend, "display") {
        Ok(p) => p.display_name().to_string(),
        Err(_) => {
            let mut s = backend.to_string();
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            s
        }
    }
}

/// Build the human-readable confirmation message for the shortcut syntax
/// (R3). Pure function so it can be unit-tested without capturing stdout.
///
/// Example: `["ollama_cloud", "gemini", "local"]` →
/// `"Provider priority set: OllamaCloud → Gemini → Local"`.
fn build_priority_set_confirmation(backends: &[&str]) -> String {
    let names: Vec<String> = backends.iter().map(|b| provider_display_name(b)).collect();
    format!("Provider priority set: {}", names.join(" → "))
}

/// Build the clear-list confirmation message (R4).
fn build_priority_clear_confirmation() -> &'static str {
    "Provider priority list cleared. Legacy backend selection will be used."
}

/// Reload the config and validate that at least one provider is configured and
/// that every backend is a known variant.
fn validate_and_print_priority(layout: &Layout) -> Result<()> {
    let config = crate::config::load_config(layout)
        .map_err(|e| miette::miette!("Configuration is invalid after update: {e}"))?;
    let priority = &config.ask.providers.priority;
    if priority.is_empty() {
        return Err(miette::miette!("at least one provider must be configured"));
    }
    Ok(())
}

/// Parse a right-hand-side value literal into an editable TOML item.
///
/// First tries to parse the RHS as a TOML value (so `true`, `42`, `3.14`,
/// `"quoted"`, `[1,2]` are typed correctly). If that fails, treats the RHS as
/// a bare string literal (so `services.alias=mylabel` works without
/// quoting). If the RHS is structurally broken (e.g. `[1,` — unclosed array),
/// the original TOML parse error is surfaced rather than silently storing a
/// malformed string.
fn parse_rhs(s: &str) -> Result<toml_edit::Item> {
    // Parse `__x__ = <rhs>` as a toml_edit document so the RHS inherits the
    // correct typed form (bool/int/float/string/array) and is returned as a
    // `toml_edit::Item` directly — no cross-crate conversion needed.
    let candidate = format!("__x__ = {s}");
    match candidate.parse::<toml_edit::DocumentMut>() {
        Ok(doc) => {
            let root = doc.as_table();
            let item = root
                .get("__x__")
                .ok_or_else(|| miette::miette!("internal error: parsed RHS is missing"))?;
            Ok(item.clone())
        }
        Err(parse_err) => {
            // Fall back to treating the RHS as a bare string ONLY when it is
            // a clean bareword (non-empty, no TOML-significant punctuation that
            // would indicate the user intended a typed value but mistyped it).
            // An unclosed array like `[1,` is NOT a clean bareword, so its
            // original parse error is surfaced instead of being silently
            // stored as the literal string "[1,".
            if is_clean_bareword(s) {
                Ok(toml_edit::value(s.to_string()))
            } else {
                Err(miette::miette!("invalid value `{s}`: {parse_err}"))
            }
        }
    }
}

/// A "clean bareword" is a non-empty token that contains none of the
/// characters that would make it ambiguous whether the user intended a typed
/// TOML value (`[`, `]`, `{`, `}`, `=`, `,`, `"`, `'`, `#`). When the RHS
/// fails TOML parsing but is a clean bareword, we store it as a string so
/// `services.alias=mylabel` works without quoting.
fn is_clean_bareword(s: &str) -> bool {
    !s.is_empty()
        && !s.chars().any(|c| {
            matches!(
                c,
                '[' | ']' | '{' | '}' | '=' | ',' | '"' | '\'' | '#' | '\n' | '\r'
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the CG-F35 review finding: `is_test_or_example_path`
    /// must not misclassify genuine production files whose name happens to
    /// contain the substring `test_` (e.g. `test_mapping.rs`). It must still
    /// correctly classify real test/example paths via path segments or an
    /// anchored `_test(s).rs` filename suffix.
    #[test]
    fn is_test_or_example_path_does_not_misclassify_production_test_mapping_files() {
        assert!(
            !is_test_or_example_path("src/commands/test_mapping.rs"),
            "src/commands/test_mapping.rs is a real production file and must not be \
             classified as test/example-only"
        );
        assert!(
            !is_test_or_example_path("src/index/test_mapping.rs"),
            "src/index/test_mapping.rs is a real production file and must not be \
             classified as test/example-only"
        );
    }

    #[test]
    fn is_test_or_example_path_classifies_real_test_and_example_paths() {
        assert!(is_test_or_example_path("tests/integration/cli_config.rs"));
        assert!(is_test_or_example_path("examples/foo.rs"));
    }

    #[test]
    fn is_test_or_example_path_handles_windows_separators_and_anchored_suffixes() {
        // Backslash-separated paths normalize the same as forward-slash ones.
        assert!(!is_test_or_example_path("src\\commands\\test_mapping.rs"));
        assert!(!is_test_or_example_path("src\\index\\test_mapping.rs"));

        // Filename-anchored `_test.rs` / `_tests.rs` suffixes still match,
        // even outside a tests/examples directory.
        assert!(is_test_or_example_path("src/foo_test.rs"));
        assert!(is_test_or_example_path("src/foo_tests.rs"));

        // Plain production files are unaffected.
        assert!(!is_test_or_example_path("src/main.rs"));
    }

    // --- parse_rhs / is_clean_bareword (Track DX3) -----------------------

    #[test]
    fn parse_rhs_bool() {
        let item = parse_rhs("true").expect("bool");
        assert_eq!(item.as_value().and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn parse_rhs_int() {
        let item = parse_rhs("42").expect("int");
        assert_eq!(item.as_value().and_then(|v| v.as_integer()), Some(42));
    }

    #[test]
    fn parse_rhs_float() {
        let item = parse_rhs("2.5").expect("float");
        let f = item.as_value().and_then(|v| v.as_float()).expect("float");
        assert!((f - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_rhs_quoted_string() {
        let item = parse_rhs("\"bar-baz\"").expect("string");
        assert_eq!(item.as_value().and_then(|v| v.as_str()), Some("bar-baz"));
    }

    #[test]
    fn parse_rhs_array() {
        let item = parse_rhs("[1, 2]").expect("array");
        let arr = item.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr.get(0).and_then(|v| v.as_integer()), Some(1));
        assert_eq!(arr.get(1).and_then(|v| v.as_integer()), Some(2));
    }

    #[test]
    fn parse_rhs_bareword_falls_back_to_string() {
        let item = parse_rhs("mylabel").expect("bareword fallback");
        assert_eq!(item.as_value().and_then(|v| v.as_str()), Some("mylabel"));
    }

    #[test]
    fn parse_rhs_unclosed_array_errors() {
        assert!(
            parse_rhs("[1,").is_err(),
            "unclosed array must not be stored"
        );
    }

    #[test]
    fn is_clean_bareword_classification() {
        assert!(is_clean_bareword("mylabel"));
        assert!(is_clean_bareword("foo-bar_2"));
        assert!(!is_clean_bareword(""));
        assert!(!is_clean_bareword("[1,"));
        assert!(!is_clean_bareword("\"quoted\""));
        assert!(!is_clean_bareword("a # b"));
    }

    // TA22 R3: shortcut confirmation message matches spec exactly.
    #[test]
    fn build_priority_set_confirmation_matches_spec_format() {
        let msg = build_priority_set_confirmation(&["ollama_cloud", "gemini", "local"]);
        assert_eq!(
            msg, "Provider priority set: OllamaCloud → Gemini → Local",
            "R3 confirmation message must match spec exactly"
        );
    }

    #[test]
    fn build_priority_set_confirmation_single_backend() {
        let msg = build_priority_set_confirmation(&["local"]);
        assert_eq!(msg, "Provider priority set: Local");
    }

    // TA22 R4: clear confirmation message matches spec exactly.
    #[test]
    fn build_priority_clear_confirmation_matches_spec() {
        assert_eq!(
            build_priority_clear_confirmation(),
            "Provider priority list cleared. Legacy backend selection will be used."
        );
    }

    // TA27: internal env var classification
    #[test]
    fn is_internal_env_var_verbose_dry_run() {
        assert!(
            is_internal_env_var("VERBOSE_DRY_RUN"),
            "VERBOSE_DRY_RUN must be classified as internal"
        );
    }

    #[test]
    fn is_internal_env_var_no_color() {
        assert!(
            is_internal_env_var("NO_COLOR"),
            "NO_COLOR must be classified as internal"
        );
    }

    #[test]
    fn is_internal_env_var_non_interactive() {
        assert!(
            is_internal_env_var("NON_INTERACTIVE"),
            "NON_INTERACTIVE must be classified as internal"
        );
    }

    #[test]
    fn is_internal_env_var_ledgerful_prefix() {
        assert!(
            is_internal_env_var("LEDGERFUL_SOME_INTERNAL_VAR"),
            "LEDGERFUL_-prefixed vars must be classified as internal"
        );
    }

    #[test]
    fn is_internal_env_var_rejects_unrelated_var() {
        assert!(
            !is_internal_env_var("SOME_PUBLIC_API_KEY"),
            "Unrelated vars must not be classified as internal"
        );
    }

    #[test]
    fn is_ignored_env_var_rust_ecosystem() {
        assert!(
            is_ignored_env_var("RUST_LOG"),
            "RUST_LOG must be ignored as a standard ecosystem var"
        );
        assert!(
            is_ignored_env_var("RUST_BACKTRACE"),
            "RUST_BACKTRACE must be ignored as a standard ecosystem var"
        );
        assert!(
            is_ignored_env_var("CARGO_TARGET_DIR"),
            "CARGO_TARGET_DIR must be ignored (starts with CARGO_)"
        );
        assert!(
            is_ignored_env_var("CARGO_HOME"),
            "CARGO_HOME must be ignored (starts with CARGO_)"
        );
        assert!(
            is_ignored_env_var("CARGO_INCREMENTAL"),
            "CARGO_INCREMENTAL must be ignored (starts with CARGO_)"
        );
        assert!(
            is_ignored_env_var("RUSTC_WRAPPER"),
            "RUSTC_WRAPPER must be ignored as a standard ecosystem var"
        );
        assert!(
            is_ignored_env_var("OTEL_EXPORTER_OTLP_ENDPOINT"),
            "OTEL_EXPORTER_OTLP_ENDPOINT must be ignored as a standard convention var"
        );
        assert!(
            is_ignored_env_var("CLICOLOR"),
            "CLICOLOR must be ignored as a standard terminal color convention var"
        );
        assert!(
            is_ignored_env_var("CLICOLOR_FORCE"),
            "CLICOLOR_FORCE must be ignored as a standard terminal color convention var"
        );
    }
}
