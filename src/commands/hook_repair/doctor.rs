use super::detect::detect_third_party_hook_manager;
use super::resolve::{HooksDirResolution, resolve_hooks_dir};
use super::rewrite::{CURRENT_BINARY, GATE_SUFFIXES, LEGACY_BINARY, contains_legacy_invocation};
use camino::Utf8Path;
use std::fs;

// ---------------------------------------------------------------------------
// Doctor helpers (detection only — no rewrite)
// ---------------------------------------------------------------------------

/// Scan hooks for legacy migration residue. Returns sorted structured findings
/// (empty when clean). Does not modify any file.
///
/// RT-H5 detection half only: reports gate-present-but-inert when a gate
/// marker exists but every invocation still names the retired binary (binary
/// missing → guard skips → commit/push proceeds). Enforcement (absolute-path
/// pin, fail-closed) is out of scope for 0094. Severity **warn** / migration.
pub fn doctor_legacy_hook_findings(
    repo_root: &Utf8Path,
) -> Vec<crate::commands::doctor::DoctorFinding> {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding};

    let mut findings = Vec::new();

    if detect_third_party_hook_manager(repo_root).is_some() {
        // Third-party managers own hooks; still note if we can see residue.
    }

    let hooks_dir = match resolve_hooks_dir(repo_root) {
        HooksDirResolution::Found { hooks_dir } => hooks_dir,
        HooksDirResolution::OutsideRepo { hooks_dir } => {
            findings.push(DoctorFinding::warn(
                "legacy-hooks",
                DoctorCategory::Migration,
                format!(
                    "hooks path '{hooks_dir}' is outside the repository; run `ledgerful update --repair-hooks` will refuse rewrite"
                ),
            ));
            findings.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
            return findings;
        }
        HooksDirResolution::CannotLook { reason } => {
            // Only report cannot-look when there is other signal of legacy use;
            // clean repos without .git/hooks stay silent (R5).
            let _ = reason;
            return findings;
        }
    };

    if !hooks_dir.is_dir() {
        return findings;
    }

    let mut legacy_markers = false;
    let mut legacy_invocations = false;
    let mut duplicate_gates = false;
    let mut inert_gate = false;
    let mut residual_after_shape = false;

    let entries = match fs::read_dir(hooks_dir.as_std_path()) {
        Ok(e) => e,
        Err(_) => return findings,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".sample") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };

        for suffix in GATE_SUFFIXES {
            let legacy_m = format!("# {LEGACY_BINARY}-{suffix}");
            let current_m = format!("# {CURRENT_BINARY}-{suffix}");
            if content.contains(&legacy_m) {
                legacy_markers = true;
                if content.contains(&current_m) {
                    duplicate_gates = true;
                }
            }
            // 0206-D: N>1 current same-suffix is a duplicate even with no legacy.
            if content.matches(&current_m).count() > 1 {
                duplicate_gates = true;
            }
        }

        if contains_legacy_invocation(&content) {
            legacy_invocations = true;
            // RT-H5: gate marker present but invocations still retired → inert.
            let has_gate = GATE_SUFFIXES.iter().any(|s| {
                content.contains(&format!("# {LEGACY_BINARY}-{s}"))
                    || content.contains(&format!("# {CURRENT_BINARY}-{s}"))
            });
            if has_gate {
                inert_gate = true;
            }
            residual_after_shape = true;
        }
    }

    if legacy_markers {
        findings.push(DoctorFinding::warn(
            "legacy-hooks",
            DoctorCategory::Migration,
            "hook marker comments still use the retired product name; run `ledgerful update --repair-hooks`",
        ));
    }
    if legacy_invocations {
        findings.push(DoctorFinding::warn(
            "legacy-hooks",
            DoctorCategory::Migration,
            "hooks still invoke the retired binary; run `ledgerful update --repair-hooks`",
        ));
    }
    if duplicate_gates {
        findings.push(DoctorFinding::warn(
            "legacy-hooks",
            DoctorCategory::Migration,
            "duplicate gate blocks (legacy + current, or more than one current marker of the same type); run `ledgerful update --repair-hooks`",
        ));
    }
    if inert_gate {
        // RT-H5 detection only (0094): gate present but names missing binary → no-op.
        findings.push(DoctorFinding::warn(
            "legacy-hooks",
            DoctorCategory::Migration,
            "gate marker present but invocations name the retired binary (gate is inert if that binary is absent); run `ledgerful update --repair-hooks`",
        ));
    }
    let _ = residual_after_shape;

    findings.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    findings.dedup();
    findings
}
