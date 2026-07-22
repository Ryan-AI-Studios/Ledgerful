use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::commands::hook_sidecar::{
    CODE_HEAD_UNCOVERED, CODE_PROMOTE_ORPHAN, PendingHookTx, RECOVER_HINT, head_message_hash,
    read_pending_sidecar,
};
use crate::ledger::*;
use crate::state::storage::StorageManager;
use crate::util::clock::{Clock, SystemClock};
use chrono::{DateTime, Utc};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde::Serialize;

/// Lifecycle integrity signals for status / exit-code.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleSignals {
    pub promote_orphan: bool,
    pub head_uncovered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promote_orphan_tx_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promote_error: Option<String>,
}

/// Inspect the pending_hook_tx sidecar for promote-fail / HEAD-match coverage gaps.
///
/// **Honest minimum:** `head_uncovered` is co-set with orphan/sidecar detection
/// only. There is no independent scan of COMMITTED/SKIPPED rows against HEAD
/// when the sidecar is absent. See `docs/lifecycle-integrity.md`.
pub fn detect_lifecycle_signals(layout: &crate::state::layout::Layout) -> LifecycleSignals {
    let mut signals = LifecycleSignals::default();
    let sidecar_path = layout.state_subdir().join("pending_hook_tx");
    let Ok(Some(pending)) = read_pending_sidecar(sidecar_path.as_std_path()) else {
        return signals;
    };

    let head_hash = head_message_hash(layout.root.as_std_path());
    let matches_head = head_hash
        .as_deref()
        .is_some_and(|h| h == pending.commit_msg_hash);

    if pending.is_promote_failed() {
        signals.promote_orphan = true;
        signals.promote_orphan_tx_id = Some(pending.tx_id.clone());
        signals.promote_error = pending.promote_error.clone();
        // Promote-failed orphans also mean HEAD is not covered by a COMMITTED row.
        signals.head_uncovered = true;
    } else if matches_head {
        // HEAD-matching pending without successful promote → uncovered trail.
        signals.promote_orphan = true;
        signals.head_uncovered = true;
        signals.promote_orphan_tx_id = Some(pending.tx_id.clone());
    }

    signals
}

fn would_block(pending_count: usize, unaudited_count: usize, signals: &LifecycleSignals) -> bool {
    pending_count > 0 || unaudited_count > 0 || signals.promote_orphan || signals.head_uncovered
}

/// Apply --exit-code policy per phase0 observe matrix.
///
/// - enforce: exit 1 on would-block
/// - observe default: exit 0 + banner WARN
/// - observe + strict_observe_signal (or LEDGERFUL_STRICT_OBSERVE_SIGNAL=1): exit 2
fn apply_exit_code(
    config: &crate::config::model::Config,
    exit_code: bool,
    strict_observe_signal: bool,
    pending_count: usize,
    unaudited_count: usize,
    signals: &LifecycleSignals,
) {
    if !exit_code || !would_block(pending_count, unaudited_count, signals) {
        return;
    }

    let strict_env = std::env::var("LEDGERFUL_STRICT_OBSERVE_SIGNAL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let strict = strict_observe_signal || strict_env;

    if config.gate.is_enforce() {
        std::process::exit(1);
    }

    tracing::warn!(
        target: "cli_summary",
        "Gate is observe: status would block in enforce mode (pending={}, unaudited={}, promote_orphan={}, head_uncovered={})",
        pending_count,
        unaudited_count,
        signals.promote_orphan,
        signals.head_uncovered
    );
    eprintln!(
        "[Ledgerful] WARNING: observe mode would-block (pending={}, unaudited={}, promote_orphan={}, head_uncovered={}). {}",
        pending_count,
        unaudited_count,
        signals.promote_orphan,
        signals.head_uncovered,
        if signals.promote_orphan {
            RECOVER_HINT
        } else {
            "Set gate to enforce for blocking exit codes, or pass --strict-observe-signal for exit 2."
        }
    );
    if strict {
        std::process::exit(2);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_ledger_status(
    entity_filter: Option<String>,
    compact: bool,
    exit_code: bool,
    verify_signatures: bool,
    json: bool,
    all: bool,
    #[allow(unused)] _global: bool,
    #[allow(unused)] _repo_filter: Option<String>,
    #[allow(unused)] _reindex: bool,
    #[allow(unused)] _opt_out: bool,
    #[allow(unused)] _opt_in: bool,
    strict_observe_signal: bool,
) -> Result<()> {
    let layout = get_layout()?;

    if verify_signatures {
        crate::commands::verify::verify_ledger_signatures(&layout)?;
    }

    let mut storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let config = load_ledger_config(&layout)?;
    let stale_threshold = config.ledger.stale_threshold_hours as i64;
    let tx_mgr = TransactionManager::new(&mut storage, layout.root.clone().into(), config.clone());
    let clock = SystemClock;
    let signals = detect_lifecycle_signals(&layout);

    if config.gate.is_observe() && !compact && !json {
        println!(
            "{} {}",
            "Notice:".bold().yellow(),
            "Gate mode is observe — block conditions will warn, not block.".yellow()
        );
    }

    if json {
        let pending = tx_mgr
            .get_all_pending()
            .map_err(|e| miette::miette!("{}", e))?;
        let unaudited = tx_mgr
            .get_all_unaudited()
            .map_err(|e| miette::miette!("{}", e))?;
        let pending_tx_ids: Vec<String> = pending.iter().map(|t| t.tx_id.clone()).collect();

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct StatusJson {
            pending_count: usize,
            unaudited_count: usize,
            pending_tx_ids: Vec<String>,
            unaudited_file_count: usize,
            promote_orphan: bool,
            head_uncovered: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            promote_orphan_tx_id: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            promote_error: Option<String>,
        }

        let status = StatusJson {
            pending_count: pending.len(),
            unaudited_count: unaudited.len(),
            pending_tx_ids,
            unaudited_file_count: unaudited.iter().map(|u| u.drift_count as usize).sum(),
            promote_orphan: signals.promote_orphan,
            head_uncovered: signals.head_uncovered,
            promote_orphan_tx_id: signals.promote_orphan_tx_id.clone(),
            promote_error: signals.promote_error.clone(),
        };

        println!(
            "{}",
            serde_json::to_string_pretty(&status).into_diagnostic()?
        );

        apply_exit_code(
            &config,
            exit_code,
            strict_observe_signal,
            status.pending_count,
            status.unaudited_count,
            &signals,
        );
        return Ok(());
    }

    if let Some(entity) = entity_filter {
        println!("Ledger Status for {}:", entity.cyan());
        if let Some(pending) = tx_mgr
            .get_pending(&entity)
            .map_err(|e| miette::miette!("{}", e))?
        {
            let started_at = DateTime::parse_from_rfc3339(&pending.started_at).into_diagnostic()?;
            let age_str = clock.relative_time(started_at.with_timezone(&Utc));

            let status_icon = if Utc::now()
                .signed_duration_since(started_at.with_timezone(&Utc))
                .num_hours()
                >= stale_threshold
            {
                get_status_icon(LedgerStatus::Stale)
            } else {
                get_status_icon(LedgerStatus::Pending)
            };

            println!(
                "  {} PENDING: {} [{}] {}",
                status_icon,
                pending.tx_id.yellow(),
                get_category_icon(&pending.category),
                age_str.dimmed()
            );
        } else {
            println!("  No pending transaction.");
        }

        println!("\nRecent History:");
        let entries = tx_mgr
            .get_ledger_entries(&entity)
            .map_err(|e| miette::miette!("{}", e))?;

        if entries.is_empty() {
            println!("  No history found.");
        } else {
            let mut table =
                crate::output::table::build_table(vec!["Time", "Icon", "Type", "Summary"]);
            let limit = if all { usize::MAX } else { 10 };
            for entry in entries.iter().take(limit) {
                let committed_at =
                    DateTime::parse_from_rfc3339(&entry.committed_at).into_diagnostic()?;
                table.add_row(vec![
                    clock
                        .relative_time(committed_at.with_timezone(&Utc))
                        .dimmed()
                        .to_string(),
                    get_change_type_icon(&entry.change_type),
                    format!("{:?}", entry.change_type).blue().to_string(),
                    entry.summary.clone(),
                ]);
            }
            println!("{}", table);
        }
    } else {
        let pending = tx_mgr
            .get_all_pending()
            .map_err(|e| miette::miette!("{}", e))?;
        let unaudited = tx_mgr
            .get_all_unaudited()
            .map_err(|e| miette::miette!("{}", e))?;

        let pending_count = pending.len();
        let unaudited_count = unaudited.len();

        if compact {
            let mut line = format!(
                "Ledger: {} pending, {} unaudited drift.",
                pending_count.to_string().yellow(),
                unaudited_count.to_string().red()
            );
            if signals.promote_orphan {
                line.push_str(&format!(
                    " {}[{}]",
                    "CRITICAL ".red().bold(),
                    CODE_PROMOTE_ORPHAN.red()
                ));
            }
            if signals.head_uncovered {
                line.push_str(&format!(
                    " {}[{}]",
                    "CRITICAL ".red().bold(),
                    CODE_HEAD_UNCOVERED.red()
                ));
            }
            println!("{line}");
            if signals.promote_orphan {
                eprintln!("  Recover with: {RECOVER_HINT}");
            }
            apply_exit_code(
                &config,
                exit_code,
                strict_observe_signal,
                pending_count,
                unaudited_count,
                &signals,
            );
            return Ok(());
        }

        println!("{}", "Ledgerful Ledger Status".bold().underline());

        if let Ok(repo) = crate::git::repo::open_repo(layout.root.as_std_path())
            && let Ok((head_hash, branch_name)) = crate::git::repo::get_head_info(&repo)
        {
            let changes = crate::git::status::get_repo_status(&repo).unwrap_or_default();
            let filtered = crate::git::ignore::filter_ignored_changes(
                changes,
                &config.watch.ignore_patterns,
                true,
            )
            .unwrap_or_default();
            let snapshot = crate::git::RepoSnapshot {
                head_hash,
                branch_name,
                is_clean: filtered.is_empty(),
                changes: filtered,
            };
            let freshness = crate::state::reports::check_impact_freshness(&layout, &snapshot);
            let freshness_str = match freshness {
                crate::state::reports::ImpactFreshness::Missing => "None".yellow().to_string(),
                crate::state::reports::ImpactFreshness::CurrentClean => {
                    "Current (Clean)".green().to_string()
                }
                crate::state::reports::ImpactFreshness::CurrentDirty => {
                    "Current (Dirty)".green().to_string()
                }
                crate::state::reports::ImpactFreshness::Stale { reason } => {
                    format!("STALE ({}) — run 'ledgerful impact' to refresh", reason)
                        .red()
                        .to_string()
                }
                crate::state::reports::ImpactFreshness::Corrupt { .. } => {
                    "Corrupt".red().to_string()
                }
            };
            println!("Impact Report: {}", freshness_str);
        }

        if signals.promote_orphan || signals.head_uncovered {
            println!(
                "\n{} {}",
                "CRITICAL".red().bold(),
                "LIFECYCLE INTEGRITY".red().bold()
            );
            if signals.promote_orphan {
                println!(
                    "  [{}] Promote orphan retained (tx={}). Recover with: {}",
                    CODE_PROMOTE_ORPHAN.red(),
                    signals.promote_orphan_tx_id.as_deref().unwrap_or("unknown"),
                    RECOVER_HINT
                );
                if let Some(ref err) = signals.promote_error {
                    println!("    promote_error: {err}");
                }
            }
            if signals.head_uncovered {
                println!(
                    "  [{}] HEAD uncovered via promote-fail/HEAD-matching pending sidecar (message-hash heuristic; not a full material-HEAD-without-row scan).",
                    CODE_HEAD_UNCOVERED.red()
                );
            }
        }

        println!(
            "\n{} {}",
            get_status_icon(LedgerStatus::Pending),
            "PENDING TRANSACTIONS".yellow().bold()
        );

        let sidecar_path = layout.state_subdir().join("pending_hook_tx");
        if sidecar_path.exists() {
            match std::fs::read_to_string(&sidecar_path) {
                Ok(content) => match serde_json::from_str::<PendingHookTx>(&content) {
                    Ok(pending_sidecar) => {
                        let mut matches_head = false;
                        if let Some(current_hash) = head_message_hash(layout.root.as_std_path()) {
                            matches_head = current_hash == pending_sidecar.commit_msg_hash;
                        }
                        if pending_sidecar.is_promote_failed() {
                            println!(
                                "  {} [Sidecar] PROMOTE_FAILED orphan (tx {}) — do not GC; {}",
                                "󰀦".red(),
                                pending_sidecar.tx_id,
                                RECOVER_HINT
                            );
                        } else if matches_head {
                            println!(
                                "  {} [Sidecar] Pending commit sidecar message hash matches HEAD",
                                "󰀦".yellow()
                            );
                        } else {
                            let mut matches_editmsg = false;
                            let editmsg_path = layout
                                .root
                                .as_std_path()
                                .join(".git")
                                .join("COMMIT_EDITMSG");
                            let index_lock_path =
                                layout.root.as_std_path().join(".git").join("index.lock");

                            if editmsg_path.exists()
                                && index_lock_path.exists()
                                && let Ok(edit_msg) = std::fs::read_to_string(&editmsg_path)
                            {
                                let cleaned = crate::util::text::clean_commit_msg(&edit_msg);
                                let edit_hash =
                                    crate::commands::hook_sidecar::hash_message(&cleaned);
                                matches_editmsg = edit_hash == pending_sidecar.commit_msg_hash;
                            }

                            if matches_editmsg {
                                println!(
                                    "  {} [Sidecar] Pending commit sidecar matches active COMMIT_EDITMSG",
                                    "󰀦".yellow()
                                );
                            } else {
                                println!(
                                    "  {} [Sidecar] Pending commit sidecar exists but does NOT match HEAD or active commit (stale)",
                                    "󰀦".yellow()
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse pending hook sidecar: {}", e);
                        println!(
                            "  {} [Sidecar] Pending commit sidecar is broken/unparseable (stale)",
                            "󰀦".red()
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read pending hook sidecar: {}", e);
                    println!(
                        "  {} [Sidecar] Pending commit sidecar is unreadable (stale)",
                        "󰀦".red()
                    );
                }
            }
        }

        if pending.is_empty() {
            println!("  None.");
        } else {
            let mut table =
                crate::output::table::build_table(vec!["ID", "Category", "Entity", "Age"]);
            for tx in pending {
                let started_at = DateTime::parse_from_rfc3339(&tx.started_at).into_diagnostic()?;
                let age_str = clock.relative_time(started_at.with_timezone(&Utc));
                let is_stale = Utc::now()
                    .signed_duration_since(started_at.with_timezone(&Utc))
                    .num_hours()
                    >= stale_threshold;
                let stale_indicator = if is_stale {
                    format!("{} STALE", "󰀦".red())
                } else {
                    "".to_string()
                };

                table.add_row(vec![
                    tx.tx_id.yellow().to_string(),
                    format!("{} {:?}", get_category_icon(&tx.category), tx.category),
                    tx.entity.cyan().to_string(),
                    format!("{} {}", age_str.dimmed(), stale_indicator),
                ]);
            }
            println!("{}", table);
        }

        println!("\n{} {}", "󰀦".red(), "UNAUDITED DRIFT".red().bold());
        if unaudited.is_empty() {
            println!("  None.");
        } else {
            let mut table =
                crate::output::table::build_table(vec!["Entity", "Changes", "Last Seen"]);
            for tx in unaudited {
                let last_seen = if let Some(ts) = tx.last_seen_at {
                    if let Ok(dt) = DateTime::parse_from_rfc3339(&ts) {
                        clock.relative_time(dt.with_timezone(&Utc))
                    } else {
                        ts
                    }
                } else {
                    "unknown".to_string()
                };

                table.add_row(vec![
                    tx.entity.cyan().to_string(),
                    tx.drift_count.to_string().bold().to_string(),
                    last_seen.dimmed().to_string(),
                ]);
            }
            println!("{}", table);
        }

        if all {
            println!("\n{} {}", "󰋚".blue(), "RECENT HISTORY".blue().bold());
            let db = LedgerDb::new(storage.get_connection());
            let entries = db
                .get_all_committed_ledger_entries()
                .map_err(|e| miette::miette!("{}", e))?;

            if entries.is_empty() {
                println!("  No history found.");
            } else {
                let mut table =
                    crate::output::table::build_table(vec!["Time", "Entity", "Type", "Summary"]);
                for entry in entries {
                    let committed_at =
                        DateTime::parse_from_rfc3339(&entry.committed_at).into_diagnostic()?;
                    table.add_row(vec![
                        clock
                            .relative_time(committed_at.with_timezone(&Utc))
                            .dimmed()
                            .to_string(),
                        entry.entity_normalized.cyan().to_string(),
                        format!("{:?}", entry.change_type).blue().to_string(),
                        entry.summary.clone(),
                    ]);
                }
                println!("{}", table);
            }
        }

        apply_exit_code(
            &config,
            exit_code,
            strict_observe_signal,
            pending_count,
            unaudited_count,
            &signals,
        );
    }

    Ok(())
}

/// Export stable provenance as pretty-printed JSON.
///
/// When `output` is `None`, writes JSON to stdout. When `Some(path)`, writes
/// to the specified file path.
pub fn execute_ledger_export_provenance(output: Option<String>) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout.root)?;
    let db = LedgerDb::new(storage.get_connection());
    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette::miette!("{}", e))?;

    if let Some(output_path) = output {
        let file = std::fs::File::create(&output_path).into_diagnostic()?;
        serde_json::to_writer_pretty(file, &entries).into_diagnostic()?;
        println!(
            "{} Stable provenance exported to {}",
            "SUCCESS:".green().bold(),
            output_path
        );
    } else {
        serde_json::to_writer_pretty(std::io::stdout(), &entries).into_diagnostic()?;
    }
    Ok(())
}

/// Export a redacted, cryptographically verifiable public ledger bundle.
///
/// Delegates to `crate::ledger::public_export::export_public_bundle`, which
/// performs all allowlist filtering, pseudonymization, manifest generation,
/// and optional bot-key signing.
pub fn execute_ledger_export_public(options: crate::ledger::ExportOptions<'_>) -> Result<()> {
    crate::ledger::export_public_bundle(options)
}
