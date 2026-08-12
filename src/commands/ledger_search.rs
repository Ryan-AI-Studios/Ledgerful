use comfy_table::{Cell, Color, ColumnConstraint, Width};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::ledger::transaction::TransactionManager;
use crate::ledger::types::Category;
use crate::ledger::ui::{breaking_icon, get_category_icon, get_change_type_icon, with_icon};
use crate::output::table::{
    build_premium_table_with_style, prepare_width_aware_table, resolve_table_style,
};
use crate::state::storage::StorageManager;

/// Short committed display for human tables (YYYY-MM-DD HH:MM UTC when parseable).
pub(crate) fn format_committed_short(committed_at: &str) -> String {
    // Common ledger shapes: RFC3339 with fractional seconds, or date-only.
    // Keep first 16 chars of "YYYY-MM-DDTHH:MM" when present; else pass through truncated.
    let s = committed_at.trim();
    if s.len() >= 16 && s.as_bytes().get(10) == Some(&b'T') {
        let date = &s[0..10];
        let hm = &s[11..16];
        return format!("{date} {hm}");
    }
    if s.len() > 19 {
        return s.chars().take(19).collect();
    }
    s.to_string()
}

/// Truncate display text for table cells (display-only; JSON unchanged).
pub(crate) fn truncate_display(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars <= 3 {
        return s.chars().take(max_chars).collect();
    }
    let keep = max_chars.saturating_sub(3);
    let mut out: String = s.chars().take(keep).collect();
    out.push_str("...");
    out
}

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
    let mut storage = StorageManager::init_with_layout(&layout)?;
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
        println!(
            "No ledger entries found matching '{}'.",
            query.if_supports_color(Stream::Stdout, |s| s.yellow())
        );
        return Ok(());
    }

    println!(
        "\n{} matching entries for '{}':\n",
        results
            .len()
            .if_supports_color(Stream::Stdout, |s| s.bright_green()),
        query.if_supports_color(Stream::Stdout, |s| s.cyan())
    );

    let style = resolve_table_style();
    let mut table = build_premium_table_with_style(
        style,
        ["ID", "Committed", "Category", "Entity", "Change", "Summary"],
    );
    prepare_width_aware_table(&mut table, style);
    // Column upper bounds keep rows terminal-scannable (B4); display-only.
    table.set_constraints(vec![
        ColumnConstraint::UpperBoundary(Width::Fixed(10)), // ID
        ColumnConstraint::UpperBoundary(Width::Fixed(16)), // Committed short
        ColumnConstraint::UpperBoundary(Width::Fixed(18)), // Category
        ColumnConstraint::UpperBoundary(Width::Fixed(36)), // Entity
        ColumnConstraint::UpperBoundary(Width::Fixed(14)), // Change
        ColumnConstraint::UpperBoundary(Width::Fixed(48)), // Summary
    ]);

    for entry in results {
        let mut summary = entry.summary.clone();
        if entry.is_breaking {
            summary = format!(
                "{} {}",
                breaking_icon(),
                summary.if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().red()))
            );
        }
        let summary_disp = truncate_display(&summary, 48);
        let entity_disp = truncate_display(&entry.entity, 36);

        let id_prefix = if entry.tx_id.len() > 8 {
            &entry.tx_id[0..8]
        } else {
            &entry.tx_id
        };

        table.add_row(vec![
            Cell::new(id_prefix).fg(Color::DarkGrey),
            Cell::new(format_committed_short(&entry.committed_at)),
            Cell::new(with_icon(
                &get_category_icon(&entry.category),
                format!("{:?}", entry.category),
            )),
            Cell::new(entity_disp).fg(Color::Yellow),
            Cell::new(with_icon(
                &get_change_type_icon(&entry.change_type),
                format!("{:?}", entry.change_type),
            )),
            Cell::new(summary_disp),
        ]);
    }

    println!("{table}");

    Ok(())
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use crate::output::table::{
        TableStyleKind, build_premium_table_with_style, prepare_width_aware_table,
    };
    use comfy_table::{ColumnConstraint, Width};

    #[test]
    fn format_committed_short__rfc3339__date_hm() {
        assert_eq!(
            format_committed_short("2026-07-17T11:33:25.984842800+00:00"),
            "2026-07-17 11:33"
        );
    }

    #[test]
    fn truncate_display__long__ellipsis() {
        let s = "abcdefghijklmnopqrstuvwxyz";
        let t = truncate_display(s, 10);
        assert_eq!(t, "abcdefg...");
        assert!(t.chars().count() <= 10);
    }

    #[test]
    fn ledger_search_human_table_max_line_under_forced_width() {
        let style = TableStyleKind::Ascii;
        let mut table = build_premium_table_with_style(
            style,
            ["ID", "Committed", "Category", "Entity", "Change", "Summary"],
        );
        // Hermetic non-TTY width (0181-E / T9).
        table.force_no_tty();
        table.set_width(100);
        table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);
        table.set_truncation_indicator("...");
        table.set_constraints(vec![
            ColumnConstraint::UpperBoundary(Width::Fixed(10)),
            ColumnConstraint::UpperBoundary(Width::Fixed(16)),
            ColumnConstraint::UpperBoundary(Width::Fixed(18)),
            ColumnConstraint::UpperBoundary(Width::Fixed(36)),
            ColumnConstraint::UpperBoundary(Width::Fixed(14)),
            ColumnConstraint::UpperBoundary(Width::Fixed(48)),
        ]);
        table.add_row(vec![
            "e501be00",
            "2026-07-17 11:33",
            "Bugfix",
            &"x".repeat(80),
            "Modify",
            &"y".repeat(120),
        ]);
        let rendered = table.to_string();
        let max_line = rendered
            .lines()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0);
        // comfy-table may use width as soft target; allow small slack for borders.
        assert!(
            max_line <= 110,
            "expected max line ≤110 under width 100, got {max_line}:\n{rendered}"
        );
        assert!(
            rendered.contains('+') || rendered.contains('|'),
            "expected ASCII borders, got:\n{rendered}"
        );
        assert!(
            !rendered.contains('╭'),
            "Ascii table must not use rounded UTF-8, got:\n{rendered}"
        );
    }

    #[test]
    fn prepare_width_aware_table_ascii_truncation_indicator() {
        use comfy_table::Row;
        let style = TableStyleKind::Ascii;
        let mut table = build_premium_table_with_style(style, ["A"]);
        prepare_width_aware_table(&mut table, style);
        table.force_no_tty();
        table.set_width(16);
        table.set_constraints(vec![ColumnConstraint::Absolute(Width::Fixed(10))]);
        let mut row = Row::from(vec!["abcdefghijklmnopqrstuvwxyz"]);
        row.max_height(1);
        table.add_row(row);
        let rendered = table.to_string();
        assert!(
            rendered.contains("..."),
            "expected ASCII '...', got:\n{rendered}"
        );
        assert!(
            !rendered.contains('…'),
            "no U+2026 under Ascii:\n{rendered}"
        );
    }
}
