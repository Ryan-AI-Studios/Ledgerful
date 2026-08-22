use crate::commands::helpers::get_layout;
use crate::index::env_schema::EnvDeclaration;
use crate::index::env_schema::EnvSourceKind;
use crate::index::staleness::check_index_staleness;
use crate::output::empty::{EmptyReason, format_json_empty_state};
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream};
use std::str::FromStr;

pub fn execute_config_schema(json: bool) -> Result<()> {
    let layout = get_layout()?;
    let storage = crate::state::storage::StorageManager::open_read_only(&layout)?;
    let conn = storage.get_connection();

    let mut stmt = conn
        .prepare(
            "SELECT var_name, source_kind, required, is_secret, default_value_redacted, description, owner, environment
         FROM env_declarations ORDER BY var_name ASC",
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
        ) = row.into_diagnostic()?;
        let source_kind = EnvSourceKind::from_str(&source_kind_raw)?;
        results.push(EnvDeclaration {
            var_name,
            source_kind,
            required: required != 0,
            is_secret: is_secret != 0,
            default_value_redacted,
            description,
            owner,
            environment,
            confidence: 1.0,
        });
    }

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

        let mut table = Table::new();
        table.set_header(vec!["Variable", "Source", "Req", "Sec", "Default", "Owner"]);

        for d in results {
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
    fn non_empty_json_is_bare_array() {
        let items = vec![serde_json::json!({"var_name": "FOO"})];
        let output = format_json_empty_state(items, "results", || {
            (EmptyReason::NoMatches, "unused".to_string())
        });
        assert!(output.is_array());
        assert_eq!(output.as_array().unwrap().len(), 1);
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
