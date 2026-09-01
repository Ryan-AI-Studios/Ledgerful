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
use owo_colors::{OwoColorize, Stream, Style};
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

/// Stable schema version for `ledger status --json` (track 0093).
const STATUS_JSON_SCHEMA_VERSION: u32 = 1;

/// Wire payload for `ledger status --json` (v1).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StatusJson {
    pub schema_version: u32,
    pub work_root: String,
    pub state_dir: String,
    pub pending_count: usize,
    pub unaudited_count: usize,
    pub pending_tx_ids: Vec<String>,
    pub unaudited_file_count: usize,
    pub promote_orphan: bool,
    pub head_uncovered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promote_orphan_tx_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promote_error: Option<String>,
}

/// Build the status JSON payload with lexicographically sorted `pendingTxIds`
/// (determinism; 0093 DoD-8). Shared by the live path and unit tests.
///
/// `work_root` / `state_dir` are the same absolute strings doctor
/// `environment` uses (`layout.root` / `layout.state_dir`). Always present.
pub fn build_status_json(
    mut pending_tx_ids: Vec<String>,
    unaudited_count: usize,
    unaudited_file_count: usize,
    signals: &LifecycleSignals,
    work_root: &str,
    state_dir: &str,
) -> StatusJson {
    pending_tx_ids.sort();
    StatusJson {
        schema_version: STATUS_JSON_SCHEMA_VERSION,
        work_root: work_root.to_string(),
        state_dir: state_dir.to_string(),
        pending_count: pending_tx_ids.len(),
        unaudited_count,
        pending_tx_ids,
        unaudited_file_count,
        promote_orphan: signals.promote_orphan,
        head_uncovered: signals.head_uncovered,
        promote_orphan_tx_id: signals.promote_orphan_tx_id.clone(),
        promote_error: signals.promote_error.clone(),
    }
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

    // Single emission via cli_summary (0093 DoD-9). Level-split writer routes
    // warn! → stderr so `ledger status --json` keeps stdout JSON-only.
    let remediation = if signals.promote_orphan {
        RECOVER_HINT
    } else {
        "Set gate to enforce for blocking exit codes, or pass --strict-observe-signal for exit 2."
    };
    tracing::warn!(
        target: "cli_summary",
        "[Ledgerful] WARNING: observe mode would-block (pending={}, unaudited={}, promote_orphan={}, head_uncovered={}). {}",
        pending_count,
        unaudited_count,
        signals.promote_orphan,
        signals.head_uncovered,
        remediation
    );
    if strict {
        std::process::exit(2);
    }
}

/// Named options for [`execute_ledger_status`].
///
/// `LedgerCommands::Status.global` stays on the clap enum and is handled in
/// dispatch (`execute_ledger_status_global`); it is not part of this execute
/// signature.
pub struct LedgerStatusOpts {
    pub entity_filter: Option<String>,
    pub compact: bool,
    pub exit_code: bool,
    pub verify_signatures: bool,
    pub json: bool,
    pub all: bool,
    pub strict_observe_signal: bool,
}

pub fn execute_ledger_status(opts: LedgerStatusOpts) -> Result<()> {
    let LedgerStatusOpts {
        entity_filter,
        compact,
        exit_code,
        verify_signatures,
        json,
        all,
        strict_observe_signal,
    } = opts;
    let layout = get_layout()?;

    if verify_signatures {
        crate::commands::verify::verify_ledger_signatures(&layout)?;
    }

    let mut storage = StorageManager::open_read_only_sqlite_only(&layout)?;
    let config = load_ledger_config(&layout)?;
    let stale_threshold = config.ledger.stale_threshold_hours as i64;
    let tx_mgr = TransactionManager::new(&mut storage, layout.root.clone().into(), config.clone());
    let clock = SystemClock;
    let signals = detect_lifecycle_signals(&layout);

    if config.gate.is_observe() && !compact && !json {
        println!(
            "{} {}",
            "Notice:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().yellow())),
            "Gate mode is observe — block conditions will warn, not block."
                .if_supports_color(Stream::Stdout, |s| s.yellow())
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
        let unaudited_file_count = unaudited.iter().map(|u| u.drift_count as usize).sum();
        let status = build_status_json(
            pending_tx_ids,
            unaudited.len(),
            unaudited_file_count,
            &signals,
            layout.root.as_str(),
            layout.state_dir.as_str(),
        );

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
        println!(
            "Ledger Status for {}:",
            entity.if_supports_color(Stream::Stdout, |s| s.cyan())
        );
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
                pending
                    .tx_id
                    .if_supports_color(Stream::Stdout, |s| s.yellow()),
                crate::ledger::ui::with_icon(
                    &get_category_icon(&pending.category),
                    format!("{:?}", pending.category),
                ),
                age_str.if_supports_color(Stream::Stdout, |s| s.dimmed())
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
                        .if_supports_color(Stream::Stdout, |s| s.dimmed())
                        .to_string(),
                    get_change_type_icon(&entry.change_type),
                    format!("{:?}", entry.change_type)
                        .if_supports_color(Stream::Stdout, |s| s.blue())
                        .to_string(),
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
                "Ledger [{}]: {} pending, {} unaudited drift.",
                layout.root,
                pending_count
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.yellow()),
                unaudited_count
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.red())
            );
            if signals.promote_orphan {
                line.push_str(&format!(
                    " {}[{}]",
                    "CRITICAL "
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold())),
                    CODE_PROMOTE_ORPHAN.if_supports_color(Stream::Stdout, |s| s.red())
                ));
            }
            if signals.head_uncovered {
                line.push_str(&format!(
                    " {}[{}]",
                    "CRITICAL "
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold())),
                    CODE_HEAD_UNCOVERED.if_supports_color(Stream::Stdout, |s| s.red())
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

        println!(
            "{}",
            "Ledgerful Ledger Status"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
        );
        println!(
            "Work root: {}",
            layout
                .root
                .as_str()
                .if_supports_color(Stream::Stdout, |s| s.cyan())
        );

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
                crate::state::reports::ImpactFreshness::Missing => "None"
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                crate::state::reports::ImpactFreshness::CurrentClean => "Current (Clean)"
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string(),
                crate::state::reports::ImpactFreshness::CurrentDirty => "Current (Dirty)"
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string(),
                crate::state::reports::ImpactFreshness::Stale { reason } => {
                    format!("STALE ({}) — run 'ledgerful impact' to refresh", reason)
                        .if_supports_color(Stream::Stdout, |s| s.red())
                        .to_string()
                }
                crate::state::reports::ImpactFreshness::Corrupt { .. } => "Corrupt"
                    .if_supports_color(Stream::Stdout, |s| s.red())
                    .to_string(),
            };
            println!("Impact Report: {}", freshness_str);
        }

        if signals.promote_orphan || signals.head_uncovered {
            println!(
                "\n{} {}",
                "CRITICAL"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold())),
                "LIFECYCLE INTEGRITY"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
            );
            if signals.promote_orphan {
                println!(
                    "  [{}] Promote orphan retained (tx={}). Recover with: {}",
                    CODE_PROMOTE_ORPHAN.if_supports_color(Stream::Stdout, |s| s.red()),
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
                    CODE_HEAD_UNCOVERED.if_supports_color(Stream::Stdout, |s| s.red())
                );
            }
        }

        println!(
            "\n{} {}",
            get_status_icon(LedgerStatus::Pending),
            "PENDING TRANSACTIONS"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
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
                                get_status_icon(LedgerStatus::Stale),
                                pending_sidecar.tx_id,
                                RECOVER_HINT
                            );
                        } else if matches_head {
                            println!(
                                "  {} [Sidecar] Pending commit sidecar message hash matches HEAD",
                                get_status_icon(LedgerStatus::Pending)
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
                                    get_status_icon(LedgerStatus::Pending)
                                );
                            } else {
                                println!(
                                    "  {} [Sidecar] Pending commit sidecar exists but does NOT match HEAD or active commit (stale)",
                                    get_status_icon(LedgerStatus::Pending)
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse pending hook sidecar: {}", e);
                        println!(
                            "  {} [Sidecar] Pending commit sidecar is broken/unparseable (stale)",
                            get_status_icon(LedgerStatus::Stale)
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read pending hook sidecar: {}", e);
                    println!(
                        "  {} [Sidecar] Pending commit sidecar is unreadable (stale)",
                        get_status_icon(LedgerStatus::Stale)
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
                    format!("{} STALE", get_status_icon(LedgerStatus::Stale))
                } else {
                    "".to_string()
                };

                table.add_row(vec![
                    tx.tx_id
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                        .to_string(),
                    crate::ledger::ui::with_icon(
                        &get_category_icon(&tx.category),
                        format!("{:?}", tx.category),
                    ),
                    tx.entity
                        .if_supports_color(Stream::Stdout, |s| s.cyan())
                        .to_string(),
                    format!(
                        "{} {}",
                        age_str.if_supports_color(Stream::Stdout, |s| s.dimmed()),
                        stale_indicator
                    ),
                ]);
            }
            println!("{}", table);
        }

        println!(
            "\n{} {}",
            get_status_icon(LedgerStatus::Stale),
            "UNAUDITED DRIFT"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
        );
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
                    tx.entity
                        .if_supports_color(Stream::Stdout, |s| s.cyan())
                        .to_string(),
                    tx.drift_count
                        .to_string()
                        .if_supports_color(Stream::Stdout, |s| s.bold())
                        .to_string(),
                    last_seen
                        .if_supports_color(Stream::Stdout, |s| s.dimmed())
                        .to_string(),
                ]);
            }
            println!("{}", table);
        }

        if all {
            println!(
                "\n{} {}",
                get_status_icon(LedgerStatus::Committed),
                "RECENT HISTORY"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold()))
            );
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
                            .if_supports_color(Stream::Stdout, |s| s.dimmed())
                            .to_string(),
                        entry
                            .entity_normalized
                            .if_supports_color(Stream::Stdout, |s| s.cyan())
                            .to_string(),
                        format!("{:?}", entry.change_type)
                            .if_supports_color(Stream::Stdout, |s| s.blue())
                            .to_string(),
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
    let storage = StorageManager::open_read_only(&layout)?;
    let db = LedgerDb::new(storage.get_connection());
    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette::miette!("{}", e))?;

    if let Some(output_path) = output {
        let file = std::fs::File::create(&output_path).into_diagnostic()?;
        serde_json::to_writer_pretty(file, &entries).into_diagnostic()?;
        println!(
            "{} Stable provenance exported to {}",
            "SUCCESS:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold())),
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

#[cfg(test)]
mod status_json_tests {
    use super::*;

    #[test]
    fn pending_tx_ids_sorted_and_stable_via_real_builder() {
        // Exercises the production `build_status_json` / `StatusJson` path
        // (not a reimplemented fixture).
        let signals = LifecycleSignals::default();
        let a = build_status_json(
            vec!["z-tx".into(), "a-tx".into(), "m-tx".into()],
            0,
            0,
            &signals,
            "/repo",
            "/repo/.ledgerful",
        );
        let b = build_status_json(
            vec!["m-tx".into(), "z-tx".into(), "a-tx".into()],
            0,
            0,
            &signals,
            "/repo",
            "/repo/.ledgerful",
        );
        assert_eq!(a.pending_tx_ids, vec!["a-tx", "m-tx", "z-tx"]);
        assert_eq!(a.pending_count, 3);
        assert_eq!(a.schema_version, STATUS_JSON_SCHEMA_VERSION);
        assert_eq!(a.work_root, "/repo");
        assert_eq!(a.state_dir, "/repo/.ledgerful");
        assert_eq!(a, b);
        let ja = serde_json::to_string(&a).unwrap();
        let jb = serde_json::to_string(&b).unwrap();
        assert_eq!(ja, jb);
        let pretty = serde_json::to_string_pretty(&a).unwrap();
        assert!(pretty.contains("schemaVersion"));
        assert!(pretty.contains("pendingTxIds"));
        assert!(pretty.contains("workRoot"));
        assert!(pretty.contains("stateDir"));
        assert!(
            !pretty.contains("collisions"),
            "0223: StatusJson v1 must not grow collisions[] (0224 owns that array); got {pretty}"
        );
    }

    #[test]
    fn would_block_stream_stays_on_warn_cli_summary() {
        // Observe would-block is a single warn!(cli_summary) — level-split
        // writer routes WARN → stderr so stdout JSON stays pure (DoD-3/F4).
        // Structural: apply_exit_code only emits via cli_summary warn, not println.
        let src = include_str!("reporting.rs");
        assert!(
            src.contains("target: \"cli_summary\""),
            "would-block must emit via cli_summary"
        );
        // Ensure the de-duped path no longer double-emits eprintln for would-block.
        let apply_fn = src.split("fn apply_exit_code").nth(1).unwrap_or("");
        let apply_body = apply_fn
            .split("pub fn execute_ledger_status")
            .next()
            .unwrap_or("");
        assert!(
            !apply_body.contains("eprintln!"),
            "apply_exit_code must not eprintln (DoD-9 de-dup); body={apply_body}"
        );
        assert!(
            apply_body.contains("tracing::warn!"),
            "apply_exit_code must warn! on cli_summary"
        );
    }

    /// DoD-3 strengthening: real stream capture of `apply_exit_code` under the
    /// production level-split writer (info→stdout, warn/error→stderr).
    #[test]
    fn would_block_apply_exit_code_warn_on_stderr_not_stdout() {
        use std::io::{self, Write};
        use std::sync::{Arc, Mutex};
        use tracing::Level;
        use tracing_subscriber::Layer;
        use tracing_subscriber::fmt;
        use tracing_subscriber::fmt::writer::{MakeWriter, MakeWriterExt};
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        #[derive(Clone, Default)]
        struct BufWriter {
            buf: Arc<Mutex<Vec<u8>>>,
        }
        impl<'a> MakeWriter<'a> for BufWriter {
            type Writer = BufGuard;
            fn make_writer(&'a self) -> Self::Writer {
                BufGuard {
                    buf: Arc::clone(&self.buf),
                }
            }
        }
        struct BufGuard {
            buf: Arc<Mutex<Vec<u8>>>,
        }
        impl Write for BufGuard {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                self.buf
                    .lock()
                    .map_err(|e| io::Error::other(e.to_string()))?
                    .extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let stdout_buf = BufWriter::default();
        let stderr_buf = BufWriter::default();
        let stdout_capture = Arc::clone(&stdout_buf.buf);
        let stderr_capture = Arc::clone(&stderr_buf.buf);
        let writer = stderr_buf.with_max_level(Level::WARN).or_else(stdout_buf);
        let layer = fmt::layer()
            .with_writer(writer)
            .without_time()
            .with_target(false)
            .with_level(false)
            .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                meta.target() == "cli_summary"
            }));
        let _guard = tracing_subscriber::registry().with(layer).set_default();

        // Default config is observe gate — would-block warns, does not exit(1).
        let config = crate::config::model::Config::default();
        assert!(
            config.gate.is_observe(),
            "default gate must be observe for non-exiting would-block path"
        );
        apply_exit_code(
            &config,
            true,  // --exit-code
            false, // not strict
            1,     // pending
            0,
            &LifecycleSignals::default(),
        );

        let stdout = String::from_utf8_lossy(&stdout_capture.lock().unwrap()).to_string();
        let stderr = String::from_utf8_lossy(&stderr_capture.lock().unwrap()).to_string();

        assert!(
            stderr.contains("would-block") || stderr.contains("observe mode"),
            "would-block warn must land on stderr; stderr={stderr:?} stdout={stdout:?}"
        );
        assert!(
            !stdout.contains("would-block") && !stdout.contains("observe mode"),
            "would-block must not pollute stdout JSON stream; stdout={stdout:?}"
        );
        // Simulated machine payload on stdout remains parseable if only JSON is there.
        assert!(
            stdout.trim().is_empty()
                || serde_json::from_str::<serde_json::Value>(stdout.trim()).is_ok(),
            "stdout must stay JSON-pure; stdout={stdout:?}"
        );
    }
}
