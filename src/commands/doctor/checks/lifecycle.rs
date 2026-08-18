use crate::commands::doctor::finding::{DoctorCategory, DoctorFinding};
use crate::commands::doctor::remediation::{build_sig_pin_finding, build_sig_version_finding};
use crate::output::human::DoctorReport;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream};

/// Four-surface legacy migration checks (0094 DoD-6). Structured findings with
/// remediation in messages. Empty on a fully migrated repo.
pub(crate) fn collect_legacy_migration_findings(
    root: &camino::Utf8Path,
    layout: &Layout,
) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();

    // 1. Legacy state directory still present (report only — never merge/delete).
    let legacy_dir = root.join(crate::state::layout::LEGACY_STATE_DIR);
    if legacy_dir.is_dir() {
        let ledger_db = legacy_dir.join("state").join("ledger.db");
        if ledger_db.is_file() {
            findings.push(DoctorFinding::warn(
                "legacy-state",
                DoctorCategory::Migration,
                format!(
                    "retired state directory `{legacy_dir}` still present and contains ledger.db (not merged automatically). Current state is `{}`. After verifying the active ledger, remove the legacy directory manually if unused.",
                    layout.state_dir
                ),
            ));
        } else {
            findings.push(DoctorFinding::warn(
                "legacy-state",
                DoctorCategory::Migration,
                format!(
                    "retired state directory `{legacy_dir}` still present (empty or no ledger.db). Safe to remove manually after confirming `{}` is current.",
                    layout.state_dir
                ),
            ));
        }
    }

    // 2. Hooks: legacy invocations / markers / duplicates / RT-H5 inert gate.
    findings.extend(crate::commands::hook_repair::doctor_legacy_hook_findings(
        root,
    ));

    // 3. .gitignore names only the legacy path (not .ledgerful/).
    findings.extend(doctor_gitignore_legacy_findings(root));

    // 4. Config staleness / unknown keys (serde_ignored; no deny_unknown_fields).
    findings.extend(crate::config::load::doctor_config_findings(layout));

    findings.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    findings.dedup();
    findings
}

/// Warn when `.gitignore` mentions the retired state path but not `.ledgerful/`.
fn doctor_gitignore_legacy_findings(root: &camino::Utf8Path) -> Vec<DoctorFinding> {
    let gi_path = root.join(".gitignore");
    if !gi_path.is_file() {
        return Vec::new();
    }
    let Ok(content) = std::fs::read_to_string(gi_path.as_std_path()) else {
        return Vec::new();
    };
    let legacy_name = crate::state::layout::LEGACY_STATE_DIR;
    let has_legacy = content.lines().any(|l| {
        let t = l.trim();
        t == legacy_name
            || t == format!("{legacy_name}/")
            || t == format!("/{legacy_name}")
            || t == format!("/{legacy_name}/")
            || t.starts_with(&format!("{legacy_name}/"))
            || t.starts_with(&format!("/{legacy_name}/"))
    });
    let has_current = content
        .lines()
        .any(|l| crate::git::ignore::gitignore_patterns_equivalent(l, ".ledgerful/"));
    if has_legacy && !has_current {
        vec![DoctorFinding::warn(
            "legacy-gitignore",
            DoctorCategory::Migration,
            "`.gitignore` names the retired state path but not `.ledgerful/`. Run `ledgerful init` (ensures `.ledgerful/` is gitignored) or add `.ledgerful/` to `.gitignore` manually.",
        )]
    } else {
        Vec::new()
    }
}

/// Count committed ledger entries marked Verified without a verification_results row.
fn count_phantom_verified(conn: &rusqlite::Connection) -> Result<i64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries le
             WHERE le.verification_status = 'verified'
               AND NOT EXISTS (
                   SELECT 1 FROM verification_results vr WHERE vr.tx_id = le.tx_id
               )",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(count)
}

/// Count LOCAL committed ledger rows with `sig_version < below`.
///
/// Defensive: returns `Err` when the table/column is missing (fresh repos).
/// Callers should use `if let Ok(count) = …` and omit the count on error.
fn count_entries_below_sig_version(conn: &rusqlite::Connection, below: u32) -> Result<i64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries
             WHERE origin = 'LOCAL' AND sig_version < ?1",
            [below as i64],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(count)
}
enum GateModeOutcome {
    Ok(String),
    NoHistory(String),
    Mismatch {
        display: String,
        finding: DoctorFinding,
    },
}

/// Gate mode vs ledger history. Mismatch → warn finding; ok/no-history omit finding.
fn gate_mode_status(
    layout: &crate::state::layout::Layout,
    config: &crate::config::model::Config,
) -> GateModeOutcome {
    let effective_mode = config.gate.mode.clone();
    let ledger_mode = crate::ledger::mode_history::current_mode_from_ledger(layout);

    match ledger_mode {
        Some(ledger_mode) if ledger_mode == effective_mode => GateModeOutcome::Ok(format!(
            "Gate mode: {} (matches ledger history)",
            effective_mode
        )),
        Some(ledger_mode) => {
            let message = format!(
                "Gate mode: {effective_mode} (ledger history shows {ledger_mode}; run `ledgerful gate mode {ledger_mode}`)"
            );
            GateModeOutcome::Mismatch {
                display: message
                    .clone()
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                finding: DoctorFinding::warn("gate-mode-mismatch", DoctorCategory::Gate, message),
            }
        }
        None => GateModeOutcome::NoHistory(format!(
            "Gate mode: {} (no ledger transition history yet)",
            effective_mode
        )),
    }
}
/// WARN when a worktree-local `ledger.db` exists and is not the resolved shared DB.
/// Detection only — never deletes or merges orphan state (0108 DoD-7).
pub(crate) fn split_brain_ledger_finding(layout: &Layout) -> Option<DoctorFinding> {
    use crate::state::layout::{STATE_DIR, STATE_SUBDIR};

    let local_db = layout
        .root
        .join(STATE_DIR)
        .join(STATE_SUBDIR)
        .join("ledger.db");
    if !local_db.is_file() {
        return None;
    }
    let shared_db = layout.state_subdir().join("ledger.db");
    let local_canon = dunce::canonicalize(local_db.as_std_path()).ok();
    let shared_canon = dunce::canonicalize(shared_db.as_std_path()).ok();
    match (local_canon, shared_canon) {
        (Some(local), Some(shared)) if local == shared => None,
        _ => Some(DoctorFinding::warn(
            "worktree-split-brain",
            DoctorCategory::Layout,
            format!(
                "local ledger.db at {local_db} exists and differs from resolved shared state \
                 {shared_db}; linked worktrees share the main worktree's `.ledgerful` \
                 — remove the orphan local state only after confirming it is unused"
            ),
        )),
    }
}

/// Back-compat alias used by 0108 tests that assert message content.
#[cfg(test)]
pub(crate) fn split_brain_ledger_warning(layout: &Layout) -> Option<String> {
    split_brain_ledger_finding(layout).map(|f| format!("Warning [{}]: {}", f.code, f.message))
}

pub(crate) fn apply_gate_mode(
    layout: &Layout,
    config: &crate::config::model::Config,
    report: &mut DoctorReport<'_>,
    findings: &mut Vec<DoctorFinding>,
) {
    match gate_mode_status(layout, config) {
        GateModeOutcome::Ok(line) | GateModeOutcome::NoHistory(line) => {
            report.index_health.push(line);
        }
        GateModeOutcome::Mismatch { display, finding } => {
            report.index_health.push(display);
            findings.push(finding);
        }
    }
}

/// Hook sidecar, signing policy, phantom, legacy, hook-template (after spawn).
pub(crate) fn collect_lifecycle_findings(
    layout: &Layout,
    config: &crate::config::model::Config,
    storage: &StorageManager,
) -> Result<Vec<DoctorFinding>> {
    let mut findings = Vec::new();
    // Track 0074: lifecycle integrity block codes.
    let lifecycle = crate::commands::ledger::detect_lifecycle_signals(layout);
    if lifecycle.promote_orphan {
        findings.push(DoctorFinding::block(
            crate::commands::hook_sidecar::CODE_PROMOTE_ORPHAN,
            DoctorCategory::Lifecycle,
            format!(
                "promote-failed or HEAD-matching orphan retained (tx={}). Recover with: {}",
                lifecycle
                    .promote_orphan_tx_id
                    .as_deref()
                    .unwrap_or("unknown"),
                crate::commands::hook_sidecar::RECOVER_HINT
            ),
        ));
    }
    if lifecycle.head_uncovered && config.gate.is_enforce() {
        findings.push(DoctorFinding::block(
            crate::commands::hook_sidecar::CODE_HEAD_UNCOVERED,
            DoctorCategory::Lifecycle,
            format!(
                "HEAD uncovered via promote-fail/HEAD-matching pending sidecar under enforce (message-hash heuristic; not a full material-HEAD-without-row scan). Recover with: {}",
                crate::commands::hook_sidecar::RECOVER_HINT
            ),
        ));
    }
    if config.gate.is_enforce() && config.intent.required == "never" {
        findings.push(DoctorFinding::block(
            crate::commands::hook_sidecar::CODE_INTENT_NEVER_UNDER_ENFORCE,
            DoctorCategory::Lifecycle,
            "intent.required=never is incompatible with gate mode enforce.",
        ));
    }
    // 0072 M2: enforce without require_signing is block.
    if config.gate.is_enforce() && !config.intent.require_signing {
        findings.push(DoctorFinding::block(
            "sig-require",
            DoctorCategory::Lifecycle,
            "gate.mode=enforce but intent.require_signing=false. Unsigned rows will not fail verify --signatures.",
        ));
    }
    // Soft pin warn when no trusted keys are configured (0125: structured remediation).
    // Path-only keys read — never call get_keys_dir() (creates keys dir).
    if config.intent.trusted_public_keys.is_empty() {
        let pub_hex = crate::ledger::crypto::keys_dir_path()
            .ok()
            .and_then(|keys_dir| {
                crate::ledger::crypto::read_public_key_hex(&keys_dir)
                    .ok()
                    .flatten()
            });
        findings.push(build_sig_pin_finding(pub_hex.as_deref()));
    }
    if config.intent.min_sig_version < 2 {
        // Defensive count (like phantom): omit number on SQL error, still emit remediation.
        let v1_count = count_entries_below_sig_version(storage.get_connection(), 2).ok();
        findings.push(build_sig_version_finding(
            config.intent.min_sig_version,
            v1_count,
        ));
    }
    // Legacy phantom Verified without a bound verification run (forward-only flag).
    if let Ok(count) = count_phantom_verified(storage.get_connection())
        && count > 0
    {
        findings.push(DoctorFinding::warn(
            crate::commands::hook_sidecar::CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY,
            DoctorCategory::Signing,
            format!(
                "{count} committed row(s) have verification_status=Verified with no bound verification_results row (legacy promote phantoms; forward-only)."
            ),
        ));
    }

    // 0094: four-surface legacy-migration residue (warn only — never block).
    // Prefer layout.root (git work root) over cwd for nested directories.
    {
        let mut legacy = collect_legacy_migration_findings(layout.root.as_path(), layout);
        legacy.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
        findings.extend(legacy);
    }

    // 0121: product hook template stamp drift (Info + Gate; never blocks publish).
    findings.extend(
        crate::commands::hook_template::hook_template_stale_findings(layout.root.as_path()),
    );

    Ok(findings)
}
