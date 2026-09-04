use super::super::IndexArgs;
use crate::index::ProjectIndexer;
use crate::index::staleness::{
    EmptyDiscoveryDiagnostics, EmptyIndexReason, FreshnessSource, IndexFreshnessAssessment,
    IndexFreshnessState,
};
use miette::{IntoDiagnostic, Result};
use serde::Serialize;

/// Severity of a check-mode message.
/// Errors always go to stderr (including under `--json`). Info (warnings/status)
/// go to stdout in human mode and are suppressed under `--json` so success paths
/// have empty stderr for agents that merge streams (`2>&1 | ConvertFrom-Json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckMsgKind {
    Error,
    Info,
}

/// Typed verdict for `index --check`: one place decides messages and exit flags.
#[derive(Debug)]
struct CheckVerdict {
    messages: Vec<(CheckMsgKind, String)>,
    exit_missing: bool,
    exit_indeterminate: bool,
    exit_strict_stale: bool,
}

/// Collapse the is_missing / empty / stale / strict ladder into a single verdict.
/// Used by both the human and `--json` branches so messages cannot drift apart.
fn decide_check_verdict(
    status: &crate::index::orchestrator::IndexStatus,
    is_missing: bool,
    strict: bool,
) -> CheckVerdict {
    let is_empty_expected = status
        .assessment
        .as_ref()
        .map(|a| {
            matches!(
                a.state,
                IndexFreshnessState::FreshEmpty | IndexFreshnessState::StaleEmpty
            ) && matches!(
                a.empty_reason,
                Some(EmptyIndexReason::NoSupportedFiles)
                    | Some(EmptyIndexReason::AllIndexableCandidatesIgnored)
            )
        })
        .unwrap_or(false);

    let is_indeterminate = matches!(
        status.assessment.as_ref().map(|a| &a.state),
        Some(IndexFreshnessState::Indeterminate)
    );

    let mut messages = Vec::new();
    let mut exit_missing = false;
    let mut exit_indeterminate = false;
    let mut exit_strict_stale = false;

    if let Some(assessment) = &status.assessment {
        match assessment.state {
            IndexFreshnessState::FreshEmpty | IndexFreshnessState::StaleEmpty => {
                match assessment.empty_reason {
                    Some(EmptyIndexReason::NoSupportedFiles)
                    | Some(EmptyIndexReason::AllIndexableCandidatesIgnored) => {
                        messages.push((
                            CheckMsgKind::Info,
                            "Index is up to date (0 indexable files).".to_string(),
                        ));
                    }
                    Some(EmptyIndexReason::RepositoryEmpty) => {
                        messages.push((
                            CheckMsgKind::Error,
                            "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                                .to_string(),
                        ));
                        exit_missing = true;
                    }
                    _ => {
                        if is_missing {
                            messages.push((
                                CheckMsgKind::Error,
                                "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                                    .to_string(),
                            ));
                            exit_missing = true;
                        } else {
                            messages.push((CheckMsgKind::Info, "Index is up to date.".to_string()));
                        }
                    }
                }
            }
            IndexFreshnessState::Indeterminate => {
                messages.push((
                    CheckMsgKind::Error,
                    "Error: Index state is indeterminate (metadata corruption or mismatch). Run 'ledgerful index --repair-metadata' to repair."
                        .to_string(),
                ));
                exit_indeterminate = true;
            }
            IndexFreshnessState::ContentStalePopulated => {
                // Age-fresh metadata + content-hash drift (0128).
                if is_missing {
                    messages.push((
                        CheckMsgKind::Error,
                        "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                            .to_string(),
                    ));
                    exit_missing = true;
                } else if status.stale_files > 0 {
                    if strict {
                        messages.push((
                            CheckMsgKind::Error,
                            format!(
                                "Error: Index has content drift ({} files) and --strict is enabled.",
                                status.stale_files
                            ),
                        ));
                        exit_strict_stale = true;
                    } else {
                        messages.push((
                            CheckMsgKind::Info,
                            format!(
                                "Warning: Index has content drift ({} files; age-fresh metadata). Run 'ledgerful index --incremental' to update.",
                                status.stale_files
                            ),
                        ));
                    }
                } else {
                    // Defensive: ContentStale without positive top-level drift.
                    messages.push((
                        CheckMsgKind::Info,
                        "Warning: Index content may be stale. Run 'ledgerful index --incremental' to update."
                            .to_string(),
                    ));
                }
            }
            _ => {
                if is_missing {
                    messages.push((
                        CheckMsgKind::Error,
                        "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                            .to_string(),
                    ));
                    exit_missing = true;
                } else if status.stale_files > 0 {
                    if strict {
                        messages.push((
                            CheckMsgKind::Error,
                            format!(
                                "Error: Index is stale ({} files) and --strict is enabled.",
                                status.stale_files
                            ),
                        ));
                        exit_strict_stale = true;
                    } else {
                        messages.push((
                            CheckMsgKind::Info,
                            format!(
                                "Warning: Index is stale ({} files). Run 'ledgerful index --incremental' to update.",
                                status.stale_files
                            ),
                        ));
                    }
                } else {
                    messages.push((CheckMsgKind::Info, "Index is up to date.".to_string()));
                }
            }
        }
    } else {
        // Fallback if assessment is missing for some reason
        if is_missing {
            messages.push((
                CheckMsgKind::Error,
                "Error: Index is missing or empty. Run 'ledgerful index' to build it.".to_string(),
            ));
            exit_missing = true;
        } else if status.stale_files > 0 {
            if strict {
                messages.push((
                    CheckMsgKind::Error,
                    format!(
                        "Error: Index is stale ({} files) and --strict is enabled.",
                        status.stale_files
                    ),
                ));
                exit_strict_stale = true;
            } else {
                messages.push((
                    CheckMsgKind::Info,
                    format!(
                        "Warning: Index is stale ({} files). Run 'ledgerful index --incremental' to update.",
                        status.stale_files
                    ),
                ));
            }
        } else {
            messages.push((CheckMsgKind::Info, "Index is up to date.".to_string()));
        }
    }

    // Align exit flags with the process::exit sites (missing must respect empty-expected).
    if is_missing && !is_empty_expected {
        exit_missing = true;
        if !messages.iter().any(|(k, _)| *k == CheckMsgKind::Error) {
            messages.push((
                CheckMsgKind::Error,
                "Error: Index is missing or empty. Run 'ledgerful index' to build it.".to_string(),
            ));
        }
    }
    if is_indeterminate {
        exit_indeterminate = true;
        if !messages
            .iter()
            .any(|(k, m)| *k == CheckMsgKind::Error && m.contains("indeterminate"))
        {
            messages.push((
                CheckMsgKind::Error,
                "Error: Index state is indeterminate (metadata corruption or mismatch). Run 'ledgerful index --repair-metadata' to repair."
                    .to_string(),
            ));
        }
    }
    if status.stale_files > 0 && strict {
        exit_strict_stale = true;
        if !messages
            .iter()
            .any(|(k, m)| *k == CheckMsgKind::Error && m.contains("--strict"))
        {
            messages.push((
                CheckMsgKind::Error,
                format!(
                    "Error: Index is stale ({} files) and --strict is enabled.",
                    status.stale_files
                ),
            ));
        }
    }

    CheckVerdict {
        messages,
        exit_missing,
        exit_indeterminate,
        exit_strict_stale,
    }
}

fn emit_check_messages(messages: &[(CheckMsgKind, String)], json: bool) {
    for (kind, msg) in messages {
        match kind {
            // Errors always go to stderr (including under --json) so fail paths
            // keep human diagnostics; JSON is printed first before process::exit.
            CheckMsgKind::Error => eprintln!("{msg}"),
            // Under --json, suppress Info entirely so success paths have empty
            // stderr (agents often merge streams with `2>&1 | ConvertFrom-Json`).
            CheckMsgKind::Info if json => {}
            CheckMsgKind::Info => println!("{msg}"),
        }
    }
}

fn print_check_status_block(status: &crate::index::orchestrator::IndexStatus) {
    println!("Index Status:");
    println!("  Files indexed:   {}", status.total_files);
    println!("  Symbols indexed: {}", status.total_symbols);
    println!("  Stale files:     {}", status.stale_files);
    if let Some(last) = &status.last_indexed_at {
        println!("  Last indexed:    {last}");
    } else {
        println!("  Last indexed:     never");
    }
}

/// CLI DTO for `index --check --json`. Domain `IndexStatus` stays snake_case
/// for internal deserialize; field names here are camelCase, enum **values**
/// stay PascalCase (`FreshPopulated`, …).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexCheckJson {
    schema_version: u32,
    kind: &'static str,
    total_files: usize,
    total_symbols: usize,
    stale_files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_indexed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assessment: Option<IndexCheckAssessmentJson>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexCheckAssessmentJson {
    state: IndexFreshnessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    empty_reason: Option<EmptyIndexReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    empty_diagnostics: Option<IndexCheckEmptyDiagnosticsJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_indexed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    days_since_indexed: Option<u64>,
    indexed_files: usize,
    stale_files: usize,
    unindexed_files: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sample_paths: Vec<String>,
    source: FreshnessSource,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexCheckEmptyDiagnosticsJson {
    visible_files_examined: usize,
    ignored_indexable_candidates_lower_bound: usize,
    configured_exclusions_lower_bound: usize,
    scan_complete: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

impl From<&EmptyDiscoveryDiagnostics> for IndexCheckEmptyDiagnosticsJson {
    fn from(d: &EmptyDiscoveryDiagnostics) -> Self {
        Self {
            visible_files_examined: d.visible_files_examined,
            ignored_indexable_candidates_lower_bound: d.ignored_indexable_candidates_lower_bound,
            configured_exclusions_lower_bound: d.configured_exclusions_lower_bound,
            scan_complete: d.scan_complete,
            warnings: d.warnings.clone(),
        }
    }
}

impl From<&IndexFreshnessAssessment> for IndexCheckAssessmentJson {
    fn from(a: &IndexFreshnessAssessment) -> Self {
        Self {
            state: a.state.clone(),
            empty_reason: a.empty_reason.clone(),
            empty_diagnostics: a.empty_diagnostics.as_ref().map(Into::into),
            last_indexed_at: a.last_indexed_at.clone(),
            days_since_indexed: a.days_since_indexed,
            indexed_files: a.indexed_files,
            stale_files: a.stale_files,
            unindexed_files: a.unindexed_files,
            sample_paths: a.sample_paths.clone(),
            source: a.source.clone(),
            warnings: a.warnings.clone(),
        }
    }
}

fn index_check_json(status: &crate::index::orchestrator::IndexStatus) -> IndexCheckJson {
    IndexCheckJson {
        schema_version: 1,
        kind: "indexCheck",
        total_files: status.total_files,
        total_symbols: status.total_symbols,
        stale_files: status.stale_files,
        last_indexed_at: status.last_indexed_at.clone(),
        assessment: status.assessment.as_ref().map(Into::into),
    }
}

/// Check mode: report index health and staleness, exiting on missing or strict-stale.
/// Mirrors `execute_main_mode`'s `if args.json { … } else { … }` split so human
/// prose is never nested inside the JSON branch (operator-surface-policy §3).
pub(super) fn execute_check_mode(indexer: &mut ProjectIndexer, args: &IndexArgs) -> Result<()> {
    let status = indexer.check_status()?;
    let discovered = indexer.discover_files()?;
    let is_missing = status.total_files == 0 && !discovered.is_empty();

    let verdict = decide_check_verdict(&status, is_missing, args.strict);

    let will_exit = verdict.exit_missing || verdict.exit_indeterminate || verdict.exit_strict_stale;

    if args.json {
        let output = serde_json::to_string_pretty(&index_check_json(&status)).into_diagnostic()?;
        println!("{output}");
        // Info suppressed under --json; Error still on stderr. JSON already on stdout.
        emit_check_messages(&verdict.messages, true);
    } else {
        emit_check_messages(&verdict.messages, false);
        // Status block is the healthy/warning human report. On exit-1 paths keep
        // stdout empty so CI gates see diagnostics only on stderr (DoD-2).
        if !will_exit {
            print_check_status_block(&status);
        }
    }

    // Emit messages first (above); then exit so both paths diagnose before process::exit.
    if verdict.exit_missing {
        std::process::exit(1);
    }
    if verdict.exit_indeterminate {
        std::process::exit(1);
    }
    if verdict.exit_strict_stale {
        std::process::exit(1);
    }
    Ok(())
}
