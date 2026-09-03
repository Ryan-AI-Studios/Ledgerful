//! `doctor --fix` pin / ack / refuse (0226).
//!
//! `--yes` pins `intent.trusted_public_keys` and may bump `min_sig_version`
//! when every LOCAL row is already ≥2. It never invokes `ledger re-sign --all`.
//! Phantom findings are acked in `[doctor] acknowledged_codes` only.

use super::finding::DoctorFinding;
use crate::commands::hook_sidecar::CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY;
use crate::state::layout::Layout;
use miette::Result;
use serde::Serialize;

const RESIGN_FIRST: &str = "re-sign first";

/// Planned `--fix` action (JSON additive under `fix.actions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DoctorFixAction {
    pub code: String,
    pub kind: String,
    pub detail: String,
}

/// Additive `fix` object on `doctor --json --fix` (schemaVersion stays 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DoctorFixPlan {
    pub dry_run: bool,
    pub actions: Vec<DoctorFixAction>,
}

/// Build the `--fix` plan from this run's findings.
///
/// `v1_local_count` is `Some(n)` LOCAL rows with `sig_version < 2` when known.
pub(crate) fn plan_doctor_fix(
    findings: &[DoctorFinding],
    pub_key_hex: Option<&str>,
    v1_local_count: Option<i64>,
    dry_run: bool,
) -> DoctorFixPlan {
    let mut actions = Vec::new();
    let has = |code: &str| findings.iter().any(|f| f.code == code);

    if has("sig-pin") {
        let (kind, detail) = match pub_key_hex {
            Some(hex) => (
                "pin".to_string(),
                format!("intent.trusted_public_keys=[\"{hex}\"]"),
            ),
            None => (
                "pinUnavailable".to_string(),
                "cannot pin: local signing identity not found under ~/.ledgerful/keys".to_string(),
            ),
        };
        actions.push(DoctorFixAction {
            code: "sig-pin".to_string(),
            kind,
            detail,
        });
    }

    if has("sig-version") {
        match v1_local_count {
            Some(n) if n > 0 => actions.push(DoctorFixAction {
                code: "sig-version".to_string(),
                kind: "reSignFirst".to_string(),
                detail: format!("{RESIGN_FIRST}: {n} LOCAL row(s) have sig_version < 2"),
            }),
            Some(0) => actions.push(DoctorFixAction {
                code: "sig-version".to_string(),
                kind: "bumpMinSig".to_string(),
                detail: "intent.min_sig_version=2".to_string(),
            }),
            _ => actions.push(DoctorFixAction {
                code: "sig-version".to_string(),
                kind: "reSignFirst".to_string(),
                detail: format!("{RESIGN_FIRST}: LOCAL v1 count unknown"),
            }),
        }
    }

    if has(CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY) {
        actions.push(DoctorFixAction {
            code: CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY.to_string(),
            kind: "ack".to_string(),
            detail: "doctor.acknowledged_codes += PHANTOM_PROMOTED_WITHOUT_VERIFY".to_string(),
        });
    }

    actions.sort_by(|a, b| a.code.cmp(&b.code).then(a.kind.cmp(&b.kind)));
    DoctorFixPlan { dry_run, actions }
}

/// Human dry-run / apply lines (greppable `intent.trusted_public_keys`, `{RESIGN_FIRST}`).
pub(crate) fn format_fix_plan_lines(plan: &DoctorFixPlan) -> Vec<String> {
    if plan.actions.is_empty() {
        return vec!["No --fix actions.".to_string()];
    }
    let prefix = if plan.dry_run { "Would" } else { "Will" };
    plan.actions
        .iter()
        .map(|a| match a.kind.as_str() {
            "pin" => format!("{prefix} config set {detail}", detail = a.detail),
            "pinUnavailable" => a.detail.clone(),
            "reSignFirst" => format!("sig-version: {}", a.detail),
            "bumpMinSig" => format!("{prefix} config set {}", a.detail),
            "ack" => format!("{prefix} ack {}", a.code),
            other => format!("{prefix} {other} {}", a.detail),
        })
        .collect()
}

/// Apply pin / bump / phantom-ack. Never calls `ledger re-sign --all`.
///
/// Pin and ack run even when sig-version will refuse, so `--yes` still pins
/// in a repo that has LOCAL v1 rows. The refuse is returned after those writes.
pub(crate) fn apply_doctor_fix(layout: &Layout, plan: &DoctorFixPlan) -> Result<Vec<String>> {
    let mut applied = Vec::new();
    let mut resign_err: Option<String> = None;

    for action in &plan.actions {
        match action.kind.as_str() {
            "pin" => {
                crate::commands::config::execute_config_set_in_quiet(layout, &action.detail)?;
                applied.push(format!("Pinned {}", action.detail));
            }
            "pinUnavailable" => {
                return Err(miette::miette!("{}", action.detail));
            }
            "bumpMinSig" => {
                crate::commands::config::execute_config_set_in_quiet(
                    layout,
                    "intent.min_sig_version=2",
                )?;
                applied.push("Set intent.min_sig_version=2".to_string());
            }
            "ack" => {
                ack_code(layout, &action.code)?;
                applied.push(format!("Acked {}", action.code));
            }
            "reSignFirst" => {
                resign_err = Some(action.detail.clone());
            }
            _ => {}
        }
    }

    if let Some(msg) = resign_err {
        return Err(miette::miette!("{msg}"));
    }
    Ok(applied)
}

/// Append `code` to `[doctor] acknowledged_codes` (sorted, deduped). No GC.
pub(crate) fn ack_code(layout: &Layout, code: &str) -> Result<()> {
    let mut codes = crate::config::load_config(layout)
        .map(|c| c.doctor.acknowledged_codes)
        .unwrap_or_default();
    codes.push(code.to_string());
    codes.sort();
    codes.dedup();
    let rhs = format!(
        "[{}]",
        codes
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    crate::commands::config::execute_config_set_in_quiet(
        layout,
        &format!("doctor.acknowledged_codes={rhs}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::{DoctorCategory, DoctorFinding};
    use crate::state::layout::Layout;
    use camino::Utf8Path;

    fn pin_finding() -> DoctorFinding {
        DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "pin")
    }

    fn version_finding() -> DoctorFinding {
        DoctorFinding::warn("sig-version", DoctorCategory::Signing, "v1")
    }

    fn phantom_finding() -> DoctorFinding {
        DoctorFinding::warn(
            CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY,
            DoctorCategory::Signing,
            "phantoms",
        )
    }

    #[test]
    fn plan_sig_pin_names_trusted_public_keys() {
        let hex = "ab".repeat(32);
        let plan = plan_doctor_fix(&[pin_finding()], Some(&hex), None, true);
        let text = format_fix_plan_lines(&plan).join("\n");
        assert!(
            text.contains("intent.trusted_public_keys"),
            "dry-run must name config set: {text}"
        );
        assert_eq!(plan.actions[0].kind, "pin");
        assert!(plan.dry_run);
    }

    #[test]
    fn plan_sig_version_v1_is_resign_first() {
        let plan = plan_doctor_fix(&[version_finding()], None, Some(4), true);
        assert_eq!(plan.actions[0].kind, "reSignFirst");
        assert!(plan.actions[0].detail.contains(RESIGN_FIRST));
        let text = format_fix_plan_lines(&plan).join("\n");
        assert!(text.contains(RESIGN_FIRST), "{text}");
    }

    #[test]
    fn plan_sig_version_zero_v1_is_bump() {
        let plan = plan_doctor_fix(&[version_finding()], None, Some(0), false);
        assert_eq!(plan.actions[0].kind, "bumpMinSig");
        assert!(plan.actions[0].detail.contains("min_sig_version=2"));
    }

    #[test]
    fn apply_yes_pins_trusted_keys_in_tempdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let hex = "a".repeat(64);
        let plan = plan_doctor_fix(&[pin_finding()], Some(&hex), None, false);
        apply_doctor_fix(&layout, &plan).expect("pin");
        let config = crate::config::load_config(&layout).expect("load");
        assert_eq!(config.intent.trusted_public_keys, vec![hex]);
    }

    #[test]
    fn apply_yes_sig_version_refuses_resign_first_when_v1_remain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let plan = plan_doctor_fix(&[version_finding()], None, Some(2), false);
        let err = apply_doctor_fix(&layout, &plan).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains(RESIGN_FIRST), "got {msg}");
        let config = crate::config::load_config(&layout).expect("load");
        assert_eq!(config.intent.min_sig_version, 1);
    }

    #[test]
    fn apply_yes_phantom_acks_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let plan = plan_doctor_fix(&[phantom_finding()], None, None, false);
        apply_doctor_fix(&layout, &plan).expect("ack");
        let config = crate::config::load_config(&layout).expect("load");
        assert_eq!(
            config.doctor.acknowledged_codes,
            vec![CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY]
        );
    }

    #[test]
    fn apply_yes_pin_then_resign_first_still_pins() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let hex = "b".repeat(64);
        let plan = plan_doctor_fix(
            &[pin_finding(), version_finding()],
            Some(&hex),
            Some(1),
            false,
        );
        let err = apply_doctor_fix(&layout, &plan).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains(RESIGN_FIRST), "got {msg}");
        let config = crate::config::load_config(&layout).expect("load");
        assert_eq!(config.intent.trusted_public_keys, vec![hex]);
        assert_eq!(config.intent.min_sig_version, 1);
    }
}
