use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, Table};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::ledger::transaction::TransactionManager;
use crate::ledger::types::Category;
use crate::ledger::ui::{breaking_icon, get_category_icon, get_change_type_icon};
use crate::state::storage::StorageManager;

pub fn execute_ledger_search(
    query: String,
    category: Option<Category>,
    days: Option<u64>,
    breaking: bool,
    limit: usize,
    offset: usize,
    json: bool,
) -> Result<()> {
    let layout = get_layout()?;
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let manager = TransactionManager::new(&mut storage, layout.root.clone().into(), config);

    let cat_filter = category.map(|c| {
        serde_json::to_string(&c)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string()
    });

    let results = manager
        .search_ledger(
            &query,
            cat_filter.as_deref(),
            days,
            breaking,
            Some(limit),
            offset,
        )
        .map_err(|e| miette::miette!("{}", e))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).into_diagnostic()?
        );
        return Ok(());
    }

    if results.is_empty() {
        println!("No ledger entries found matching '{}'.", query.yellow());
        return Ok(());
    }

    println!(
        "\n{} matching entries for '{}':\n",
        results.len().bright_green(),
        query.cyan()
    );

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(vec![
            Cell::new("ID").fg(Color::Cyan),
            Cell::new("Committed").fg(Color::Cyan),
            Cell::new("Category").fg(Color::Cyan),
            Cell::new("Entity").fg(Color::Cyan),
            Cell::new("Change").fg(Color::Cyan),
            Cell::new("Summary").fg(Color::Cyan),
        ]);

    for entry in results {
        let mut summary = entry.summary.clone();
        if entry.is_breaking {
            summary = format!("{} {}", breaking_icon(), summary.bold().red());
        }

        let id_prefix = if entry.tx_id.len() > 8 {
            &entry.tx_id[0..8]
        } else {
            &entry.tx_id
        };

        table.add_row(vec![
            Cell::new(id_prefix).fg(Color::DarkGrey),
            Cell::new(&entry.committed_at),
            Cell::new(format!(
                "{} {:?}",
                get_category_icon(&entry.category),
                entry.category
            )),
            Cell::new(&entry.entity).fg(Color::Yellow),
            Cell::new(format!(
                "{} {:?}",
                get_change_type_icon(&entry.change_type),
                entry.change_type
            )),
            Cell::new(summary),
        ]);
    }

    println!("{table}");

    Ok(())
}
