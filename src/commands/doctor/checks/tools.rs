use crate::commands::doctor::finding::{DoctorCategory, DoctorFinding};
use crate::platform::env::ExecutableStatus;

/// Per-tool identity (0109): git missing = block; gemini missing = info/optional.
pub(crate) fn collect_tool_findings(tools: &[(String, ExecutableStatus)]) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();
    for (name, status) in tools {
        if matches!(status, ExecutableStatus::NotFound) {
            if name == "git" {
                findings.push(DoctorFinding::block(
                    "tool-git",
                    DoctorCategory::Tools,
                    "git NOT FOUND — required for publish-environment path",
                ));
            } else if name == "gemini" || name == "gemini-cli" {
                findings.push(DoctorFinding::info(
                    "tool-gemini",
                    DoctorCategory::Optional,
                    format!("{name} NOT FOUND (optional ask backend CLI)"),
                ));
            } else {
                findings.push(DoctorFinding::warn(
                    format!("tool-{name}"),
                    DoctorCategory::Tools,
                    format!("{name} NOT FOUND"),
                ));
            }
        }
    }
    findings
}
