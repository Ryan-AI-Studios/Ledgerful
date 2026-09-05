use super::helpers::{
    are_files_trivial, extract_trailers, is_trivial_commit, is_well_formed_conventional,
    load_skip_history, parse_category_from_message, risk_from_category, save_skip_history,
};
use super::record::{
    RecordEnforceSkippedArgs, SilentRecordArgs, record_enforce_skipped, silently_record_ledger,
};
use crate::ai::intent_drafter::draft_intent;
use crate::config::model::Config;
use crate::state::layout::Layout;
use crate::ui::intent_tui::{IntentState, run_tui};
use miette::{IntoDiagnostic, Result};
use std::fs;
use std::io::IsTerminal;
use std::path::Path;

/// True when a TUI final state represents the Skip (`s`) disposition.
pub fn is_tui_skip_disposition(risk: &str, what: &str) -> bool {
    risk == "TRIVIAL" && what == "Skipped intent entry"
}

/// Adaptive / conventional / LLM / TUI capture result.
///
/// [`IntentOutcome::Abort`] is TUI Esc. Execute is the sole `eprintln!` /
/// `exit(1)` site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IntentOutcome {
    Done,
    Abort,
}

pub(super) struct IntentArgs<'a> {
    pub config: &'a Config,
    pub layout: &'a Layout,
    pub repo_root: &'a Path,
    pub msg_file: &'a Path,
    pub entity: &'a str,
    pub related_files: &'a str,
    pub staged_files: &'a [String],
    pub raw_commit_msg: &'a str,
    pub snapshot_id: Option<i64>,
}

pub(super) fn capture_intent(args: IntentArgs<'_>) -> Result<IntentOutcome> {
    let IntentArgs {
        config,
        layout,
        repo_root,
        msg_file,
        entity,
        related_files,
        staged_files,
        raw_commit_msg,
        snapshot_id,
    } = args;

    // 4. Check adaptive bypass
    let skip_history_path = layout.state_subdir().join("skip_history.json");
    let mut history = load_skip_history(&skip_history_path);

    let is_trivial = is_trivial_commit(raw_commit_msg) || are_files_trivial(staged_files);

    if history.bypass_remaining > 0 {
        if is_trivial {
            history.bypass_remaining -= 1;
            save_skip_history(&skip_history_path, &history);
            if config.gate.is_enforce() {
                // Enforce: durable SKIPPED row (coverage, never Verified).
                tracing::info!(
                    target: "cli_summary",
                    "[Ledgerful] Auto-accepting trivial commit under enforce — recording durable [SKIPPED] row."
                );
                record_enforce_skipped(RecordEnforceSkippedArgs {
                    config,
                    entity,
                    related_files,
                    raw_commit_msg,
                    why: "Adaptive trivial bypass under enforce (acknowledged non-coverage)",
                    snapshot_id,
                })?;
                return Ok(IntentOutcome::Done);
            }
            tracing::info!(target: "cli_summary", "[Ledgerful] Auto-accepting trivial commit (consecutive skips bypass).");
            return Ok(IntentOutcome::Done);
        } else {
            // Reset bypass on non-trivial commit
            history.consecutive_skips = 0;
            history.bypass_remaining = 0;
            save_skip_history(&skip_history_path, &history);
        }
    }

    // 5. Run LLM Drafter
    let drafted_what;
    let drafted_why;
    let drafted_risk;
    let drafted_related;
    let confidence;

    let is_terminal = crate::util::term::is_interactive() && std::io::stdout().is_terminal();
    let term_env = std::env::var("TERM").unwrap_or_default();
    let env_no_tui = term_env == "dumb"
        || std::env::var("LEDGERFUL_NO_TUI")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false)
        || std::env::var("LEDGERFUL_NON_INTERACTIVE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false)
        || std::env::var("NON_INTERACTIVE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false)
        || std::env::var("CI")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false)
        || std::env::var("ANTIGRAVITY_AGENT")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

    // Fast-path bypass for well-formed conventional commits
    if is_well_formed_conventional(raw_commit_msg) {
        tracing::info!(
            target: "cli_summary",
            "[Ledgerful] Well-formed conventional commit detected; skipping LLM intent drafting."
        );
        let lines: Vec<&str> = raw_commit_msg.lines().collect();
        drafted_what = lines[0].trim().to_string();
        drafted_why = lines
            .iter()
            .skip(1)
            .copied()
            .collect::<Vec<&str>>()
            .join("\n")
            .trim()
            .to_string();
        let category = parse_category_from_message(&drafted_what);
        drafted_risk = risk_from_category(category).to_string();
        drafted_related = Vec::new();
        confidence = 1.0;
    } else {
        tracing::info!(target: "cli_summary", "[Ledgerful] Drafting change intent via local LLM...");

        let spinner = if is_terminal && !env_no_tui {
            Some(crate::ui::spinner::Spinner::new(
                "Drafting change intent via local LLM...",
            ))
        } else {
            None
        };

        let draft = draft_intent(&config.local_model, repo_root).unwrap_or_default();

        if let Some(s) = spinner {
            s.finish();
        }

        // Fill defaults from git if LLM returned empty
        drafted_what = if draft.what.is_empty() {
            raw_commit_msg.lines().next().unwrap_or("").to_string()
        } else {
            draft.what
        };
        drafted_why = if draft.why.is_empty() {
            raw_commit_msg.to_string()
        } else {
            draft.why
        };
        drafted_risk = if draft.risk.is_empty() {
            if is_trivial {
                "TRIVIAL".to_string()
            } else {
                "MEDIUM".to_string()
            }
        } else {
            draft.risk
        };
        drafted_related = draft.related;
        confidence = draft.confidence;
    }

    // 6. Check if we can commit silently (confidence >= 0.85)
    let tui_allowed = config.intent.tui_enabled && is_terminal && !env_no_tui;

    if confidence >= 0.85 || !tui_allowed {
        if confidence >= 0.85 {
            tracing::info!(target: "cli_summary", "[Ledgerful] High-confidence intent drafted silently.");
        } else {
            tracing::info!(target: "cli_summary", "[Ledgerful] Non-interactive shell detected; committing silently.");
        }

        // Update commit message file if LLM refined it
        let mut final_commit_msg = raw_commit_msg.to_string();
        if confidence >= 0.85 && !drafted_what.is_empty() {
            let trailers = extract_trailers(raw_commit_msg);
            let updated_msg = if trailers.is_empty() {
                format!("{}\n\n{}", drafted_what, drafted_why)
            } else {
                format!("{}\n\n{}\n\n{}", drafted_what, drafted_why, trailers)
            };
            fs::write(msg_file, &updated_msg).into_diagnostic()?;
            final_commit_msg = updated_msg;
        }

        silently_record_ledger(SilentRecordArgs {
            config,
            entity,
            what: &drafted_what,
            why: &drafted_why,
            risk: &drafted_risk,
            related: drafted_related,
            related_files,
            raw_commit_msg: &final_commit_msg,
            snapshot_id,
            skipped: false,
        })?;

        // Reset skips
        history.consecutive_skips = 0;
        history.bypass_remaining = 0;
        save_skip_history(&skip_history_path, &history);
        return Ok(IntentOutcome::Done);
    }

    // 7. Launch TUI on low confidence
    let initial_state = IntentState::new(
        drafted_what,
        drafted_why,
        drafted_risk,
        drafted_related,
        confidence,
    );

    if let Some(final_state) = run_tui(initial_state).into_diagnostic()? {
        if is_tui_skip_disposition(&final_state.risk, &final_state.what) {
            // User hit 's' (Skip) in TUI
            history.consecutive_skips += 1;
            if history.consecutive_skips >= 2 {
                history.bypass_remaining = 2;
            }
            save_skip_history(&skip_history_path, &history);
            if config.gate.is_enforce() {
                // Enforce: durable SKIPPED row (counts as coverage, never Verified).
                tracing::info!(
                    target: "cli_summary",
                    "[Ledgerful] Intent entry skipped under enforce — recording durable [SKIPPED] row."
                );
                record_enforce_skipped(RecordEnforceSkippedArgs {
                    config,
                    entity,
                    related_files,
                    raw_commit_msg,
                    why: "TUI Skip under enforce (acknowledged non-coverage / non-material)",
                    snapshot_id,
                })?;
                return Ok(IntentOutcome::Done);
            }
            tracing::info!(target: "cli_summary", "[Ledgerful] Intent entry skipped.");
            return Ok(IntentOutcome::Done);
        } else {
            // Reset skips
            history.consecutive_skips = 0;
            history.bypass_remaining = 0;
            save_skip_history(&skip_history_path, &history);
        }

        // Update commit message file with TUI values
        let trailers = extract_trailers(raw_commit_msg);
        let updated_msg = if trailers.is_empty() {
            format!("{}\n\n{}", final_state.what, final_state.why)
        } else {
            format!(
                "{}\n\n{}\n\n{}",
                final_state.what, final_state.why, trailers
            )
        };
        fs::write(msg_file, &updated_msg).into_diagnostic()?;

        silently_record_ledger(SilentRecordArgs {
            config,
            entity,
            what: &final_state.what,
            why: &final_state.why,
            risk: &final_state.risk,
            related: final_state.related.clone(),
            related_files,
            raw_commit_msg: &updated_msg,
            snapshot_id,
            skipped: false,
        })?;

        Ok(IntentOutcome::Done)
    } else {
        Ok(IntentOutcome::Abort)
    }
}
