//! Pure helpers for structured verify failure output (track 0121).
//!
//! Human fail block + formatter path extraction. Shared by the default human
//! path and `VerifyCliStepJson` enrichment. Bridge `error_snippet` truncation
//! is intentionally separate and untouched.

use crate::verify::results::{VerificationReport, VerificationResult};

/// Max paths emitted in fail block / JSON (DoD-4).
pub const FORMATTER_PATH_CAP: usize = 50;

/// Structured human fail block for the first failed plan step.
///
/// Field names use camelCase to correlate with JSON wire fields.
pub fn format_fail_block(
    step_name: &str,
    command: &str,
    exit_code: i32,
    failure_detail: &str,
    failed_paths: &[String],
) -> String {
    let mut lines = vec![
        "[Ledgerful] verify failed".to_string(),
        format!("step: {step_name}"),
        format!("command: {command}"),
        format!("exitCode: {exit_code}"),
        format!("failureDetail: {failure_detail}"),
    ];
    if !failed_paths.is_empty() {
        let (shown, overflow) = if failed_paths.len() > FORMATTER_PATH_CAP {
            (
                &failed_paths[..FORMATTER_PATH_CAP],
                failed_paths.len() - FORMATTER_PATH_CAP,
            )
        } else {
            (failed_paths, 0)
        };
        let mut path_line = format!("failedPaths: {}", shown.join(" "));
        if overflow > 0 {
            path_line.push_str(&format!(" (+{overflow} more)"));
        }
        lines.push(path_line);
    }
    lines.join("\n")
}

/// Prefer stderr summary, then stdout, then exit code (same sources as
/// `VerifyCliStepJson::from_result` / bridge detail, without inventing a third
/// truncate path).
pub fn failure_detail_from_result(result: &VerificationResult) -> String {
    if !result.stderr_summary.is_empty() {
        result.stderr_summary.clone()
    } else if !result.stdout_summary.is_empty() {
        result.stdout_summary.clone()
    } else {
        format!("exit code {}", result.exit_code)
    }
}

/// Derive a short step name from a command (first three tokens) — same as
/// `VerifyCliStepJson::from_result`.
pub fn step_name_from_command(command: &str) -> String {
    command
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the fail block for the first failed step in plan order, if any.
pub fn format_fail_block_from_report(report: &VerificationReport) -> Option<String> {
    let first = report.results.iter().find(|r| r.exit_code != 0)?;
    let detail = failure_detail_from_result(first);
    let paths =
        extract_formatter_paths(&first.command, &first.stdout_summary, &first.stderr_summary);
    Some(format_fail_block(
        &step_name_from_command(&first.command),
        &first.command,
        first.exit_code,
        &detail,
        &paths,
    ))
}

/// Best-effort path extraction from formatter tool output.
///
/// Never invents paths. Unknown tools → empty. Normalizes `\` → `/`. Caps at
/// [`FORMATTER_PATH_CAP`].
pub fn extract_formatter_paths(command: &str, stdout: &str, stderr: &str) -> Vec<String> {
    let cmd = command.to_lowercase();
    let combined = {
        let mut s = String::with_capacity(stdout.len() + stderr.len() + 1);
        s.push_str(stdout);
        if !stdout.is_empty() && !stderr.is_empty() {
            s.push('\n');
        }
        s.push_str(stderr);
        s
    };

    let mut paths = if is_rustfmt_check(&cmd) {
        extract_rustfmt_paths(&combined)
    } else if is_ruff_format_check(&cmd) {
        extract_ruff_format_paths(&combined)
    } else {
        Vec::new()
    };

    // Deterministic: normalize, de-dup preserving order, cap.
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| {
        if p.is_empty() || !seen.insert(p.clone()) {
            return false;
        }
        true
    });
    if paths.len() > FORMATTER_PATH_CAP {
        paths.truncate(FORMATTER_PATH_CAP);
    }
    paths
}

fn is_rustfmt_check(cmd: &str) -> bool {
    let has_check = cmd.contains("--check");
    if !has_check {
        return false;
    }
    // `cargo fmt … -- --check` or direct `rustfmt --check`
    cmd.contains("cargo fmt") || cmd.contains("rustfmt")
}

fn is_ruff_format_check(cmd: &str) -> bool {
    cmd.contains("ruff") && cmd.contains("format") && cmd.contains("--check")
}

fn extract_rustfmt_paths(output: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        // `Diff in <path>:` and `Diff in <path> at line N:`
        if let Some(rest) = t.strip_prefix("Diff in ") {
            let path = if let Some((p, _)) = rest.split_once(" at line ") {
                p.trim()
            } else if let Some((p, _)) = rest.split_once(':') {
                p.trim()
            } else {
                rest.trim_end_matches(':').trim()
            };
            if !path.is_empty() {
                paths.push(normalize_path(path));
            }
        }
    }
    paths
}

fn extract_ruff_format_paths(output: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Would reformat: ") {
            let path = rest.trim();
            if !path.is_empty() {
                paths.push(normalize_path(path));
            }
        }
    }
    paths
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::results::VerificationResult;

    #[test]
    fn format_fail_block_without_paths() {
        let out = format_fail_block(
            "cargo fmt --all",
            "cargo fmt --all -- --check",
            1,
            "Diff in src/lib.rs:",
            &[],
        );
        assert!(out.starts_with("[Ledgerful] verify failed\n"));
        assert!(out.contains("step: cargo fmt --all\n"));
        assert!(out.contains("command: cargo fmt --all -- --check\n"));
        assert!(out.contains("exitCode: 1\n"));
        assert!(out.contains("failureDetail: Diff in src/lib.rs:"));
        assert!(!out.contains("failedPaths:"));
    }

    #[test]
    fn format_fail_block_with_paths() {
        let paths = vec!["src/a.rs".into(), "src/b.rs".into()];
        let out = format_fail_block("cargo fmt", "cargo fmt -- --check", 1, "detail", &paths);
        assert!(out.contains("failedPaths: src/a.rs src/b.rs"));
        assert!(!out.contains("(+"));
    }

    #[test]
    fn format_fail_block_path_overflow_annotation() {
        let paths: Vec<String> = (0..52).map(|i| format!("p{i}.rs")).collect();
        let out = format_fail_block("fmt", "cargo fmt -- --check", 1, "d", &paths);
        assert!(out.contains("(+2 more)"));
        // Cap display at 50 path tokens after the label.
        let line = out
            .lines()
            .find(|l| l.starts_with("failedPaths:"))
            .expect("paths line");
        let listed = line
            .strip_prefix("failedPaths: ")
            .unwrap()
            .split(" (+")
            .next()
            .unwrap()
            .split_whitespace()
            .count();
        assert_eq!(listed, 50);
    }

    #[test]
    fn extract_rustfmt_diff_in_colon() {
        let out = "Diff in src/lib.rs:\nDiff in src\\main.rs:\n";
        let paths = extract_formatter_paths("cargo fmt --all -- --check", out, "");
        assert_eq!(paths, vec!["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn extract_rustfmt_diff_in_at_line() {
        let out =
            "Diff in crates/foo/src/a.rs at line 12:\nDiff in crates/foo/src/b.rs at line 1:\n";
        let paths = extract_formatter_paths("rustfmt --check", out, "");
        assert_eq!(paths, vec!["crates/foo/src/a.rs", "crates/foo/src/b.rs"]);
    }

    #[test]
    fn extract_ruff_would_reformat() {
        let out = "Would reformat: app\\main.py\nWould reformat: app/util.py\n";
        let paths = extract_formatter_paths("ruff format --check .", out, "");
        assert_eq!(paths, vec!["app/main.py", "app/util.py"]);
    }

    #[test]
    fn extract_unknown_tool_empty() {
        let paths = extract_formatter_paths(
            "cargo clippy --all-targets",
            "error: unused import\n  --> src/lib.rs:1:1\n",
            "",
        );
        assert!(paths.is_empty());
    }

    #[test]
    fn extract_without_check_flag_empty() {
        // Without --check, cargo fmt rewrites in place and is not our parse target.
        let paths = extract_formatter_paths("cargo fmt --all", "Diff in src/lib.rs:\n", "");
        assert!(paths.is_empty());
    }

    #[test]
    fn extract_cap_at_50() {
        let mut out = String::new();
        for i in 0..60 {
            out.push_str(&format!("Diff in file{i}.rs:\n"));
        }
        let paths = extract_formatter_paths("cargo fmt -- --check", &out, "");
        assert_eq!(paths.len(), 50);
    }

    #[test]
    fn extract_dedups_preserving_order() {
        let out = "Diff in a.rs:\nDiff in b.rs:\nDiff in a.rs:\n";
        let paths = extract_formatter_paths("cargo fmt -- --check", out, "");
        assert_eq!(paths, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn format_fail_block_from_report_first_failed() {
        let report = VerificationReport {
            plan: None,
            results: vec![
                VerificationResult {
                    command: "cargo fmt --all -- --check".into(),
                    exit_code: 0,
                    duration_ms: 1,
                    stdout_summary: String::new(),
                    stderr_summary: String::new(),
                    truncated: false,
                    timestamp: "t".into(),
                },
                VerificationResult {
                    command: "cargo clippy --all-targets".into(),
                    exit_code: 1,
                    duration_ms: 2,
                    stdout_summary: String::new(),
                    stderr_summary: "error: bad".into(),
                    truncated: false,
                    timestamp: "t".into(),
                },
            ],
            prediction_warnings: vec![],
            suggested_actions: vec![],
            overall_pass: false,
            timestamp: "t".into(),
            tx_id: None,
        };
        let block = format_fail_block_from_report(&report).expect("fail block");
        assert!(block.contains("step: cargo clippy --all-targets"));
        assert!(block.contains("exitCode: 1"));
        assert!(block.contains("failureDetail: error: bad"));
    }
}
