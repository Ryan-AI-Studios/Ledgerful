use std::io::IsTerminal;

use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::{ASCII_FULL, UTF8_FULL};
use comfy_table::{Cell, Color};

pub use comfy_table::Table;

/// Table border style for human (non-JSON) output.
///
/// Resolved by [`resolve_table_style`] from env + platform console capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableStyleKind {
    /// ASCII borders only (`+`, `-`, `|`) — safe on CP437 and other non-UTF consoles.
    Ascii,
    /// UTF-8 full borders with rounded corners (premium).
    Utf8,
}

const UTF8_CONSOLE_CP: u32 = 65001;

/// Pure style resolver for unit tests and production.
///
/// Priority:
/// 1. `force_ascii` wins over `force_utf8`
/// 2. Explicit `LEDGERFUL_TABLE_STYLE` (`ascii` / `utf8` / `auto`)
/// 3. Auto: on Windows, non-TTY or OutputCP ≠ 65001 → Ascii; else Utf8
///
/// `NO_COLOR` is intentionally ignored (color-only policy; 0181-D).
pub fn resolve_table_style_with(
    env_style: Option<&str>,
    force_ascii: bool,
    force_utf8: bool,
    is_windows: bool,
    stdout_is_tty: bool,
    console_output_cp: Option<u32>,
) -> TableStyleKind {
    if force_ascii {
        return TableStyleKind::Ascii;
    }
    if force_utf8 {
        return TableStyleKind::Utf8;
    }

    match env_style.map(str::trim).map(|s| s.to_ascii_lowercase()) {
        Some(ref s) if s == "ascii" => return TableStyleKind::Ascii,
        Some(ref s) if s == "utf8" || s == "utf-8" => return TableStyleKind::Utf8,
        // "auto" or unknown/missing → fall through
        _ => {}
    }

    if is_windows {
        // Prefer ASCII when piped/non-TTY (safer for CP437 log hosts) or when
        // the console output code page is not UTF-8.
        let utf8_console = matches!(console_output_cp, Some(cp) if cp == UTF8_CONSOLE_CP);
        if !stdout_is_tty || !utf8_console {
            return TableStyleKind::Ascii;
        }
    }

    TableStyleKind::Utf8
}

fn env_truthy(key: &str) -> bool {
    match std::env::var(key) {
        Ok(v) => {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        }
        Err(_) => false,
    }
}

/// Probe Windows console output code page. Non-Windows: `None`.
#[cfg(windows)]
pub fn console_output_cp() -> Option<u32> {
    // SAFETY: GetConsoleOutputCP is a simple kernel32 query; no pointers.
    Some(unsafe { windows_sys::Win32::System::Console::GetConsoleOutputCP() })
}

#[cfg(not(windows))]
pub fn console_output_cp() -> Option<u32> {
    None
}

/// Resolve table style from process env + platform (cheap; no cache — env overrides work mid-process for tests).
pub fn resolve_table_style() -> TableStyleKind {
    let env_style = std::env::var("LEDGERFUL_TABLE_STYLE").ok();
    resolve_table_style_with(
        env_style.as_deref(),
        env_truthy("LEDGERFUL_TABLE_ASCII"),
        env_truthy("LEDGERFUL_TABLE_UTF8"),
        cfg!(windows),
        std::io::stdout().is_terminal(),
        console_output_cp(),
    )
}

/// Whether human icons should use Nerd Font private-use glyphs.
///
/// False under Ascii table style so consoles without PUA fonts do not show tofu.
pub fn icons_use_nerd_glyphs() -> bool {
    resolve_table_style() == TableStyleKind::Utf8
}

/// Apply border preset for the given style (no headers).
pub fn apply_table_style(table: &mut Table, style: TableStyleKind) {
    match style {
        TableStyleKind::Ascii => {
            table.load_preset(ASCII_FULL);
            // Explicit ASCII dots — never U+2026 under Ascii (0181-I).
            table.set_truncation_indicator("...");
        }
        TableStyleKind::Utf8 => {
            table
                .load_preset(UTF8_FULL)
                .apply_modifier(UTF8_ROUND_CORNERS);
            table.set_truncation_indicator("…");
        }
    }
}

pub fn build_table(headers: impl IntoIterator<Item = impl ToString>) -> Table {
    let mut table = Table::new();
    table.set_header(
        headers
            .into_iter()
            .map(|header| header.to_string())
            .collect::<Vec<_>>(),
    );
    table
}

/// Build a premium table using the process-resolved style (B1/B2).
///
/// Contract (0181-F): style + cyan headers **only**. Callers may then set
/// width, arrangement, constraints, or truncation overrides.
pub fn build_premium_table(headers: impl IntoIterator<Item = impl ToString>) -> Table {
    build_premium_table_with_style(resolve_table_style(), headers)
}

/// Build a premium table with an explicit style (tests + hermetic callers).
pub fn build_premium_table_with_style(
    style: TableStyleKind,
    headers: impl IntoIterator<Item = impl ToString>,
) -> Table {
    let mut table = Table::new();
    apply_table_style(&mut table, style);
    table.set_header(
        headers
            .into_iter()
            .map(|header| Cell::new(header.to_string()).fg(Color::Cyan))
            .collect::<Vec<_>>(),
    );
    table
}

/// Default human table width when stdout is not a TTY (`COLUMNS` or 120).
pub fn human_table_width_fallback() -> u16 {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|&w| w >= 40)
        .unwrap_or(120)
}

/// Prepare a table for dynamic width arrangement; non-TTY uses COLUMNS|120.
pub fn prepare_width_aware_table(table: &mut Table, style: TableStyleKind) {
    use comfy_table::ContentArrangement;
    table.set_content_arrangement(ContentArrangement::Dynamic);
    if !std::io::stdout().is_terminal() {
        // 0181-E: do not rely on Table::width() when piped.
        table.force_no_tty();
        table.set_width(human_table_width_fallback());
    }
    // Re-assert truncation for style (Ascii must stay "...").
    match style {
        TableStyleKind::Ascii => {
            table.set_truncation_indicator("...");
        }
        TableStyleKind::Utf8 => {
            table.set_truncation_indicator("…");
        }
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    #[test]
    fn resolve__force_ascii__wins_over_force_utf8() {
        assert_eq!(
            resolve_table_style_with(None, true, true, true, true, Some(65001)),
            TableStyleKind::Ascii
        );
    }

    #[test]
    fn resolve__force_utf8__utf8() {
        assert_eq!(
            resolve_table_style_with(None, false, true, true, true, Some(437)),
            TableStyleKind::Utf8
        );
    }

    #[test]
    fn resolve__env_ascii__ascii() {
        assert_eq!(
            resolve_table_style_with(Some("ASCII"), false, false, false, true, None),
            TableStyleKind::Ascii
        );
    }

    #[test]
    fn resolve__env_utf8__utf8() {
        assert_eq!(
            resolve_table_style_with(Some("utf8"), false, false, true, true, Some(437)),
            TableStyleKind::Utf8
        );
    }

    #[test]
    fn resolve__auto_win_cp437_tty__ascii() {
        assert_eq!(
            resolve_table_style_with(None, false, false, true, true, Some(437)),
            TableStyleKind::Ascii
        );
    }

    #[test]
    fn resolve__auto_win_cp65001_tty__utf8() {
        assert_eq!(
            resolve_table_style_with(None, false, false, true, true, Some(65001)),
            TableStyleKind::Utf8
        );
    }

    #[test]
    fn resolve__auto_win_non_tty__ascii() {
        assert_eq!(
            resolve_table_style_with(None, false, false, true, false, Some(65001)),
            TableStyleKind::Ascii
        );
    }

    #[test]
    fn resolve__auto_non_windows__utf8() {
        assert_eq!(
            resolve_table_style_with(None, false, false, false, true, None),
            TableStyleKind::Utf8
        );
    }

    #[test]
    fn resolve__env_auto_falls_through_to_cp() {
        assert_eq!(
            resolve_table_style_with(Some("auto"), false, false, true, true, Some(437)),
            TableStyleKind::Ascii
        );
    }

    #[test]
    fn premium_table_with_style_ascii__plus_borders_no_rounded() {
        let table = build_premium_table_with_style(TableStyleKind::Ascii, ["Name", "Score"]);
        let rendered = table.to_string();
        assert!(
            rendered.contains('+'),
            "expected ASCII corner +, got:\n{rendered}"
        );
        assert!(
            !rendered.contains('╭'),
            "Ascii must not use rounded UTF-8 corner, got:\n{rendered}"
        );
        assert!(
            rendered.contains("Name"),
            "expected header content, got:\n{rendered}"
        );
    }

    #[test]
    fn premium_table_with_style_utf8__rounded_corners() {
        let table = build_premium_table_with_style(TableStyleKind::Utf8, ["Name", "Score"]);
        let rendered = table.to_string();
        assert!(
            rendered.contains('╭'),
            "expected top-left rounded corner, got:\n{rendered}"
        );
        assert!(
            rendered.contains('╮'),
            "expected top-right rounded corner, got:\n{rendered}"
        );
        assert!(
            rendered.contains('─'),
            "expected horizontal border, got:\n{rendered}"
        );
        assert!(
            rendered.contains("Name"),
            "expected header content, got:\n{rendered}"
        );
    }

    #[test]
    fn premium_table_adds_rows() {
        let mut table = build_premium_table_with_style(TableStyleKind::Ascii, ["A", "B"]);
        table.add_row(vec!["1", "2"]);
        let rendered = table.to_string();
        assert!(rendered.contains('1'));
        assert!(rendered.contains('2'));
    }

    #[test]
    fn ascii_style_truncation_indicator_is_three_dots() {
        // comfy-table only inserts the indicator when a row's max_height is hit.
        use comfy_table::{ColumnConstraint, ContentArrangement, Row, Width};
        let mut table = build_premium_table_with_style(TableStyleKind::Ascii, ["Col"]);
        apply_table_style(&mut table, TableStyleKind::Ascii);
        table
            .set_content_arrangement(ContentArrangement::Dynamic)
            .force_no_tty()
            .set_width(20);
        table.set_constraints(vec![ColumnConstraint::Absolute(Width::Fixed(10))]);
        let mut row = Row::from(vec!["abcdefghijklmnopqrstuvwxyz"]);
        row.max_height(1);
        table.add_row(row);
        let rendered = table.to_string();
        assert!(
            rendered.contains("..."),
            "expected ASCII truncation '...', got:\n{rendered}"
        );
        assert!(
            !rendered.contains('…'),
            "Ascii must not use U+2026 ellipsis, got:\n{rendered}"
        );
    }

    #[test]
    fn utf8_style_truncation_indicator_is_ellipsis() {
        use comfy_table::{ColumnConstraint, ContentArrangement, Row, Width};
        let mut table = build_premium_table_with_style(TableStyleKind::Utf8, ["Col"]);
        apply_table_style(&mut table, TableStyleKind::Utf8);
        table
            .set_content_arrangement(ContentArrangement::Dynamic)
            .force_no_tty()
            .set_width(20);
        table.set_constraints(vec![ColumnConstraint::Absolute(Width::Fixed(10))]);
        let mut row = Row::from(vec!["abcdefghijklmnopqrstuvwxyz"]);
        row.max_height(1);
        table.add_row(row);
        let rendered = table.to_string();
        assert!(
            rendered.contains('…'),
            "expected UTF-8 truncation U+2026, got:\n{rendered}"
        );
    }
}
