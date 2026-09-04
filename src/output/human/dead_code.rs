use crate::impact::packet::DeadCodeFinding;
use crate::output::table::{apply_table_style, resolve_table_style};
use comfy_table::{Cell, Table};
use owo_colors::{OwoColorize, Stream};

/// Honest-ceiling footer for dead-code human output (0100 Option 1 / DoD-4).
pub const DEAD_CODE_HONESTY_FOOTER: &str = "Heuristic evidence — not proof of dead code. Factors include reachability, git activity, and test coverage.";

/// Empty-state copy when no findings pass the confidence threshold.
pub const DEAD_CODE_EMPTY_STATE: &str = "No findings above threshold (heuristic analysis).";

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
                .map(|fac| format!("{:?}", fac))
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
    use std::collections::BTreeMap;

    println!(
        "\n{}",
        "Dead Code Analysis (grouped by file)".if_supports_color(Stream::Stdout, |s| s.bold())
    );

    if findings.is_empty() {
        println!("  {DEAD_CODE_EMPTY_STATE}");
        println!("  {DEAD_CODE_HONESTY_FOOTER}");
        return;
    }

    // Group by file path, computing avg confidence, symbol count, top factor.
    let mut groups: BTreeMap<String, Vec<&DeadCodeFinding>> = BTreeMap::new();
    for f in findings {
        let path = f.file_path.display().to_string();
        groups.entry(path).or_default().push(f);
    }

    // Build rows: (file, symbols, avg_confidence, top_factor)
    let mut rows: Vec<(String, usize, f64, String)> = groups
        .iter()
        .map(|(file, finds)| {
            let count = finds.len();
            let avg: f64 = finds.iter().map(|f| f.confidence).sum::<f64>() / count as f64;
            // Top factor = most common factor across symbols in this file.
            // Use BTreeMap for deterministic iteration order on ties.
            let mut factor_counts: std::collections::BTreeMap<
                &crate::impact::packet::ConfidenceFactor,
                usize,
            > = std::collections::BTreeMap::new();
            for f in finds.iter() {
                for fac in &f.factors {
                    *factor_counts.entry(fac).or_default() += 1;
                }
            }
            let top_factor = factor_counts
                .into_iter()
                .max_by_key(|(_, c)| *c)
                .map(|(fac, _)| format!("{:?}", fac))
                .unwrap_or_else(|| "Unknown".to_string());
            (file.clone(), count, avg, top_factor)
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
    table.set_header(vec!["File", "Symbols", "Avg Confidence", "Top Factor"]);

    for (file, count, avg, factor) in &rows {
        table.add_row(vec![
            Cell::new(file),
            Cell::new(count),
            Cell::new(format!("{:.0}%", avg * 100.0)),
            Cell::new(factor),
        ]);
    }
    println!("{table}");
    println!("  {DEAD_CODE_HONESTY_FOOTER}");
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
            let name = match &factor.kind {
                crate::impact::packet::ConfidenceFactor::UnreachableFromEntrypoints => {
                    "UnreachableFromEntrypoints"
                }
                crate::impact::packet::ConfidenceFactor::GitInactive { .. } => "GitInactive",
                crate::impact::packet::ConfidenceFactor::NoTestCoverage => "NoTestCoverage",
            };
            println!("    {}: {}", name, factor.description);
        }
        println!();
    }
    println!("  {DEAD_CODE_HONESTY_FOOTER}");
}
