use crate::commands::helpers::get_layout;
use crate::output::table::build_premium_table;
use crate::state::storage::StorageManager;
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use std::collections::HashMap;

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

/// One `data_models` row as selected for list / impact surfaces.
#[derive(Debug, Clone, PartialEq)]
struct DataModelRow {
    id: i64,
    model_name: String,
    language: String,
    model_kind: String,
    confidence: f64,
    model_file_id: i64,
    file_path: String,
}

/// Dedupe key: (model_name, language, model_kind, model_file_id).
type DataModelDedupeKey = (String, String, String, i64);

/// Collapse stacked identical model identities to one row.
///
/// Keep-best: higher confidence; equal confidence keeps lower `id` (no flip).
/// After dedupe, sort name ASC, file_path ASC, language ASC, kind ASC.
fn dedupe_data_model_rows(rows: Vec<DataModelRow>) -> Vec<DataModelRow> {
    let mut best: HashMap<DataModelDedupeKey, DataModelRow> = HashMap::new();

    for row in rows {
        let key = (
            row.model_name.clone(),
            row.language.clone(),
            row.model_kind.clone(),
            row.model_file_id,
        );
        match best.get(&key) {
            None => {
                best.insert(key, row);
            }
            Some(prev) => {
                if data_model_row_better_than(&row, prev) {
                    best.insert(key, row);
                }
            }
        }
    }

    let mut out: Vec<DataModelRow> = best.into_values().collect();
    out.sort_by(|a, b| {
        a.model_name
            .cmp(&b.model_name)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.language.cmp(&b.language))
            .then_with(|| a.model_kind.cmp(&b.model_kind))
    });
    out
}

/// Whether `cand` should replace `prev` under keep-best rules.
/// Strict replace-if-better only: higher confidence; equal conf keeps lower id
/// (no flip on equal confidence when cand has higher id).
fn data_model_row_better_than(cand: &DataModelRow, prev: &DataModelRow) -> bool {
    if cand.confidence > prev.confidence {
        return true;
    }
    if cand.confidence < prev.confidence {
        return false;
    }
    // Equal confidence: keep lower id (replace only if cand id is lower).
    cand.id < prev.id
}

/// SELECT `data_models` JOIN `project_files`, apply confidence threshold, then
/// emit-time dedupe. Mirrors endpoints `query_filter_and_dedupe_endpoints`.
fn query_and_dedupe_data_models(
    conn: &rusqlite::Connection,
    threshold: f64,
) -> Result<Vec<DataModelRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT dm.id, dm.model_name, dm.language, dm.model_kind, dm.confidence, \
             dm.model_file_id, pf.file_path \
             FROM data_models dm \
             INNER JOIN project_files pf ON dm.model_file_id = pf.id \
             WHERE dm.confidence >= ?1",
        )
        .into_diagnostic()?;

    let rows_iter = stmt
        .query_map([threshold], |row| {
            Ok(DataModelRow {
                id: row.get::<_, i64>(0)?,
                model_name: row.get::<_, String>(1)?,
                language: row.get::<_, String>(2)?,
                model_kind: row.get::<_, String>(3)?,
                confidence: row.get::<_, f64>(4)?,
                model_file_id: row.get::<_, i64>(5)?,
                file_path: row.get::<_, String>(6)?.replace('\\', "/"),
            })
        })
        .into_diagnostic()?;

    let mut model_rows: Vec<DataModelRow> = Vec::new();
    for row in rows_iter {
        model_rows.push(row.into_diagnostic()?);
    }
    // Dedupe after confidence filter; sort inside helper.
    Ok(dedupe_data_model_rows(model_rows))
}

pub fn execute_data_models(args: DataModelsArgs) -> Result<()> {
    let layout = get_layout()?;

    match args.command {
        DataModelSubcommands::List {
            all,
            min_confidence,
            json,
        } => {
            let storage = StorageManager::open_read_only(&layout)?;
            let conn = storage.get_connection();

            let threshold = if all { 0.0 } else { min_confidence };
            let model_rows = query_and_dedupe_data_models(conn, threshold)?;

            if json {
                let results: Vec<serde_json::Value> = model_rows
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "name": r.model_name,
                            "language": r.language,
                            "kind": r.model_kind,
                            "confidence": r.confidence,
                            "file_path": r.file_path,
                        })
                    })
                    .collect();
                let output = crate::output::empty::format_json_list_envelope(results, "models");
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output).into_diagnostic()?
                );
            } else {
                println!(
                    "{}",
                    "Data Models"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                if model_rows.is_empty() {
                    println!("  No data models indexed.");
                } else {
                    let mut table =
                        build_premium_table(["Name", "Language", "Kind", "Confidence", "File"]);
                    for r in model_rows {
                        table.add_row(vec![
                            r.model_name
                                .if_supports_color(Stream::Stdout, |s| s.bold())
                                .to_string(),
                            r.language,
                            r.model_kind,
                            format!("{:.2}", r.confidence),
                            r.file_path,
                        ]);
                    }
                    println!("{}", table);
                }
            }
        }
        DataModelSubcommands::Impact { changed, json } => {
            // Path membership only — git status + ignore filter, not full impact.
            // Avoids federation, cache rewrite, and multi-second empty paths (0146).
            let changed_files: std::collections::HashSet<String> =
                crate::git::status::collect_changed_files_for_filter(&layout)?
                    .iter()
                    .map(|c| crate::git::status::normalize_filter_path(&c.path))
                    .collect();

            let storage = StorageManager::open_read_only(&layout)?;
            let conn = storage.get_connection();

            // Query with id + model_file_id for keep-best; JOIN for path (C1/C4).
            let mut stmt = conn
                .prepare(
                    "SELECT dm.id, dm.model_name, pf.file_path, dm.language, dm.model_kind, \
                     dm.confidence, dm.model_file_id \
                     FROM data_models dm \
                     INNER JOIN project_files pf ON dm.model_file_id = pf.id",
                )
                .into_diagnostic()?;

            let rows = stmt
                .query_map([], |row| {
                    Ok(DataModelRow {
                        id: row.get::<_, i64>(0)?,
                        model_name: row.get::<_, String>(1)?,
                        file_path: row.get::<_, String>(2)?.replace('\\', "/"),
                        language: row.get::<_, String>(3)?,
                        model_kind: row.get::<_, String>(4)?,
                        confidence: row.get::<_, f64>(5)?,
                        model_file_id: row.get::<_, i64>(6)?,
                    })
                })
                .into_diagnostic()?;

            let mut filtered: Vec<DataModelRow> = Vec::new();
            let mut changed_flags: HashMap<i64, bool> = HashMap::new();
            for row in rows {
                let r = row.into_diagnostic()?;
                let is_impacted = changed_files.contains(&r.file_path);
                if !changed || is_impacted {
                    changed_flags.insert(r.id, is_impacted);
                    filtered.push(r);
                }
            }

            // Dedupe after --changed filter; preserve empty-state on raw COUNT.
            let deduped = dedupe_data_model_rows(filtered);
            let impacted: Vec<serde_json::Value> = deduped
                .into_iter()
                .map(|r| {
                    let is_changed = changed_flags.get(&r.id).copied().unwrap_or(false);
                    serde_json::json!({
                        "name": r.model_name,
                        "file_path": r.file_path,
                        "language": r.language,
                        "kind": r.model_kind,
                        "confidence": r.confidence,
                        "is_changed": is_changed,
                    })
                })
                .collect();

            if json {
                let output = crate::output::empty::format_json_empty_state(
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
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output).into_diagnostic()?
                );
            } else {
                println!(
                    "{}",
                    "Data Model Impact Analysis"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                if impacted.is_empty() {
                    let total_models: i64 = conn
                        .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
                        .into_diagnostic()?;

                    if total_models > 0 && changed {
                        println!(
                            "{}",
                            "  No changed data models found."
                                .if_supports_color(Stream::Stdout, |s| s.dimmed())
                        );
                    } else {
                        println!(
                            "{}",
                            "  No data models indexed. Data models are extracted from ORM structs, \
                             SQL table definitions, and migration files. Run `ledgerful index \
                             --incremental` if models exist, or confirm your ORM/framework is supported.".if_supports_color(Stream::Stdout, |s| s.dimmed())

                        );
                    }
                } else {
                    let mut table =
                        build_premium_table(["Name", "File", "Language", "Kind", "Changed?"]);
                    for item in &impacted {
                        table.add_row(vec![
                            item["name"]
                                .as_str()
                                .unwrap_or("")
                                .if_supports_color(Stream::Stdout, |s| s.bold())
                                .to_string(),
                            item["file_path"].as_str().unwrap_or("").to_string(),
                            item["language"].as_str().unwrap_or("").to_string(),
                            item["kind"].as_str().unwrap_or("").to_string(),
                            if item["is_changed"].as_bool().unwrap_or(false) {
                                "YES"
                                    .if_supports_color(Stream::Stdout, |s| {
                                        s.style(Style::new().red().bold())
                                    })
                                    .to_string()
                            } else {
                                "NO".if_supports_color(Stream::Stdout, |s| s.dimmed())
                                    .to_string()
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
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn row(
        id: i64,
        name: &str,
        lang: &str,
        kind: &str,
        conf: f64,
        file_id: i64,
        path: &str,
    ) -> DataModelRow {
        DataModelRow {
            id,
            model_name: name.to_string(),
            language: lang.to_string(),
            model_kind: kind.to_string(),
            confidence: conf,
            model_file_id: file_id,
            file_path: path.to_string(),
        }
    }

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn seed_model(
        conn: &Connection,
        file_id: i64,
        name: &str,
        language: &str,
        kind: &str,
        confidence: f64,
        last_indexed_at: &str,
    ) {
        conn.execute(
            "INSERT INTO data_models \
             (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                name,
                file_id,
                language,
                kind,
                confidence,
                "test",
                last_indexed_at,
            ],
        )
        .unwrap();
    }

    #[test]
    fn dedupe_collapses_identical_name_lang_kind_file_id() {
        let rows = vec![
            row(1, "User", "Rust", "STRUCT", 0.9, 10, "src/user.rs"),
            row(2, "User", "Rust", "STRUCT", 0.9, 10, "src/user.rs"),
        ];
        let out = dedupe_data_model_rows(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, 1, "equal conf keeps lower id");
    }

    #[test]
    fn dedupe_keeps_different_file_id_same_name() {
        let rows = vec![
            row(1, "User", "Rust", "STRUCT", 0.9, 10, "src/a.rs"),
            row(2, "User", "Rust", "STRUCT", 0.9, 11, "src/b.rs"),
        ];
        let out = dedupe_data_model_rows(rows);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn dedupe_keep_best_higher_confidence() {
        let rows = vec![
            row(1, "User", "Rust", "STRUCT", 0.5, 10, "src/user.rs"),
            row(2, "User", "Rust", "STRUCT", 0.95, 10, "src/user.rs"),
        ];
        let out = dedupe_data_model_rows(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, 2);
        assert!((out[0].confidence - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn dedupe_equal_conf_keeps_lower_id_no_flip() {
        // Higher id first in input — must not flip when conf equal.
        let rows = vec![
            row(5, "User", "Rust", "STRUCT", 0.9, 10, "src/user.rs"),
            row(2, "User", "Rust", "STRUCT", 0.9, 10, "src/user.rs"),
            row(9, "User", "Rust", "STRUCT", 0.9, 10, "src/user.rs"),
        ];
        let out = dedupe_data_model_rows(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, 2, "equal conf must keep lowest id");
    }

    #[test]
    fn dedupe_sort_stable_name_path_lang_kind() {
        let rows = vec![
            row(1, "User", "Rust", "STRUCT", 0.9, 11, "src/b.rs"),
            row(2, "Account", "Rust", "STRUCT", 0.9, 10, "src/a.rs"),
            row(3, "User", "Rust", "STRUCT", 0.9, 10, "src/a.rs"),
            row(4, "User", "Go", "STRUCT", 0.9, 12, "src/c.go"),
        ];
        let out = dedupe_data_model_rows(rows);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].model_name, "Account");
        assert_eq!(out[1].model_name, "User");
        assert_eq!(out[1].file_path, "src/a.rs");
        assert_eq!(out[2].model_name, "User");
        assert_eq!(out[2].file_path, "src/b.rs");
        assert_eq!(out[3].model_name, "User");
        assert_eq!(out[3].language, "Go");
    }

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
            rendered.contains('╭') || rendered.contains('+'),
            "expected premium table border (utf8 rounded or ascii +), got:\n{rendered}"
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

    /// SELECT + confidence filter + dedupe against a real migrated SQLite conn:
    /// three stacked identical model identities collapse to one emit row
    /// alongside a distinct second model; low-confidence stack is filtered out.
    #[test]
    fn query_and_dedupe_collapses_stacked_identical_models() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "src/models/user.rs",
                "Rust",
                "hash_stack_dm",
                100,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "src/models/account.rs",
                "Rust",
                "hash_stack_dm2",
                100,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
        let account_file_id = conn.last_insert_rowid();

        // Three stacked identical User identities (legacy multi-pass residue).
        // Vary confidence so keep-best is exercised (highest conf wins).
        seed_model(
            conn,
            file_id,
            "User",
            "Rust",
            "STRUCT",
            0.7,
            "2026-05-01T00:00:00Z",
        );
        seed_model(
            conn,
            file_id,
            "User",
            "Rust",
            "STRUCT",
            0.95,
            "2026-05-02T00:00:00Z",
        );
        seed_model(
            conn,
            file_id,
            "User",
            "Rust",
            "STRUCT",
            0.8,
            "2026-05-03T00:00:00Z",
        );
        // Distinct model must survive.
        seed_model(
            conn,
            account_file_id,
            "Account",
            "Rust",
            "STRUCT",
            0.9,
            "2026-05-01T00:00:00Z",
        );
        // Below default List threshold (0.5) — filtered before dedupe.
        seed_model(
            conn,
            file_id,
            "LowConf",
            "Rust",
            "STRUCT",
            0.2,
            "2026-05-01T00:00:00Z",
        );

        let raw_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |r| r.get(0))
            .unwrap();
        assert_eq!(raw_count, 5, "fixture must leave stacked rows in the table");

        let rows = query_and_dedupe_data_models(conn, 0.5).expect("query+dedupe");
        assert_eq!(
            rows.len(),
            2,
            "three stacked User collapse to one; Account remains; LowConf filtered"
        );
        assert_eq!(rows[0].model_name, "Account");
        assert_eq!(rows[1].model_name, "User");
        assert!(
            (rows[1].confidence - 0.95).abs() < f64::EPSILON,
            "keep-best must retain highest confidence stacked User"
        );
        assert_eq!(rows[1].file_path, "src/models/user.rs");

        // Uniqueness of emit keys (name, language, kind, file_id).
        let mut keys: Vec<(String, String, String, i64)> = rows
            .iter()
            .map(|r| {
                (
                    r.model_name.clone(),
                    r.language.clone(),
                    r.model_kind.clone(),
                    r.model_file_id,
                )
            })
            .collect();
        keys.sort();
        let mut uniq = keys.clone();
        uniq.dedup();
        assert_eq!(
            keys, uniq,
            "emit rows must be unique on (name, language, kind, file_id)"
        );
    }

    #[test]
    fn query_and_dedupe_all_threshold_includes_low_confidence() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "src/models/low.rs",
                "Rust",
                "hash_low",
                50,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        seed_model(
            conn,
            file_id,
            "LowConf",
            "Rust",
            "STRUCT",
            0.2,
            "2026-05-01T00:00:00Z",
        );

        let filtered = query_and_dedupe_data_models(conn, 0.5).expect("query+dedupe");
        assert!(filtered.is_empty(), "0.2 conf must fail default threshold");

        let all = query_and_dedupe_data_models(conn, 0.0).expect("query+dedupe --all");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].model_name, "LowConf");
    }
}
