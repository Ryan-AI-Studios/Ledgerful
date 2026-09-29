//! `ledgerful surfaces` (alias `tour`) — read-only advanced-surface inventory (0185).
//!
//! Closed six-row map: ready / empty / gated. Ready is live-command index/config
//! data only; filesystem presence only chooses empty `reason` / `next`.

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::output::session_notice::{
    SessionNoticeId, attach_session_notices, collapsed_next, notice_id_for_surface,
};
use crate::output::table::build_premium_table;
use crate::state::cli_session::{CliSession, env_session_id};
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};

pub use crate::surfaces::{
    SurfaceCounts, SurfaceItem, SurfaceProbes, SurfaceStatus, SurfacesReport, classify_from_probes,
    classify_surfaces, gated_ids, repo_root_cedar_present, repo_root_openslo_present,
};

pub fn execute_surfaces(json: bool) -> Result<()> {
    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;
    let storage = StorageManager::open_read_only(&layout)?;
    let report = classify_surfaces(&config, &layout, &storage)?;
    let mut session = CliSession::load(&layout, env_session_id().as_deref(), Utc::now());
    let mut already: Vec<SessionNoticeId> = Vec::new();
    let mut participating: Vec<SessionNoticeId> = Vec::new();
    for s in &report.surfaces {
        if let Some(id) = notice_id_for_surface(s.id.as_str(), s.status.as_str(), s.gate.as_str()) {
            participating.push(id);
            if session.is_shown(id.as_str()) {
                already.push(id);
            }
        }
    }
    if json {
        let mut value = serde_json::to_value(&report).into_diagnostic()?;
        attach_session_notices(&mut value, already);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).into_diagnostic()?
        );
    } else {
        print_human_report(&report, &session);
    }
    for id in participating {
        session.mark_shown(id.as_str());
    }
    session.persist();
    Ok(())
}

fn print_human_report(report: &SurfacesReport, session: &CliSession) {
    let mut table = build_premium_table(["Surface", "Status", "Why", "Next"]);
    for s in &report.surfaces {
        let next = if let Some(id) =
            notice_id_for_surface(s.id.as_str(), s.status.as_str(), s.gate.as_str())
        {
            collapsed_next(session.is_shown(id.as_str()), s.next.as_str())
        } else {
            s.next.clone()
        };
        table.add_row(vec![
            s.name.clone(),
            s.status.as_str().to_string(),
            s.reason.clone(),
            next,
        ]);
    }
    println!("{table}");
    println!(
        "{} gated · {} empty · {} ready",
        report.counts.gated, report.counts.empty, report.counts.ready
    );
    if report.counts.gated == 0 && report.counts.empty == 0 && report.counts.ready == 6 {
        println!("All listed advanced surfaces are populated.");
    }
}
