use crate::commands::doctor::finding::{DoctorCategory, DoctorFinding};
use crate::commands::doctor::remediation::build_surfaces_gated_finding;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;

/// Optional informational hint: suggest sccache for cold/CI builds when no
/// `RUSTC_WRAPPER` is set. Severity **info** / category **optional** (0109).
fn sccache_hint_finding() -> Option<DoctorFinding> {
    if std::env::var("RUSTC_WRAPPER").is_err() {
        Some(DoctorFinding::info(
            "sccache-hint",
            DoctorCategory::Optional,
            "Cold or CI builds may benefit from sccache 0.17.0+. Set RUSTC_WRAPPER=sccache and CARGO_INCREMENTAL=0. Note: do not combine with CARGO_INCREMENTAL=1; use one or the other.",
        ))
    } else {
        None
    }
}

/// 0119: when a signed chain head exists, remind operators to retain checkpoints
/// off-machine. Info + Optional — never blocks readyForPublish / dashboard_failures.
/// No head or unsigned head → no finding.
pub(crate) fn chain_checkpoint_practice_finding(
    conn: &rusqlite::Connection,
) -> Option<DoctorFinding> {
    let db = crate::ledger::db::LedgerDb::new(conn);
    let head = db.get_chain_head().ok()??;
    let sig = head.head_signature.as_deref().unwrap_or("");
    let pub_key = head.head_public_key.as_deref().unwrap_or("");
    if sig.is_empty() || pub_key.is_empty() {
        return None;
    }
    Some(DoctorFinding::info(
        "chain-checkpoint-practice",
        DoctorCategory::Optional,
        "Signed chain head present. Periodically run `ledgerful export head`, retain the file outside this machine and outside `.ledgerful/`, then `ledgerful verify --signatures --against-export <path>` (checkpoint: live must extend or equal the retained head). See docs/chain-checkpoint.md.",
    ))
}

/// 0110: light team-sync honesty findings.
///
/// Only emit when `[sync].enabled = true` and init/target are incomplete.
/// Severity is **warn** / category **optional** so sync-off never sole-blocks
/// `readyForPublish`. See `docs/team-sync.md`.
fn sync_doctor_findings(
    layout: &Layout,
    config: &crate::config::model::Config,
) -> Vec<DoctorFinding> {
    if !config.sync.enabled {
        return Vec::new();
    }

    let mut findings = Vec::new();
    let key_path = layout.state_dir.join("sync").join("device.key");
    let pub_path = layout.state_dir.join("sync").join("device.pub");
    let keys_ok = key_path.exists() && pub_path.exists();

    // SoT: non-empty sync_state.device_id (row id=1). Missing DB is treated as uninitialized.
    let sot_ok = (|| {
        let storage = crate::state::storage::StorageManager::init_with_layout(layout).ok()?;
        let conn = storage.get_connection();
        let id: Option<String> = conn
            .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .ok();
        id.filter(|s| !s.trim().is_empty() && s != "unknown")
    })()
    .is_some();

    if !keys_ok || !sot_ok {
        findings.push(DoctorFinding::warn(
            "sync-enabled-not-initialized",
            DoctorCategory::Optional,
            "[sync].enabled=true but team sync is not fully initialized (need device.key + device.pub + sync_state.device_id). Run `ledgerful sync init` or set enabled=false. Run `ledgerful sync setup` for a readiness checklist. See docs/team-sync.md.",
        ));
    }
    if config.sync.target.trim().is_empty() {
        findings.push(DoctorFinding::warn(
            "sync-enabled-empty-target",
            DoctorCategory::Optional,
            "[sync].enabled=true but [sync].target is empty. Set a shared-folder target (e.g. dir:///path) or disable sync. Run `ledgerful sync setup` for a readiness checklist. See docs/team-sync.md.",
        ));
    }
    // 0111: enabled with zero trusted peers — actionable next step; never sole-blocks publish.
    // Do not treat list IO errors as zero peers (honesty — same class as status R1-04).
    #[cfg(feature = "sync")]
    if keys_ok && sot_ok {
        let sync_dir = layout.state_dir.join("sync");
        match crate::sync::peers::list_peers(sync_dir.as_std_path()) {
            Ok(peers) if peers.is_empty() => {
                findings.push(DoctorFinding::warn(
                    "sync-enabled-no-peers",
                    DoctorCategory::Optional,
                    "[sync].enabled=true but no trusted peers under sync/peers/. Exchange LF-PAIR-1 invites with `ledgerful sync pair` (mutual accept) or disable sync. Run `ledgerful sync setup` for a readiness checklist. See docs/team-sync.md.",
                ));
            }
            Ok(_) => {}
            Err(e) => {
                findings.push(DoctorFinding::warn(
                    "sync-peers-list-error",
                    DoctorCategory::Optional,
                    format!(
                        "[sync].enabled=true but trusted peer list could not be read: {e}. Check permissions on sync/peers/. Run `ledgerful sync setup` for a readiness checklist. See docs/team-sync.md."
                    ),
                ));
            }
        }
    }
    findings
}

/// Timings, SCIP, sccache, chain-checkpoint, sync, surfaces (after spawn).
pub(crate) fn collect_optional_findings(
    config: &crate::config::model::Config,
    layout: &Layout,
    storage: &StorageManager,
) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();
    // Track 0043: warn on oversized / high-cardinality timing tables.
    for (i, w) in crate::commands::timings::doctor_timing_warnings(storage.get_connection())
        .into_iter()
        .enumerate()
    {
        findings.push(DoctorFinding::warn(
            format!("timings-{i}"),
            DoctorCategory::Other,
            w,
        ));
    }
    // SCIP + sccache → info / optional
    findings.extend(crate::scip::presence::collect_scip_findings(
        config,
        &layout.root,
    ));
    if let Some(f) = sccache_hint_finding() {
        findings.push(f);
    }

    // 0119: operator chain-head retention hygiene (info/optional only).
    if let Some(f) = chain_checkpoint_practice_finding(storage.get_connection()) {
        findings.push(f);
    }

    // 0110: light team-sync findings (warn/info only). Disabled sync never blocks publish.
    findings.extend(sync_doctor_findings(layout, config));

    // 0185: coverage-gated advanced surfaces (Info/Optional; collapsed unless --full).
    match crate::commands::surfaces::classify_surfaces(config, layout, storage) {
        Ok(report) => {
            let ids = crate::commands::surfaces::gated_ids(&report);
            if let Some(finding) = build_surfaces_gated_finding(&ids) {
                findings.push(finding);
            }
        }
        Err(e) => {
            tracing::debug!("doctor surfaces inventory probe failed: {e}");
        }
    }

    findings
}
