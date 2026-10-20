//! `doctor --fix` pin / ack / refuse (0226).
//!
//! `--yes` pins `intent.trusted_public_keys` and may bump `min_sig_version`
//! when every LOCAL row is already ≥2. It never invokes `ledger re-sign --all`.
//! Phantom findings are acked in `[doctor] acknowledged_codes` only.

use super::finding::DoctorFinding;
use crate::commands::config::DOCTOR_CONFIG_REFUSAL;
use crate::commands::config::edit::read_ack_codes;
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
///
/// `blocked` and `refusal` are set together only when a write-bearing plan
/// cannot proceed against the existing project config. Allowed previews omit
/// both fields. Absence of `blocked` is not a writability guarantee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DoctorFixPlan {
    pub dry_run: bool,
    pub actions: Vec<DoctorFixAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
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
    DoctorFixPlan {
        dry_run,
        actions,
        blocked: None,
        refusal: None,
    }
}

/// True when the plan includes an action that writes the project config.
/// `reSignFirst`, `pinUnavailable` and diagnostic-only plans do not write.
pub(crate) fn plan_writes_config(plan: &DoctorFixPlan) -> bool {
    plan.actions
        .iter()
        .any(|a| matches!(a.kind.as_str(), "pin" | "bumpMinSig" | "ack"))
}

/// Read-only precondition for a write-bearing plan against the existing
/// project config. Returns the fixed refusal when the file is missing, not a
/// regular file, unreadable, not TOML, or has an unsupported target shape.
///
/// Write access is deliberately not probed here; it is checked at apply time.
pub(crate) fn config_write_precondition(
    layout: &Layout,
    plan: &DoctorFixPlan,
) -> Option<&'static str> {
    if !plan_writes_config(plan) {
        return None;
    }
    let path = layout.config_file();
    let refused = Some(DOCTOR_CONFIG_REFUSAL);
    let Ok(meta) = std::fs::metadata(path.as_std_path()) else {
        return refused;
    };
    if !meta.is_file() {
        return refused;
    }
    let Ok(content) = std::fs::read_to_string(path.as_std_path()) else {
        return refused;
    };
    let Ok(doc) = content.parse::<toml_edit::DocumentMut>() else {
        return refused;
    };
    let writes_intent = plan
        .actions
        .iter()
        .any(|a| matches!(a.kind.as_str(), "pin" | "bumpMinSig"));
    if writes_intent && doc.get("intent").is_some_and(|item| !item.is_table()) {
        return refused;
    }
    let writes_ack = plan.actions.iter().any(|a| a.kind == "ack");
    if writes_ack && read_ack_codes(&doc).is_err() {
        return refused;
    }
    None
}

/// Annotate a write-bearing plan that cannot proceed. Sets `blocked` and
/// `refusal` together; leaves allowed plans unchanged.
pub(crate) fn annotate_config_write_refusal(layout: &Layout, plan: &mut DoctorFixPlan) {
    if let Some(refusal) = config_write_precondition(layout, plan) {
        plan.blocked = Some(true);
        plan.refusal = Some(refusal.to_string());
    }
}

/// Human dry-run / apply lines (greppable `intent.trusted_public_keys`, `{RESIGN_FIRST}`).
///
/// A blocked plan ends with the fixed refusal as its last preview line.
pub(crate) fn format_fix_plan_lines(plan: &DoctorFixPlan) -> Vec<String> {
    if plan.actions.is_empty() {
        return vec!["No --fix actions.".to_string()];
    }
    let prefix = if plan.dry_run { "Would" } else { "Will" };
    let mut lines: Vec<String> = plan
        .actions
        .iter()
        .map(|a| match a.kind.as_str() {
            "pin" => format!("{prefix} config set {detail}", detail = a.detail),
            "pinUnavailable" => a.detail.clone(),
            "reSignFirst" => format!("sig-version: {}", a.detail),
            "bumpMinSig" => format!("{prefix} config set {}", a.detail),
            "ack" => format!("{prefix} ack {}", a.code),
            other => format!("{prefix} {other} {}", a.detail),
        })
        .collect();
    if let Some(refusal) = plan.refusal.as_ref() {
        lines.push(refusal.clone());
    }
    lines
}

/// Apply pin / bump / phantom-ack. Never calls `ledger re-sign --all`.
///
/// Pin and ack run even when sig-version will refuse, so `--yes` still pins
/// in a repo that has LOCAL v1 rows. The refuse is returned after those writes.
pub(crate) fn apply_doctor_fix(layout: &Layout, plan: &DoctorFixPlan) -> Result<Vec<String>> {
    // Fresh boundary check before the first action, for direct callers and
    // races. A metadata-only preview check is not sufficient on its own.
    if let Some(refusal) = config_write_precondition(layout, plan) {
        return Err(miette::miette!("{}", refusal));
    }

    let mut applied = Vec::new();
    let mut resign_err: Option<String> = None;

    for action in &plan.actions {
        match action.kind.as_str() {
            "pin" => {
                crate::commands::config::execute_existing_config_set_in_quiet(
                    layout,
                    &action.detail,
                )?;
                applied.push(format!("Pinned {}", action.detail));
            }
            "pinUnavailable" => {
                return Err(miette::miette!("{}", action.detail));
            }
            "bumpMinSig" => {
                crate::commands::config::execute_existing_config_set_in_quiet(
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
///
/// Reads and writes the existing project document in one instance; a missing
/// project config is refused, never materialized from a template.
pub(crate) fn ack_code(layout: &Layout, code: &str) -> Result<()> {
    crate::commands::config::execute_existing_config_ack_in_quiet(layout, code)
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

    /// Seed a valid regular project config. Write-bearing doctor plans require
    /// an existing project file; they never materialize a template.
    fn seed_project_config(layout: &Layout) {
        layout.ensure_state_dir().expect("state dir");
        std::fs::write(layout.config_file(), "[core]\nstrict = false\n").expect("seed config");
    }

    #[test]
    fn apply_yes_pins_trusted_keys_in_tempdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let layout = Layout::new(root);
        seed_project_config(&layout);
        let hex = "a".repeat(64);
        let plan = plan_doctor_fix(&[pin_finding()], Some(&hex), None, false);
        apply_doctor_fix(&layout, &plan).expect("pin");
        let config = crate::config::load_config(&layout).expect("load");
        assert_eq!(config.intent.trusted_public_keys, vec![hex]);
    }

    #[test]
    fn apply_yes_sig_version_refuses_resign_first_when_v1_remain() {
        // No-write control: reSignFirst is not write-bearing, so no project
        // config is seeded or required. The refusal must not touch the disk.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let plan = plan_doctor_fix(&[version_finding()], None, Some(2), false);
        let err = apply_doctor_fix(&layout, &plan).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains(RESIGN_FIRST), "got {msg}");
        assert!(
            !layout.config_file().exists(),
            "re-sign-first refusal must not create a project config"
        );
    }

    #[test]
    fn apply_yes_phantom_acks_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let layout = Layout::new(root);
        seed_project_config(&layout);
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
        seed_project_config(&layout);
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

    fn fresh_layout() -> (tempfile::TempDir, Layout) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8").to_path_buf();
        let layout = Layout::new(&root);
        layout.ensure_state_dir().expect("state dir");
        (tmp, layout)
    }

    #[test]
    fn user_template_secret_missing_config_pin_refuses_without_materializing() {
        let (_tmp, layout) = fresh_layout();
        let hex = "c".repeat(64);
        let plan = plan_doctor_fix(&[pin_finding()], Some(&hex), None, false);
        let err = apply_doctor_fix(&layout, &plan).unwrap_err();
        assert_eq!(format!("{err}"), DOCTOR_CONFIG_REFUSAL);
        assert!(
            !layout.config_file().exists(),
            "a missing project config must not be created from a template"
        );
    }

    #[test]
    fn user_template_secret_missing_config_ack_refuses_without_materializing() {
        let (_tmp, layout) = fresh_layout();
        let err = ack_code(&layout, CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY).unwrap_err();
        assert_eq!(format!("{err}"), DOCTOR_CONFIG_REFUSAL);
        assert!(!layout.config_file().exists());
    }

    #[test]
    fn apply_doctor_fix_pin_unavailable_no_config_write() {
        let (_tmp, layout) = fresh_layout();
        let plan = plan_doctor_fix(&[pin_finding()], None, None, false);
        let err = apply_doctor_fix(&layout, &plan).unwrap_err();
        assert!(format!("{err}").contains("cannot pin"), "got {err}");
        assert!(!format!("{err}").contains(DOCTOR_CONFIG_REFUSAL));
        assert!(!layout.config_file().exists());
    }

    #[test]
    fn user_template_secret_directory_at_config_path_is_blocked_preview() {
        let (_tmp, layout) = fresh_layout();
        std::fs::create_dir(layout.config_file()).expect("dir at config path");
        let mut plan = plan_doctor_fix(&[phantom_finding()], None, None, true);
        annotate_config_write_refusal(&layout, &mut plan);
        assert_eq!(plan.blocked, Some(true));
        assert_eq!(plan.refusal.as_deref(), Some(DOCTOR_CONFIG_REFUSAL));
        let json = serde_json::to_value(&plan).expect("serialize");
        assert_eq!(json["blocked"], serde_json::json!(true));
        assert_eq!(json["refusal"], serde_json::json!(DOCTOR_CONFIG_REFUSAL));
        assert_eq!(json["actions"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn user_template_secret_allowed_preview_omits_blocked_and_refusal() {
        let (_tmp, layout) = fresh_layout();
        seed_project_config(&layout);
        let mut plan = plan_doctor_fix(&[phantom_finding()], None, None, true);
        annotate_config_write_refusal(&layout, &mut plan);
        assert_eq!(plan.blocked, None);
        let json = serde_json::to_value(&plan).expect("serialize");
        assert!(json.get("blocked").is_none(), "allowed preview: {json}");
        assert!(json.get("refusal").is_none(), "allowed preview: {json}");
        assert_eq!(json["dryRun"], serde_json::json!(true));
    }

    #[test]
    fn user_template_secret_blocked_human_preview_ends_with_refusal() {
        let (_tmp, layout) = fresh_layout();
        let mut plan = plan_doctor_fix(&[phantom_finding()], None, None, true);
        annotate_config_write_refusal(&layout, &mut plan);
        let lines = format_fix_plan_lines(&plan);
        assert_eq!(
            lines.last().map(String::as_str),
            Some(DOCTOR_CONFIG_REFUSAL)
        );
        assert_eq!(lines.len(), plan.actions.len() + 1);
    }

    #[test]
    fn user_template_secret_wrong_typed_ack_refuses_and_keeps_bytes() {
        let (_tmp, layout) = fresh_layout();
        let original = "[doctor]\nacknowledged_codes = \"NOT-AN-ARRAY\"\n";
        std::fs::write(layout.config_file(), original).expect("seed");
        let plan = plan_doctor_fix(&[phantom_finding()], None, None, false);
        let err = apply_doctor_fix(&layout, &plan).unwrap_err();
        assert_eq!(format!("{err}"), DOCTOR_CONFIG_REFUSAL);
        assert_eq!(
            std::fs::read_to_string(layout.config_file()).unwrap(),
            original
        );
    }

    #[test]
    fn user_template_secret_non_string_ack_element_refuses_and_keeps_bytes() {
        let (_tmp, layout) = fresh_layout();
        let original = "[doctor]\nacknowledged_codes = [\"OK\", 7]\n";
        std::fs::write(layout.config_file(), original).expect("seed");
        let err = ack_code(&layout, "NEW").unwrap_err();
        assert_eq!(format!("{err}"), DOCTOR_CONFIG_REFUSAL);
        assert_eq!(
            std::fs::read_to_string(layout.config_file()).unwrap(),
            original
        );
    }

    #[test]
    fn user_template_secret_ack_escaped_codes_round_trip_sorted_and_keep_unknown() {
        let (_tmp, layout) = fresh_layout();
        std::fs::write(
            layout.config_file(),
            "[doctor]\n# keep this comment\nacknowledged_codes = [\"Zed\", \"back\\\\slash\"] # tail\n",
        )
        .expect("seed");
        ack_code(&layout, "quote\"code").expect("ack escaped");
        let written = std::fs::read_to_string(layout.config_file()).unwrap();
        let parsed: toml::Table = written.parse().expect("valid toml after edit");
        let codes: Vec<String> = parsed["doctor"]["acknowledged_codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            codes,
            vec![
                "Zed".to_string(),
                "back\\slash".to_string(),
                "quote\"code".to_string(),
            ]
        );
        assert!(
            written.contains("# keep this comment"),
            "comment lost: {written}"
        );
        assert!(written.contains("# tail"), "inline comment lost: {written}");
    }

    #[test]
    fn user_template_secret_preview_target_shape() {
        // Mixed write-bearing plan (pin then ack) over a non-table intent: the
        // pin must not run before the structural refusal, so the config is
        // byte-identical after apply, and preview reports blocked.
        let (_tmp, layout) = fresh_layout();
        let original = "intent = \"not-a-table\"\n\n[doctor]\nacknowledged_codes = []\n";
        std::fs::write(layout.config_file(), original).expect("seed");
        let hex = "d".repeat(64);
        let plan = plan_doctor_fix(&[pin_finding(), phantom_finding()], Some(&hex), None, false);
        let mut preview = plan.clone();
        preview.dry_run = true;
        annotate_config_write_refusal(&layout, &mut preview);
        assert_eq!(preview.blocked, Some(true), "preview must report blocked");
        assert_eq!(preview.refusal.as_deref(), Some(DOCTOR_CONFIG_REFUSAL));

        let err = apply_doctor_fix(&layout, &plan).unwrap_err();
        assert_eq!(format!("{err}"), DOCTOR_CONFIG_REFUSAL);
        assert_eq!(
            std::fs::read_to_string(layout.config_file()).unwrap(),
            original,
            "structural refusal must produce zero prior writes"
        );
    }

    #[test]
    fn user_template_secret_preview_target_shape_non_table_doctor_refuses_ack() {
        let (_tmp, layout) = fresh_layout();
        let original = "doctor = \"flat\"\n";
        std::fs::write(layout.config_file(), original).expect("seed");
        let plan = plan_doctor_fix(&[phantom_finding()], None, None, false);
        let err = apply_doctor_fix(&layout, &plan).unwrap_err();
        assert_eq!(format!("{err}"), DOCTOR_CONFIG_REFUSAL);
        assert_eq!(
            std::fs::read_to_string(layout.config_file()).unwrap(),
            original
        );
    }
}
