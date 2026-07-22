use crate::ai::intent_drafter::draft_intent;
use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::commands::hook_sidecar::{
    CODE_INTENT_NEVER_UNDER_ENFORCE, CODE_PROMOTE_ORPHAN, GcContext, RECOVER_HINT, editmsg_hash,
    hash_message, head_message_hash, is_gc_eligible, write_pending_sidecar,
};
use crate::config::model::Config;
use crate::ledger::crypto::sign_ledger_entry;
use crate::ledger::{Category, TransactionManager, TransactionRequest};
use crate::state::storage::StorageManager;
use crate::ui::intent_tui::{IntentState, run_tui};
use miette::{IntoDiagnostic, Result};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

// Re-export for tests / historical imports.
pub use crate::commands::hook_sidecar::PendingHookTx;

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct SkipHistory {
    consecutive_skips: u32,
    bypass_remaining: u32,
}

pub fn extract_trailers(msg: &str) -> String {
    let lines: Vec<&str> = msg.lines().collect();
    let mut trailer_lines = Vec::new();
    let mut in_trailer_block = true;

    for line in lines.iter().rev() {
        if line.trim().is_empty() {
            // Hit the blank line preceding the trailer block
            break;
        }

        if !in_trailer_block {
            break;
        }

        if let Some(pos) = line.find(':') {
            let token = line[..pos].trim();
            // Git trailers are typically Alphanumeric and dashes, e.g., Signed-off-by, Co-authored-by
            if !token.is_empty()
                && !token.contains(' ')
                && token.chars().all(|c| c.is_alphanumeric() || c == '-')
            {
                trailer_lines.push(*line);
            } else {
                // Not a valid trailer token format, meaning this isn't a true trailer block
                trailer_lines.clear();
                in_trailer_block = false;
            }
        } else {
            // No colon, not a trailer block
            trailer_lines.clear();
            in_trailer_block = false;
        }
    }
    trailer_lines.reverse();
    trailer_lines.join("\n")
}

pub fn canonical_entity(files: &[String]) -> String {
    if files.is_empty() {
        return "unknown".to_string();
    }
    if files.len() == 1 {
        return files[0].clone();
    }

    // Try to find a common directory prefix
    let mut common_prefix = PathBuf::new();
    let first_path = Path::new(&files[0]);

    for component in first_path.components() {
        let next_prefix = common_prefix.join(component);
        let all_match = files.iter().all(|f| Path::new(f).starts_with(&next_prefix));
        if all_match {
            common_prefix = next_prefix;
        } else {
            break;
        }
    }

    let prefix_str = common_prefix.to_string_lossy().to_string();
    if !prefix_str.is_empty() && prefix_str != "." && prefix_str != "/" && prefix_str != "\\" {
        prefix_str.replace("\\", "/")
    } else {
        format!("{} (+{} more)", files[0], files.len() - 1)
    }
}

pub fn execute_hook_commit_msg(msg_file: &Path) -> Result<()> {
    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;

    // 1. intent.required=never under enforce → hard-fail + doctor CRITICAL code
    if config.intent.required == "never" {
        if config.gate.is_enforce() {
            return Err(miette::miette!(
                "[{}] intent.required=never is not allowed under gate mode enforce. \
                 Set intent.required to \"always\" or switch gate mode to observe. \
                 Doctor will flag this as CRITICAL.",
                CODE_INTENT_NEVER_UNDER_ENFORCE
            ));
        }
        return Ok(());
    }

    let repo_root = layout.root.as_std_path();

    // Proactive GC: clean up true-stale sidecars only (shared policy with 0035/0074).
    // Promote-failed and HEAD-matching orphans are GC-ineligible.
    let sidecar_path = layout.state_subdir().join("pending_hook_tx");
    if sidecar_path.exists() {
        match fs::read_to_string(&sidecar_path) {
            Ok(content) => match serde_json::from_str::<PendingHookTx>(&content) {
                Ok(pending) => {
                    let head_hash = head_message_hash(repo_root);
                    let edit_hash = editmsg_hash(repo_root);
                    let ctx = GcContext {
                        head_msg_hash: head_hash.as_deref(),
                        editmsg_hash: edit_hash.as_deref(),
                    };

                    let matches_editmsg = edit_hash
                        .as_deref()
                        .is_some_and(|h| h == pending.commit_msg_hash);
                    let matches_head = head_hash
                        .as_deref()
                        .is_some_and(|h| h == pending.commit_msg_hash);
                    let promote_failed = pending.is_promote_failed();

                    if matches_editmsg && !promote_failed {
                        // Sidecar matches the active commit-msg (amend/re-run). Keep it.
                        return Ok(());
                    }

                    if promote_failed || matches_head {
                        // Orphan: previous commit succeeded but promote failed/skipped.
                        let detail = if promote_failed {
                            pending.promote_error.as_deref().unwrap_or("promote failed")
                        } else {
                            "HEAD-matching pending without successful promote"
                        };
                        if config.gate.is_enforce() {
                            return Err(miette::miette!(
                                "[{}] Promote orphan retained (tx {}): {}. \
                                 Next commit blocked under enforce until recovery. \
                                 Recover with: {}",
                                CODE_PROMOTE_ORPHAN,
                                pending.tx_id,
                                detail,
                                RECOVER_HINT
                            ));
                        }
                        eprintln!(
                            "[Ledgerful] WARNING [{}]: promote orphan retained (tx {}): {}. \
                             Recover with: {}",
                            CODE_PROMOTE_ORPHAN, pending.tx_id, detail, RECOVER_HINT
                        );
                        tracing::warn!(
                            target: "cli_summary",
                            "[Ledgerful] WARNING [{}]: promote orphan (tx {}): {}",
                            CODE_PROMOTE_ORPHAN,
                            pending.tx_id,
                            detail
                        );
                        // Observe: do not GC; continue so the new commit can proceed with a banner.
                        // Still block writing a second concurrent sidecar — hard-fail only under enforce.
                        // Under observe we return Ok early so we don't clobber the orphan.
                        return Ok(());
                    }

                    if is_gc_eligible(&pending, &ctx) {
                        tracing::warn!(
                            "Found stale pending sidecar (does not match HEAD). Rolling back pending transaction and cleaning up."
                        );

                        let db_path = layout.state_subdir().join("ledger.db");
                        match StorageManager::init(db_path.as_std_path()) {
                            Ok(mut storage) => {
                                let mut tx_mgr = TransactionManager::new(
                                    &mut storage,
                                    layout.root.clone().into(),
                                    config.clone(),
                                );
                                if let Err(e) = tx_mgr.rollback_change(
                                    pending.tx_id.clone(),
                                    "Stale sidecar cleaned up by commit-msg hook".to_string(),
                                ) {
                                    return Err(miette::miette!(
                                        "Failed to rollback stale pending transaction {}: {}",
                                        pending.tx_id,
                                        e
                                    ));
                                }
                            }
                            Err(e) => {
                                return Err(miette::miette!(
                                    "Failed to initialize storage for sidecar rollback: {}",
                                    e
                                ));
                            }
                        }

                        if let Err(e) = fs::remove_file(&sidecar_path) {
                            tracing::warn!("Failed to remove stale sidecar file: {}", e);
                        }
                    }
                }
                Err(e) => {
                    // Unparseable sidecars are still removable (cannot recover).
                    tracing::warn!("Failed to parse pending hook sidecar for GC: {}", e);
                    if let Err(e) = fs::remove_file(&sidecar_path) {
                        tracing::warn!("Failed to remove unparseable sidecar file: {}", e);
                    }
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read pending hook sidecar for GC: {}", e);
                if let Err(e) = fs::remove_file(&sidecar_path) {
                    tracing::warn!("Failed to remove unreadable sidecar file: {}", e);
                }
            }
        }
    }

    // 2. Read git staged files and capture a snapshot so the post-commit hook
    // can attach per-file diff stats later.
    let staged_files = get_staged_files(repo_root);
    if staged_files.is_empty() {
        return Ok(()); // Nothing staged, nothing to record
    }
    let entity = canonical_entity(&staged_files);
    let related_files = staged_files.join(", ");

    // Capture snapshot for diff stats. This is best-effort; failure is logged
    // but the commit-msg hook continues.
    let snapshot_capture = capture_staged_snapshot(&layout, repo_root);

    // 3. Read current commit message
    if !msg_file.exists() {
        return Err(miette::miette!(
            "Commit message file does not exist at '{}'",
            msg_file.display()
        ));
    }
    let raw_commit_msg = fs::read_to_string(msg_file)
        .into_diagnostic()?
        .trim()
        .to_string();

    // 4. Check adaptive bypass
    let skip_history_path = layout.state_subdir().join("skip_history.json");
    let mut history = load_skip_history(&skip_history_path);

    let is_trivial = is_trivial_commit(&raw_commit_msg) || are_files_trivial(&staged_files);

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
                    config: &config,
                    entity: &entity,
                    related_files: &related_files,
                    raw_commit_msg: &raw_commit_msg,
                    why: "Adaptive trivial bypass under enforce (acknowledged non-coverage)",
                    snapshot_id: snapshot_capture.as_ref().map(|s| s.snapshot_id),
                })?;
                return Ok(());
            }
            tracing::info!(target: "cli_summary", "[Ledgerful] Auto-accepting trivial commit (consecutive skips bypass).");
            return Ok(());
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
    if is_well_formed_conventional(&raw_commit_msg) {
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
            raw_commit_msg.clone()
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
        let mut final_commit_msg = raw_commit_msg.clone();
        if confidence >= 0.85 && !drafted_what.is_empty() {
            let trailers = extract_trailers(&raw_commit_msg);
            let updated_msg = if trailers.is_empty() {
                format!("{}\n\n{}", drafted_what, drafted_why)
            } else {
                format!("{}\n\n{}\n\n{}", drafted_what, drafted_why, trailers)
            };
            fs::write(msg_file, &updated_msg).into_diagnostic()?;
            final_commit_msg = updated_msg;
        }

        silently_record_ledger(SilentRecordArgs {
            config: &config,
            entity: &entity,
            what: &drafted_what,
            why: &drafted_why,
            risk: &drafted_risk,
            related: drafted_related,
            related_files: &related_files,
            raw_commit_msg: &final_commit_msg,
            snapshot_id: snapshot_capture.as_ref().map(|s| s.snapshot_id),
            skipped: false,
        })?;

        // Reset skips
        history.consecutive_skips = 0;
        history.bypass_remaining = 0;
        save_skip_history(&skip_history_path, &history);
        return Ok(());
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
                    config: &config,
                    entity: &entity,
                    related_files: &related_files,
                    raw_commit_msg: &raw_commit_msg,
                    why: "TUI Skip under enforce (acknowledged non-coverage / non-material)",
                    snapshot_id: snapshot_capture.as_ref().map(|s| s.snapshot_id),
                })?;
                return Ok(());
            }
            tracing::info!(target: "cli_summary", "[Ledgerful] Intent entry skipped.");
            return Ok(());
        } else {
            // Reset skips
            history.consecutive_skips = 0;
            history.bypass_remaining = 0;
            save_skip_history(&skip_history_path, &history);
        }

        // Update commit message file with TUI values
        let trailers = extract_trailers(&raw_commit_msg);
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
            config: &config,
            entity: &entity,
            what: &final_state.what,
            why: &final_state.why,
            risk: &final_state.risk,
            related: final_state.related.clone(),
            related_files: &related_files,
            raw_commit_msg: &updated_msg,
            snapshot_id: snapshot_capture.as_ref().map(|s| s.snapshot_id),
            skipped: false,
        })?;

        Ok(())
    } else {
        // User hit Esc (Abort)
        eprintln!("[Ledgerful] Transaction aborted. Commit blocked.");
        std::process::exit(1);
    }
}

struct SilentRecordArgs<'a> {
    config: &'a Config,
    entity: &'a str,
    what: &'a str,
    why: &'a str,
    risk: &'a str,
    related: Vec<String>,
    related_files: &'a str,
    raw_commit_msg: &'a str,
    snapshot_id: Option<i64>,
    /// When true, summary is already `[SKIPPED]`-prefixed; observed=false under enforce.
    skipped: bool,
}

/// Risk for durable `[SKIPPED]` coverage rows under enforce.
///
/// Non-TRIVIAL so post-commit promote sets `verification_status = Unverified`
/// (phase0: SKIPPED is never Verified and never silent None-as-green).
pub const SKIPPED_COVERAGE_RISK: &str = "MEDIUM";

/// Prefix for durable skip coverage summaries.
pub const SKIPPED_SUMMARY_PREFIX: &str = "[SKIPPED]";

/// Build a durable SKIPPED summary line from a commit subject.
pub fn skipped_coverage_summary(subject_line: &str) -> String {
    format!("{SKIPPED_SUMMARY_PREFIX} {subject_line}")
}

/// True when a TUI final state represents the Skip (`s`) disposition.
pub fn is_tui_skip_disposition(risk: &str, what: &str) -> bool {
    risk == "TRIVIAL" && what == "Skipped intent entry"
}

pub(crate) struct RecordEnforceSkippedArgs<'a> {
    pub config: &'a Config,
    pub entity: &'a str,
    pub related_files: &'a str,
    pub raw_commit_msg: &'a str,
    pub why: &'a str,
    pub snapshot_id: Option<i64>,
}

/// Write a durable PENDING + `[SKIPPED]` sidecar under enforce.
///
/// Shared by adaptive trivial bypass and TUI Skip so both paths produce the
/// same coverage model (CHORE category, MEDIUM risk → Unverified on promote).
pub(crate) fn record_enforce_skipped(args: RecordEnforceSkippedArgs<'_>) -> Result<()> {
    let subject = args
        .raw_commit_msg
        .lines()
        .next()
        .unwrap_or("skipped")
        .trim();
    silently_record_ledger(SilentRecordArgs {
        config: args.config,
        entity: args.entity,
        what: &skipped_coverage_summary(subject),
        why: args.why,
        risk: SKIPPED_COVERAGE_RISK,
        related: Vec::new(),
        related_files: args.related_files,
        raw_commit_msg: args.raw_commit_msg,
        snapshot_id: args.snapshot_id,
        skipped: true,
    })
}

fn silently_record_ledger(args: SilentRecordArgs) -> Result<()> {
    let layout = get_layout()?;
    let category = if args.skipped {
        Category::Chore
    } else {
        parse_category_from_message(args.what)
    };
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let mut tx_mgr = TransactionManager::new(
        &mut storage,
        layout.root.clone().into(),
        args.config.clone(),
    );

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category,
            entity: args.entity.to_string(),
            planned_action: Some(args.what.to_string()),
            ..Default::default()
        })
        .map_err(|e| miette::miette!("{}", e))?;

    let observe_warned = tx_mgr.observe_warned();

    let committed_at = chrono::Utc::now().to_rfc3339();

    let sign_result = sign_ledger_entry(
        &tx_id,
        &category.to_string(),
        args.what,
        args.why,
        &committed_at,
    );
    let (signature, pub_key) = match sign_result {
        Ok(keys) => keys,
        Err(e) => {
            if args.config.intent.require_signing {
                return Err(miette::miette!(
                    "Signing failed and require_signing is true: {}",
                    e
                ));
            } else {
                tracing::warn!(
                    "Ledger entry signing failed (continuing as require_signing=false): {}",
                    e
                );
                (None, None)
            }
        }
    };

    let tickets = args.related.join(", ");
    let combined_related = if tickets.is_empty() {
        args.related_files.to_string()
    } else {
        format!("{} | {}", tickets, args.related_files)
    };

    // SKIPPED under enforce: observed false/None. Observe soft-skip does not reach here.
    let observed = if args.skipped {
        if args.config.gate.is_observe() {
            Some(true)
        } else {
            None
        }
    } else if observe_warned {
        Some(true)
    } else {
        None
    };

    let pending = PendingHookTx {
        tx_id,
        commit_msg_hash: hash_message(&crate::util::text::clean_commit_msg(args.raw_commit_msg)),
        summary: args.what.to_string(),
        reason: args.why.to_string(),
        committed_at: Some(committed_at),
        risk: Some(args.risk.to_string()),
        related_tickets: Some(combined_related),
        signature,
        public_key: pub_key,
        snapshot_id: args.snapshot_id,
        observed,
        promote_failed: None,
        promote_error: None,
    };

    let sidecar_path = layout.state_subdir().join("pending_hook_tx");
    write_pending_sidecar(sidecar_path.as_std_path(), &pending)?;

    Ok(())
}

/// Staged-snapshot capture result carried from commit-msg to post-commit via
/// the pending sidecar.
#[derive(Debug, Clone, Copy)]
struct CapturedSnapshot {
    snapshot_id: i64,
}

/// Capture a snapshot of the staged (pre-commit) working tree so the
/// post-commit hook has `changed_files` rows to attach diff stats to.
///
/// This is best-effort: the packet is persisted with `head_hash` = current
/// HEAD, and the post-commit hook recomputes stats against the new HEAD.
fn capture_staged_snapshot(
    layout: &crate::state::layout::Layout,
    repo_root: &Path,
) -> Option<CapturedSnapshot> {
    use crate::git::repo::{get_head_info, open_repo};
    use crate::git::status::get_repo_status;
    use crate::impact::orchestrator::map_snapshot_to_packet;
    use crate::state::storage::StorageManager;

    let repo = open_repo(repo_root).ok()?;
    let (head_hash, branch_name) = get_head_info(&repo).ok()?;
    let all_changes = get_repo_status(&repo).ok()?;
    let changes: Vec<_> = all_changes.into_iter().filter(|c| c.is_staged).collect();
    let is_clean = changes.is_empty();

    let snapshot = crate::git::RepoSnapshot {
        head_hash,
        branch_name,
        is_clean,
        changes,
    };

    let mut packet = map_snapshot_to_packet(snapshot, repo_root).ok()?;
    packet.finalize();
    crate::impact::redact::redact_secrets(&mut packet);

    let db_path = layout.state_subdir().join("ledger.db");
    let storage = match StorageManager::init(db_path.as_std_path()) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("capture_staged_snapshot: StorageManager::init failed: {e}");
            return None;
        }
    };
    let snapshot_id = match storage.save_packet(&packet) {
        Ok(id) => id,
        Err(e) => {
            tracing::debug!("capture_staged_snapshot: save_packet failed: {e}");
            return None;
        }
    };
    tracing::debug!("capture_staged_snapshot: saved snapshot_id={snapshot_id}");

    Some(CapturedSnapshot { snapshot_id })
}

fn get_staged_files(repo_root: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["diff", "--name-only", "--cached"])
        .current_dir(repo_root)
        .output()
        .ok();

    if let Some(out) = output
        && out.status.success()
    {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    }
}

pub fn is_trivial_commit(msg: &str) -> bool {
    let msg_lower = msg.to_lowercase();
    msg_lower.starts_with("chore:")
        || msg_lower.starts_with("docs:")
        || msg_lower.starts_with("style:")
        || msg_lower.starts_with("test:")
}

pub fn is_well_formed_conventional(msg: &str) -> bool {
    let lines: Vec<&str> = msg.lines().collect();
    if lines.is_empty() {
        return false;
    }
    let subject = lines[0].trim();

    // Standard conventional commit prefixes
    let prefixes = [
        "feat", "fix", "chore", "docs", "refactor", "perf", "ci", "build", "test", "revert",
        "style",
    ];

    let has_prefix = prefixes.iter().any(|&p| {
        subject.starts_with(p)
            && (subject[p.len()..].starts_with(':') || subject[p.len()..].starts_with('('))
            && subject.contains(':')
    });

    // Also require a body for "well-formed" bypass to ensure sufficient intent
    let has_body = lines.iter().skip(1).any(|l| !l.trim().is_empty());

    has_prefix && has_body
}

fn are_files_trivial(files: &[String]) -> bool {
    files
        .iter()
        .all(|f| f.ends_with(".md") || f.contains(".ledgerful/") || f.contains("ignore_patterns"))
}

fn load_skip_history(path: &camino::Utf8Path) -> SkipHistory {
    if path.exists()
        && let Ok(data) = fs::read_to_string(path.as_std_path())
        && let Ok(history) = serde_json::from_str(&data)
    {
        return history;
    }
    SkipHistory::default()
}

fn save_skip_history(path: &camino::Utf8Path, history: &SkipHistory) {
    if let Ok(data) = serde_json::to_string(history) {
        let _ = fs::write(path.as_std_path(), data);
    }
}

pub fn parse_category_from_message(msg: &str) -> Category {
    let msg_lower = msg.to_lowercase();
    if msg_lower.starts_with("feat") {
        Category::Feature
    } else if msg_lower.starts_with("fix") || msg_lower.starts_with("bug") {
        Category::Bugfix
    } else if msg_lower.starts_with("docs") {
        Category::Docs
    } else if msg_lower.starts_with("refactor") || msg_lower.starts_with("perf") {
        Category::Refactor
    } else if msg_lower.starts_with("chore") {
        Category::Chore
    } else if msg_lower.starts_with("ci")
        || msg_lower.starts_with("infra")
        || msg_lower.starts_with("build")
    {
        Category::Infra
    } else if msg_lower.starts_with("style") {
        Category::Tooling
    } else if msg_lower.starts_with("revert") {
        Category::Bugfix
    } else if msg_lower.starts_with("security") {
        Category::Security
    } else if msg_lower.starts_with("breaking") {
        Category::Architecture
    } else {
        tracing::debug!(
            "No conventional commit prefix found in message; falling back to Category::Chore: {}",
            msg
        );
        Category::Chore
    }
}

pub fn risk_from_category(cat: Category) -> &'static str {
    match cat {
        Category::Architecture
        | Category::Feature
        | Category::Bugfix
        | Category::Infra
        | Category::Security => "HIGH",
        Category::Refactor | Category::Tooling => "MEDIUM",
        Category::Docs | Category::Chore => "TRIVIAL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_coverage_summary_prefixed() {
        let s = skipped_coverage_summary("chore: fmt");
        assert!(s.starts_with(SKIPPED_SUMMARY_PREFIX));
        assert!(s.contains("chore: fmt"));
    }

    #[test]
    fn skipped_coverage_risk_is_not_trivial() {
        // Promote maps TRIVIAL → verification_status None; SKIPPED must be Unverified.
        assert_ne!(SKIPPED_COVERAGE_RISK, "TRIVIAL");
    }

    #[test]
    fn tui_skip_disposition_matches_s_key() {
        assert!(is_tui_skip_disposition("TRIVIAL", "Skipped intent entry"));
        assert!(!is_tui_skip_disposition("MEDIUM", "Skipped intent entry"));
        assert!(!is_tui_skip_disposition("TRIVIAL", "something else"));
    }
}
