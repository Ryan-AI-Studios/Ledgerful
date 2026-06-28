use crate::commands::helpers::get_layout;
use crate::output::table::build_premium_table;
use crate::state::storage::StorageManager;
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;

#[derive(Args, Debug)]
pub struct DataModelsArgs {
    #[command(subcommand)]
    pub command: DataModelSubcommands,
}

#[derive(Subcommand, Debug)]
pub enum DataModelSubcommands {
    /// List all extracted data models and their mapping to tables
    List {
        /// Show all candidate structs, even those with low confidence
        #[arg(long)]
        all: bool,
        /// Minimum confidence threshold
        #[arg(long, default_value_t = 0.5)]
        min_confidence: f64,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show impact of changes on data models
    Impact {
        /// Filter by changed models only
        #[arg(long)]
        changed: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn execute_data_models(args: DataModelsArgs) -> Result<()> {
    let layout = get_layout()?;

    match args.command {
        DataModelSubcommands::List {
            all,
            min_confidence,
            json,
        } => {
            let storage = StorageManager::open_read_only(&layout.root)?;
            let conn = storage.get_connection();

            let threshold = if all { 0.0 } else { min_confidence };

            let mut stmt = conn
                .prepare("SELECT model_name, language, model_kind, confidence FROM data_models WHERE confidence >= ?1")
                .into_diagnostic()?;

            let rows_iter = stmt
                .query_map([threshold], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                })
                .into_diagnostic()?;

            let mut model_rows: Vec<(String, String, String, f64)> = Vec::new();
            for row in rows_iter {
                model_rows.push(row.into_diagnostic()?);
            }
            // Deterministic ordering: by model name.
            model_rows.sort_by(|a, b| a.0.cmp(&b.0));

            if json {
                let results: Vec<serde_json::Value> = model_rows
                    .into_iter()
                    .map(|(name, lang, kind, conf)| {
                        serde_json::json!({
                            "name": name,
                            "language": lang,
                            "kind": kind,
                            "confidence": conf,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&results).into_diagnostic()?
                );
            } else {
                println!("{}", "Data Models".bold().cyan());
                if model_rows.is_empty() {
                    println!("  No data models indexed.");
                } else {
                    let mut table = build_premium_table(["Name", "Language", "Kind", "Confidence"]);
                    for (name, lang, kind, conf) in model_rows {
                        table.add_row(vec![
                            name.bold().to_string(),
                            lang,
                            kind,
                            format!("{:.2}", conf),
                        ]);
                    }
                    println!("{}", table);
                }
            }
        }
        DataModelSubcommands::Impact { changed, json } => {
            let packet = crate::commands::impact::execute_impact_silent()?;

            let storage = StorageManager::open_read_only(&layout.root)?;
            let conn = storage.get_connection();

            // Collect the files that changed
            let changed_files: std::collections::HashSet<String> = packet
                .changes
                .iter()
                .map(|c| c.path.to_string_lossy().replace('\\', "/"))
                .collect();

            // Now query data models and see which ones are in changed files
            let mut stmt = conn
                .prepare(
                    "SELECT dm.model_name, pf.file_path, dm.language, dm.model_kind, dm.confidence \
                 FROM data_models dm \
                 JOIN project_files pf ON dm.model_file_id = pf.id",
                )
                .into_diagnostic()?;

            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, f64>(4)?,
                    ))
                })
                .into_diagnostic()?;

            let mut impacted = Vec::new();
            for row in rows {
                let (name, file_path, lang, kind, conf) = row.into_diagnostic()?;
                let file_path_norm = file_path.replace('\\', "/");
                let is_impacted = changed_files.contains(&file_path_norm);

                if !changed || is_impacted {
                    impacted.push(serde_json::json!({
                        "name": name,
                        "file_path": file_path_norm,
                        "language": lang,
                        "kind": kind,
                        "confidence": conf,
                        "is_changed": is_impacted,
                    }));
                }
            }

            // Deterministic ordering: by model name, then file path.
            impacted.sort_by(|a, b| {
                let a_key = (
                    a["name"].as_str().unwrap_or(""),
                    a["file_path"].as_str().unwrap_or(""),
                );
                let b_key = (
                    b["name"].as_str().unwrap_or(""),
                    b["file_path"].as_str().unwrap_or(""),
                );
                a_key.cmp(&b_key)
            });

            if json {
                let mut output = crate::output::empty::format_json_empty_state(
                    impacted,
                    "impacted",
                    || {
                        let total_models: i64 = conn
                            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
                            .unwrap_or(0);
                        if total_models > 0 && changed {
                            (
                                crate::output::empty::EmptyReason::CleanDiff,
                                "No changed data models found.".to_string(),
                            )
                        } else {
                            (
                            crate::output::empty::EmptyReason::NoIndexedData,
                            "No data models indexed. Data models are extracted from ORM structs, \
                             SQL table definitions, and migration files. Run `ledgerful index \
                             --incremental` if models exist, or confirm your ORM/framework is supported."
                                .to_string(),
                        )
                        }
                    },
                );
                if output.is_array() {
                    output = serde_json::json!({ "impacted": output });
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output).into_diagnostic()?
                );
            } else {
                println!("{}", "Data Model Impact Analysis".bold().cyan());
                if impacted.is_empty() {
                    let total_models: i64 = conn
                        .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
                        .into_diagnostic()?;

                    if total_models > 0 && changed {
                        println!("{}", "  No changed data models found.".dimmed());
                    } else {
                        println!(
                            "{}",
                            "  No data models indexed. Data models are extracted from ORM structs, \
                             SQL table definitions, and migration files. Run `ledgerful index \
                             --incremental` if models exist, or confirm your ORM/framework is supported."
                                .dimmed()
                        );
                    }
                } else {
                    let mut table =
                        build_premium_table(["Name", "File", "Language", "Kind", "Changed?"]);
                    for item in &impacted {
                        table.add_row(vec![
                            item["name"].as_str().unwrap_or("").bold().to_string(),
                            item["file_path"].as_str().unwrap_or("").to_string(),
                            item["language"].as_str().unwrap_or("").to_string(),
                            item["kind"].as_str().unwrap_or("").to_string(),
                            if item["is_changed"].as_bool().unwrap_or(false) {
                                "YES".red().bold().to_string()
                            } else {
                                "NO".dimmed().to_string()
                            },
                        ]);
                    }
                    println!("{}", table);
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_model_impact_sorts_deterministically_and_uses_premium_table() {
        let mut impacted = vec![
            serde_json::json!({
                "name": "User",
                "file_path": "src/b.rs",
                "language": "Rust",
                "kind": "STRUCT",
                "is_changed": true,
            }),
            serde_json::json!({
                "name": "Account",
                "file_path": "src/a.rs",
                "language": "Rust",
                "kind": "STRUCT",
                "is_changed": false,
            }),
            serde_json::json!({
                "name": "User",
                "file_path": "src/a.rs",
                "language": "Rust",
                "kind": "STRUCT",
                "is_changed": true,
            }),
        ];

        impacted.sort_by(|a, b| {
            let a_key = (
                a["name"].as_str().unwrap_or(""),
                a["file_path"].as_str().unwrap_or(""),
            );
            let b_key = (
                b["name"].as_str().unwrap_or(""),
                b["file_path"].as_str().unwrap_or(""),
            );
            a_key.cmp(&b_key)
        });

        let mut table = build_premium_table(["Name", "File", "Language", "Kind", "Changed?"]);
        for item in &impacted {
            table.add_row(vec![
                item["name"].as_str().unwrap_or("").to_string(),
                item["file_path"].as_str().unwrap_or("").to_string(),
                item["language"].as_str().unwrap_or("").to_string(),
                item["kind"].as_str().unwrap_or("").to_string(),
                if item["is_changed"].as_bool().unwrap_or(false) {
                    "YES".to_string()
                } else {
                    "NO".to_string()
                },
            ]);
        }
        let rendered = table.to_string();
        assert!(
            rendered.contains('╭'),
            "expected rounded table border, got:\n{rendered}"
        );
        assert!(
            rendered.contains("Name") && rendered.contains("Changed?"),
            "expected headers, got:\n{rendered}"
        );
        // Deterministic order: Account/a.rs before User/a.rs before User/b.rs.
        let account_pos = rendered.find("Account").unwrap_or(usize::MAX);
        let user_a_pos = rendered.find("src/a.rs").unwrap_or(usize::MAX);
        let user_b_pos = rendered.find("src/b.rs").unwrap_or(usize::MAX);
        assert!(
            account_pos < user_a_pos && user_a_pos < user_b_pos,
            "expected deterministic order, got:\n{rendered}"
        );
    }
}
