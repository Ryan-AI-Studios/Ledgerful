use crate::commands::helpers::get_layout;
use crate::index::env_schema::EnvSourceKind;
use crate::index::staleness::check_index_staleness;
use crate::output::empty::{EmptyReason, format_json_empty_state};
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream};
use serde::Serialize;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SchemaEmitRow {
    pub var_name: String,
    pub source_kind: EnvSourceKind,
    pub required: bool,
    pub is_secret: bool,
    pub default_value_redacted: Option<String>,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub environment: Option<String>,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requiredness: Option<&'static str>,
}

pub(crate) fn requiredness_label(
    kind: EnvSourceKind,
    required: bool,
    confidence: f64,
) -> Option<&'static str> {
    if kind == EnvSourceKind::Docs || confidence < 1.0 {
        Some("unknown")
    } else if matches!(kind, EnvSourceKind::DotenvExample | EnvSourceKind::Config) && !required {
        Some("optional")
    } else {
        None
    }
}

pub(crate) fn load_schema_rows(conn: &rusqlite::Connection) -> Result<Vec<SchemaEmitRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT d.var_name, d.source_kind, d.required, d.is_secret, d.default_value_redacted,
                    d.description, d.owner, d.environment, d.confidence, f.file_path
             FROM env_declarations d
             LEFT JOIN project_files f ON f.id = d.source_file_id
             ORDER BY d.var_name ASC, d.source_kind ASC",
        )
        .into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, f64>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })
        .into_diagnostic()?;

    let mut results = Vec::new();
    for row in rows {
        let (
            var_name,
            source_kind_raw,
            required,
            is_secret,
            default_value_redacted,
            description,
            owner,
            environment,
            confidence,
            file_path,
        ) = row.into_diagnostic()?;
        let source_kind = EnvSourceKind::from_str(&source_kind_raw)?;
        let required = required != 0;
        let requiredness = requiredness_label(source_kind.clone(), required, confidence);
        results.push(SchemaEmitRow {
            var_name,
            source_kind,
            required,
            is_secret: is_secret != 0,
            default_value_redacted,
            description,
            owner,
            environment,
            confidence,
            file_path,
            requiredness,
        });
    }
    Ok(results)
}

pub fn execute_config_schema(json: bool) -> Result<()> {
    let layout = get_layout()?;
    let storage = crate::state::storage::StorageManager::open_read_only(&layout)?;
    let conn = storage.get_connection();
    let results = load_schema_rows(conn)?;

    if json {
        let output = format_json_empty_state(results, "results", || {
            empty_state_message(&storage, &layout)
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        if results.is_empty() {
            let (_, msg) = empty_state_message(&storage, &layout);
            println!("{}", msg.if_supports_color(Stream::Stdout, |s| s.dimmed()));
        }

        let optional_n = results
            .iter()
            .filter(|d| d.requiredness == Some("optional"))
            .count();
        let unknown_n = results
            .iter()
            .filter(|d| d.requiredness == Some("unknown"))
            .count();

        let mut table = Table::new();
        table.set_header(vec![
            "Variable", "Source", "Req", "Sec", "Default", "Owner", "File",
        ]);

        for d in results {
            table.add_row(vec![
                d.var_name,
                d.source_kind.to_string(),
                if d.required { "YES" } else { "no" }.to_string(),
                if d.is_secret { "🔒" } else { "-" }.to_string(),
                d.default_value_redacted.unwrap_or_else(|| "-".to_string()),
                d.owner.unwrap_or_else(|| "-".to_string()),
                d.file_path.unwrap_or_else(|| "-".to_string()),
            ]);
        }
        println!("{}", table);
        println!("{optional_n} optional (declared), {unknown_n} unknown requiredness.");
    }

    Ok(())
}

/// Why `config schema` is empty + next step (no coverage kill-switch for env
/// schema — never `DisabledByConfig`).
pub fn empty_state_message(
    storage: &StorageManager,
    layout: &crate::state::layout::Layout,
) -> (EmptyReason, String) {
    let threshold_days = crate::config::load_config(layout)
        .map(|c| c.index.stale_threshold_days)
        .unwrap_or(7);
    let stale = check_index_staleness(storage, threshold_days);

    match stale {
        Some(w) if w.is_missing => (
            EmptyReason::NoIndexedData,
            "  No env schema declarations found. The index has never been built. Run \
             `ledgerful index --incremental` to extract env declarations (typically from \
             `.env.example`)."
                .to_string(),
        ),
        Some(_) => (
            EmptyReason::StaleIndex,
            "  No env schema declarations found. The index looks stale. Run \
             `ledgerful index --incremental` to refresh, then re-check. If this repo has no \
             `.env.example` (or other declaration sources), add one and re-index."
                .to_string(),
        ),
        None => (
            EmptyReason::NoMatches,
            "  No env schema declarations found. The index is present but contains no \
             declarations — add a `.env.example` (or other supported declaration sources), then \
             run `ledgerful index --incremental`."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::empty::EmptyReason;
    use crate::state::layout::Layout;
    use crate::state::migrations::get_migrations;
    use chrono::Utc;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn set_last_indexed_at(storage: &StorageManager, ts: &str) {
        let conn = storage.get_connection();
        conn.execute(
            "INSERT OR REPLACE INTO index_metadata (key, value) VALUES ('last_indexed_at', ?1)",
            [ts],
        )
        .unwrap();
    }

    /// Layout only used for `load_config` threshold (defaults to 7 when absent).
    fn temp_layout() -> (tempfile::TempDir, Layout) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 temp path");
        let layout = Layout::new(root);
        (tmp, layout)
    }

    #[test]
    fn empty_json_envelope_uses_results_key() {
        let items: Vec<serde_json::Value> = vec![];
        let output = format_json_empty_state(items, "results", || {
            (
                EmptyReason::NoMatches,
                "  No env schema declarations found.".to_string(),
            )
        });
        assert!(output.is_object());
        assert_eq!(output["schemaVersion"], 1);
        assert_eq!(output["resultCount"], 0);
        assert_eq!(output["emptyReason"], "noMatches");
        assert!(
            output["message"]
                .as_str()
                .unwrap()
                .contains("No env schema")
        );
        assert!(output["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn non_empty_json_is_object_envelope() {
        let items = vec![serde_json::json!({"var_name": "FOO"})];
        let output = format_json_empty_state(items, "results", || {
            panic!("reason_fn must not run for populated lists");
        });
        assert!(
            output.is_object(),
            "populated schema JSON must be an object"
        );
        assert_eq!(output["schemaVersion"], 1);
        assert_eq!(output["resultCount"], 1);
        assert_eq!(output["results"].as_array().map(|a| a.len()), Some(1));
        assert!(
            output.get("emptyReason").is_none(),
            "populated arm must omit emptyReason: {output}"
        );
    }

    #[test]
    fn empty_state_missing_index_is_no_indexed_data() {
        let storage = in_memory_storage();
        let (_tmp, layout) = temp_layout();

        let (reason, msg) = empty_state_message(&storage, &layout);
        assert_eq!(reason, EmptyReason::NoIndexedData);
        assert!(
            msg.contains("index --incremental"),
            "missing-index message should mention index --incremental, got: {msg}"
        );
        assert!(
            msg.contains("never been built") || msg.to_lowercase().contains("index"),
            "missing-index message should reference index state, got: {msg}"
        );
    }

    #[test]
    fn empty_state_fresh_index_is_no_matches() {
        let storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        set_last_indexed_at(&storage, &now);
        let (_tmp, layout) = temp_layout();

        let (reason, msg) = empty_state_message(&storage, &layout);
        assert_eq!(reason, EmptyReason::NoMatches);
        assert!(
            msg.contains(".env.example"),
            "fresh-empty message should mention .env.example, got: {msg}"
        );
    }

    #[test]
    fn requiredness_unknown_wins_over_config_optional() {
        assert_eq!(
            requiredness_label(EnvSourceKind::Config, false, 0.7),
            Some("unknown")
        );
        assert_eq!(
            requiredness_label(EnvSourceKind::DotenvExample, false, 1.0),
            Some("optional")
        );
        assert_eq!(
            requiredness_label(EnvSourceKind::Docs, false, 1.0),
            Some("unknown")
        );
    }

    #[test]
    fn load_schema_rows_joins_file_path_and_reads_confidence() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (id, file_path, language, last_indexed_at)
             VALUES (1, '.env.example', 'Dotenv', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO env_declarations (
                var_name, source_file_id, source_kind, required, default_value_redacted,
                confidence, last_indexed_at, is_secret
             ) VALUES ('FOO', 1, 'DOTENV_EXAMPLE', 0, 'EMPTY_DEFAULT', 1.0, '2026-01-01T00:00:00Z', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO env_declarations (
                var_name, source_file_id, source_kind, required, default_value_redacted,
                confidence, last_indexed_at, is_secret
             ) VALUES ('BAR', 1, 'CONFIG', 0, 'HAS_DEFAULT', 0.7, '2026-01-01T00:00:00Z', 0)",
            [],
        )
        .unwrap();
        let rows = load_schema_rows(conn).unwrap();
        let foo = rows.iter().find(|r| r.var_name == "FOO").unwrap();
        assert_eq!(foo.file_path.as_deref(), Some(".env.example"));
        assert_eq!(foo.requiredness, Some("optional"));
        assert_eq!(foo.confidence, 1.0);
        assert!(!foo.required);
        let bar = rows.iter().find(|r| r.var_name == "BAR").unwrap();
        assert_eq!(bar.requiredness, Some("unknown"));
        assert_eq!(bar.confidence, 0.7);
        assert!(!bar.required);
    }

    #[test]
    fn empty_state_stale_index_is_stale_index() {
        let storage = in_memory_storage();
        // Default threshold is 7 days when config is absent.
        let old = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        set_last_indexed_at(&storage, &old);
        let (_tmp, layout) = temp_layout();

        let (reason, msg) = empty_state_message(&storage, &layout);
        assert_eq!(reason, EmptyReason::StaleIndex);
        assert!(
            msg.contains("stale") || msg.contains("index --incremental"),
            "stale-index message should mention stale/refresh, got: {msg}"
        );
    }
}
