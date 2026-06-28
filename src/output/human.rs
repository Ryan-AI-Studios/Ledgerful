use crate::exec::ExecutionResult;
use crate::impact::packet::{DeadCodeFinding, Hotspot, ImpactPacket, RiskLevel, TemporalCoupling};
use crate::observability::signal::{ObservabilitySignal, SignalSeverity};
use crate::platform::env::ExecutableStatus;
use crate::verify::plan::VerificationPlan;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, Table};
use owo_colors::OwoColorize;

pub struct DoctorReport<'a> {
    pub platform: &'a str,
    pub shell: &'a str,
    pub tools: &'a Vec<(String, ExecutableStatus)>,
    pub path_display: &'a str,
    pub path_kind: &'a str,
    pub is_wsl_mounted: bool,
    pub embedding_model_status: String,
    pub completion_model_status: String,
    pub native_graph_status: String,
    pub active_ask_backend: String,
    pub index_health: Vec<String>,
    pub target_triple: &'a str,
}

pub fn print_doctor_report(report: &DoctorReport) {
    println!("\nLedgerful Doctor - Environment Health Check");
    println!("==================================================");
    println!("{:<20} {}", "Environment:", report.platform);
    println!("{:<20} {}", "Active Shell:", report.shell);

    let family = if cfg!(windows) { "windows" } else { "unix" };
    let telemetry = format!(
        "os={}, arch={}, family={}, target_triple={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        family,
        report.target_triple
    );
    println!("{:<20} {}", "LEDGERFUL_PLATFORM:", telemetry);

    println!("\nTools:");
    for (name, status) in report.tools {
        let status_str = match status {
            ExecutableStatus::Found(p) => format!("Found ({})", p.display()),
            ExecutableStatus::NotFound => "NOT FOUND".red().to_string(),
        };
        println!("  {:<18} {}", name, status_str);
    }

    println!("\nCurrent Path:        {}", report.path_display);
    println!("Path Type:           {}", report.path_kind);
    if report.is_wsl_mounted {
        println!("WSL Support:         Active (Mounted)");
    }

    println!("\nActive Ask Backend:  {}", report.active_ask_backend);
    println!("Embedding Model:     {}", report.embedding_model_status);
    println!("Completion Model:    {}", report.completion_model_status);
    println!("Native Graph:        {}", report.native_graph_status);

    if !report.index_health.is_empty() {
        println!("\nIndex Health:");
        for health in &report.index_health {
            println!("  • {}", health);
        }
    }
}

pub fn print_scan_summary(snapshot: &crate::git::RepoSnapshot) {
    println!("\n{}", "Ledgerful Git Scan Summary".bold().underline());
    println!(
        "{:<15} {}",
        "Branch:".bold(),
        snapshot.branch_name.as_deref().unwrap_or("unknown")
    );
    println!(
        "{:<15} {}",
        "HEAD:".bold(),
        snapshot.head_hash.as_deref().unwrap_or("unknown")
    );

    let state_str = if snapshot.is_clean {
        "CLEAN".green().to_string()
    } else {
        "DIRTY".yellow().to_string()
    };
    println!("{:<15} {}", "State:".bold(), state_str);

    if !snapshot.changes.is_empty() {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let layout = crate::state::layout::Layout::new(current_dir.to_string_lossy().as_ref());
        let config = crate::config::load::load_config(&layout).unwrap_or_default();
        let ignore_set = if !config.watch.ignore_patterns.is_empty() {
            let mut builder = globset::GlobSetBuilder::new();
            for pattern in &config.watch.ignore_patterns {
                if let Ok(glob) = globset::Glob::new(pattern) {
                    builder.add(glob);
                }
            }
            builder.build().ok()
        } else {
            None
        };

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_header(vec!["State", "Action", "File Path"]);

        for change in &snapshot.changes {
            let state = if change.is_staged {
                "Staged".green().to_string()
            } else {
                "Unstaged".dimmed().to_string()
            };
            let action = match &change.change_type {
                crate::git::ChangeType::Added => "Added".green().to_string(),
                crate::git::ChangeType::Modified => "Modified".yellow().to_string(),
                crate::git::ChangeType::Deleted => "Deleted".red().to_string(),
                crate::git::ChangeType::Renamed { old_path } => {
                    format!("Renamed ({})", old_path.display())
                        .blue()
                        .to_string()
                }
            };

            let is_ignored = if let Some(ref set) = ignore_set {
                let path_str = change.path.to_string_lossy().replace('\\', "/");
                set.is_match(path_str)
            } else {
                false
            };

            let path_display = if is_ignored {
                format!("{} (ignored)", change.path.display())
                    .dimmed()
                    .to_string()
            } else {
                change.path.display().to_string()
            };

            table.add_row(vec![
                Cell::new(state),
                Cell::new(action),
                Cell::new(path_display),
            ]);
        }
        println!("{table}");
    }
}

pub fn print_impact_summary(packet: &ImpactPacket) {
    println!("\n{}", "Change Impact Analysis".bold().underline());

    let risk_color = match packet.risk_level {
        RiskLevel::High => Color::Red,
        RiskLevel::Medium => Color::Yellow,
        RiskLevel::Low => Color::Green,
    };

    let mut risk_table = Table::new();
    risk_table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .add_row(vec![
            Cell::new("OVERALL RISK"),
            Cell::new(format!("{:?}", packet.risk_level).to_uppercase()).fg(risk_color),
        ]);
    println!("{risk_table}");

    if !packet.hotspots.is_empty() {
        print_hotspots(&packet.hotspots);
    }

    if !packet.temporal_couplings.is_empty() {
        print_temporal_couplings(&packet.temporal_couplings);
    }

    if !packet.observability.is_empty() {
        print_observability_signals(&packet.observability);
    }
}

pub fn print_impact_brief(packet: &ImpactPacket) {
    let risk = format!("{:?}", packet.risk_level).to_uppercase();
    match packet.risk_level {
        RiskLevel::High => println!("Impact Analysis: Risk is {}", risk.red().bold()),
        RiskLevel::Medium => println!("Impact Analysis: Risk is {}", risk.yellow().bold()),
        RiskLevel::Low => println!("Impact Analysis: Risk is {}", risk.green().bold()),
    }
}

pub fn print_hotspots(hotspots: &[Hotspot]) {
    println!("\n{}", "Codebase Hotspots (Risk Density)".bold());
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(vec!["Rank", "Score", "Freq", "Comp", "File Path"]);

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
    println!("\n{}", "Codebase Hotspots (with Centrality)".bold());
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(vec!["Rank", "Score", "Freq", "Comp", "Cent", "File Path"]);

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
    println!("\n{}", "Semantic Hotspots (Duplicate Density)".bold());
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(vec!["Rank", "Similarity", "File 1", "File 2"]);

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

fn print_temporal_couplings(couplings: &[TemporalCoupling]) {
    println!("\n{}", "Temporal Couplings (>70% co-change)".bold());
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(vec!["Strength", "File A", "File B"]);

    for c in couplings {
        table.add_row(vec![
            Cell::new(format!("{:.0}%", c.score * 100.0)),
            Cell::new(c.file_a.display().to_string()),
            Cell::new(c.file_b.display().to_string()),
        ]);
    }
    println!("{table}");
}

fn print_observability_signals(signals: &[ObservabilitySignal]) {
    println!("\n{}", "Observability Signals".bold());
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(vec!["Source", "Severity", "Signal"]);

    for signal in signals {
        let sev = match signal.severity {
            SignalSeverity::Critical => "CRITICAL".red().to_string(),
            SignalSeverity::Warning => "WARN".yellow().to_string(),
            SignalSeverity::Normal => "NORMAL".blue().to_string(),
        };
        table.add_row(vec![
            Cell::new(signal.source.clone()),
            Cell::new(sev),
            Cell::new(signal.signal_label.clone()),
        ]);
    }
    println!("{table}");
}

pub fn print_dead_code_summary(
    findings: &[DeadCodeFinding],
    _threshold: f64,
    include_traits: bool,
) {
    println!("\n{}", "Dead Code Analysis".bold());
    if findings.is_empty() {
        println!("  No dead code found above threshold.");
    } else {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_header(vec!["Symbol", "File", "Confidence", "Factors"]);

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

    // DX4: the broad `HINT: Derived traits ...` warning was removed because
    // derive-based and standard-trait false positives are now suppressed
    // structurally (derive penalty in `dead_code::filters::derive_penalty`
    // and the `is_standard_trait` filter from CG-F6). The `--include-traits`
    // flag's own help text in `args.rs` remains as user documentation.
    let _ = include_traits;
}

pub fn print_dead_code_grouped(findings: &[DeadCodeFinding]) {
    use std::collections::BTreeMap;

    println!("\n{}", "Dead Code Analysis (grouped by file)".bold());

    if findings.is_empty() {
        println!("  No dead code found above threshold.");
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
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(vec!["File", "Symbols", "Avg Confidence", "Top Factor"]);

    for (file, count, avg, factor) in &rows {
        table.add_row(vec![
            Cell::new(file),
            Cell::new(count),
            Cell::new(format!("{:.0}%", avg * 100.0)),
            Cell::new(factor),
        ]);
    }
    println!("{table}");
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
            "\nNo dead code findings for '{}' above threshold.",
            explanation.file
        );
        return;
    }

    println!(
        "\n{}",
        format!("Dead Code Analysis: {}", explanation.file).bold()
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
}

pub fn print_verify_plan(plan: &VerificationPlan) {
    // Detect whether nextest is used from the first step's command
    let runner = plan
        .steps
        .first()
        .map(|s| {
            if s.command.contains("nextest") {
                "nextest"
            } else {
                "cargo test"
            }
        })
        .unwrap_or("cargo test");
    println!("\n{}", "Verification Plan".bold().underline());
    if let Some(source) = &plan.source {
        let source_str = match source {
            crate::verify::plan::PlanSource::AutoPolicy => "Auto-Policy",
            crate::verify::plan::PlanSource::ExplicitConfig => "Explicit Config",
            crate::verify::plan::PlanSource::HistoricalRulesFallback => {
                "Historical Rules (Auto-Policy Fallback)"
            }
            crate::verify::plan::PlanSource::Manual => "Manual",
        };
        println!("  {} {}", "Source:".dimmed(), source_str);
    }
    println!("  {} {}", "Runner:".dimmed(), runner);
    for step in &plan.steps {
        let desc = if step.description.is_empty() {
            &step.command
        } else {
            &step.description
        };
        println!("  {} {}", "•".dimmed(), desc);
    }
}

pub fn print_verify_result(name: &str, _timeout: u64, result: &ExecutionResult) {
    if result.exit_code == 0 {
        println!(
            "\n{} Verification passed for: {}",
            "SUCCESS".green().bold(),
            name
        );
    } else {
        println!(
            "\n{} Verification failed for: {}",
            "FAILURE".red().bold(),
            name
        );
    }
}
