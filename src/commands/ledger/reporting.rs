use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::ledger::*;
use crate::state::storage::StorageManager;
use crate::util::clock::{Clock, SystemClock};
use chrono::{DateTime, Utc};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde::Serialize;

pub fn execute_ledger_status(
    entity_filter: Option<String>,
    compact: bool,
    exit_code: bool,
    verify_signatures: bool,
    json: bool,
    all: bool,
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
        }

        let status = StatusJson {
            pending_count: pending.len(),
            unaudited_count: unaudited.len(),
            pending_tx_ids,
            unaudited_file_count: unaudited.iter().map(|u| u.drift_count as usize).sum(),
        };

        println!(
            "{}",
            serde_json::to_string_pretty(&status).into_diagnostic()?
        );

        if exit_code && (status.pending_count > 0 || status.unaudited_count > 0) {
            std::process::exit(1);
        }
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
            println!(
                "Ledger: {} pending, {} unaudited drift.",
                pending_count.to_string().yellow(),
                unaudited_count.to_string().red()
            );
            if exit_code && (pending_count > 0 || unaudited_count > 0) {
                std::process::exit(1);
            }
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

        println!(
            "\n{} {}",
            get_status_icon(LedgerStatus::Pending),
            "PENDING TRANSACTIONS".yellow().bold()
        );
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

        if exit_code && (pending_count > 0 || unaudited_count > 0) {
            std::process::exit(1);
        }
    }

    Ok(())
}

pub fn execute_ledger_export_provenance(output: Option<String>) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout.root)?;
    let db = LedgerDb::new(storage.get_connection());
    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette::miette!("{}", e))?;

    let output_path = output.unwrap_or_else(|| "provenance-export.json".to_string());
    let file = std::fs::File::create(&output_path).into_diagnostic()?;
    serde_json::to_writer_pretty(file, &entries).into_diagnostic()?;

    println!(
        "{} Stable provenance exported to {}",
        "SUCCESS:".green().bold(),
        output_path
    );
    Ok(())
}
