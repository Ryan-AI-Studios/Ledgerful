use crate::ai::intent_drafter::draft_intent;
use crate::commands::helpers::{get_layout, load_ledger_config};
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

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct SkipHistory {
    consecutive_skips: u32,
    bypass_remaining: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct PendingHookTx {
    pub tx_id: String,
    pub commit_msg_hash: String,
    pub summary: String,
    pub reason: String,
    pub committed_at: Option<String>,
    pub risk: Option<String>,
    pub related_tickets: Option<String>,
    pub signature: Option<String>,
    pub public_key: Option<String>,
}

fn write_pending_sidecar(
    layout: &crate::state::layout::Layout,
    pending: &PendingHookTx,
) -> Result<()> {
    let sidecar_path = layout.state_subdir().join("pending_hook_tx");
    let content = serde_json::to_string(pending).into_diagnostic()?;
    fs::write(sidecar_path, content).into_diagnostic()?;
    Ok(())
}

fn hash_message(msg: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(msg.as_bytes());
    hex::encode(hasher.finalize())
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

    // 1. If required is "never", skip hook processing entirely
    if config.intent.required == "never" {
        return Ok(());
    }

    let repo_root = layout.root.as_std_path();

    // Proactive GC: clean up any pre-existing sidecar before writing a new one.
    // A new commit-msg invocation implies any old sidecar is stale, UNLESS it matches HEAD.
    let sidecar_path = layout.state_subdir().join("pending_hook_tx");
    if sidecar_path.exists() {
        match fs::read_to_string(&sidecar_path) {
            Ok(content) => match serde_json::from_str::<PendingHookTx>(&content) {
                Ok(pending) => {
                    let mut matches_head = false;
                    if let Ok(output) = std::process::Command::new("git").args(["log", "-1", "--format=%B"]).current_dir(repo_root).output() {
                        if output.status.success() {
                            let head_msg = String::from_utf8_lossy(&output.stdout).to_string();
                            let cleaned = crate::util::text::clean_commit_msg(&head_msg);
                            let current_hash = hash_message(&cleaned);
                            matches_head = current_hash == pending.commit_msg_hash;
                        }
                    }

                    if matches_head {
                        return Err(miette::miette!(
                            "A pending commit sidecar exists that matches HEAD. The previous commit succeeded but its post-commit hook failed or was skipped. Please recover it by running 'ledgerful ledger commit' or manually remove the sidecar."
                        ));
                    }

                    match StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()) {
                        Ok(mut storage) => {
                            let mut tx_mgr = TransactionManager::new(
                                &mut storage,
                                layout.root.clone().into(),
                                config.clone(),
                            );
                            if let Err(e) = tx_mgr.rollback_change(pending.tx_id, "Aborted: stale hook sidecar".to_string()) {
                                tracing::warn!("Failed to rollback stale sidecar transaction: {}. Keeping sidecar.", e);
                            } else if let Err(e) = fs::remove_file(&sidecar_path) {
                                tracing::warn!("Failed to remove stale sidecar file: {}. Keeping sidecar.", e);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to init storage for sidecar GC: {}. Keeping sidecar.", e);
                        }
                    }
                }
                Err(e) => {
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

    // 2. Read git staged files
    let staged_files = get_staged_files(repo_root);
    if staged_files.is_empty() {
        return Ok(()); // Nothing staged, nothing to record
    }
    let entity = canonical_entity(&staged_files);
    let related_files = staged_files.join(", ");

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

        silently_record_ledger(SilentRecordArgs {
            config: &config,
            entity: &entity,
            what: &drafted_what,
            why: &drafted_why,
            risk: &drafted_risk,
            related: drafted_related,
            related_files: &related_files,
            raw_commit_msg: &raw_commit_msg,
        })?;

        // Update commit message file if LLM refined it
        if confidence >= 0.85 && !drafted_what.is_empty() {
            let trailers = extract_trailers(&raw_commit_msg);
            let updated_msg = if trailers.is_empty() {
                format!("{}\n\n{}", drafted_what, drafted_why)
            } else {
                format!("{}\n\n{}\n\n{}", drafted_what, drafted_why, trailers)
            };
            fs::write(msg_file, updated_msg).into_diagnostic()?;
        }

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
        if final_state.risk == "TRIVIAL" && final_state.what == "Skipped intent entry" {
            // User hit 's' (Skip) in TUI
            history.consecutive_skips += 1;
            if history.consecutive_skips >= 2 {
                history.bypass_remaining = 2;
            }
            save_skip_history(&skip_history_path, &history);
            tracing::info!(target: "cli_summary", "[Ledgerful] Intent entry skipped.");
            return Ok(());
        } else {
            // Reset skips
            history.consecutive_skips = 0;
            history.bypass_remaining = 0;
            save_skip_history(&skip_history_path, &history);
        }

        silently_record_ledger(SilentRecordArgs {
            config: &config,
            entity: &entity,
            what: &final_state.what,
            why: &final_state.why,
            risk: &final_state.risk,
            related: final_state.related.clone(),
            related_files: &related_files,
            raw_commit_msg: &raw_commit_msg,
        })?;

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
        fs::write(msg_file, updated_msg).into_diagnostic()?;

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
}

fn silently_record_ledger(args: SilentRecordArgs) -> Result<()> {
    let layout = get_layout()?;
    let category = parse_category_from_message(args.what);
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
    };

    write_pending_sidecar(&layout, &pending)?;

    Ok(())
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
