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
    /// List extracted data models
    List {
        /// Show all candidate structs, even those with low confidence
        #[arg(long)]
        all: bool,
        /// Minimum confidence threshold
        #[arg(long, default_value_t = 0.5)]
        min_confidence: f64,
        /// Include fixture and test-path models omitted from the default list
        #[arg(long)]
        include_fixtures: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show impact of changes on data models
    Impact {
        /// Filter by changed models only
        #[arg(long)]
        changed: bool,
        /// Include fixture and test-path models omitted from the default list
        #[arg(long)]
        include_fixtures: bool,
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

fn omit_fixture_data_model_rows(
    rows: Vec<DataModelRow>,
    include_fixtures: bool,
) -> (Vec<DataModelRow>, usize) {
    if include_fixtures {
        return (rows, 0);
    }
    let mut kept = Vec::new();
    let mut omitted = 0usize;
    for row in rows {
        if crate::index::test_mapping::is_test_path(&row.file_path) {
            omitted += 1;
        } else {
            kept.push(row);
        }
    }
    (kept, omitted)
}

fn attach_fixture_flags(output: &mut serde_json::Value, include_fixtures: bool, omitted: usize) {
    if let Some(obj) = output.as_object_mut() {
        obj.insert(
            "includeFixtures".to_string(),
            serde_json::json!(include_fixtures),
        );
        obj.insert("fixturesOmitted".to_string(), serde_json::json!(omitted));
    }
}

fn data_model_row_to_list_json(r: &DataModelRow) -> serde_json::Value {
    serde_json::json!({
        "name": r.model_name,
        "language": r.language,
        "kind": r.model_kind,
        "confidence": r.confidence,
        "file_path": r.file_path,
        "fieldImpact": "unsupported",
    })
}

fn data_model_row_to_impact_json(r: &DataModelRow, is_changed: bool) -> serde_json::Value {
    serde_json::json!({
        "name": r.model_name,
        "file_path": r.file_path,
        "language": r.language,
        "kind": r.model_kind,
        "confidence": r.confidence,
        "is_changed": is_changed,
        "fieldImpact": "unsupported",
    })
}

fn print_data_models_omit_footer(omitted: usize) {
    if omitted > 0 {
        println!("  {omitted} fixture models omitted. Pass --include-fixtures to show them.");
    }
}

fn product_empty_omit_message(omitted: usize) -> String {
    format!(
        "No product data models indexed. {omitted} fixture models omitted. Pass --include-fixtures to show them."
    )
}

fn list_json_empty_reason(fixtures_omitted: usize) -> (crate::output::empty::EmptyReason, String) {
    (
        crate::output::empty::EmptyReason::NoMatches,
        product_empty_omit_message(fixtures_omitted),
    )
}

/// Empty-state reason for impact JSON. `total_models` is queried on the outer
/// `execute` `Result` before `format_json_empty_state` (no silent-zero).
fn impact_json_empty_reason(
    changed: bool,
    total_models: i64,
    fixtures_omitted: usize,
) -> (crate::output::empty::EmptyReason, String) {
    if changed && total_models > 0 {
        (
            crate::output::empty::EmptyReason::CleanDiff,
            "No changed data models found.".to_string(),
        )
    } else if !changed && fixtures_omitted > 0 {
        list_json_empty_reason(fixtures_omitted)
    } else {
        (
            crate::output::empty::EmptyReason::NoIndexedData,
            "No data models indexed. Data models are extracted from ORM structs, \
             SQL table definitions, and migration files. Run `ledgerful index \
             --incremental` if models exist, or confirm your ORM/framework is supported."
                .to_string(),
        )
    }
}

pub fn execute_data_models(args: DataModelsArgs) -> Result<()> {
    let layout = get_layout()?;

    match args.command {
        DataModelSubcommands::List {
            all,
            min_confidence,
            include_fixtures,
            json,
        } => {
            let storage = StorageManager::open_read_only(&layout)?;
            let conn = storage.get_connection();

            let threshold = if all { 0.0 } else { min_confidence };
            let model_rows = query_and_dedupe_data_models(conn, threshold)?;
            let (model_rows, fixtures_omitted) =
                omit_fixture_data_model_rows(model_rows, include_fixtures);

            if json {
                let results: Vec<serde_json::Value> =
                    model_rows.iter().map(data_model_row_to_list_json).collect();
                let mut output = if results.is_empty() && fixtures_omitted > 0 {
                    crate::output::empty::format_json_empty_state(results, "models", || {
                        list_json_empty_reason(fixtures_omitted)
                    })
                } else {
                    crate::output::empty::format_json_list_envelope(results, "models")
                };
                attach_fixture_flags(&mut output, include_fixtures, fixtures_omitted);
                crate::output::json::emit(&output)?;
            } else {
                println!(
                    "{}",
                    "Data Models"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                if model_rows.is_empty() {
                    if fixtures_omitted > 0 {
                        println!("  {}", product_empty_omit_message(fixtures_omitted));
                    } else {
                        println!("  No data models indexed.");
                    }
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
                    print_data_models_omit_footer(fixtures_omitted);
                }
            }
        }
        DataModelSubcommands::Impact {
            changed,
            include_fixtures,
            json,
        } => {
            // Path membership only — git status + ignore filter, not full impact.
            // Avoids federation, cache rewrite, and multi-second empty paths (0146).
            let changed_files: std::collections::HashSet<String> =
                crate::git::status::collect_changed_files_for_filter(&layout)?
                    .iter()
                    .map(|c| crate::git::status::normalize_filter_path(&c.path))
                    .collect();

            let storage = StorageManager::open_read_only(&layout)?;
            let conn = storage.get_connection();

            // COUNT on the outer Result — helper closure cannot `?` (0243 same-class).
            let total_models: i64 = conn
                .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
                .into_diagnostic()?;

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

            // Dedupe after --changed filter, then emit-time fixture omit.
            let deduped = dedupe_data_model_rows(filtered);
            let (deduped, fixtures_omitted) =
                omit_fixture_data_model_rows(deduped, include_fixtures);
            let impacted: Vec<serde_json::Value> = deduped
                .iter()
                .map(|r| {
                    let is_changed = changed_flags.get(&r.id).copied().unwrap_or(false);
                    data_model_row_to_impact_json(r, is_changed)
                })
                .collect();

            if json {
                let mut output =
                    crate::output::empty::format_json_empty_state(impacted, "impacted", || {
                        impact_json_empty_reason(changed, total_models, fixtures_omitted)
                    });
                attach_fixture_flags(&mut output, include_fixtures, fixtures_omitted);
                crate::output::json::emit(&output)?;
            } else {
                println!(
                    "{}",
                    "Data Model Impact Analysis"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                if impacted.is_empty() {
                    if changed && total_models > 0 {
                        println!(
                            "{}",
                            "  No changed data models found."
                                .if_supports_color(Stream::Stdout, |s| s.dimmed())
                        );
                    } else if !changed && fixtures_omitted > 0 {
                        println!("  {}", product_empty_omit_message(fixtures_omitted));
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
                    print_data_models_omit_footer(fixtures_omitted);
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

    #[test]
    fn data_models_default_omits_test_path() {
        let rows = vec![
            row(
                1,
                "User",
                "go",
                "STRUCT",
                1.0,
                10,
                "tests/fixtures/go_sample/pkg/user.go",
            ),
            row(
                2,
                "UserRow",
                "Rust",
                "SCHEMA",
                0.9,
                11,
                "src/models/user.rs",
            ),
        ];
        let (kept, omitted) = omit_fixture_data_model_rows(rows, false);
        assert_eq!(omitted, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].model_name, "UserRow");
    }

    #[test]
    fn data_models_windows_test_path_still_omits() {
        let rows = vec![row(
            1,
            "User",
            "go",
            "STRUCT",
            1.0,
            10,
            r"tests\fixtures\go_sample\pkg\user.go",
        )];
        let (kept, omitted) = omit_fixture_data_model_rows(rows, false);
        assert_eq!(omitted, 1);
        assert!(kept.is_empty());
    }

    #[test]
    fn data_models_include_fixtures_restores() {
        let rows = vec![
            row(
                1,
                "User",
                "go",
                "STRUCT",
                1.0,
                10,
                "tests/fixtures/go_sample/pkg/user.go",
            ),
            row(
                2,
                "UserRow",
                "Rust",
                "SCHEMA",
                0.9,
                11,
                "src/models/user.rs",
            ),
        ];
        let (kept, omitted) = omit_fixture_data_model_rows(rows, true);
        assert_eq!(omitted, 0);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn data_models_include_fixtures_echoed_on_empty() {
        let mut empty = crate::output::empty::format_json_empty_state(
            Vec::<serde_json::Value>::new(),
            "models",
            || list_json_empty_reason(3),
        );
        attach_fixture_flags(&mut empty, false, 3);
        assert_eq!(empty["includeFixtures"], false);
        assert_eq!(empty["fixturesOmitted"], 3);
        assert_eq!(empty["emptyReason"], "noMatches");
        assert!(empty.get("fieldImpact").is_none());
        assert!(
            empty["message"]
                .as_str()
                .unwrap_or("")
                .contains("No product data models indexed")
        );

        let mut clean = crate::output::empty::format_json_empty_state(
            Vec::<serde_json::Value>::new(),
            "impacted",
            || impact_json_empty_reason(true, 4, 0),
        );
        attach_fixture_flags(&mut clean, true, 0);
        assert_eq!(clean["includeFixtures"], true);
        assert_eq!(clean["fixturesOmitted"], 0);
        assert_eq!(clean["emptyReason"], "cleanDiff");
        assert!(clean.get("fieldImpact").is_none());
    }

    #[test]
    fn data_models_json_emits_field_impact_unsupported() {
        let item = data_model_row_to_list_json(&row(
            1,
            "UserRow",
            "Rust",
            "SCHEMA",
            0.9,
            11,
            "src/models/user.rs",
        ));
        assert_eq!(item["fieldImpact"], "unsupported");
        assert!(item.get("fields").is_none());
        assert!(item.get("table").is_none());
        assert!(item.get("evidence").is_none());

        let impact = data_model_row_to_impact_json(
            &row(
                1,
                "UserRow",
                "Rust",
                "SCHEMA",
                0.9,
                11,
                "src/models/user.rs",
            ),
            false,
        );
        assert_eq!(impact["fieldImpact"], "unsupported");
        assert!(impact.get("fields").is_none());
        assert!(impact.get("table").is_none());
        assert!(impact.get("evidence").is_none());
    }

    #[test]
    fn data_models_post_omit_empty_not_no_indexed() {
        let rows = vec![row(
            1,
            "User",
            "go",
            "STRUCT",
            1.0,
            10,
            "tests/fixtures/go_sample/pkg/user.go",
        )];
        let (kept, omitted) = omit_fixture_data_model_rows(rows, false);
        assert!(kept.is_empty());
        assert_eq!(omitted, 1);

        let human = product_empty_omit_message(omitted);
        assert!(!human.contains("No data models indexed."));
        assert!(human.contains("No product data models indexed."));

        let (reason, message) = list_json_empty_reason(omitted);
        assert_eq!(reason, crate::output::empty::EmptyReason::NoMatches);
        assert!(!message.contains("No data models indexed."));
        assert!(!message.contains("noIndexedData"));

        let (impact_reason, _) = impact_json_empty_reason(false, 1, omitted);
        assert_eq!(impact_reason, crate::output::empty::EmptyReason::NoMatches);
    }

    #[test]
    fn data_models_impact_count_before_empty_helper() {
        let src = include_str!("data_models.rs");
        assert!(
            src.contains("COUNT on the outer Result"),
            "COUNT must be documented on the outer execute Result"
        );
        assert!(
            src.contains("impact_json_empty_reason(changed, total_models, fixtures_omitted)"),
            "empty helper must receive the precomputed total, not query inside the closure"
        );
        let (reason, message) = impact_json_empty_reason(true, 5, 0);
        assert_eq!(reason, crate::output::empty::EmptyReason::CleanDiff);
        assert_eq!(message, "No changed data models found.");
        assert!(!message.contains("no compatibility risk"));
    }
}
