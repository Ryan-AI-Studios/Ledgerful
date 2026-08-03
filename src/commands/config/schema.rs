use crate::commands::helpers::get_layout;
use crate::index::env_schema::EnvDeclaration;
use crate::index::env_schema::EnvSourceKind;
use crate::index::staleness::check_index_staleness;
use crate::output::empty::{EmptyReason, format_json_empty_state};
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream};

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
            Ok(EnvDeclaration {
                var_name: row.get(0)?,
                source_kind: serde_json::from_str(&format!("\"{}\"", row.get::<_, String>(1)?))
                    .unwrap_or(EnvSourceKind::Config),
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

    let mut results = Vec::new();
    for row in rows {
        results.push(row.into_diagnostic()?);
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
}
