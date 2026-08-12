use crate::commands::scan_pr::{HistoryEnrichment, PrScanContext, PrScanReport};
use crate::config::load::load_config;
use crate::git::RepoSnapshot;
use crate::git::diff::get_diff_summary;
use crate::git::metadata::{DEFAULT_MAX_COMMITS, collect_path_history};
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::git::{ChangeType, FileChange};
use crate::output::human::print_scan_summary;
use crate::state::layout::Layout;
use crate::state::reports::{ScanDiffSummary, ScanReport};
use crate::state::storage::StorageManager;
use camino::Utf8Path;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, Table};
use globset::{Glob, GlobSetBuilder};
use miette::{IntoDiagnostic, Result};
use std::env;
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

/// Whether scan-report RO honesty may print on stdout (human only).
///
/// Machine paths (`--json` / `--out`) must not prefix stdout with honesty text
/// that would break pure-JSON parse (0174 review P1 / Codex P2).
pub(crate) fn should_print_scan_report_honesty(json: bool, has_out: bool) -> bool {
    !json && !has_out
}

/// Emit greppable scan-report RO honesty for human mode; log-only for machine.
fn emit_scan_report_ro_honesty(json: bool, has_out: bool) {
    if should_print_scan_report_honesty(json, has_out) {
        println!("{}", crate::state::reports::SCAN_REPORT_RO_HONESTY);
    } else {
        tracing::warn!("{}", crate::state::reports::SCAN_REPORT_RO_HONESTY);
    }
}

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
    layout: &Layout,
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
    // analysis needs a writable CozoDB/SQLite handle. Use the caller's
    // resolved layout (shared state_dir on linked worktrees) — never invent
    // Layout::new(project_root) here.
    let write_storage = StorageManager::init_with_layout(layout)?;

    crate::index::run_graph_analysis(
        write_storage,
        project_root,
        config,
        false,
        false,
        false,
        None,
        Some(layout),
    )
    .map(|_| ())
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
///
/// `pub(crate)` so `change-context` can build a base-ref `RepoSnapshot` and run
/// the in-memory impact path without calling silent-persist helpers.
///
/// Resolves `base_ref` to a commit OID first so option-like values
/// (e.g. `--output=…`) cannot be interpreted as `git diff` options.
pub(crate) fn files_changed_since(
    repo_root: &std::path::Path,
    base_ref: &str,
) -> Result<Vec<FileChange>> {
    let resolved = resolve_commit_oid(repo_root, base_ref)?;
    files_changed_between(repo_root, &format!("{resolved}...HEAD"), base_ref)
}

/// Resolve a user-supplied revision to a full commit OID.
///
/// Rejects empty / option-like values, then uses
/// `git rev-parse --verify --end-of-options <rev>^{commit}`.
pub(crate) fn resolve_commit_oid(repo_root: &std::path::Path, rev: &str) -> Result<String> {
    let rev = rev.trim();
    if rev.is_empty() {
        return Err(miette::miette!("git revision must not be empty"));
    }
    if rev.starts_with('-') {
        return Err(miette::miette!(
            "git revision must not start with '-': refused option-like ref '{rev}'"
        ));
    }
    let peel = format!("{rev}^{{commit}}");
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--end-of-options", &peel])
        .current_dir(repo_root)
        .output()
        .map_err(|e| miette::miette!("Failed to run git rev-parse: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_missing_base_commit_error(&stderr) {
            return Err(missing_base_commit_error(rev));
        }
        return Err(miette::miette!(
            "git rev-parse --verify failed for '{rev}': {}",
            stderr.trim()
        ));
    }
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if oid.is_empty() || oid.starts_with('-') {
        return Err(miette::miette!(
            "git rev-parse returned an unusable OID for '{rev}'"
        ));
    }
    Ok(oid)
}

/// Collect changed files by running `git diff --name-status <range>`.
/// `base_ref_for_errors` is used when formatting the missing-base-commit hint.
///
/// Prefer resolved commit OIDs in `range` (see [`resolve_commit_oid`]) so
/// untrusted ref strings cannot inject git options.
pub(crate) fn files_changed_between(
    repo_root: &std::path::Path,
    range: &str,
    base_ref_for_errors: &str,
) -> Result<Vec<FileChange>> {
    // Guard residual option injection if a caller passes a raw range.
    if range.trim_start().starts_with('-') {
        return Err(miette::miette!(
            "git diff range must not start with '-': refused '{range}'"
        ));
    }
    let output = Command::new("git")
        .args(["diff", "--name-status", "--end-of-options", range])
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
pub(crate) fn parse_pr_range(range: &str) -> Result<(String, String, String)> {
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

/// Validate combinations of `scan` flags.
///
/// Enforces: `--pr` is mutually exclusive with `--impact` and `--base-ref`;
/// `--format` requires `--pr`; `--summary`/`--json` are not valid with `--pr`;
/// `--out` with `--pr` requires `--format json`; `--summary` alone still requires
/// `--impact` (impact brief). Bare `--json`/`--out` without `--impact` are allowed
/// (0180 gitScan envelope) — they do **not** auto-run impact.
fn validate_scan_args(
    pr: &Option<String>,
    base_ref: &Option<String>,
    format: &Option<String>,
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

    if pr.is_some() && base_ref.is_some() {
        return Err(miette::miette!(
            "--pr and --base-ref are mutually exclusive"
        ));
    }

    // --format (any value, including "text") requires --pr. An explicit
    // `--format text` without `--pr` is rejected — it is indistinguishable
    // from the default only when the flag is absent, not when the user sets
    // it explicitly.
    if pr.is_none() && format.is_some() {
        return Err(miette::miette!("--format requires --pr"));
    }

    if let Some(fmt) = format
        && !matches!(fmt.as_str(), "json" | "text")
    {
        return Err(miette::miette!(
            "unsupported --format '{}'; use 'json' or 'text'",
            fmt
        ));
    }

    if pr.is_some() && (summary || json) {
        return Err(miette::miette!(
            "--summary and --json are not compatible with --pr; use --format json or --format text"
        ));
    }

    if pr.is_some() && out.is_some() && format.as_deref() != Some("json") {
        return Err(miette::miette!("--out with --pr requires --format json"));
    }

    // 0180-E: --summary still means impact brief — no PR --format tip here.
    if pr.is_none() && !impact && summary {
        return Err(miette::miette!(
            "--summary requires --impact (impact brief summary)"
        ));
    }

    Ok(())
}

/// Emit gitScan envelope (0180-D): `--out` → file only (no stdout); else pretty stdout.
fn emit_git_scan_json(report: &ScanReport, out: Option<&PathBuf>) -> Result<()> {
    use crate::state::reports::ScanGitJson;
    let envelope = ScanGitJson::from_report(report);
    let json_output = serde_json::to_string_pretty(&envelope).into_diagnostic()?;
    if let Some(path) = out {
        std::fs::write(path, json_output).into_diagnostic()?;
    } else {
        println!("{json_output}");
    }
    Ok(())
}

/// Reject `--blast-depth` without a path that runs impact enrichment (no silent no-op).
fn validate_blast_depth_requires_impact(
    run_impact: bool,
    pr: &Option<String>,
    blast_depth: Option<u32>,
) -> Result<()> {
    if blast_depth.is_some() && !run_impact {
        // --pr is a separate PR-scan surface and does not run impact enrichment.
        if pr.is_some() {
            return Err(miette::miette!(
                "--blast-depth is not used with --pr; use scan --impact --blast-depth N (or impact --blast-depth N)"
            ));
        }
        return Err(miette::miette!(
            "--blast-depth requires --impact (structural blast is part of impact enrichment)"
        ));
    }
    Ok(())
}

/// Scan entrypoint (default blast depth from config).
pub fn execute_scan(
    run_impact: bool,
    summary: bool,
    json: bool,
    out: Option<PathBuf>,
    base_ref: Option<String>,
    pr: Option<String>,
    format: Option<String>,
) -> Result<()> {
    execute_scan_with_opts(
        run_impact,
        summary,
        json,
        out,
        base_ref,
        pr,
        format,
        None,
        Vec::new(),
        false,
    )
}

/// Scan entrypoint with optional CLI `--blast-depth` (DoD-9 dual surface).
#[allow(clippy::too_many_arguments)]
pub fn execute_scan_with_blast_depth(
    run_impact: bool,
    summary: bool,
    json: bool,
    out: Option<PathBuf>,
    base_ref: Option<String>,
    pr: Option<String>,
    format: Option<String>,
    blast_depth: Option<u32>,
) -> Result<()> {
    execute_scan_with_opts(
        run_impact,
        summary,
        json,
        out,
        base_ref,
        pr,
        format,
        blast_depth,
        Vec::new(),
        false,
    )
}

/// Scan entrypoint with 0173 `--paths` / `--include-governance`.
#[allow(clippy::too_many_arguments)]
pub fn execute_scan_with_opts(
    run_impact: bool,
    summary: bool,
    json: bool,
    out: Option<PathBuf>,
    base_ref: Option<String>,
    pr: Option<String>,
    format: Option<String>,
    blast_depth: Option<u32>,
    paths: Vec<String>,
    include_governance: bool,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    validate_scan_args(&pr, &base_ref, &format, run_impact, summary, json, &out)?;
    validate_blast_depth_requires_impact(run_impact, &pr, blast_depth)?;

    if !paths.is_empty() {
        if !run_impact {
            return Err(miette::miette!("--paths requires --impact"));
        }
        if base_ref.is_some() {
            return Err(miette::miette!(
                "--paths and --base-ref are mutually exclusive"
            ));
        }
        if pr.is_some() {
            return Err(miette::miette!("--paths and --pr are mutually exclusive"));
        }
    }

    // open_repo first so no-repo errors keep the stable discover message
    // (MCP test_scan_no_repo / CLI). Then layout.root for repo-root path
    // resolution when invoked from a subdirectory.
    let repo = open_repo(&current_dir)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;
    let layout = crate::commands::helpers::get_layout()?;
    let config = load_config(&layout).unwrap_or_default();
    let project_root = layout.root.as_std_path();
    let work_dir = if project_root.exists() {
        project_root
    } else {
        current_dir.as_path()
    };

    let prospective = !paths.is_empty();
    let prospective_parsed = if prospective {
        Some(crate::commands::impact::parse_prospective_paths(&paths)?)
    } else {
        None
    };

    let (changes, is_clean, pr_base_ref, pr_head_ref) = if let Some(ref range) = pr {
        let (base, head, git_range) = parse_pr_range(range)?;
        let all_changes = files_changed_between(work_dir, &git_range, &base)?;
        let filtered = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            run_impact,
        )?;
        let clean = filtered.is_empty();
        (filtered, clean, Some(base), Some(head))
    } else if let Some(ref ref_str) = base_ref {
        let all_changes = files_changed_since(work_dir, ref_str)?;
        let filtered = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            run_impact,
        )?;
        let clean = filtered.is_empty();
        (filtered, clean, None, None)
    } else if let Some(ref parsed) = prospective_parsed {
        let snap = crate::commands::impact::build_prospective_snapshot(work_dir, parsed)?;
        (snap.changes, false, None, None)
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

    // PR path is intentionally index-free (0115 DoD-5): never create `.ledgerful`
    // via write_scan_report / tombstone. Soft-open for testGaps is existence-check only.
    // Prospective (--paths): also skip durable scan report write (0173-G — no
    // hypothetical clobber of latest-scan.json).
    let mut durable_scan_report: Option<ScanReport> = None;
    if pr.is_none() && !prospective {
        // Working-tree diffs are empty when --base-ref is used; skip get_diff_summary.
        let mut diff_summaries = if base_ref.is_some() {
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
        // Soft-degrade report writes under RO-class fail (0174-E) — no hard-fail.
        let scan_written = crate::state::reports::soft_write_scan_report(&layout, &scan_report)?;
        // Honesty: human only — never prefix machine stdout for --json / --out
        // (0174 review P1; impact puts honesty in analysis_warnings instead).
        if !scan_written {
            emit_scan_report_ro_honesty(json, out.is_some());
        }

        if !run_impact && snapshot.is_clean {
            let tomb_ok = crate::state::reports::soft_write_clean_tree_tombstone(
                &layout,
                snapshot.head_hash.clone(),
                snapshot.branch_name.clone(),
            )?;
            if !tomb_ok && scan_written {
                // Avoid duplicate honesty if scan report already printed it.
                emit_scan_report_ro_honesty(json, out.is_some());
            }
        }
        durable_scan_report = Some(scan_report);
    }

    // 0180: bare scan --json / --out → gitScan envelope (not auto-impact). Early
    // return avoids human summary and all impact/storage work (AI1 P2-3).
    if !run_impact && pr.is_none() && (json || out.is_some()) {
        let report =
            durable_scan_report.unwrap_or_else(|| ScanReport::from_snapshot(&snapshot, vec![]));
        emit_git_scan_json(&report, out.as_ref())?;
        return Ok(());
    }

    // write_impact_json is only impact/PR-reachable after the non-impact machine
    // early return above (0180-C).
    let write_impact_json = json || out.is_some();

    // PR-mode output: either JSON report or human summary.
    if let (Some(base), Some(head)) = (pr_base_ref, pr_head_ref) {
        // Index-free history enrichment (schema v2): churn + recency from a
        // bounded first-parent walk. No author names; see git::metadata docs.
        let history = match Utf8Path::from_path(&current_dir) {
            Some(root) => match collect_path_history(root, DEFAULT_MAX_COMMITS) {
                Ok(result) => HistoryEnrichment::from_path_history(result),
                Err(e) => {
                    tracing::warn!("PR history enrichment failed; emitting empty history: {e}");
                    HistoryEnrichment::empty()
                }
            },
            None => {
                tracing::warn!(
                    "PR history enrichment skipped: current_dir is not valid UTF-8: {}",
                    current_dir.display()
                );
                HistoryEnrichment::empty()
            }
        };
        // Soft-open test_gaps + affected_flows: existence-check only; never
        // init_with_layout (0115 / 0118).
        let test_gaps = compute_pr_scan_test_gaps(&layout, &snapshot);
        let affected_flows = compute_pr_scan_affected_flows(&layout, &snapshot);
        let report = PrScanReport::new_with_test_gaps(
            PrScanContext {
                base_ref: base,
                head_ref: head,
                head_hash: snapshot.head_hash.clone(),
                branch_name: snapshot.branch_name.clone(),
                tree_clean: snapshot.is_clean,
            },
            &snapshot.changes,
            &[], // analysisWarnings reserved — always empty (0086)
            &history,
            test_gaps,
            affected_flows,
        );

        if format.as_deref() == Some("json") {
            let json_output = serde_json::to_string_pretty(&report).into_diagnostic()?;
            if let Some(path) = out {
                std::fs::write(&path, json_output).into_diagnostic()?;
            } else {
                println!("{}", json_output);
            }
        } else {
            if report.test_gaps.unmapped_count > 0 {
                eprintln!(
                    "warning: {} changed source path(s) lack structural test mapping (see testGaps)",
                    report.test_gaps.unmapped_count
                );
            }
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
            if let Ok(read_only_storage) = StorageManager::open_read_only(&layout) {
                maybe_auto_analyze_graph(
                    &snapshot.changes,
                    &read_only_storage,
                    &current_dir,
                    &config,
                    &layout,
                )?;
            } else {
                tracing::debug!(
                    "Skipping observability auto-analysis: storage not initialized yet"
                );
            }
        }

        // Always use the snapshot derived above so that --base-ref / --paths
        // changes are passed through regardless of whether --json / --out is set.
        // Thread --blast-depth so scan --impact matches impact CLI (DoD-9).
        // Prospective (--paths): in-memory only — no latest-impact.json clobber (0173-G).
        if prospective {
            let parsed = prospective_parsed
                .clone()
                .ok_or_else(|| miette::miette!("internal: prospective paths missing"))?;
            let mut config = load_config(&layout).unwrap_or_default();
            let depth_note = crate::impact::enrichment::blast::apply_cli_blast_depth(
                &mut config.impact.blast_depth,
                config.impact.blast_depth_max,
                blast_depth,
            );
            // Soft-open when DB exists (0174-T13); prospective never writes report.
            let storage = crate::commands::impact::open_storage_for_impact(&layout)?;
            let snap = crate::commands::impact::build_prospective_snapshot(work_dir, &parsed)?;
            let mut impact_packet =
                crate::commands::impact::compute_impact_from_snapshot_in_memory_with_mode(
                    &storage,
                    &config,
                    work_dir,
                    snap,
                    include_governance,
                    "prospective",
                    parsed,
                )?;
            if let Some(note) = depth_note {
                impact_packet.analysis_warnings.push(note);
                impact_packet.analysis_warnings.sort();
                impact_packet.analysis_warnings.dedup();
            }
            let _ = storage.shutdown();

            if write_impact_json {
                let json_output = serde_json::to_string_pretty(&impact_packet).into_diagnostic()?;
                if let Some(path) = out {
                    std::fs::write(&path, json_output).into_diagnostic()?;
                } else {
                    println!("{}", json_output);
                }
            } else if summary {
                crate::output::human::print_impact_brief(&impact_packet);
                println!(
                    "\nProspective analysis (in-memory only — did not rewrite latest-impact.json)"
                );
            } else {
                crate::output::human::print_impact_summary(&impact_packet);
                println!(
                    "\nProspective analysis (in-memory only — did not rewrite latest-impact.json)"
                );
            }
            return Ok(());
        }

        let (impact_packet, report_write_outcome) = if base_ref.is_some() {
            crate::commands::impact::execute_impact_silent_with_snapshot_opts(
                snapshot,
                blast_depth,
                include_governance,
                "base_ref",
            )?
        } else {
            crate::commands::impact::execute_impact_silent_with_depth_opts(
                blast_depth,
                include_governance,
            )?
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
                report_write_outcome,
            )?;
        }
    }

    Ok(())
}

/// Soft-open structural test gaps for PR scan (0115).
///
/// - Missing `ledger.db` → `unavailable` without creating any state.
/// - Open via `open_read_only_sqlite_only` only (never `init_with_layout`).
/// - File-level path only (no `resolve_seeds`).
fn compute_pr_scan_test_gaps(
    layout: &Layout,
    snapshot: &RepoSnapshot,
) -> crate::impact::enrichment::test_gaps::TestGapsReport {
    use crate::impact::enrichment::test_gaps::{
        TestGapsOpts, TestGapsReport, compute_change_set_test_gaps_from_files,
    };

    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return TestGapsReport::unavailable();
    }

    let storage = match StorageManager::open_read_only_sqlite_only(layout) {
        Ok(s) => s,
        Err(_) => return TestGapsReport::unavailable(),
    };
    let conn = storage.get_connection();
    let paths: Vec<String> = snapshot
        .changes
        .iter()
        .map(|c| c.path.to_string_lossy().replace('\\', "/"))
        .collect();
    let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let opts = TestGapsOpts {
        head_hash: snapshot.head_hash.clone(),
    };
    compute_change_set_test_gaps_from_files(conn, &path_refs, &opts)
}

/// Soft-open affected HTTP flows for PR scan (0118).
///
/// - Missing `ledger.db` → `unavailable` without creating any state.
/// - Open via `open_read_only_sqlite_only` only (never `init_with_layout`).
/// - File-path seeds only (no symbol resolution / no blast on this path).
fn compute_pr_scan_affected_flows(
    layout: &Layout,
    snapshot: &RepoSnapshot,
) -> crate::impact::enrichment::affected_flows::AffectedFlowsReport {
    use crate::git::ChangeType;
    use crate::impact::enrichment::affected_flows::{
        AffectedFlowsOpts, AffectedFlowsReport, compute_pr_affected_flows_soft,
    };
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};

    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return AffectedFlowsReport::unavailable();
    }

    let storage = match StorageManager::open_read_only_sqlite_only(layout) {
        Ok(s) => s,
        Err(_) => return AffectedFlowsReport::unavailable(),
    };
    let conn = storage.get_connection();

    let changes: Vec<ChangedFile> = snapshot
        .changes
        .iter()
        .map(|c| {
            let (status, old_path) = match &c.change_type {
                ChangeType::Added => ("Added".to_string(), None),
                ChangeType::Modified => ("Modified".to_string(), None),
                ChangeType::Deleted => ("Deleted".to_string(), None),
                ChangeType::Renamed { old_path } => ("Renamed".to_string(), Some(old_path.clone())),
            };
            ChangedFile {
                path: c.path.clone(),
                status,
                old_path,
                is_staged: c.is_staged,
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }
        })
        .collect();

    let opts = AffectedFlowsOpts {
        head_hash: snapshot.head_hash.clone(),
    };
    // No blast on PR soft path (index-free CI default; kinds 1–3 only).
    compute_pr_affected_flows_soft(Some(conn), &changes, None, &opts)
}

/// Human-readable summary for `scan --pr --format text`.
fn print_pr_scan_summary(report: &PrScanReport) {
    use owo_colors::{OwoColorize, Stream, Style};

    println!(
        "\n{}",
        "Ledgerful PR Scan Summary"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "{:<15} {}",
        "Base:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.base_ref
    );
    println!(
        "{:<15} {}",
        "Head:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.head_ref
    );
    println!(
        "{:<15} {}",
        "HEAD commit:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.head_hash.as_deref().unwrap_or("<none>")
    );
    println!(
        "{:<15} {}",
        "Branch:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.branch_name.as_deref().unwrap_or("<none>")
    );
    println!(
        "{:<15} {}",
        "Working tree:".if_supports_color(Stream::Stdout, |s| s.bold()),
        match report.tree_clean {
            true => "CLEAN"
                .if_supports_color(Stream::Stdout, |s| s.green())
                .to_string(),
            false => "DIRTY"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
        }
    );
    println!(
        "{:<15} {}",
        "Files changed:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.change_count
    );

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
        println!(
            "{}",
            "Risk reasons:".if_supports_color(Stream::Stdout, |s| s.bold())
        );
        for reason in &report.risk_reasons {
            println!("  • {}", reason);
        }
    }

    if !report.analysis_warnings.is_empty() {
        println!(
            "{}",
            "Analysis warnings:".if_supports_color(Stream::Stdout, |s| s.bold())
        );
        for warning in &report.analysis_warnings {
            println!("  • {}", warning);
        }
    }

    println!(
        "{:<15} {} (unmapped={})",
        "Test gaps:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.test_gaps.status.as_str(),
        report.test_gaps.unmapped_count
    );
    println!(
        "{:<15} {} (flowCount={})",
        "Affected flows:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.affected_flows.status.as_str(),
        report.affected_flows.flow_count
    );

    if !report.changes.is_empty() {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_header(vec!["Action", "File Path"]);
        for change in &report.changes {
            let action = match change.change_type.as_str() {
                "added" => "Added"
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string(),
                "modified" => "Modified"
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                "deleted" => "Deleted"
                    .if_supports_color(Stream::Stdout, |s| s.red())
                    .to_string(),
                "renamed" => {
                    if let Some(old) = &change.old_path {
                        format!("Renamed ({} → {})", old, change.path)
                            .if_supports_color(Stream::Stdout, |s| s.blue())
                            .to_string()
                    } else {
                        "Renamed"
                            .if_supports_color(Stream::Stdout, |s| s.blue())
                            .to_string()
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

    /// 0174: scan RO honesty must not prefix machine stdout.
    #[test]
    fn scan_report_honesty_human_only_gate() {
        assert!(should_print_scan_report_honesty(false, false));
        assert!(!should_print_scan_report_honesty(true, false));
        assert!(!should_print_scan_report_honesty(false, true));
        assert!(!should_print_scan_report_honesty(true, true));
    }

    #[test]
    fn resolve_commit_oid_rejects_option_like_ref() {
        let err = resolve_commit_oid(std::path::Path::new("."), "--output=evil")
            .expect_err("option-like ref must fail before git option parse");
        let msg = format!("{err}");
        assert!(
            msg.contains("must not start with") || msg.contains("option-like"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_commit_oid_rejects_empty() {
        assert!(resolve_commit_oid(std::path::Path::new("."), "   ").is_err());
    }

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
    fn pr_test_gaps_unavailable_without_db_does_not_create_state() {
        use crate::impact::enrichment::test_gaps::TestGapsStatus;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        // Do NOT create .ledgerful or ledger.db
        let snapshot = RepoSnapshot {
            head_hash: Some("abc".into()),
            branch_name: Some("feature".into()),
            is_clean: false,
            changes: vec![FileChange {
                path: PathBuf::from("src/lib.rs"),
                change_type: ChangeType::Modified,
                is_staged: true,
            }],
        };
        let gaps = compute_pr_scan_test_gaps(&layout, &snapshot);
        assert_eq!(gaps.status, TestGapsStatus::Unavailable);
        // Soft-open must not create .ledgerful
        assert!(
            !layout.state_dir.exists(),
            ".ledgerful must not be created by PR soft-open"
        );
        assert!(!layout.state_subdir().join("ledger.db").exists());
    }

    #[test]
    fn pr_affected_flows_unavailable_without_db_does_not_create_state() {
        use crate::impact::enrichment::affected_flows::AffectedFlowsStatus;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let snapshot = RepoSnapshot {
            head_hash: Some("abc".into()),
            branch_name: Some("feature".into()),
            is_clean: false,
            changes: vec![FileChange {
                path: PathBuf::from("src/lib.rs"),
                change_type: ChangeType::Modified,
                is_staged: true,
            }],
        };
        let flows = compute_pr_scan_affected_flows(&layout, &snapshot);
        assert_eq!(flows.status, AffectedFlowsStatus::Unavailable);
        assert!(
            !layout.state_dir.exists(),
            ".ledgerful must not be created by PR soft-open"
        );
        assert!(!layout.state_subdir().join("ledger.db").exists());
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

    #[test]
    fn blast_depth_requires_impact_flag() {
        // Silent no-op banned (codex R1 P2 / 0106 DoD-9).
        let err = validate_blast_depth_requires_impact(false, &None, Some(2))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--impact"),
            "expected require --impact, got {err}"
        );
        assert!(validate_blast_depth_requires_impact(true, &None, Some(2)).is_ok());
        assert!(validate_blast_depth_requires_impact(false, &None, None).is_ok());
        let pr_err =
            validate_blast_depth_requires_impact(false, &Some("main...HEAD".into()), Some(2))
                .unwrap_err()
                .to_string();
        assert!(
            pr_err.contains("--pr") || pr_err.contains("impact"),
            "expected pr rejection, got {pr_err}"
        );
    }

    #[test]
    fn json_out_ok_without_impact_summary_still_requires_impact() {
        // 0180: bare --json / --out allowed (gitScan); --summary still requires --impact.
        assert!(
            validate_scan_args(&None, &None, &None, false, false, true, &None).is_ok(),
            "json without impact must be allowed (gitScan)"
        );
        assert!(
            validate_scan_args(
                &None,
                &None,
                &None,
                false,
                false,
                false,
                &Some(std::path::PathBuf::from("out.json"))
            )
            .is_ok(),
            "out without impact must be allowed (gitScan file)"
        );
        let summary_err = validate_scan_args(&None, &None, &None, false, true, false, &None)
            .unwrap_err()
            .to_string();
        assert!(
            summary_err.contains("--summary") && summary_err.contains("--impact"),
            "expected summary requires impact, got {summary_err}"
        );
        assert!(
            !summary_err.contains("--format json") && !summary_err.contains("scan --pr"),
            "summary reject must not tip PR format, got {summary_err}"
        );
        assert!(
            validate_scan_args(&None, &None, &None, true, false, true, &None).is_ok(),
            "json with impact must be allowed"
        );
    }

    #[test]
    fn scan_git_json_envelope_keys() {
        use crate::state::reports::{ScanGitJson, ScanReport};
        let report = ScanReport {
            head_hash: Some("abc".into()),
            branch_name: Some("main".into()),
            is_clean: true,
            changes: vec![],
            diff_summaries: vec![],
        };
        let env = ScanGitJson::from_report(&report);
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["kind"], "gitScan");
        assert_eq!(v["isClean"], true);
        assert!(v["changes"].as_array().unwrap().is_empty());
        assert!(v["diffSummaries"].as_array().unwrap().is_empty());
    }

    /// Mirrors scan --impact --paths prospective branch: in-memory only, no
    /// `write_impact_report` / `write_scan_report` clobber (0173-G).
    #[test]
    fn scan_prospective_impact_path_does_not_clobber_latest_impact() {
        use crate::commands::impact::{
            build_prospective_snapshot, compute_impact_from_snapshot_in_memory_with_mode,
            parse_prospective_paths,
        };
        use crate::state::reports::{
            LATEST_IMPACT_REPORT, LATEST_SCAN_REPORT, ScanReport, write_impact_report,
            write_scan_report,
        };
        use crate::state::storage::StorageManager;
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(root)
            .output()
            .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/exists.rs"), "fn x() {}").unwrap();
        fs::write(root.join("README.md"), "hi").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .unwrap();

        let utf8 = camino::Utf8Path::from_path(root).unwrap();
        let layout = Layout::new(utf8);
        layout.ensure_state_dir().unwrap();
        let seed = crate::impact::packet::ImpactPacket {
            schema_version: "v1".to_string(),
            head_hash: Some("SEED_MARKER_0173_SCAN".to_string()),
            risk_reasons: vec!["seed-scan-do-not-clobber".to_string()],
            ..Default::default()
        };
        write_impact_report(&layout, &seed).unwrap();
        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let before = fs::read_to_string(report_path.as_std_path()).unwrap();
        assert!(before.contains("SEED_MARKER_0173_SCAN"));

        // Seed latest-scan.json with a marker; prospective must not clobber it.
        let scan_seed = ScanReport::from_snapshot(
            &RepoSnapshot {
                head_hash: Some("SEED_SCAN_0173".into()),
                branch_name: Some("main".into()),
                is_clean: true,
                changes: vec![],
            },
            vec![],
        );
        write_scan_report(&layout, &scan_seed).unwrap();
        let scan_path = layout.reports_dir().join(LATEST_SCAN_REPORT);
        let scan_before = fs::read_to_string(scan_path.as_std_path()).unwrap();
        assert!(scan_before.contains("SEED_SCAN_0173"));

        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        let config = crate::config::model::Config::default();
        let parsed = parse_prospective_paths(&["src/exists.rs".into()]).unwrap();
        let snap = build_prospective_snapshot(root, &parsed).unwrap();
        // Same SoT as scan.rs prospective branch (no write_impact_report).
        let packet = compute_impact_from_snapshot_in_memory_with_mode(
            &storage,
            &config,
            root,
            snap,
            false,
            "prospective",
            parsed,
        )
        .unwrap();
        assert_eq!(packet.analysis_mode, "prospective");
        assert!(!packet.changes.is_empty());

        let after = fs::read_to_string(report_path.as_std_path()).unwrap();
        assert_eq!(
            before, after,
            "scan prospective path must not rewrite latest-impact.json"
        );
        // Policy: prospective skips write_scan_report (execute_scan_with_opts).
        // Assert seed still present after in-memory compute (no accidental write helper).
        let scan_after = fs::read_to_string(scan_path.as_std_path()).unwrap();
        assert_eq!(
            scan_before, scan_after,
            "scan prospective path must not rewrite latest-scan.json"
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn prospective_snapshot_roots_paths_at_repo_root_not_cwd_subdir() {
        use crate::commands::impact::{build_prospective_snapshot, parse_prospective_paths};
        use crate::git::ChangeType;
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(root)
            .output()
            .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("src/exists.rs"), "fn x() {}").unwrap();
        fs::write(root.join("README.md"), "hi").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .unwrap();

        let parsed = parse_prospective_paths(&["src/exists.rs".into()]).unwrap();
        // Resolve against repo root even if a subdir exists (caller must pass root).
        let snap = build_prospective_snapshot(root, &parsed).unwrap();
        assert_eq!(snap.changes.len(), 1);
        assert_eq!(snap.changes[0].change_type, ChangeType::Modified);
        // Wrong root (nested subdir) would mark the same path as Added/missing.
        let wrong = build_prospective_snapshot(&root.join("nested"), &parsed).unwrap();
        assert_eq!(wrong.changes[0].change_type, ChangeType::Added);
    }
}
