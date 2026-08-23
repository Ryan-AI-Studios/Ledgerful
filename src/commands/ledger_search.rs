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
///
/// If the char cut is mid-token, snap to the last whitespace in the kept prefix
/// only when the snapped prefix is at least `max(8, keep/2)` chars and the
/// whitespace is not index 0. Otherwise keep mid-word ellipsis.
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
    let prefix: String = s.chars().take(keep).collect();

    let last_kept_ws = prefix.chars().next_back().is_some_and(char::is_whitespace);
    let first_dropped_ws = s.chars().nth(keep).is_some_and(char::is_whitespace);
    let mid_token = !last_kept_ws && !first_dropped_ws;

    if mid_token
        && let Some((ws_byte, _)) = prefix.char_indices().rev().find(|(_, c)| c.is_whitespace())
        && ws_byte != 0
    {
        let snapped = &prefix[..ws_byte];
        let snapped_chars = snapped.chars().count();
        let min_kept = 8.max(keep / 2);
        if snapped_chars >= min_kept {
            return format!("{snapped}...");
        }
    }

    format!("{prefix}...")
}

/// Human omitted-honesty line (CLI table footer / empty-visible path).
pub(crate) fn omitted_rollback_line(n: usize) -> String {
    format!("{n} rolled-back matches omitted. Pass --include-rollback to show them.")
}

/// Empty human-search message. Never a bare miss when rollbacks were omitted.
pub(crate) fn format_empty_human_search(miss_line: &str, omitted_rollbacks: usize) -> String {
    if omitted_rollbacks == 0 {
        miss_line.to_string()
    } else {
        format!("{miss_line}\n{}", omitted_rollback_line(omitted_rollbacks))
    }
}

#[allow(clippy::too_many_arguments)] // include_rollback is a required extra filter (0213)
pub fn execute_ledger_search(
    query: String,
    category: Option<Category>,
    days: Option<u64>,
    breaking: bool,
    limit: usize,
    offset: usize,
    json: bool,
    include_rollback: bool,
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
            include_rollback,
        )
        .map_err(|e| miette::miette!("{}", e))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).into_diagnostic()?
        );
        return Ok(());
    }

    let omitted = if include_rollback {
        0
    } else {
        manager
            .count_rollback_matches(&query, cat_filter.as_deref(), days, breaking)
            .map_err(|e| miette::miette!("{}", e))?
    };

    if results.is_empty() {
        let miss = format!(
            "No ledger entries found matching '{}'.",
            query.if_supports_color(Stream::Stdout, |s| s.yellow())
        );
        println!("{}", format_empty_human_search(&miss, omitted));
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

    if omitted > 0 {
        println!("{}", omitted_rollback_line(omitted));
    }

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
    fn human_formatters_do_not_mutate_source_fields() {
        // T10: display helpers are pure; JSON path serializes raw entry fields
        // (full committed_at / entity / summary) without calling these truncators.
        let full_ts = "2026-07-17T11:33:25.984842800+00:00";
        let full_entity =
            "src/commands/scan.rs,src/commands/scan_pr.rs,tests/integration/scan_pr_tests.rs";
        let full_summary = "Fix review findings for 0047 engine slice with a longer summary text";
        let _ = format_committed_short(full_ts);
        let _ = truncate_display(full_entity, 36);
        let _ = truncate_display(full_summary, 48);
        // Source strings unchanged (helpers take &str).
        assert_eq!(full_ts, "2026-07-17T11:33:25.984842800+00:00");
        assert!(full_entity.len() > 36);
        assert!(full_summary.len() > 48);
        // Display forms are strictly shorter when over budget.
        assert!(format_committed_short(full_ts).len() < full_ts.len());
        assert!(truncate_display(full_entity, 36).chars().count() <= 36);
    }

    #[test]
    fn truncate_display__long__ellipsis() {
        let s = "abcdefghijklmnopqrstuvwxyz";
        let t = truncate_display(s, 10);
        assert_eq!(t, "abcdefg...");
        assert!(t.chars().count() <= 10);
    }

    #[test]
    fn truncate_display__spaced_sentence__no_partial_word() {
        let s = "The quick brown fox jumps over the lazy dog";
        let t = truncate_display(s, 20);
        assert_eq!(t, "The quick brown...");
        assert!(!t.contains("f..."));
        assert!(!t.ends_with("f..."));
    }

    #[test]
    fn truncate_display__long_token_after_early_space__min_kept() {
        let s = "New feature: Supercalifragilisticexpialidocious extra";
        let t = truncate_display(s, 48);
        assert_ne!(t, "New feature: ...");
        assert!(!t.starts_with("New feature: ..."));
        let keep = 48usize.saturating_sub(3);
        let min_kept = 8.max(keep / 2);
        let body = t.strip_suffix("...").unwrap_or(&t);
        assert!(
            body.chars().count() >= min_kept,
            "expected at least {min_kept} kept chars, got {t:?}"
        );
        assert!(t.ends_with("..."));
    }

    #[test]
    fn omitted_rollback_line__contains_honesty_tokens() {
        let line = omitted_rollback_line(3);
        assert!(line.contains("rolled-back matches omitted"));
        assert!(line.contains("--include-rollback"));
        assert!(line.contains('3'));
    }

    #[test]
    fn empty_visible_with_omitted_rollbacks_is_not_a_bare_miss() {
        let miss = "No ledger entries found matching 'q'.";
        let combined = format_empty_human_search(miss, 2);
        assert!(combined.contains("No ledger entries found"));
        assert!(combined.contains("rolled-back matches omitted"));
        assert!(combined.contains("--include-rollback"));
        assert_ne!(combined.trim(), miss);
    }

    #[test]
    fn empty_visible_with_zero_omitted_has_no_extra_line() {
        let miss = "No ledger entries found matching 'q'.";
        let combined = format_empty_human_search(miss, 0);
        assert_eq!(combined, miss);
        assert!(!combined.contains("rolled-back matches omitted"));
    }

    #[test]
    fn json_path_has_no_omitted_footer() {
        // Greppable helper tokens stay here. Production JSON stdout is pinned by
        // CLI subprocess tests in tests/integration/ledger_search_cli.rs (they
        // spawn execute_ledger_search; a Vec<String> pretty-print would still
        // pass if that path grew a human omitted footer).
        let payload =
            serde_json::to_string_pretty(&Vec::<crate::ledger::types::LedgerEntry>::new())
                .expect("json");
        assert!(payload.trim_start().starts_with('['));
        assert!(!payload.contains("rolled-back matches omitted"));
        assert!(!payload.contains("--include-rollback"));
        let human = omitted_rollback_line(1);
        assert!(human.contains("rolled-back matches omitted"));
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
