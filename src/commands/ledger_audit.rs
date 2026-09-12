use comfy_table::{Cell, Color, Table};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use serde::Serialize;

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::impact::budget::{
    AnalysisBudget, CompletenessFilter, apply_resolved_history_budget, completeness_for_error,
    completeness_for_walk, eprint_walk_stop,
};
use crate::impact::hotspots::calculate_hotspots_detailed;
use crate::impact::packet::Hotspot;
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::db::LedgerDb;
use crate::ledger::transaction::TransactionManager;
use crate::ledger::types::LedgerEntry;
use crate::ledger::ui::{LedgerStatus, get_change_type_icon, get_status_icon, with_icon};
use crate::output::table::{apply_table_style, resolve_table_style};
use crate::state::storage::StorageManager;
use crate::verify::results::VERIFY_HISTORY;
use std::path::Path;

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAuditReport {
    pub velocity: VelocitySummary,
    pub churn: Vec<ChurnEntry>,
    pub unaudited_drift: Vec<DriftEntry>,
    pub hotspots: Vec<Hotspot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completeness: Option<crate::impact::budget::AnalysisCompleteness>,
    pub ci_trend: Vec<bool>,
    /// True when `VERIFY_HISTORY` exists but JSON parse failed. Omitted when false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ci_trend_corrupt: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_basis: Option<String>,
    #[serde(skip)]
    pub category: crate::ledger::types::Category,
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
    timeout: Option<u64>,
) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::open_read_only_sqlite_only(&layout)?;

    if !json {
        println!(
            "{}",
            "Ledgerful Project Audit"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
        );
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
        audit_global(&storage, include_unaudited, limit, offset, json, timeout)?;
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
    timeout: Option<u64>,
) -> Result<ProjectAuditReport> {
    let layout = get_layout()?;
    let db = LedgerDb::new(storage.get_connection());
    let mut config = load_ledger_config(&layout)?;
    apply_resolved_history_budget(&mut config, timeout);

    // Opening a second connection for the manager avoids borrow conflicts
    let mut storage_mut = StorageManager::open_read_only_sqlite_only(&layout)?;
    let manager =
        TransactionManager::new(&mut storage_mut, layout.root.clone().into(), config.clone());

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
    let cancel = crate::impact::budget::install_cancel_flag();
    let commits = config.hotspots.max_commits;
    let query = crate::impact::hotspots::HotspotQuery {
        commits,
        limit,
        decay_half_life: config.hotspots.decay_half_life,
        budget: Some(AnalysisBudget::from_secs(
            config.hotspots.history_budget_secs,
            cancel,
        )),
        ..Default::default()
    };
    let (hotspots, completeness) =
        match calculate_hotspots_detailed(storage, &history_provider, &query) {
            Ok(calc) => {
                eprint_walk_stop(calc.walk_stop, calc.commits_walked, commits);
                let completeness = completeness_for_walk(
                    calc.walk_stop,
                    commits as u64,
                    calc.commits_walked as u64,
                    query.days,
                    CompletenessFilter::Unfiltered,
                    calc.head,
                    Some(config.hotspots.history_budget_secs).filter(|s| *s > 0),
                );
                (calc.hotspots, completeness)
            }
            Err(e) => {
                tracing::warn!(error = %e, "audit hotspots history failed");
                eprintln!("warning: audit hotspots unavailable: {e}");
                (
                    Vec::new(),
                    Some(completeness_for_error(
                        commits as u64,
                        query.days,
                        CompletenessFilter::Unfiltered,
                        Some(config.hotspots.history_budget_secs).filter(|s| *s > 0),
                    )),
                )
            }
        };

    let history_path = layout.reports_dir().join(VERIFY_HISTORY);
    let mut ci_trend_corrupt = false;
    let ci_trend = if history_path.exists() {
        let content = std::fs::read_to_string(&history_path).into_diagnostic()?;
        match crate::verify::results::parse_verify_history(&content) {
            Ok(history) => history
                .into_iter()
                .rev()
                .take(limit)
                .map(|h| h.passed)
                .collect(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %history_path,
                    "failed to parse verify history JSON"
                );
                ci_trend_corrupt = true;
                vec![]
            }
        }
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
        completeness,
        ci_trend,
        ci_trend_corrupt,
        recent_entries,
    })
}

fn audit_entry_from_ledger(
    entry: LedgerEntry,
    provenance: Vec<ProvenanceEntry>,
    match_basis: Option<&str>,
) -> AuditEntry {
    let reason_kind =
        crate::ledger::reason::classify_reason_kind(&entry.reason).map(|s| s.to_string());
    let risk_source =
        crate::ledger::reason::classify_risk_source(entry.risk.as_deref(), entry.category)
            .map(|s| s.to_string());
    AuditEntry {
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
        reason_kind,
        risk_source,
        match_basis: match_basis.map(|s| s.to_string()),
        category: entry.category,
    }
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

        audit_entries.push(audit_entry_from_ledger(entry, provenance, None));
    }

    Ok(audit_entries)
}

fn audit_global(
    storage: &StorageManager,
    include_unaudited: bool,
    limit: usize,
    offset: usize,
    json: bool,
    timeout: Option<u64>,
) -> Result<()> {
    let report = gather_audit_data(storage, include_unaudited, limit, offset, timeout)?;

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

/// Uncolored TOP CHURNED FILES row (count word is `entries`, not `commits`).
pub(crate) fn format_churn_line(entity: &str, count: i64) -> String {
    format!("  {entity:<40} {count} entries")
}

/// Uncolored TOP HOTSPOTS row. Label is `display:` (ln), not `score:` (0–1).
pub(crate) fn format_audit_hotspot_line(path: &Path, display_score: f32) -> String {
    format!("  {:<40} display: {:.2}", path.display(), display_score)
}

fn ci_trend_human_placeholder(ci_trend_corrupt: bool) -> &'static str {
    if ci_trend_corrupt {
        "CI trend unreadable (ciTrendCorrupt)."
    } else {
        "No history yet."
    }
}

fn render_project_audit_human(
    report: &ProjectAuditReport,
    include_unaudited: bool,
    limit: usize,
    _offset: usize,
) {
    println!(
        "\n{}",
        "PROJECT VELOCITY"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold()))
    );
    println!(
        "  Last 7 Days:   {}",
        report
            .velocity
            .last_7_days
            .to_string()
            .if_supports_color(Stream::Stdout, |s| s.yellow())
    );
    println!(
        "  Last 30 Days:  {}",
        report
            .velocity
            .last_30_days
            .to_string()
            .if_supports_color(Stream::Stdout, |s| s.yellow())
    );
    println!(
        "  Total Commits: {}",
        report
            .velocity
            .total
            .to_string()
            .if_supports_color(Stream::Stdout, |s| s.cyan())
    );
    println!(
        "  Pending:       {}",
        report
            .velocity
            .pending
            .to_string()
            .if_supports_color(Stream::Stdout, |s| s.magenta())
    );
    println!(
        "  Federated:     {}",
        report
            .velocity
            .federated
            .to_string()
            .if_supports_color(Stream::Stdout, |s| s.magenta())
    );

    println!(
        "\n{}",
        format!("TOP CHURNED FILES (Limit: {})", limit)
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold()))
    );
    if report.churn.is_empty() {
        println!("  None.");
    } else {
        for c in &report.churn {
            println!(
                "{}",
                format_churn_line(&c.entity, c.count)
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
        }
    }

    if include_unaudited {
        println!(
            "\n{}",
            "UNAUDITED DRIFT"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
        );
        if report.unaudited_drift.is_empty() {
            println!("  None.");
        } else {
            for d in &report.unaudited_drift {
                println!(
                    "  {:<40} {}",
                    d.file_path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                    d.change_type
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                );
            }
        }
    }

    println!(
        "\n{}",
        format!("TOP HOTSPOTS (Limit: {})", limit)
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
    );
    if report.hotspots.is_empty() {
        println!("  None.");
    } else {
        for h in &report.hotspots {
            println!("{}", format_audit_hotspot_line(&h.path, h.display_score));
        }
    }

    println!(
        "\n{}",
        format!("CI TREND (Last {})", limit)
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
    );
    if report.ci_trend_corrupt || report.ci_trend.is_empty() {
        println!("  {}", ci_trend_human_placeholder(report.ci_trend_corrupt));
    } else {
        let mut trend_str = String::new();
        for passed in report.ci_trend.iter().rev() {
            if *passed {
                trend_str.push_str(
                    &"PASS"
                        .if_supports_color(Stream::Stdout, |s| s.green())
                        .to_string(),
                );
            } else {
                trend_str.push_str(
                    &"FAIL"
                        .if_supports_color(Stream::Stdout, |s| s.red())
                        .to_string(),
                );
            }
            trend_str.push(' ');
        }
        println!("  {}", trend_str);
    }

    println!(
        "\n{}",
        "RECENT COMMITTED ENTRIES"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
    );
    if report.recent_entries.is_empty() {
        println!("  None.");
    } else {
        let mut table = Table::new();
        apply_table_style(&mut table, resolve_table_style());
        table.set_header(vec![
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
                Cell::new(with_icon(
                    &get_change_type_icon(&entry.change_type),
                    format!("{:?}", entry.change_type),
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
    let payload = audit_entity_payload(manager, &db, entity, resolved_file, limit, offset)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).into_diagnostic()?
        );
        return Ok(());
    }

    println!(
        "\nAudit History for {}:",
        entity.if_supports_color(Stream::Stdout, |s| s.cyan())
    );

    if payload.exact.is_empty() {
        println!("  No exact committed entries found.");
    } else {
        print_audit_entry_list_from_audit(&payload.exact)?;
    }

    if !payload.related.is_empty() {
        println!(
            "\n{}",
            "--- Related Entries (Adjacent Modules/Directory) ---"
                .if_supports_color(Stream::Stdout, |s| s.dimmed())
        );
        print_audit_entry_list_from_audit(&payload.related)?;
    }

    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditEntityPayload {
    exact: Vec<AuditEntry>,
    related: Vec<AuditEntry>,
}

fn provenance_for_tx(db: &LedgerDb<'_>, tx_id: &str) -> Result<Vec<ProvenanceEntry>> {
    let provenance_data = db
        .get_token_provenance_for_tx(tx_id)
        .map_err(|e| miette::miette!("{}", e))?;
    Ok(provenance_data
        .into_iter()
        .map(|p| ProvenanceEntry {
            entity: p.entity,
            symbol_name: p.symbol_name,
            symbol_type: p.symbol_type,
            action: p.action,
        })
        .collect())
}

pub(crate) fn audit_entity_payload(
    manager: &TransactionManager,
    db: &LedgerDb<'_>,
    entity: &str,
    resolved_file: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<AuditEntityPayload> {
    let mut hits: Vec<(LedgerEntry, Option<&'static str>)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let entity_entries = manager
        .get_ledger_entries_paginated(entity, 10_000, 0)
        .map_err(|e| miette::miette!("{}", e))?;
    let entity_basis = if resolved_file.is_some() {
        Some("entity")
    } else {
        None
    };
    for entry in entity_entries {
        if seen.insert(entry.tx_id.clone()) {
            hits.push((entry, entity_basis));
        }
    }

    if let Some(file_path) = resolved_file {
        let file_entries = db
            .find_transactions_by_file(file_path)
            .map_err(|e| miette::miette!("{}", e))?;
        for fe in file_entries {
            if fe.entity_normalized.replace('\\', "/") == file_path.replace('\\', "/")
                && seen.insert(fe.tx_id.clone())
            {
                hits.push((fe, Some("entity")));
            }
        }

        let changed = db
            .find_ledger_entries_by_changed_file(file_path)
            .map_err(|e| miette::miette!("{}", e))?;
        for fe in changed {
            if seen.insert(fe.tx_id.clone()) {
                hits.push((fe, Some("changed_files")));
            }
        }
    }

    hits.sort_by(|a, b| {
        b.0.committed_at
            .cmp(&a.0.committed_at)
            .then_with(|| b.0.tx_id.cmp(&a.0.tx_id))
    });
    let exact_ids: std::collections::HashSet<String> =
        hits.iter().map(|(e, _)| e.tx_id.clone()).collect();
    let exact_hits: Vec<(LedgerEntry, Option<&'static str>)> =
        hits.into_iter().skip(offset).take(limit).collect();

    let related_entries = if let Some(file_path) = resolved_file {
        let dir = std::path::Path::new(file_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("");

        if !dir.is_empty() {
            let mut related = db
                .get_related_ledger_entries(dir, limit)
                .map_err(|e| miette::miette!("{}", e))?;
            related.retain(|e| !exact_ids.contains(&e.tx_id));
            related
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let mut exact = Vec::new();
    for (entry, basis) in exact_hits {
        let provenance = provenance_for_tx(db, &entry.tx_id)?;
        exact.push(audit_entry_from_ledger(entry, provenance, basis));
    }
    let mut related = Vec::new();
    for entry in related_entries {
        let provenance = provenance_for_tx(db, &entry.tx_id)?;
        let basis = resolved_file.map(|_| "directory");
        related.push(audit_entry_from_ledger(entry, provenance, basis));
    }

    Ok(AuditEntityPayload { exact, related })
}

pub(crate) fn format_audit_reason_line(reason: &str) -> String {
    if crate::ledger::reason::classify_reason_kind(reason) == Some("trailer") {
        format!("[trailer] {reason}")
    } else {
        reason.to_string()
    }
}

pub(crate) fn format_audit_risk_line(
    risk: &str,
    category: crate::ledger::types::Category,
) -> String {
    if crate::ledger::reason::classify_risk_source(Some(risk), category) == Some("category") {
        format!("{risk} (from category {category})")
    } else {
        risk.to_string()
    }
}

fn print_audit_entry_list_from_audit(entries: &[AuditEntry]) -> Result<()> {
    for entry in entries {
        let prefix = if entry.origin == "LOCAL" {
            format!(
                "{} [{:04}]",
                get_status_icon(LedgerStatus::Committed),
                entry.id
            )
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string()
        } else {
            format!(
                "{} [FEDERATED: {}]",
                get_status_icon(LedgerStatus::Federated),
                entry.trace_id.as_deref().unwrap_or("UNKNOWN")
            )
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().magenta().bold()))
            .to_string()
        };

        println!(
            "\n{} committed on {}",
            prefix,
            entry
                .committed_at
                .if_supports_color(Stream::Stdout, |s| s.dimmed())
        );
        println!(
            "  Entity:  {}",
            entry.entity.if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        println!(
            "  Summary: {}",
            entry
                .summary
                .if_supports_color(Stream::Stdout, |s| s.bold())
        );
        println!(
            "  Change:  {}",
            with_icon(
                &get_change_type_icon(&entry.change_type),
                format!("{:?}", entry.change_type),
            )
        );
        println!("  Reason:  {}", format_audit_reason_line(&entry.reason));
        if let Some(risk) = &entry.risk {
            println!(
                "  Risk:    {}",
                format_audit_risk_line(risk, entry.category)
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
            );
        }
        if let Some(sig) = &entry.signature {
            let display_sig = if sig.len() > 16 { &sig[..16] } else { sig };
            println!(
                "  Sig:     {}...",
                display_sig
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.dimmed())
            );
        }
        if !entry.provenance.is_empty() {
            println!("  Symbols:");
            for p in &entry.provenance {
                let action_str = p.action.to_string();
                let formatted = match p.action {
                    crate::ledger::provenance::ProvenanceAction::Added => action_str
                        .if_supports_color(Stream::Stdout, |s| s.green())
                        .to_string(),
                    crate::ledger::provenance::ProvenanceAction::Modified => action_str
                        .if_supports_color(Stream::Stdout, |s| s.blue())
                        .to_string(),
                    crate::ledger::provenance::ProvenanceAction::Deleted => action_str
                        .if_supports_color(Stream::Stdout, |s| s.red())
                        .to_string(),
                };
                println!(
                    "    {} {:<10} {} ({})",
                    "•".if_supports_color(Stream::Stdout, |s| s.dimmed()),
                    formatted,
                    p.symbol_name,
                    p.symbol_type
                        .if_supports_color(Stream::Stdout, |s| s.dimmed())
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_audit_report(ci_trend_corrupt: bool) -> ProjectAuditReport {
        ProjectAuditReport {
            velocity: VelocitySummary {
                last_7_days: 0,
                last_30_days: 0,
                total: 0,
                pending: 0,
                federated: 0,
            },
            churn: vec![],
            unaudited_drift: vec![],
            hotspots: vec![],
            completeness: None,
            ci_trend: vec![],
            ci_trend_corrupt,
            recent_entries: vec![],
        }
    }

    #[test]
    fn verify_history_corrupt_human_does_not_say_no_history_yet() {
        let corrupt = ci_trend_human_placeholder(true);
        assert!(
            corrupt.contains("ciTrendCorrupt"),
            "corrupt placeholder must be greppable: {corrupt}"
        );
        assert!(
            !corrupt.contains("No history yet."),
            "file exists but unreadable must not look like missing history"
        );
        assert_eq!(ci_trend_human_placeholder(false), "No history yet.");
    }

    #[test]
    fn verify_history_ci_trend_corrupt_false_is_quiet_in_json() {
        let json = serde_json::to_value(empty_audit_report(false)).unwrap();
        assert!(
            json.get("ciTrendCorrupt").is_none(),
            "false must skip_serializing_if: {json}"
        );
    }

    #[test]
    fn verify_history_ci_trend_corrupt_true_is_serialized() {
        let json = serde_json::to_value(empty_audit_report(true)).unwrap();
        assert_eq!(json["ciTrendCorrupt"], true);
    }

    #[test]
    fn audit_history_error_is_not_silent_empty() {
        let mut report = empty_audit_report(false);
        report.completeness = Some(crate::impact::budget::completeness_for_error(
            500,
            None,
            crate::impact::budget::CompletenessFilter::Unfiltered,
            Some(45),
        ));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["hotspots"].as_array().map(Vec::len), Some(0));
        assert_eq!(json["completeness"]["stop"], "error");
        assert!(
            json["completeness"].get("commitsWalked").is_none(),
            "error must omit commitsWalked: {json}"
        );
    }

    #[test]
    fn audit_budget_emits_completeness() {
        let mut report = empty_audit_report(false);
        report.completeness = crate::impact::budget::completeness_for_walk(
            crate::impact::budget::HistoryWalkStop::Budget,
            500,
            4,
            None,
            crate::impact::budget::CompletenessFilter::Unfiltered,
            Some("deadbeef".to_string()),
            Some(45),
        );
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["completeness"]["stop"], "budget");
        assert!(
            json["completeness"]["commitsWalked"].as_u64().unwrap()
                < json["completeness"]["commitsRequested"].as_u64().unwrap()
        );
        assert!(json.get("hotspots").is_some(), "hotspots sibling required");
    }

    #[test]
    fn format_churn_line_uses_entries_not_commits() {
        let line = format_churn_line("CHANGELOG.md", 12);
        assert!(
            line.contains("entries"),
            "churn unit must be entries: {line}"
        );
        assert!(
            !line.contains("commits"),
            "churn unit must not be commits: {line}"
        );
        assert!(line.contains("CHANGELOG.md"));
        assert!(line.contains("12"));
    }

    #[test]
    fn format_audit_hotspot_line_uses_display_not_score() {
        let line = format_audit_hotspot_line(std::path::Path::new("src/lib.rs"), 3.29);
        assert!(
            line.contains("display:"),
            "hotspot unit must be display: {line}"
        );
        assert!(
            !line.contains("score:"),
            "hotspot unit must not be score: {line}"
        );
        assert!(line.contains("src/lib.rs"));
        assert!(line.contains("3.29"));
    }

    #[test]
    fn audit_human_trailer_reason_prefixed() {
        let trailer = "Co-authored-by: Cursor <cursoragent@cursor.com>";
        let reason = format_audit_reason_line(trailer);
        assert!(
            reason.starts_with("[trailer] "),
            "human Reason must prefix [trailer]: {reason}"
        );
        assert!(reason.contains(trailer));
        let risk = format_audit_risk_line("HIGH", crate::ledger::types::Category::Bugfix);
        assert_eq!(risk, "HIGH (from category BUGFIX)");
        let prose = format_audit_reason_line("Store a substantive why.");
        assert_eq!(prose, "Store a substantive why.");
        let printed_reason = format!("  Reason:  {reason}");
        let printed_risk = format!("  Risk:    {risk}");
        assert!(
            printed_reason.contains("Reason:") && printed_reason.contains("[trailer]"),
            "printer line must keep Reason: + [trailer]: {printed_reason}"
        );
        assert!(
            printed_risk.contains("Risk:") && printed_risk.contains("(from category BUGFIX)"),
            "printer line must keep Risk: + category source: {printed_risk}"
        );
    }

    fn with_changed_files_fixture(
        f: impl FnOnce(&crate::ledger::TransactionManager, &crate::ledger::db::LedgerDb<'_>, &str, &str),
    ) {
        with_changed_files_fixture_setup(|_| {}, f);
    }

    fn with_changed_files_fixture_setup(
        extra: impl FnOnce(&mut crate::ledger::TransactionManager),
        f: impl FnOnce(&crate::ledger::TransactionManager, &crate::ledger::db::LedgerDb<'_>, &str, &str),
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("ledger.db");
        let mut storage =
            crate::state::storage::StorageManager::init(&db_path).expect("init storage");
        let mut manager = crate::ledger::TransactionManager::new(
            &mut storage,
            tmp.path().to_path_buf(),
            crate::config::model::Config::default(),
        );
        let file_path = "src/commands/configure.rs";
        let snapshot_id = {
            let conn = manager.get_connection();
            conn.execute(
                "INSERT INTO snapshots (timestamp, head_hash, branch_name, is_clean, packet_json)
                 VALUES ('2026-09-12T00:00:00Z', 'deadbeef', 'main', 1, '{}')",
                [],
            )
            .expect("insert snapshot");
            conn.last_insert_rowid()
        };
        manager
            .get_connection()
            .execute(
                "INSERT INTO changed_files (snapshot_id, path, status, is_staged) VALUES (?1, ?2, 'MODIFIED', 1)",
                rusqlite::params![snapshot_id, file_path],
            )
            .expect("insert changed_files");

        let track_tx = manager
            .start_change(crate::ledger::TransactionRequest {
                category: crate::ledger::types::Category::Bugfix,
                entity: "0319-fixture-track".to_string(),
                ..Default::default()
            })
            .expect("start track tx");
        manager
            .commit_change(
                track_tx.clone(),
                crate::ledger::CommitRequest {
                    summary: "fixture track commit".to_string(),
                    reason: "Co-authored-by: Cursor <cursoragent@cursor.com>".to_string(),
                    risk: Some("HIGH".to_string()),
                    snapshot_id: Some(snapshot_id),
                    ..Default::default()
                },
                false,
            )
            .expect("commit track tx");

        let neighbor_tx = manager
            .start_change(crate::ledger::TransactionRequest {
                category: crate::ledger::types::Category::Docs,
                entity: "src/commands/other.rs".to_string(),
                ..Default::default()
            })
            .expect("start neighbor");
        manager
            .commit_change(
                neighbor_tx,
                crate::ledger::CommitRequest {
                    summary: "neighbor".to_string(),
                    reason: "Store a substantive why.".to_string(),
                    risk: Some("TRIVIAL".to_string()),
                    ..Default::default()
                },
                false,
            )
            .expect("commit neighbor");

        extra(&mut manager);

        let db = crate::ledger::db::LedgerDb::new(manager.get_connection());
        f(&manager, &db, &track_tx, file_path);
    }

    #[test]
    fn audit_exact_includes_changed_files_track_entity() {
        with_changed_files_fixture(|manager, db, track_id, file_path| {
            let payload = audit_entity_payload(manager, db, file_path, Some(file_path), 20, 0)
                .expect("payload");
            let exact = payload
                .exact
                .iter()
                .find(|e| e.tx_id == track_id)
                .unwrap_or_else(|| panic!("expected track tx in exact: {:?}", payload.exact));
            assert_eq!(
                exact.match_basis.as_deref(),
                Some("changed_files"),
                "track-slug TX must join via changed_files"
            );
            assert_eq!(exact.reason_kind.as_deref(), Some("trailer"));

            let v = serde_json::to_value(&payload).expect("serialize {exact, related}");
            assert!(
                v.get("exact").is_some() && v.get("related").is_some(),
                "{v}"
            );
            let item = v["exact"]
                .as_array()
                .expect("exact")
                .iter()
                .find(|e| e["txId"] == track_id)
                .unwrap_or_else(|| panic!("expected track tx in exact JSON: {v}"));
            assert_eq!(item["matchBasis"], "changed_files");
            assert_eq!(item["reasonKind"], "trailer");
            assert_eq!(item["riskSource"], "category");
            assert!(
                item.get("match_basis").is_none(),
                "audit JSON must be camelCase: {item}"
            );
            assert!(
                item.get("reason_kind").is_none(),
                "audit JSON must be camelCase: {item}"
            );
        });
    }

    #[test]
    fn audit_related_is_directory_not_changed_file() {
        with_changed_files_fixture(|manager, db, track_id, file_path| {
            let payload = audit_entity_payload(manager, db, file_path, Some(file_path), 20, 0)
                .expect("payload");
            assert!(
                payload
                    .exact
                    .iter()
                    .all(|e| e.entity != "src/commands/other.rs"),
                "directory neighbor must not be exact"
            );
            let related = payload
                .related
                .iter()
                .find(|e| e.entity == "src/commands/other.rs")
                .expect("neighbor in related");
            assert_eq!(related.match_basis.as_deref(), Some("directory"));
            assert!(
                payload.related.iter().all(|e| e.tx_id != track_id),
                "exact tx must be filtered from related"
            );

            let v = serde_json::to_value(&payload).expect("serialize {exact, related}");
            let related_json = v["related"]
                .as_array()
                .expect("related")
                .iter()
                .find(|e| e["entity"] == "src/commands/other.rs")
                .unwrap_or_else(|| panic!("expected neighbor in related JSON: {v}"));
            assert_eq!(related_json["matchBasis"], "directory");
        });
    }

    #[test]
    fn audit_match_basis_omitted_on_non_file_entity() {
        with_changed_files_fixture(|manager, db, track_id, _file_path| {
            let payload = audit_entity_payload(manager, db, "0319-fixture-track", None, 20, 0)
                .expect("payload");
            let exact = payload
                .exact
                .iter()
                .find(|e| e.tx_id == track_id)
                .expect("track slug exact");
            assert!(
                exact.match_basis.is_none(),
                "non-file audit must omit matchBasis"
            );
            let v = serde_json::to_value(&payload).expect("json");
            let item = v["exact"]
                .as_array()
                .expect("exact")
                .iter()
                .find(|e| e["txId"] == track_id)
                .expect("track json");
            assert!(
                item.get("matchBasis").is_none(),
                "non-file audit JSON must omit matchBasis: {item}"
            );
        });
    }

    #[test]
    fn audit_offset_limit_applies_to_changed_files_union() {
        let second_id = std::cell::RefCell::new(String::new());
        with_changed_files_fixture_setup(
            |manager| {
                let snapshot_id: i64 = manager
                    .get_connection()
                    .query_row(
                        "SELECT snapshot_id FROM transactions WHERE entity = '0319-fixture-track'",
                        [],
                        |row| row.get(0),
                    )
                    .expect("first snapshot");
                let second = manager
                    .start_change(crate::ledger::TransactionRequest {
                        category: crate::ledger::types::Category::Bugfix,
                        entity: "0319-fixture-track-b".to_string(),
                        ..Default::default()
                    })
                    .expect("start second");
                manager
                    .commit_change(
                        second.clone(),
                        crate::ledger::CommitRequest {
                            summary: "second track commit".to_string(),
                            reason: "Co-authored-by: Cursor <cursoragent@cursor.com>".to_string(),
                            risk: Some("HIGH".to_string()),
                            snapshot_id: Some(snapshot_id),
                            ..Default::default()
                        },
                        false,
                    )
                    .expect("commit second");
                *second_id.borrow_mut() = second;
            },
            |manager, db, track_id, file_path| {
                let second = second_id.borrow();
                let page0 = audit_entity_payload(manager, db, file_path, Some(file_path), 1, 0)
                    .expect("page0");
                let page1 = audit_entity_payload(manager, db, file_path, Some(file_path), 1, 1)
                    .expect("page1");
                assert_eq!(page0.exact.len(), 1, "offset 0 limit 1");
                assert_eq!(page1.exact.len(), 1, "offset 1 limit 1 on the union");
                assert_ne!(
                    page0.exact[0].tx_id, page1.exact[0].tx_id,
                    "pages must be distinct union slices"
                );
                let ids: std::collections::HashSet<_> = page0
                    .exact
                    .iter()
                    .chain(page1.exact.iter())
                    .map(|e| e.tx_id.as_str())
                    .collect();
                assert!(
                    ids.contains(track_id) && ids.contains(second.as_str()),
                    "union paging must include both changed_files TXs: {ids:?}"
                );
            },
        );
    }

    #[test]
    fn audit_related_excludes_exact_ids_outside_page() {
        let entity_exact_id = std::cell::RefCell::new(String::new());
        with_changed_files_fixture_setup(
            |manager| {
                let entity_exact = manager
                    .start_change(crate::ledger::TransactionRequest {
                        category: crate::ledger::types::Category::Bugfix,
                        entity: "src/commands/configure.rs".to_string(),
                        ..Default::default()
                    })
                    .expect("start entity-exact");
                manager
                    .commit_change(
                        entity_exact.clone(),
                        crate::ledger::CommitRequest {
                            summary: "entity-exact file commit".to_string(),
                            reason: "Store a substantive why.".to_string(),
                            risk: Some("HIGH".to_string()),
                            ..Default::default()
                        },
                        false,
                    )
                    .expect("commit entity-exact");
                *entity_exact_id.borrow_mut() = entity_exact;
            },
            |manager, db, track_id, file_path| {
                let entity_exact = entity_exact_id.borrow();
                let all = audit_entity_payload(manager, db, file_path, Some(file_path), 20, 0)
                    .expect("all");
                let exact_ids: std::collections::HashSet<_> =
                    all.exact.iter().map(|e| e.tx_id.as_str()).collect();
                assert!(
                    exact_ids.contains(track_id) && exact_ids.contains(entity_exact.as_str()),
                    "union must include changed_files + entity-exact: {exact_ids:?}"
                );
                assert!(
                    all.related
                        .iter()
                        .any(|e| e.entity == "src/commands/other.rs"),
                    "directory neighbor must remain related on the unpaged union"
                );
                let page0 = audit_entity_payload(manager, db, file_path, Some(file_path), 1, 0)
                    .expect("page0");
                let page1 = audit_entity_payload(manager, db, file_path, Some(file_path), 1, 1)
                    .expect("page1");
                assert_eq!(page0.exact.len(), 1);
                assert_eq!(page1.exact.len(), 1);
                assert_ne!(page0.exact[0].tx_id, page1.exact[0].tx_id);
                for (label, page) in [("page0", &page0), ("page1", &page1)] {
                    let leaked: Vec<_> = page
                        .related
                        .iter()
                        .filter(|e| exact_ids.contains(e.tx_id.as_str()))
                        .map(|e| e.tx_id.as_str())
                        .collect();
                    assert!(
                        leaked.is_empty(),
                        "{label} related must exclude every exact tx_id (including off-page): {leaked:?}"
                    );
                }
            },
        );
    }
}
