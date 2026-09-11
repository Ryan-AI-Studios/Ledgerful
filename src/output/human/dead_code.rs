use crate::impact::packet::{ConfidenceFactor, DeadCodeFinding};
use crate::output::table::{apply_table_style, resolve_table_style};
use comfy_table::{Cell, Table};
use owo_colors::{OwoColorize, Stream};

/// Honest-ceiling footer for dead-code human output (0100 Option 1 / DoD-4).
pub const DEAD_CODE_HONESTY_FOOTER: &str = "Heuristic evidence — not proof of dead code. Factors include reachability, git activity, and test coverage.";

/// Empty-state copy when no findings pass the confidence threshold.
pub const DEAD_CODE_EMPTY_STATE: &str = "No findings above threshold (heuristic analysis).";

/// Human-only scope line (not part of the frozen heuristic note).
pub const DEAD_CODE_SCOPE_LINE: &str = "Only Function/Method symbols are scored.";

pub fn print_dead_code_scope_line() {
    println!("  {DEAD_CODE_SCOPE_LINE}");
}

pub fn print_dead_code_omit_footer(test_paths: usize, vendor_paths: usize) {
    if test_paths > 0 {
        println!("  {test_paths} test/fixture files omitted; --include-tests");
    }
    if vendor_paths > 0 {
        println!("  {vendor_paths} vendored files omitted; --include-vendor");
    }
}

/// File-level Kind: first match Unreachable > Untested > GitInactive > Unknown.
pub(crate) fn grouped_file_kind<'a>(
    findings: impl IntoIterator<Item = &'a DeadCodeFinding>,
) -> &'static str {
    let mut unreachable = false;
    let mut untested = false;
    let mut git_inactive = false;
    for f in findings {
        for fac in &f.factors {
            match fac {
                ConfidenceFactor::UnreachableFromEntrypoints => unreachable = true,
                ConfidenceFactor::NoTestCoverage => untested = true,
                ConfidenceFactor::GitInactive { .. } => git_inactive = true,
            }
        }
    }
    if unreachable {
        "Unreachable"
    } else if untested {
        "Untested"
    } else if git_inactive {
        "GitInactive"
    } else {
        "Unknown"
    }
}

/// Human factor label for `--expand` / `--explain` (not Debug).
pub(crate) fn human_factor_label(fac: &ConfidenceFactor) -> &'static str {
    match fac {
        ConfidenceFactor::UnreachableFromEntrypoints => "Unreachable",
        ConfidenceFactor::NoTestCoverage => "Untested",
        ConfidenceFactor::GitInactive { .. } => "GitInactive",
    }
}

pub fn print_dead_code_summary(
    findings: &[DeadCodeFinding],
    _threshold: f64,
    include_traits: bool,
) {
    println!(
        "\n{}",
        "Dead Code Analysis".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    if findings.is_empty() {
        println!("  {DEAD_CODE_EMPTY_STATE}");
    } else {
        let mut table = Table::new();
        apply_table_style(&mut table, resolve_table_style());
        table.set_header(vec!["Symbol", "File", "Confidence", "Factors"]);

        for f in findings {
            let factors_str = f
                .factors
                .iter()
                .map(human_factor_label)
                .collect::<Vec<_>>()
                .join(", ");

            table.add_row(vec![
                Cell::new(f.symbol_name.clone()),
                Cell::new(f.file_path.display().to_string()),
                Cell::new(format!("{:.0}%", f.confidence * 100.0)),
                Cell::new(factors_str),
            ]);
        }
        println!("{table}");
    }
    // 0100 Option 1: honest-ceiling footer (title kept; not proof of dead code).
    println!("  {DEAD_CODE_HONESTY_FOOTER}");

    // DX4: the broad `HINT: Derived traits ...` warning was removed because
    // derive-based and standard-trait false positives are now suppressed
    // structurally (derive penalty in `dead_code::filters::derive_penalty`
    // and the `is_standard_trait` filter from CG-F6). The `--include-traits`
    // flag's own help text in `args.rs` remains as user documentation.
    let _ = include_traits;
}

pub fn print_dead_code_grouped(findings: &[DeadCodeFinding]) {
    let mut out = std::io::stdout();
    super::print_dead_code_grouped_to(&mut out, findings);
}

pub(crate) fn print_dead_code_grouped_to(
    w: &mut impl std::io::Write,
    findings: &[DeadCodeFinding],
) {
    let _ = writeln!(
        w,
        "\n{}",
        "Dead Code Analysis (grouped by file)".if_supports_color(Stream::Stdout, |s| s.bold())
    );

    if findings.is_empty() {
        let _ = writeln!(w, "  {DEAD_CODE_EMPTY_STATE}");
        let _ = writeln!(w, "  {DEAD_CODE_HONESTY_FOOTER}");
        return;
    }

    // Group by file path, computing avg confidence, symbol count, Kind.
    let mut groups: std::collections::BTreeMap<String, Vec<&DeadCodeFinding>> =
        std::collections::BTreeMap::new();
    for f in findings {
        let path = f.file_path.display().to_string();
        groups.entry(path).or_default().push(f);
    }

    // Build rows: (file, symbols, avg_confidence, kind)
    let mut rows: Vec<(String, usize, f64, &'static str)> = groups
        .iter()
        .map(|(file, finds)| {
            let count = finds.len();
            let avg: f64 = finds.iter().map(|f| f.confidence).sum::<f64>() / count as f64;
            let kind = grouped_file_kind(finds.iter().copied());
            (file.clone(), count, avg, kind)
        })
        .collect();

    // Deterministic sort: avg confidence desc, then file path asc
    rows.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["File", "Symbols", "Avg Confidence", "Kind"]);

    for (file, count, avg, kind) in &rows {
        table.add_row(vec![
            Cell::new(file),
            Cell::new(count),
            Cell::new(format!("{:.0}%", avg * 100.0)),
            Cell::new(*kind),
        ]);
    }
    let _ = writeln!(w, "{table}");
    let _ = writeln!(w, "  {DEAD_CODE_HONESTY_FOOTER}");
}

pub fn print_dead_code_explanation(findings: &[DeadCodeFinding], file_path: &str) {
    let explanation =
        crate::impact::analysis::dead_code::compute_dead_code_explanation(file_path, findings);
    print_dead_code_explanation_struct(&explanation);
}

pub fn print_dead_code_explanation_struct(
    explanation: &crate::impact::analysis::dead_code::DeadCodeExplanation,
) {
    if explanation.symbols.is_empty() {
        println!(
            "\nNo findings for '{}' above threshold (heuristic analysis).",
            explanation.file
        );
        println!("  {DEAD_CODE_HONESTY_FOOTER}");
        return;
    }

    println!(
        "\n{}",
        format!("Dead Code Analysis: {}", explanation.file)
            .if_supports_color(Stream::Stdout, |s| s.bold())
    );
    println!("\nSymbols flagged: {}\n", explanation.symbols.len());

    for symbol in &explanation.symbols {
        println!(
            "  {} ({:.0}% confidence)",
            symbol.symbol_name,
            symbol.confidence * 100.0
        );
        for factor in &symbol.factors {
            let name = human_factor_label(&factor.kind);
            println!("    {}: {}", name, factor.description);
        }
        println!();
    }
    println!("  {DEAD_CODE_HONESTY_FOOTER}");
}
