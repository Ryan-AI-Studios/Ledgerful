use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, Table};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde::Serialize;

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::impact::hotspots::calculate_hotspots;
use crate::impact::packet::Hotspot;
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::db::LedgerDb;
use crate::ledger::transaction::TransactionManager;
use crate::ledger::types::LedgerEntry;
use crate::ledger::ui::{LedgerStatus, get_change_type_icon, get_status_icon};
use crate::state::storage::StorageManager;
use crate::verify::results::VERIFY_HISTORY;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAuditReport {
    pub velocity: VelocitySummary,
    pub churn: Vec<ChurnEntry>,
    pub unaudited_drift: Vec<DriftEntry>,
    pub hotspots: Vec<Hotspot>,
    pub ci_trend: Vec<bool>,
    pub recent_entries: Vec<AuditEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VelocitySummary {
    pub last_7_days: i64,
    pub last_30_days: i64,
    pub total: i64,
    pub pending: i64,
    pub federated: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChurnEntry {
    pub entity: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftEntry {
    pub file_path: String,
    pub change_type: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: i64,
    pub tx_id: String,
    pub entity: String,
    pub trace_id: Option<String>,
    pub origin: String,
    pub summary: String,
    pub reason: String,
    pub change_type: crate::ledger::ChangeType,
    pub committed_at: String,
    pub is_breaking: bool,
    pub signature: Option<String>,
    pub public_key: Option<String>,
    pub risk: Option<String>,
    pub related_tickets: Option<String>,
    pub provenance: Vec<ProvenanceEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceEntry {
    pub entity: String,
    pub symbol_name: String,
    pub symbol_type: String,
    pub action: crate::ledger::provenance::ProvenanceAction,
}

pub fn execute_ledger_audit(
    entity: Option<String>,
    include_unaudited: bool,
    limit: usize,
    offset: usize,
    json: bool,
) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;

    if !json {
        println!("{}", "Ledgerful Project Audit".bold().underline());
    }

    if let Some(path) = entity {
        let config = load_ledger_config(&layout)?;
        let manager = TransactionManager::new(&mut storage, layout.root.clone().into(), config);
        let normalized =
            crate::util::path::normalize_relative_path(layout.root.as_std_path(), &path)
                .unwrap_or_else(|_| path.clone());

        let is_file = looks_like_file_path(&path);

        audit_entity(
            &manager,
            &path,
            if is_file { Some(&normalized) } else { None },
            limit,
            offset,
            json,
        )?;
    } else {
        audit_global(&storage, include_unaudited, limit, offset, json)?;
    }

    Ok(())
}

fn looks_like_file_path(s: &str) -> bool {
    // Contains a path separator (forward or backslash)
    if s.contains('/') || s.contains('\\') {
        return true;
    }
    // Has a file extension: a '.' followed by 1-8 alphanumeric chars at the end
    if let Some(dot_pos) = s.rfind('.') {
        let after_dot = &s[dot_pos + 1..];
        if !after_dot.is_empty()
            && after_dot.len() <= 8
            && after_dot.chars().all(|c| c.is_alphanumeric())
        {
            return true;
        }
    }
    false
}

fn gather_audit_data(
    storage: &StorageManager,
    include_unaudited: bool,
    limit: usize,
    offset: usize,
) -> Result<ProjectAuditReport> {
    let layout = get_layout()?;
    let db = LedgerDb::new(storage.get_connection());
    let config = load_ledger_config(&layout)?;

    // Opening a second connection for the manager avoids borrow conflicts
    let mut storage_mut = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let manager = TransactionManager::new(&mut storage_mut, layout.root.clone().into(), config);

    let v_7 = db
        .get_transaction_velocity(7)
        .map_err(|e| miette::miette!("{}", e))?;
    let v_30 = db
        .get_transaction_velocity(30)
        .map_err(|e| miette::miette!("{}", e))?;
    let total = db
        .get_transaction_velocity(36500)
        .map_err(|e| miette::miette!("{}", e))?;
    let pending_count = manager
        .get_all_pending()
        .map_err(|e| miette::miette!("{}", e))?
        .len() as i64;

    let federated_count = db
        .get_federated_entries_by_entity("%", "%", 1000000)
        .map(|entries| entries.len() as i64)
        .unwrap_or(0);

    let velocity = VelocitySummary {
        last_7_days: v_7 as i64,
        last_30_days: v_30 as i64,
        total: total as i64,
        pending: pending_count,
        federated: federated_count,
    };

    let churn_data = db
        .get_top_churned_entities(limit)
        .map_err(|e| miette::miette!("{}", e))?;
    let churn = churn_data
        .into_iter()
        .map(|(e, c)| ChurnEntry {
            entity: e,
            count: c as i64,
        })
        .collect();

    let unaudited_drift = if include_unaudited {
        let unaudited = manager
            .get_all_unaudited()
            .map_err(|e| miette::miette!("{}", e))?;
        unaudited
            .into_iter()
            .map(|u| DriftEntry {
                file_path: u.entity,
                change_type: format!("{:?}", u.category),
            })
            .collect()
    } else {
        vec![]
    };

    let discovered = gix::discover(&layout.root).into_diagnostic()?;
    let history_provider = GixHistoryProvider::new(&discovered);
    let hotspots = calculate_hotspots(
        storage,
        &history_provider,
        &crate::impact::hotspots::HotspotQuery {
            commits: 500,
            limit,
            decay_half_life: 100,
            ..Default::default()
        },
    )
    .unwrap_or_default();

    let history_path = layout.reports_dir().join(VERIFY_HISTORY);
    let ci_trend = if history_path.exists() {
        let content = std::fs::read_to_string(&history_path).into_diagnostic()?;
        let history: Vec<crate::verify::results::VerifyHistoryRecord> =
            serde_json::from_str(&content).unwrap_or_default();
        history
            .into_iter()
            .rev()
            .take(limit)
            .map(|h| h.passed)
            .collect()
    } else {
        vec![]
    };

    let recent = db
        .get_recent_ledger_entries_paginated(limit, offset)
        .map_err(|e| miette::miette!("{}", e))?;

    let recent_entries = audit_entries_from_ledger_entries(&db, recent)?;

    Ok(ProjectAuditReport {
        velocity,
        churn,
        unaudited_drift,
        hotspots,
        ci_trend,
        recent_entries,
    })
}

fn audit_entries_from_ledger_entries(
    db: &LedgerDb<'_>,
    entries: Vec<LedgerEntry>,
) -> Result<Vec<AuditEntry>> {
    let mut audit_entries = Vec::new();
    for entry in entries {
        let provenance_data = db
            .get_token_provenance_for_tx(&entry.tx_id)
            .map_err(|e| miette::miette!("{}", e))?;

        let provenance = provenance_data
            .into_iter()
            .map(|p| ProvenanceEntry {
                entity: p.entity,
                symbol_name: p.symbol_name,
                symbol_type: p.symbol_type,
                action: p.action,
            })
            .collect();

        audit_entries.push(AuditEntry {
            id: entry.id,
            tx_id: entry.tx_id,
            entity: entry.entity,
            trace_id: entry.trace_id,
            origin: entry.origin,
            summary: entry.summary,
            reason: entry.reason,
            change_type: entry.change_type,
            committed_at: entry.committed_at,
            is_breaking: entry.is_breaking,
            signature: entry.signature,
            public_key: entry.public_key,
            risk: entry.risk,
            related_tickets: entry.related_tickets,
            provenance,
        });
    }

    Ok(audit_entries)
}

fn audit_global(
    storage: &StorageManager,
    include_unaudited: bool,
    limit: usize,
    offset: usize,
    json: bool,
) -> Result<()> {
    let report = gather_audit_data(storage, include_unaudited, limit, offset)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).into_diagnostic()?
        );
        return Ok(());
    }

    render_project_audit_human(&report, include_unaudited, limit, offset);
    Ok(())
}

fn render_project_audit_human(
    report: &ProjectAuditReport,
    include_unaudited: bool,
    limit: usize,
    _offset: usize,
) {
    println!("\n{}", "PROJECT VELOCITY".blue().bold());
    println!(
        "  Last 7 Days:   {}",
        report.velocity.last_7_days.to_string().yellow()
    );
    println!(
        "  Last 30 Days:  {}",
        report.velocity.last_30_days.to_string().yellow()
    );
    println!(
        "  Total Commits: {}",
        report.velocity.total.to_string().cyan()
    );
    println!(
        "  Pending:       {}",
        report.velocity.pending.to_string().magenta()
    );
    println!(
        "  Federated:     {}",
        report.velocity.federated.to_string().magenta()
    );

    println!(
        "\n{}",
        format!("TOP CHURNED FILES (Limit: {})", limit)
            .blue()
            .bold()
    );
    if report.churn.is_empty() {
        println!("  None.");
    } else {
        for c in &report.churn {
            println!(
                "  {:<40} {} commits",
                c.entity.cyan(),
                c.count.to_string().yellow()
            );
        }
    }

    if include_unaudited {
        println!("\n{}", "UNAUDITED DRIFT".red().bold());
        if report.unaudited_drift.is_empty() {
            println!("  None.");
        } else {
            for d in &report.unaudited_drift {
                println!("  {:<40} {}", d.file_path.cyan(), d.change_type.yellow());
            }
        }
    }

    println!(
        "\n{}",
        format!("TOP HOTSPOTS (Limit: {})", limit).yellow().bold()
    );
    if report.hotspots.is_empty() {
        println!("  None.");
    } else {
        for h in &report.hotspots {
            println!(
                "  {:<40} score: {:.2}",
                h.path.display().to_string().cyan(),
                h.display_score.yellow()
            );
        }
    }

    println!("\n{}", format!("CI TREND (Last {})", limit).yellow().bold());
    if report.ci_trend.is_empty() {
        println!("  No history yet.");
    } else {
        let mut trend_str = String::new();
        for passed in report.ci_trend.iter().rev() {
            if *passed {
                trend_str.push_str(&"PASS".green().to_string());
            } else {
                trend_str.push_str(&"FAIL".red().to_string());
            }
            trend_str.push(' ');
        }
        println!("  {}", trend_str);
    }

    println!("\n{}", "RECENT COMMITTED ENTRIES".green().bold());
    if report.recent_entries.is_empty() {
        println!("  None.");
    } else {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_header(vec![
                Cell::new("ID").fg(Color::Cyan),
                Cell::new("TX ID").fg(Color::Cyan),
                Cell::new("Entity").fg(Color::Cyan),
                Cell::new("Change").fg(Color::Cyan),
                Cell::new("Summary").fg(Color::Cyan),
                Cell::new("Committed").fg(Color::Cyan),
            ]);

        for entry in &report.recent_entries {
            let entity_display = if entry.provenance.is_empty() {
                entry.entity.clone()
            } else {
                entry.provenance[0].entity.clone()
            };

            table.add_row(vec![
                Cell::new(entry.id.to_string()),
                Cell::new(&entry.tx_id[..8]).fg(Color::Yellow),
                Cell::new(&entity_display).fg(Color::Cyan),
                Cell::new(format!(
                    "{} {:?}",
                    get_change_type_icon(&entry.change_type),
                    entry.change_type
                )),
                Cell::new(&entry.summary),
                Cell::new(&entry.committed_at),
            ]);
        }
        println!("{table}");
    }
}

fn audit_entity(
    manager: &TransactionManager,
    entity: &str,
    resolved_file: Option<&str>,
    limit: usize,
    offset: usize,
    json: bool,
) -> Result<()> {
    let db = LedgerDb::new(manager.get_connection());
    let mut exact_entries = manager
        .get_ledger_entries_paginated(entity, limit, offset)
        .map_err(|e| miette::miette!("{}", e))?;

    // Also try to find transactions via token provenance if resolved_file is present
    if let Some(file_path) = resolved_file {
        let file_entries = db
            .find_transactions_by_file(file_path)
            .map_err(|e| miette::miette!("{}", e))?;
        let mut seen: std::collections::HashSet<String> =
            exact_entries.iter().map(|e| e.tx_id.clone()).collect();
        for fe in file_entries {
            // Check if fe.entity_normalized == resolved_file to count it as exact
            if fe.entity_normalized == file_path {
                if seen.insert(fe.tx_id.clone()) {
                    exact_entries.push(fe);
                }
            } else {
                // Ignore token provenance from other files for exact match
            }
        }
    }

    // Sort exact entries descending by committed_at
    exact_entries.sort_by(|a, b| b.committed_at.cmp(&a.committed_at));
    exact_entries.truncate(limit);

    // Get related entries
    let related_entries = if let Some(file_path) = resolved_file {
        let dir = std::path::Path::new(file_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("");

        if !dir.is_empty() {
            let mut related = db
                .get_related_ledger_entries(dir, limit)
                .map_err(|e| miette::miette!("{}", e))?;

            // Filter out exact matches
            let exact_ids: std::collections::HashSet<String> =
                exact_entries.iter().map(|e| e.tx_id.clone()).collect();

            related.retain(|e| !exact_ids.contains(&e.tx_id));
            related
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    if json {
        let audit_exact = audit_entries_from_ledger_entries(&db, exact_entries)?;
        let audit_related = audit_entries_from_ledger_entries(&db, related_entries)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "exact": audit_exact,
                "related": audit_related
            }))
            .into_diagnostic()?
        );
        return Ok(());
    }

    println!("\nAudit History for {}:", entity.cyan());

    if exact_entries.is_empty() {
        println!("  No exact committed entries found.");
    } else {
        print_audit_entry_list(&db, entity, &exact_entries)?;
    }

    if !related_entries.is_empty() {
        println!(
            "\n{}",
            "--- Related Entries (Adjacent Modules/Directory) ---".dimmed()
        );
        print_audit_entry_list(&db, entity, &related_entries)?;
    }

    Ok(())
}

fn print_audit_entry_list(
    db: &LedgerDb,
    search_entity: &str,
    entries: &[crate::ledger::types::LedgerEntry],
) -> Result<()> {
    for entry in entries {
        let prefix = if entry.origin == "LOCAL" {
            format!(
                "{} [{:04}]",
                get_status_icon(LedgerStatus::Committed),
                entry.id
            )
            .yellow()
            .to_string()
        } else {
            format!(
                "{} [FEDERATED: {}]",
                get_status_icon(LedgerStatus::Federated),
                entry.trace_id.as_deref().unwrap_or("UNKNOWN")
            )
            .magenta()
            .bold()
            .to_string()
        };

        println!("\n{} committed on {}", prefix, entry.committed_at.dimmed());
        println!("  Entity:  {}", entry.entity_normalized.cyan());
        println!("  Summary: {}", entry.summary.bold());
        println!(
            "  Change:  {} {:?}",
            get_change_type_icon(&entry.change_type),
            entry.change_type
        );
        println!("  Reason:  {}", entry.reason);
        if let Some(risk) = &entry.risk {
            println!("  Risk:    {}", risk.yellow());
        }
        if let Some(sig) = &entry.signature {
            let display_sig = if sig.len() > 16 { &sig[..16] } else { sig };
            println!("  Sig:     {}...", display_sig.to_string().dimmed());
        }

        let provenance = db
            .get_token_provenance_for_tx(&entry.tx_id)
            .map_err(|e| miette::miette!("{}", e))?;
        let entity_prov: Vec<_> = provenance
            .into_iter()
            .filter(|p| {
                p.entity == search_entity
                    || p.entity_normalized == search_entity
                    || p.entity_normalized == entry.entity_normalized
            })
            .collect();

        if !entity_prov.is_empty() {
            println!("  Symbols:");
            for p in entity_prov {
                let action_str = p.action.to_string();
                let formatted = match p.action {
                    crate::ledger::provenance::ProvenanceAction::Added => {
                        action_str.green().to_string()
                    }
                    crate::ledger::provenance::ProvenanceAction::Modified => {
                        action_str.blue().to_string()
                    }
                    crate::ledger::provenance::ProvenanceAction::Deleted => {
                        action_str.red().to_string()
                    }
                };
                println!(
                    "    {} {:<10} {} ({})",
                    "•".dimmed(),
                    formatted,
                    p.symbol_name,
                    p.symbol_type.dimmed()
                );
            }
        }
    }
    Ok(())
}
