use crate::commands::scan_pr::PrScanReport;
use crate::config::load::load_config;
use crate::git::RepoSnapshot;
use crate::git::diff::get_diff_summary;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::git::{ChangeType, FileChange};
use crate::output::human::print_scan_summary;
use crate::state::layout::Layout;
use crate::state::reports::{
    ScanDiffSummary, ScanReport, write_clean_tree_tombstone, write_scan_report,
};
use crate::state::storage::StorageManager;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, Table};
use globset::{Glob, GlobSetBuilder};
use miette::{IntoDiagnostic, Result};
use std::env;
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

/// Patterns that identify observability configuration files whose changes
/// should trigger automatic graph analysis in `scan --impact`.
const OBSERVABILITY_CONFIG_PATTERNS: &[&str] = &[
    "**/OpenSLO.yaml",
    "**/OpenSLO.yml",
    "**/*.openslo.yaml",
    "**/*.openslo.yml",
    "**/observability/*.yaml",
    "**/observability/*.yml",
    "**/otel-collector.yaml",
    "**/otel-collector.yml",
    "**/prometheus.yml",
    "**/prometheus.yaml",
    "**/jaeger*.yaml",
    "**/jaeger*.yml",
    "**/datadog*.yaml",
    "**/datadog*.yml",
];

/// Compile the observability config glob set. Invalid patterns are ignored and
/// logged, matching the permissive behavior of `coverage::traces`.
fn observability_config_glob_set() -> Option<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in OBSERVABILITY_CONFIG_PATTERNS {
        match Glob::new(pattern) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => {
                tracing::warn!(
                    "Invalid observability config glob pattern '{}': {}",
                    pattern,
                    e
                );
            }
        }
    }
    match builder.build() {
        Ok(set) => Some(set),
        Err(e) => {
            tracing::warn!("Failed to build observability config glob set: {}", e);
            None
        }
    }
}

/// Returns `true` if any changed path matches a known observability config
/// pattern.
fn changes_include_observability_config(changes: &[FileChange]) -> bool {
    let Some(set) = observability_config_glob_set() else {
        return false;
    };
    changes.iter().any(|change| {
        let path_str = change.path.to_string_lossy().replace('\\', "/");
        set.is_match(&path_str)
    })
}

/// Check whether the CozoDB knowledge graph is missing or stale for the current
/// repository state. "Stale" means no index has ever been run in this storage.
fn graph_is_missing_or_stale(storage: &StorageManager, threshold_days: u64) -> bool {
    crate::index::staleness::check_index_staleness(storage, threshold_days).is_some()
}

/// Run automatic graph analysis when an observability config file changed and
/// the graph is missing/stale. This prevents empty-state errors in
/// `observability diff` without requiring a manual `index --analyze-graph`.
fn maybe_auto_analyze_graph(
    changes: &[FileChange],
    storage: &StorageManager,
    project_root: &std::path::Path,
    config: &crate::config::model::Config,
) -> Result<()> {
    if !changes_include_observability_config(changes) {
        return Ok(());
    }
    if !graph_is_missing_or_stale(storage, config.index.stale_threshold_days) {
        return Ok(());
    }

    info!(
        "Auto-triggering graph analysis: observability config changed and graph is missing/stale"
    );

    // Re-open storage in write mode: `storage` may be read-only, and graph
    // analysis needs a writable CozoDB/SQLite handle.
    let db_path = Layout::new(
        camino::Utf8PathBuf::from_path_buf(project_root.to_path_buf())
            .map_err(|_| miette::miette!("Repository root is not valid UTF-8"))?
            .as_str(),
    )
    .state_subdir()
    .join("ledger.db");
    let write_storage = StorageManager::init(db_path.as_std_path())?;

    crate::index::run_graph_analysis(write_storage, project_root, config, false, false).map(|_| ())
}

/// Parse `git diff --name-status` output into `FileChange` values.
fn parse_name_status_output(stdout: &str) -> Vec<FileChange> {
    let mut changes = Vec::new();
    for line in stdout.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let status = parts.next().unwrap_or("").trim();
        let path_a = parts.next().unwrap_or("").trim();
        let path_b = parts.next().map(str::trim);

        let (change_type, path) = if status.starts_with('R') {
            // Renamed: status is R<score>, path_a=old, path_b=new
            let new_path = path_b.unwrap_or(path_a);
            (
                ChangeType::Renamed {
                    old_path: PathBuf::from(path_a),
                },
                PathBuf::from(new_path),
            )
        } else {
            let ct = match status {
                "A" => ChangeType::Added,
                "D" => ChangeType::Deleted,
                _ => ChangeType::Modified,
            };
            (ct, PathBuf::from(path_a))
        };

        changes.push(FileChange {
            path,
            change_type,
            is_staged: true,
        });
    }
    changes
}

/// Detect whether a git-diff failure is because the base commit is missing from
/// the local clone (typical shallow checkout with `fetch-depth: 1`).
fn is_missing_base_commit_error(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    lowered.contains("not a valid object name")
        || lowered.contains("unknown revision")
        || lowered.contains("bad revision")
        || lowered.contains("does not exist")
        || lowered.contains("invalid symmetric difference expression")
}

/// Format the actionable fetch-depth error.
fn missing_base_commit_error(base_ref: &str) -> miette::Error {
    miette::miette!(
        "base commit '{}' is not present in the local clone.\n       This usually means the checkout was shallow (fetch-depth: 1).\n       Fix: set `fetch-depth: 0` in your actions/checkout step, or fetch the base ref explicitly.",
        base_ref
    )
}

/// Collect changed files by running `git diff --name-status <base_ref>...HEAD`.
/// Returns a `Vec<FileChange>` with accurate `ChangeType` values per entry.
fn files_changed_since(repo_root: &std::path::Path, base_ref: &str) -> Result<Vec<FileChange>> {
    files_changed_between(repo_root, &format!("{}...HEAD", base_ref), base_ref)
}

/// Collect changed files by running `git diff --name-status <range>`.
/// `base_ref_for_errors` is used when formatting the missing-base-commit hint.
fn files_changed_between(
    repo_root: &std::path::Path,
    range: &str,
    base_ref_for_errors: &str,
) -> Result<Vec<FileChange>> {
    let output = Command::new("git")
        .args(["diff", "--name-status", range])
        .current_dir(repo_root)
        .output()
        .map_err(|e| miette::miette!("Failed to run git diff: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_missing_base_commit_error(&stderr) {
            return Err(missing_base_commit_error(base_ref_for_errors));
        }
        return Err(miette::miette!(
            "git diff --name-status {} failed: {}",
            range,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_name_status_output(&stdout))
}

/// Parse a `--pr <RANGE>` value into `(base_ref, head_ref, git_range)`.
///
/// Supports `base...head`, `base..head`, or a bare `base` (default head to
/// `HEAD`). Validates that base is non-empty. `git_range` is the normalized
/// three-dot range to pass to `git diff --name-status`.
///
/// Two-dot (`A..B`) is normalized to three-dot (`A...B`) because, in git,
/// `A..B` diffs A against B directly while `A...B` diffs merge-base(A,B)
/// against B. For PR risk assessment three-dot is always correct: two-dot
/// can include base-branch changes that are not part of the PR.
fn parse_pr_range(range: &str) -> Result<(String, String, String)> {
    let trimmed = range.trim();
    if trimmed.is_empty() {
        return Err(miette::miette!("--pr range must not be empty"));
    }

    let (base, head, normalized_git_range) = if let Some(pos) = trimmed.find("...") {
        let (base, head) = trimmed.split_at(pos);
        (base, &head[3..], trimmed.to_string())
    } else if let Some(pos) = trimmed.find("..") {
        let (base, head) = trimmed.split_at(pos);
        let head = &head[2..];
        let normalized = format!("{}...{}", base, head);
        (base, head, normalized)
    } else {
        (trimmed, "HEAD", format!("{}...HEAD", trimmed))
    };

    let base = base.trim();
    let head = head.trim();

    if base.is_empty() {
        return Err(miette::miette!(
            "--pr range '{}' has an empty base ref",
            range
        ));
    }
    if head.is_empty() {
        return Err(miette::miette!(
            "--pr range '{}' has an empty head ref",
            range
        ));
    }

    Ok((base.to_string(), head.to_string(), normalized_git_range))
}

/// Validate that `--pr` and `--impact` are not used together, and that
/// `--format` (when present with `--pr`) is one of the supported values.
fn validate_scan_args(
    pr: &Option<String>,
    format: &str,
    impact: bool,
    summary: bool,
    json: bool,
    out: &Option<PathBuf>,
) -> Result<()> {
    if pr.is_some() && impact {
        return Err(miette::miette!(
            "`--pr` and `--impact` are mutually exclusive"
        ));
    }

    if pr.is_some() && !matches!(format, "json" | "text") {
        return Err(miette::miette!(
            "unsupported --format '{}'; use 'json' or 'text'",
            format
        ));
    }

    if pr.is_none() && !impact && (summary || json || out.is_some()) {
        return Err(miette::miette!(
            "--summary, --json and --out require --impact"
        ));
    }

    Ok(())
}

pub fn execute_scan(
    run_impact: bool,
    summary: bool,
    json: bool,
    out: Option<PathBuf>,
    base_ref: Option<String>,
    pr: Option<String>,
    format: String,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    validate_scan_args(&pr, &format, run_impact, summary, json, &out)?;

    let repo = open_repo(&current_dir)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;

    // When --base-ref is provided, derive the changed file list from git diff
    // instead of from the working-tree status (which is empty in CI).
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let config = load_config(&layout).unwrap_or_default();

    let (changes, is_clean, pr_base_ref, pr_head_ref) = if let Some(ref range) = pr {
        let (base, head, git_range) = parse_pr_range(range)?;
        let all_changes = files_changed_between(&current_dir, &git_range, &base)?;
        let filtered = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            run_impact,
        )?;
        let clean = filtered.is_empty();
        (filtered, clean, Some(base), Some(head))
    } else if let Some(ref ref_str) = base_ref {
        let all_changes = files_changed_since(&current_dir, ref_str)?;
        let filtered = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            run_impact,
        )?;
        let clean = filtered.is_empty();
        (filtered, clean, None, None)
    } else {
        let all_changes = get_repo_status(&repo)?;
        let filtered = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            run_impact,
        )?;
        let clean = filtered.is_empty();
        (filtered, clean, None, None)
    };

    let snapshot = RepoSnapshot {
        head_hash,
        branch_name,
        is_clean,
        changes,
    };

    // Working-tree diffs are empty in CI when --base-ref or --pr is used; skip get_diff_summary calls.
    let mut diff_summaries = if base_ref.is_some() || pr.is_some() {
        vec![]
    } else {
        snapshot
            .changes
            .iter()
            .filter_map(|change| {
                get_diff_summary(&repo, &change.path).map(|summary| ScanDiffSummary {
                    path: change.path.to_string_lossy().to_string(),
                    summary,
                })
            })
            .collect::<Vec<_>>()
    };
    diff_summaries.sort_by(|a, b| a.path.cmp(&b.path));

    let scan_report = ScanReport::from_snapshot(&snapshot, diff_summaries);
    write_scan_report(&layout, &scan_report)?;

    if !run_impact && pr.is_none() && snapshot.is_clean {
        write_clean_tree_tombstone(
            &layout,
            snapshot.head_hash.clone(),
            snapshot.branch_name.clone(),
        )?;
    }

    let write_impact_json = json || out.is_some();

    // PR-mode output: either JSON report or human summary.
    if let (Some(base), Some(head)) = (pr_base_ref, pr_head_ref) {
        let report = PrScanReport::new(
            base,
            head,
            snapshot.head_hash.clone(),
            snapshot.branch_name.clone(),
            snapshot.is_clean,
            &snapshot.changes,
            &[],
        );

        if format == "json" {
            let json_output = serde_json::to_string_pretty(&report).into_diagnostic()?;
            if let Some(path) = out {
                std::fs::write(&path, json_output).into_diagnostic()?;
            } else {
                println!("{}", json_output);
            }
        } else {
            print_pr_scan_summary(&report);
        }
        return Ok(());
    }

    if !write_impact_json {
        print_scan_summary(&snapshot);
    }

    if run_impact {
        // Auto-trigger graph analysis when observability config files changed
        // and the graph is missing/stale, so `observability diff` can populate
        // correctly without a manual `index --analyze-graph`. Guarded by a
        // non-empty changes list so a clean tree (or a repo with no
        // `.ledgerful` state yet) never pays the storage-open cost or fails
        // just because state has not been initialized. Storage open errors are
        // treated as "skip auto-analysis" rather than aborting the scan: the
        // impact path below handles uninitialized state on its own terms, and
        // auto-analysis is strictly an optimization for the observability-diff
        // empty-state case.
        if !snapshot.changes.is_empty() {
            let root_utf8 = camino::Utf8PathBuf::from_path_buf(current_dir.clone())
                .map_err(|_| miette::miette!("Current directory is not valid UTF-8"))?;
            if let Ok(read_only_storage) = StorageManager::open_read_only(&root_utf8) {
                maybe_auto_analyze_graph(
                    &snapshot.changes,
                    &read_only_storage,
                    &current_dir,
                    &config,
                )?;
            } else {
                tracing::debug!(
                    "Skipping observability auto-analysis: storage not initialized yet"
                );
            }
        }

        // Always use the snapshot derived above so that --base-ref changes are
        // passed through regardless of whether --json / --out is set.
        let impact_packet = if base_ref.is_some() {
            crate::commands::impact::execute_impact_silent_with_snapshot(snapshot)?
        } else {
            crate::commands::impact::execute_impact_silent()?
        };

        if write_impact_json {
            let json_output = serde_json::to_string_pretty(&impact_packet).into_diagnostic()?;

            if let Some(path) = out {
                std::fs::write(&path, json_output).into_diagnostic()?;
            } else {
                println!("{}", json_output);
            }
        } else {
            crate::commands::impact::execute_impact_human(
                &impact_packet,
                summary,
                base_ref.is_some(),
            )?;
        }
    }

    Ok(())
}

/// Human-readable summary for `scan --pr --format text`.
fn print_pr_scan_summary(report: &PrScanReport) {
    use owo_colors::OwoColorize;

    println!("\n{}", "Ledgerful PR Scan Summary".bold().underline());
    println!("{:<15} {}", "Base:".bold(), report.base_ref);
    println!("{:<15} {}", "Head:".bold(), report.head_ref);
    println!(
        "{:<15} {}",
        "HEAD commit:".bold(),
        report.head_hash.as_deref().unwrap_or("<none>")
    );
    println!(
        "{:<15} {}",
        "Branch:".bold(),
        report.branch_name.as_deref().unwrap_or("<none>")
    );
    println!(
        "{:<15} {}",
        "Working tree:".bold(),
        match report.tree_clean {
            true => "CLEAN".green().to_string(),
            false => "DIRTY".yellow().to_string(),
        }
    );
    println!("{:<15} {}", "Files changed:".bold(), report.change_count);

    let risk_color = match report.risk_level {
        crate::commands::scan_pr::PrRiskLevel::Low => Color::Green,
        crate::commands::scan_pr::PrRiskLevel::Medium => Color::Yellow,
        crate::commands::scan_pr::PrRiskLevel::High => Color::Red,
    };
    let mut risk_table = Table::new();
    risk_table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .add_row(vec![
            Cell::new("PR RISK"),
            Cell::new(format!("{:?}", report.risk_level).to_uppercase()).fg(risk_color),
        ]);
    println!("{risk_table}");

    if !report.risk_reasons.is_empty() {
        println!("{}", "Risk reasons:".bold());
        for reason in &report.risk_reasons {
            println!("  • {}", reason);
        }
    }

    if !report.analysis_warnings.is_empty() {
        println!("{}", "Analysis warnings:".bold());
        for warning in &report.analysis_warnings {
            println!("  • {}", warning);
        }
    }

    if !report.changes.is_empty() {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_header(vec!["Action", "File Path"]);
        for change in &report.changes {
            let action = match change.change_type.as_str() {
                "added" => "Added".green().to_string(),
                "modified" => "Modified".yellow().to_string(),
                "deleted" => "Deleted".red().to_string(),
                "renamed" => {
                    if let Some(old) = &change.old_path {
                        format!("Renamed ({} → {})", old, change.path)
                            .blue()
                            .to_string()
                    } else {
                        "Renamed".blue().to_string()
                    }
                }
                _ => change.change_type.clone(),
            };
            table.add_row(vec![Cell::new(action), Cell::new(&change.path)]);
        }
        println!("{table}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use chrono::Utc;
    use rusqlite::Connection;

    #[test]
    fn observability_config_patterns_match_expected_files() {
        let changes = vec![
            FileChange {
                path: PathBuf::from("observability/OpenSLO.yaml"),
                change_type: ChangeType::Modified,
                is_staged: true,
            },
            FileChange {
                path: PathBuf::from("config/otel-collector.yaml"),
                change_type: ChangeType::Modified,
                is_staged: true,
            },
        ];
        assert!(changes_include_observability_config(&changes));

        let non_obs_changes = vec![FileChange {
            path: PathBuf::from("src/main.rs"),
            change_type: ChangeType::Modified,
            is_staged: true,
        }];
        assert!(!changes_include_observability_config(&non_obs_changes));
    }

    #[test]
    fn graph_staleness_detects_empty_storage() {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);

        assert!(graph_is_missing_or_stale(&storage, u64::MAX));
    }

    #[test]
    fn graph_freshness_respects_threshold() {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES (?1, ?2, ?3)",
            ("src/lib.rs", "OK", Utc::now().to_rfc3339()),
        )
        .unwrap();
        let storage = StorageManager::init_from_conn(conn);

        assert!(!graph_is_missing_or_stale(&storage, 3));
    }

    #[test]
    fn parse_pr_range_three_dot() {
        let (base, head, git_range) = parse_pr_range("main...HEAD").unwrap();
        assert_eq!(base, "main");
        assert_eq!(head, "HEAD");
        assert_eq!(git_range, "main...HEAD");
    }

    #[test]
    fn parse_pr_range_two_dot_normalizes_to_three_dot() {
        let (base, head, git_range) = parse_pr_range("main..HEAD").unwrap();
        assert_eq!(base, "main");
        assert_eq!(head, "HEAD");
        assert_eq!(git_range, "main...HEAD");
    }

    #[test]
    fn parse_pr_range_bare_base_defaults_head_to_three_dot() {
        let (base, head, git_range) = parse_pr_range("main").unwrap();
        assert_eq!(base, "main");
        assert_eq!(head, "HEAD");
        assert_eq!(git_range, "main...HEAD");
    }

    #[test]
    fn parse_pr_range_rejects_empty_base() {
        let err = parse_pr_range("...HEAD").unwrap_err().to_string();
        assert!(err.contains("empty base ref"));
    }

    #[test]
    fn parse_pr_range_rejects_empty_head() {
        let err = parse_pr_range("main..").unwrap_err().to_string();
        assert!(err.contains("empty head ref"));
    }

    #[test]
    fn parse_pr_range_rejects_empty_range() {
        let err = parse_pr_range("").unwrap_err().to_string();
        assert!(err.contains("must not be empty"));
    }

    #[test]
    fn is_missing_base_commit_error_detects_known_phrases() {
        assert!(is_missing_base_commit_error(
            "fatal: Not a valid object name main"
        ));
        assert!(is_missing_base_commit_error("unknown revision: main"));
        assert!(is_missing_base_commit_error("bad revision 'main'"));
        assert!(is_missing_base_commit_error("does not exist: 'main'"));
        assert!(!is_missing_base_commit_error("some other git failure"));
    }
}
