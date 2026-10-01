use crate::impact::packet::Hotspot;
use crate::output::table::{apply_table_style, resolve_table_style};
use comfy_table::{Cell, Table};
use owo_colors::{OwoColorize, Stream};
use std::path::Path;

/// Human table header for ln `displayScore` (0222). Not the 0–1 JSON `score`.
pub const HOTSPOT_DISPLAY_HEADER: &str = "Display";

/// Uncolored TOP HOTSPOTS row. Label is `display:` (ln), not `score:` (0–1).
/// Audit row (0471); two decimal places; not the `{:.3}` cell in `print_hotspots`, and not the `"Display"` table header.
pub(crate) fn format_audit_hotspot_line(path: &Path, display_score: f32) -> String {
    format!("  {:<40} display: {:.2}", path.display(), display_score)
}

pub fn print_hotspots(hotspots: &[Hotspot]) {
    println!(
        "\n{}",
        "Codebase Hotspots (Risk Density)".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec![
        "Rank",
        HOTSPOT_DISPLAY_HEADER,
        "Freq",
        "Comp",
        "File Path",
    ]);

    for (i, h) in hotspots.iter().enumerate() {
        table.add_row(vec![
            Cell::new((i + 1).to_string()),
            Cell::new(format!("{:.3}", h.display_score)),
            Cell::new(format!("{:.1}", h.frequency)),
            Cell::new(h.complexity.to_string()),
            Cell::new(h.path.display().to_string()),
        ]);
    }
    println!("{table}");
}

pub fn print_hotspots_table(hotspots: &[Hotspot]) {
    print_hotspots(hotspots);
}

pub fn print_hotspots_table_with_centrality(hotspots: &[Hotspot]) {
    println!(
        "\n{}",
        "Codebase Hotspots (with Centrality)".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec![
        "Rank",
        HOTSPOT_DISPLAY_HEADER,
        "Freq",
        "Comp",
        "Cent",
        "File Path",
    ]);

    for (i, h) in hotspots.iter().enumerate() {
        let cent = h
            .centrality
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".to_string());
        table.add_row(vec![
            Cell::new((i + 1).to_string()),
            Cell::new(format!("{:.3}", h.display_score)),
            Cell::new(format!("{:.1}", h.frequency)),
            Cell::new(h.complexity.to_string()),
            Cell::new(cent),
            Cell::new(h.path.display().to_string()),
        ]);
    }
    println!("{table}");
}

pub fn print_semantic_hotspots(matches: &[crate::semantic::hotspots::SemanticMatch]) {
    println!(
        "\n{}",
        "Semantic Hotspots (Duplicate Density)".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["Rank", "Similarity", "File 1", "File 2"]);

    for (i, m) in matches.iter().enumerate() {
        table.add_row(vec![
            Cell::new((i + 1).to_string()),
            Cell::new(format!("{:.3}", m.similarity)),
            Cell::new(format!("{}:{}", m.file1, m.name1)),
            Cell::new(format!("{}:{}", m.file2, m.name2)),
        ]);
    }
    println!("{table}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_audit_hotspot_line_uses_display_not_score() {
        let line = format_audit_hotspot_line(std::path::Path::new("src/lib.rs"), 3.29);
        assert!(
            line.contains("display:"),
            "hotspot unit must be display: {line}"
        );
        assert!(
            !line.contains("score:"),
            "hotspot unit must not be score: {line}"
        );
        assert!(line.contains("src/lib.rs"));
        assert!(line.contains("3.29"));
        assert!(
            line.contains("display: 3.29"),
            "two-decimal display stays 3.29: {line}"
        );
        assert!(
            !line.contains("3.290"),
            "audit row must not use three decimals: {line}"
        );
        assert_eq!(line, format!("  {:<40} display: {:.2}", "src/lib.rs", 3.29));
        assert!(
            line.starts_with("  src/lib.rs"),
            "two-space indent and path prefix: {line}"
        );
        let half = format_audit_hotspot_line(std::path::Path::new("src/lib.rs"), 0.5);
        assert!(
            half.contains("display: 0.50"),
            "0.5 renders as 0.50: {half}"
        );
        assert!(
            !half.contains("0.500"),
            "0.5 must not render as 0.500: {half}"
        );
    }
}
