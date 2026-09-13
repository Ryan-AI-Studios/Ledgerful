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
/// Priority (spec B1 first-match):
/// 1. Explicit `LEDGERFUL_TABLE_STYLE` (`ascii` / `utf8`; `auto` falls through)
/// 2. Simple flags: `LEDGERFUL_TABLE_ASCII` wins over `LEDGERFUL_TABLE_UTF8`
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
    match env_style.map(str::trim).map(|s| s.to_ascii_lowercase()) {
        Some(ref s) if s == "ascii" => return TableStyleKind::Ascii,
        Some(ref s) if s == "utf8" || s == "utf-8" => return TableStyleKind::Utf8,
        // "auto" or unknown/missing → fall through to simple flags / auto
        _ => {}
    }

    if force_ascii {
        return TableStyleKind::Ascii;
    }
    if force_utf8 {
        return TableStyleKind::Utf8;
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
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
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

/// Thin alias of [`resolve_table_style`] `== Utf8` (same predicate as
/// [`icons_use_nerd_glyphs`]). Residual human chrome uses this switch.
pub fn human_unicode_ok() -> bool {
    resolve_table_style() == TableStyleKind::Utf8
}

/// Doctor pass/fail mark. Ascii: `OK` / `FAIL`. Utf8: `✓` / `✗`.
pub fn status_mark(ok: bool) -> &'static str {
    status_mark_with_style(ok, resolve_table_style())
}

pub fn status_mark_with_style(ok: bool, style: TableStyleKind) -> &'static str {
    match (style, ok) {
        (TableStyleKind::Utf8, true) => "✓",
        (TableStyleKind::Utf8, false) => "✗",
        (TableStyleKind::Ascii, true) => "OK",
        (TableStyleKind::Ascii, false) => "FAIL",
    }
}

/// Doctor middle-dot separator. Ascii: `-`. Utf8: `·`.
pub fn middle_dot() -> &'static str {
    middle_dot_with_style(resolve_table_style())
}

pub fn middle_dot_with_style(style: TableStyleKind) -> &'static str {
    match style {
        TableStyleKind::Utf8 => "·",
        TableStyleKind::Ascii => "-",
    }
}

/// Finding / setup bullet. Ascii: `*`. Utf8: `•`.
pub fn bullet() -> &'static str {
    bullet_with_style(resolve_table_style())
}

pub fn bullet_with_style(style: TableStyleKind) -> &'static str {
    match style {
        TableStyleKind::Utf8 => "•",
        TableStyleKind::Ascii => "*",
    }
}

/// Em dash in trailers and captions. Ascii: `--`. Utf8: `—`.
pub fn em_dash() -> &'static str {
    em_dash_with_style(resolve_table_style())
}

pub fn em_dash_with_style(style: TableStyleKind) -> &'static str {
    match style {
        TableStyleKind::Utf8 => "—",
        TableStyleKind::Ascii => "--",
    }
}

/// Arrow in captions / staleness / setup. Ascii: `->`. Utf8: `→` (or `➜` at
/// the staleness call site via this helper — 0181-B maps both to `->`).
pub fn arrow() -> &'static str {
    arrow_with_style(resolve_table_style())
}

pub fn arrow_with_style(style: TableStyleKind) -> &'static str {
    match style {
        TableStyleKind::Utf8 => "→",
        TableStyleKind::Ascii => "->",
    }
}

/// Staleness stderr arrow. Utf8 keeps `➜`; Ascii is `->`.
pub fn heavy_arrow() -> &'static str {
    heavy_arrow_with_style(resolve_table_style())
}

pub fn heavy_arrow_with_style(style: TableStyleKind) -> &'static str {
    match style {
        TableStyleKind::Utf8 => "➜",
        TableStyleKind::Ascii => "->",
    }
}

/// Schema secret cell. Ascii: `Y`. Utf8: `🔒`. Non-secret stays caller `-`.
pub fn lock_mark() -> &'static str {
    lock_mark_with_style(resolve_table_style())
}

pub fn lock_mark_with_style(style: TableStyleKind) -> &'static str {
    match style {
        TableStyleKind::Utf8 => "🔒",
        TableStyleKind::Ascii => "Y",
    }
}

/// Setup git-discovery warning. Ascii: `[!]`. Utf8: `⚠`.
pub fn warning_mark() -> &'static str {
    warning_mark_with_style(resolve_table_style())
}

pub fn warning_mark_with_style(style: TableStyleKind) -> &'static str {
    match style {
        TableStyleKind::Utf8 => "⚠",
        TableStyleKind::Ascii => "[!]",
    }
}

/// Truncation ellipsis. Ascii: `...` (0181-I never U+2026). Utf8: `…`.
pub fn ellipsis() -> &'static str {
    ellipsis_with_style(resolve_table_style())
}

pub fn ellipsis_with_style(style: TableStyleKind) -> &'static str {
    match style {
        TableStyleKind::Utf8 => "…",
        TableStyleKind::Ascii => "...",
    }
}

/// Doctor “Optional Accelerators” rule. Ascii uses `=`; Utf8 keeps `─`.
pub fn doctor_section_rule(title: &str, style: TableStyleKind) -> String {
    match style {
        TableStyleKind::Utf8 => format!("── {title} ──────────────────────"),
        TableStyleKind::Ascii => format!("== {title} ======================"),
    }
}

/// Map rounded box-drawing to ASCII `+` `-` `|` under Ascii style.
pub fn asciiize_box_drawing(s: &str, style: TableStyleKind) -> String {
    if style == TableStyleKind::Utf8 {
        return s.to_string();
    }
    s.replace(['╭', '╮', '╰', '╯'], "+")
        .replace('│', "|")
        .replace('─', "-")
}

/// Char-safe truncate that appends a style-aware ellipsis.
pub fn truncate_chars(s: &str, max_len: usize, style: TableStyleKind) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(max_len).collect();
        format!("{}{}", prefix, ellipsis_with_style(style))
    }
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

/// Human duration for timings tables (Track 0330). JSON stays integer `*_ms`.
///
/// - `<1000` → `Nms`
/// - `<60s` → `N.Ns`
/// - `<60m` → `Nm Ns`
/// - else → `Nh Nm`
///
/// Do **not** change [`crate::output::verification::format_duration_compact`].
pub fn format_timing_millis(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1000;
        format!("{minutes}m {seconds}s")
    } else {
        let hours = ms / 3_600_000;
        let minutes = (ms % 3_600_000) / 60_000;
        format!("{hours}h {minutes}m")
    }
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
    fn resolve__style_utf8__beats_force_ascii() {
        // Spec B1: LEDGERFUL_TABLE_STYLE is first-match.
        assert_eq!(
            resolve_table_style_with(Some("utf8"), true, false, true, true, Some(437)),
            TableStyleKind::Utf8
        );
    }

    #[test]
    fn resolve__style_ascii__beats_force_utf8() {
        assert_eq!(
            resolve_table_style_with(Some("ascii"), false, true, false, true, None),
            TableStyleKind::Ascii
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

    const ASCII_FORBIDDEN_CHROME: &[char] = &[
        '✓', '✗', '🔒', '→', '—', '•', '·', '➜', '…', '─', '⚠', '╭', '╮', '╰', '╯',
    ];

    fn assert_no_utf8_chrome(s: &str) {
        for ch in ASCII_FORBIDDEN_CHROME {
            assert!(
                !s.contains(*ch),
                "Ascii chrome leaked {ch:?} (U+{:04X}) in {s}",
                *ch as u32
            );
        }
    }

    #[test]
    fn glyph_helpers_ascii_have_no_forbidden_codepoints() {
        let s = TableStyleKind::Ascii;
        assert_eq!(status_mark_with_style(true, s), "OK");
        assert_eq!(status_mark_with_style(false, s), "FAIL");
        assert_eq!(middle_dot_with_style(s), "-");
        assert_eq!(bullet_with_style(s), "*");
        assert_eq!(em_dash_with_style(s), "--");
        assert_eq!(arrow_with_style(s), "->");
        assert_eq!(heavy_arrow_with_style(s), "->");
        assert_eq!(lock_mark_with_style(s), "Y");
        assert_eq!(warning_mark_with_style(s), "[!]");
        assert_eq!(ellipsis_with_style(s), "...");
        let rule = doctor_section_rule("Optional Accelerators", s);
        assert!(rule.starts_with("== Optional Accelerators"));
        assert_no_utf8_chrome(status_mark_with_style(true, s));
        assert_no_utf8_chrome(status_mark_with_style(false, s));
        assert_no_utf8_chrome(middle_dot_with_style(s));
        assert_no_utf8_chrome(bullet_with_style(s));
        assert_no_utf8_chrome(em_dash_with_style(s));
        assert_no_utf8_chrome(arrow_with_style(s));
        assert_no_utf8_chrome(heavy_arrow_with_style(s));
        assert_no_utf8_chrome(lock_mark_with_style(s));
        assert_no_utf8_chrome(warning_mark_with_style(s));
        assert_no_utf8_chrome(ellipsis_with_style(s));
        assert_no_utf8_chrome(&rule);
        assert_no_utf8_chrome(&asciiize_box_drawing(
            "╭──────────────────────────────────────────────────────╮",
            s,
        ));
        assert_eq!(truncate_chars("abcdefghij", 4, s), "abcd...");
        assert!(!truncate_chars("abcdefghij", 4, s).contains('…'));
    }

    #[test]
    fn glyph_helpers_utf8_keep_today_marks() {
        let s = TableStyleKind::Utf8;
        assert_eq!(status_mark_with_style(true, s), "✓");
        assert_eq!(status_mark_with_style(false, s), "✗");
        assert_eq!(middle_dot_with_style(s), "·");
        assert_eq!(bullet_with_style(s), "•");
        assert_eq!(em_dash_with_style(s), "—");
        assert_eq!(arrow_with_style(s), "→");
        assert_eq!(heavy_arrow_with_style(s), "➜");
        assert_eq!(lock_mark_with_style(s), "🔒");
        assert_eq!(warning_mark_with_style(s), "⚠");
        assert_eq!(ellipsis_with_style(s), "…");
        assert_eq!(
            doctor_section_rule("Optional Accelerators", s),
            "── Optional Accelerators ──────────────────────"
        );
        assert_eq!(truncate_chars("abcdefghij", 4, s), "abcd…");
    }

    #[test]
    fn human_unicode_ok_matches_icons_predicate() {
        assert_eq!(human_unicode_ok(), icons_use_nerd_glyphs());
        assert_eq!(
            human_unicode_ok(),
            resolve_table_style() == TableStyleKind::Utf8
        );
    }

    #[test]
    fn truncate_chars_does_not_panic_on_multibyte() {
        let s = "αβγδε";
        let out = truncate_chars(s, 2, TableStyleKind::Ascii);
        assert_eq!(out, "αβ...");
        let intact = truncate_chars(s, 20, TableStyleKind::Ascii);
        assert_eq!(intact, s);
    }

    #[test]
    fn format_timing_millis_units() {
        assert_eq!(format_timing_millis(256), "256ms");
        assert_eq!(format_timing_millis(2200), "2.2s");
        assert_eq!(format_timing_millis(451_691), "7m 31s");
        assert_eq!(format_timing_millis(3_600_000), "1h 0m");
    }
}
