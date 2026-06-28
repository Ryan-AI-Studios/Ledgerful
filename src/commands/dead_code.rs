use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::impact::analysis::dead_code::ConfidenceScorer;
use crate::impact::packet::DeadCodeFinding;
use crate::index::staleness::{check_index_staleness, warn_if_stale};
use crate::ledger::provenance::{ProvenanceAction, TokenProvenance};
use crate::ledger::transaction::TransactionManager;
use crate::ledger::types::{Category, TransactionRequest};
use crate::output::diagnostics::success_marker;
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::path::Path;

/// Trait abstracting an interactive confirmation prompt so tests can inject
/// deterministic answers without relying on real TTY input.
pub trait ConfirmPrompt: Send + Sync {
    fn ask(&self, message: &str, default: bool) -> Result<bool>;
}

/// Production prompt backed by `inquire::Confirm`.
struct InquireConfirm;

impl ConfirmPrompt for InquireConfirm {
    fn ask(&self, message: &str, default: bool) -> Result<bool> {
        use inquire::Confirm;
        let answer = Confirm::new(message)
            .with_default(default)
            .prompt()
            .map_err(|e| miette::miette!("Confirmation prompt failed: {e}"))?;
        Ok(answer)
    }
}

/// A line-removal target derived from a dead-code finding. Line numbers are
/// 1-based, inclusive, matching the `project_symbols` schema used by the
/// extractors.
#[derive(Debug, Clone)]
pub struct PruneTarget {
    pub symbol_name: String,
    pub file_path: Utf8PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub confidence: f64,
}

impl PruneTarget {
    fn provenance_line(&self) -> String {
        format!(
            "{}:{}-{} {}",
            self.file_path, self.line_start, self.line_end, self.symbol_name
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_dead_code(
    threshold: f64,
    limit: usize,
    auto_index: bool,
    include_traits: bool,
    prune: bool,
    expand: bool,
    explain: Option<String>,
) -> Result<()> {
    execute_dead_code_with_prompt(
        threshold,
        limit,
        auto_index,
        include_traits,
        prune,
        expand,
        explain,
        &InquireConfirm,
    )
}

/// Execute `dead-code` with an injectable confirmation prompt. Public so
/// integration tests can drive deterministic approval without real TTY input.
#[allow(clippy::too_many_arguments)]
pub fn execute_dead_code_with_prompt(
    threshold: f64,
    limit: usize,
    auto_index: bool,
    include_traits: bool,
    prune: bool,
    expand: bool,
    explain: Option<String>,
    prompt: &dyn ConfirmPrompt,
) -> Result<()> {
    let layout = get_layout()?;
    let mut config = load_ledger_config(&layout)?;

    let storage = crate::state::storage::StorageManager::open_read_only(&layout.root)?;
    let threshold_days = config.index.stale_threshold_days;

    let storage = if auto_index {
        crate::index::staleness::try_auto_index(storage, threshold_days)?
    } else if explain.is_none() {
        let _ = warn_if_stale(&storage, threshold_days);
        storage
    } else {
        storage
    };

    // CLI overrides
    config.dead_code.enabled = true;
    config.dead_code.confidence_threshold = threshold;

    let cozo = storage.cozo.as_ref();
    let repo_path = layout.root.as_std_path();

    let mut scorer =
        ConfidenceScorer::new(cozo, &storage, &config.dead_code, repo_path, include_traits);

    // R1: `dead-code --explain <file>` short-circuits the full-repo scan and
    // only scores symbols for the requested file. Path normalization and
    // fallback matching is handled inside `explain_file` / `get_symbols_for_file`
    // so the command layer can pass the raw user input through.
    if let Some(file_path) = explain {
        let target = Path::new(&file_path);

        // R2a: emit a stale-index warning before using the optimized KG path,
        // but only when the index has been built before. A missing index
        // (never built) is reported via the "not found" message below (R2
        // step 4) instead, so the stale banner is not misleading.
        if let Some(warning) = check_index_staleness(&storage, threshold_days as u64)
            && !warning.is_missing
        {
            eprintln!(
                "\n{} Index is stale — reachability results may be inaccurate. Run `{}` for up-to-date results.",
                "WARN".yellow().bold(),
                "ledgerful index --incremental".cyan().bold()
            );
        }
        let explanation = scorer.explain_file(target)?;
        if explanation.symbols.is_empty() {
            println!(
                "\nFile '{}' not found in the knowledge graph. Run `ledgerful index --incremental` if it was added recently.",
                file_path
            );
            return Ok(());
        }

        crate::output::human::print_dead_code_explanation_struct(&explanation);
        return Ok(());
    }

    let spinner = crate::ui::spinner::Spinner::new("Precomputing dead-code evidence...");
    scorer.precompute()?;
    spinner.finish();

    let spinner = crate::ui::spinner::Spinner::new("Scanning repository for dead code...");
    let all_findings = scorer.scan_repo(limit.max(1))?;
    spinner.finish();

    // The `--limit` flag caps the number of findings *displayed*. When
    // `--prune` is active, silently truncating the candidate list would hide
    // qualifying symbols from the prune loop (M2 from Claude cross-review), so
    // we only truncate the display copy and let the prune loop see every
    // qualifying finding the scorer returned. The scorer itself already
    // short-circuits at `limit` for performance; when pruning we ask for a
    // larger ceiling so the loop is not artificially capped by the display
    // limit.
    let mut display_findings = all_findings.clone();
    display_findings.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    display_findings.truncate(limit);

    if expand || prune {
        crate::output::human::print_dead_code_summary(&display_findings, threshold, include_traits);
    } else {
        crate::output::human::print_dead_code_grouped(&display_findings);
    }

    // `open_read_only` returns a read-only StorageManager. Pruning writes a
    // ledger transaction, so re-open storage in read/write mode when needed.
    if prune {
        // When pruning, re-scan with a larger ceiling so the display `--limit`
        // does not artificially cap the prune candidate set. The scorer
        // short-circuits at its `limit` argument, so we only pay for a second
        // scan when the first one actually hit the cap
        // (`all_findings.len() >= limit`); otherwise the first scan already
        // returned every qualifying symbol and we reuse it (perf note from
        // Claude phase_2 review).
        let original_scan_was_capped = all_findings.len() >= limit;
        let prune_findings = if original_scan_was_capped {
            let prune_limit = limit.saturating_mul(10).max(limit).min(10_000);
            let mut scorer =
                ConfidenceScorer::new(cozo, &storage, &config.dead_code, repo_path, include_traits);
            scorer.precompute()?;
            scorer.scan_repo(prune_limit)?
        } else {
            all_findings
        };

        let targets = build_prune_targets(&prune_findings, threshold, &layout.root)?;
        if targets.is_empty() {
            println!(
                "\n{} No high-confidence dead code to prune.",
                success_marker()
            );
        } else {
            let removed = run_prune_loop(prompt, targets)?;
            if !removed.is_empty() {
                let mut write_storage = crate::state::storage::StorageManager::init(
                    layout.state_subdir().join("ledger.db").as_std_path(),
                )?;
                start_prune_transaction(&mut write_storage, &removed, &layout.root)?;
                let _ = write_storage.shutdown();
            }
        }
    }

    println!(
        "\n{} Scanned repository for dead code (threshold: {:.0}%, limit: {})",
        success_marker(),
        threshold * 100.0,
        limit
    );

    Ok(())
}

/// Convert findings into deterministic prune targets. Only findings whose
/// confidence strictly exceeds the threshold are eligible, and results are
/// sorted by (file_path, line_start) to make the interactive order stable and
/// avoid editing a file twice in non-deterministic order.
///
/// `repo_root` is joined with each finding's repo-relative `file_path` so the
/// returned `PruneTarget.file_path` is an absolute path that resolves
/// correctly regardless of the caller's current working directory. The
/// previous behavior passed the repo-relative path straight to
/// `std::fs::read_to_string`, which silently edited the wrong file (or
/// missed entirely) when `ledgerful dead-code --prune` was invoked from a
/// subdirectory of the repo (H1 from Claude cross-review).
fn build_prune_targets(
    findings: &[DeadCodeFinding],
    threshold: f64,
    repo_root: &camino::Utf8Path,
) -> Result<Vec<PruneTarget>> {
    let mut targets = Vec::new();
    for finding in findings {
        // The scorer already applies the configured threshold, but the CLI
        // threshold may differ from the config value; re-apply it here to honor
        // the runtime flag exactly.
        if finding.confidence <= threshold {
            continue;
        }

        let Some(line_start) = finding.line_start() else {
            continue;
        };
        let Some(line_end) = finding.line_end() else {
            continue;
        };

        // Findings store repo-relative paths; join with repo_root so
        // remove_line_range reads the right file regardless of CWD.
        let relative = Utf8PathBuf::from_path_buf(finding.file_path.clone())
            .map_err(|_| miette::miette!("Dead-code file path is not valid UTF-8"))?;
        let absolute = repo_root.join(&relative);

        targets.push(PruneTarget {
            symbol_name: finding.symbol_name.clone(),
            file_path: absolute,
            line_start,
            line_end,
            confidence: finding.confidence,
        });
    }

    targets.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.line_start.cmp(&b.line_start))
    });

    Ok(targets)
}

/// Ask the user to confirm each target and perform the line removals. Targets
/// whose file cannot be read/written are reported but do not abort the loop.
fn run_prune_loop(
    prompt: &dyn ConfirmPrompt,
    targets: Vec<PruneTarget>,
) -> Result<Vec<PruneTarget>> {
    let mut removed = Vec::new();
    for target in targets {
        let message = format!(
            "Remove '{}' in {} (lines {}-{}, confidence {:.0}%)?",
            target.symbol_name,
            target.file_path,
            target.line_start,
            target.line_end,
            target.confidence * 100.0
        );

        let approved = prompt.ask(&message, false)?;
        if !approved {
            println!("  Skipped {}", target.symbol_name);
            continue;
        }

        match remove_line_range(&target.file_path, target.line_start, target.line_end) {
            Ok(()) => {
                println!(
                    "  Removed {} ({}:{}-{})",
                    target.symbol_name, target.file_path, target.line_start, target.line_end
                );
                removed.push(target);
            }
            Err(e) => {
                eprintln!(
                    "  Could not remove {} from {}: {e}",
                    target.symbol_name, target.file_path
                );
            }
        }
    }
    Ok(removed)
}

/// Start a PENDING ledger transaction documenting the removed symbols.
/// Uses the repo root as the entity and records per-symbol token provenance.
fn start_prune_transaction(
    storage: &mut crate::state::storage::StorageManager,
    removed: &[PruneTarget],
    root: &Utf8PathBuf,
) -> Result<()> {
    use crate::ledger::db::LedgerDb;

    let mut tx_mgr = TransactionManager::new(storage, root.as_std_path().to_path_buf(), {
        // A fresh config load is required because the earlier config is tied
        // to the repo layout, not the storage manager lifetime.
        let layout = get_layout()?;
        load_ledger_config(&layout)?
    });

    let provenance_lines: Vec<String> = removed.iter().map(|t| t.provenance_line()).collect();
    let message = format!(
        "Pruned {} dead-code symbol(s): {}",
        removed.len(),
        provenance_lines.join("; ")
    );

    let entity = root.as_str().to_string();
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Refactor,
            entity: entity.clone(),
            planned_action: Some(message.clone()),
            source: Some("dead-code --prune".to_string()),
            ..Default::default()
        })
        .map_err(|e| miette::miette!("Failed to start ledger transaction: {e}"))?;

    let db = LedgerDb::new(tx_mgr.get_connection());
    for target in removed {
        let normalized = crate::util::path::normalize_relative_path(
            root.as_std_path(),
            target.file_path.as_str(),
        )
        .map_err(|e| miette::miette!("Path normalization failed: {e}"))?;
        let prov = TokenProvenance {
            id: None,
            tx_id: tx_id.clone(),
            entity: target.file_path.as_str().to_string(),
            entity_normalized: normalized,
            symbol_name: target.symbol_name.clone(),
            symbol_type: "Unknown".to_string(),
            action: ProvenanceAction::Deleted,
        };
        db.insert_token_provenance(&prov)
            .map_err(|e| miette::miette!("Failed to record token provenance: {e}"))?;
    }

    println!(
        "\n{} Started pending ledger transaction {} documenting removed symbols.",
        success_marker(),
        tx_id
    );
    println!(
        "    Run `ledgerful ledger commit {} --summary <what> --reason <why>` after tests pass.",
        tx_id
    );

    Ok(())
}

/// Remove a 1-based inclusive line range from a UTF-8 file.
///
/// Strategy when removing the last content in a file: leave a single trailing
/// newline rather than deleting the file or leaving a module comment. This is
/// the safe default — an empty source file can confuse module discovery in
/// Rust/Cargo projects, while a file with only a comment is still a valid
/// module. A single newline is also deterministic and diff-friendly.
pub fn remove_line_range(path: &Utf8PathBuf, line_start: usize, line_end: usize) -> Result<()> {
    if line_start == 0 || line_end == 0 || line_start > line_end {
        return Err(miette::miette!(
            "Invalid line range: {}-{}",
            line_start,
            line_end
        ));
    }

    let content = std::fs::read_to_string(path.as_std_path())
        .into_diagnostic()
        .map_err(|e| miette::miette!("Failed to read {}: {e}", path))?;

    let mut lines: Vec<&str> = content.lines().collect();
    // Preserve the original line terminator style if possible. `lines()` strips
    // the newline characters, so we remember whether the file ended with one.
    let ends_with_newline = content.ends_with('\n');

    let start = line_start.saturating_sub(1);
    let end = line_end.min(lines.len());
    if start >= lines.len() || start >= end {
        return Err(miette::miette!(
            "Line range {}-{} is outside file {} ({} lines)",
            line_start,
            line_end,
            path,
            lines.len()
        ));
    }

    lines.drain(start..end);

    let new_content = if lines.is_empty() {
        // Safe default: leave exactly one trailing newline.
        "\n".to_string()
    } else {
        let mut out = lines.join("\n");
        if ends_with_newline {
            out.push('\n');
        }
        out
    };

    std::fs::write(path.as_std_path(), new_content)
        .into_diagnostic()
        .map_err(|e| miette::miette!("Failed to write {}: {e}", path))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct _AlwaysYes;
    impl ConfirmPrompt for _AlwaysYes {
        fn ask(&self, _message: &str, _default: bool) -> Result<bool> {
            Ok(true)
        }
    }

    struct _AlwaysNo;
    impl ConfirmPrompt for _AlwaysNo {
        fn ask(&self, _message: &str, _default: bool) -> Result<bool> {
            Ok(false)
        }
    }

    #[test]
    fn remove_line_range_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let file = Utf8PathBuf::from_path_buf(tmp.path().join("sample.rs")).unwrap();
        std::fs::write(&file, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();

        remove_line_range(&file, 2, 2).unwrap();
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "fn a() {}\nfn c() {}\n");
    }

    #[test]
    fn remove_line_range_preserves_no_trailing_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let file = Utf8PathBuf::from_path_buf(tmp.path().join("sample.rs")).unwrap();
        std::fs::write(&file, "fn a() {}\nfn b() {}\nfn c() {}").unwrap();

        remove_line_range(&file, 2, 2).unwrap();
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "fn a() {}\nfn c() {}");
    }

    #[test]
    fn remove_line_range_last_content_leaves_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let file = Utf8PathBuf::from_path_buf(tmp.path().join("sample.rs")).unwrap();
        std::fs::write(&file, "fn dead() {}\n").unwrap();

        remove_line_range(&file, 1, 1).unwrap();
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "\n");
    }

    #[test]
    fn remove_line_range_invalid_range_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let file = Utf8PathBuf::from_path_buf(tmp.path().join("sample.rs")).unwrap();
        std::fs::write(&file, "fn a() {}\n").unwrap();

        assert!(remove_line_range(&file, 2, 1).is_err());
        assert!(remove_line_range(&file, 5, 5).is_err());
    }
}
