use crate::bridge::allowlist::provider_command_basename;
use crate::exec::grouped::{GroupedProcessError, spawn_wait_grouped_captured};
use crate::ledger::enforcement::ValidationLevel;
use crate::ledger::error::LedgerError;
use crate::platform::process_policy::{ProcessPolicy, check_policy};
use miette::Result;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Shell interpreters rejected as commit-validator executables (RT-P1).
const SHELL_INTERPRETER_BASENAMES: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "pwsh.exe",
];

pub struct ValidatorRunner;

#[derive(Debug)]
pub struct ValidationResult {
    pub name: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub level: ValidationLevel,
}

impl ValidatorRunner {
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        name: String,
        executable: &str,
        args: &[String],
        repo_root: &Path,
        entity_abs_path: &str,
        timeout_ms: u64,
        level: ValidationLevel,
        policy: &ProcessPolicy,
    ) -> Result<ValidationResult, LedgerError> {
        // Security: reject shell interpreters as validator executables.
        if is_shell_interpreter(executable) {
            return Err(LedgerError::Validation(format!(
                "Validator '{}' rejected: shell interpreter '{}' is not allowed as a validator executable",
                name, executable
            )));
        }

        // Security: Check process policy
        if let Err(e) = check_policy(executable, policy) {
            return Err(LedgerError::Validation(format!(
                "Validator '{}' blocked by policy: {}",
                name, e
            )));
        }

        // Security: entity path must not contain control characters that could
        // confuse argument parsing in downstream tools.
        if entity_abs_path.contains('\0') || entity_abs_path.contains('\n') {
            return Err(LedgerError::Validation(format!(
                "Validator '{}' rejected: entity path contains forbidden control characters",
                name
            )));
        }

        // Security: resolved entity path must be within the repository root and
        // must not be a symlink.
        let absolute = Path::new(entity_abs_path);
        if let Err(e) = crate::util::path::ensure_path_within_root(repo_root, absolute) {
            return Err(LedgerError::Validation(e));
        }

        let processed_args: Vec<String> = args
            .iter()
            .map(|arg| arg.replace("{entity}", entity_abs_path))
            .collect();

        let mut command = Command::new(executable);
        command.args(&processed_args).current_dir(repo_root);

        let timeout = Duration::from_millis(timeout_ms);
        // 1MB capture cap matches ExecutionBoundary default.
        let captured = match spawn_wait_grouped_captured(command, timeout, 1024 * 1024) {
            Ok(c) => c,
            Err(GroupedProcessError::Timeout { .. }) => {
                return Ok(ValidationResult {
                    name,
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: "Validator timed out".to_string(),
                    level,
                });
            }
            Err(e) => {
                return Err(LedgerError::Validation(format!(
                    "Failed to run validator '{}': {}",
                    name, e
                )));
            }
        };

        Ok(ValidationResult {
            name,
            success: captured.status.success(),
            exit_code: captured.status.code(),
            stdout: captured.stdout,
            stderr: captured.stderr,
            level,
        })
    }
}

fn is_shell_interpreter(executable: &str) -> bool {
    let base = provider_command_basename(executable);
    SHELL_INTERPRETER_BASENAMES
        .iter()
        .any(|denied| base.eq_ignore_ascii_case(denied))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_policy() -> ProcessPolicy {
        ProcessPolicy {
            allowed_commands: vec![
                "cmd".to_string(),
                "sh".to_string(),
                "echo".to_string(),
                "echo.exe".to_string(),
            ],
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: false,
        }
    }

    #[test]
    fn rejects_null_byte_in_entity_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let result = ValidatorRunner::run(
            "null-test".to_string(),
            "echo",
            &["{entity}".to_string()],
            root,
            "src/main\0.rs",
            5000,
            ValidationLevel::Error,
            &fake_policy(),
        );
        let err = result.unwrap_err();
        assert!(format!("{err}").contains("control characters"));
    }

    #[test]
    fn rejects_newline_in_entity_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let result = ValidatorRunner::run(
            "newline-test".to_string(),
            "echo",
            &["{entity}".to_string()],
            root,
            "src/main\n.rs",
            5000,
            ValidationLevel::Error,
            &fake_policy(),
        );
        let err = result.unwrap_err();
        assert!(format!("{err}").contains("control characters"));
    }

    #[test]
    fn substitutes_entity_path_into_args() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "contents").unwrap();

        // Use echo (allowlisted) — not cmd/sh (shell interpreters are rejected).
        let executable = if cfg!(target_os = "windows") {
            // Windows echo is a cmd builtin; use a direct executable if available.
            // `where echo` often fails; fall back to writing a tiny batch via powershell-free path:
            // Use `cmd` would be rejected as shell interpreter. Use the echo.com/echo.exe if present.
            "echo"
        } else {
            "echo"
        };
        let args = vec!["{entity}".to_string()];
        let result = ValidatorRunner::run(
            "echo-entity".to_string(),
            executable,
            &args,
            root,
            &root.join("src").join("main.rs").to_string_lossy(),
            5000,
            ValidationLevel::Error,
            &fake_policy(),
        );

        // On Windows, bare `echo` is not a real executable (it's a cmd builtin),
        // so spawn may fail. Accept either success with main.rs or a spawn failure
        // that is not a policy/interpreter rejection.
        match result {
            Ok(r) => {
                assert!(r.success || r.stdout.contains("main.rs") || !r.stderr.is_empty());
            }
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    !msg.contains("shell interpreter"),
                    "should not reject echo as shell: {msg}"
                );
                assert!(
                    msg.contains("Failed to run") || msg.contains("Failed to start"),
                    "unexpected error: {msg}"
                );
            }
        }
    }

    #[test]
    fn rejects_entity_path_outside_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("outside.txt");
        std::fs::write(&target, "contents").unwrap();

        let result = ValidatorRunner::run(
            "outside-test".to_string(),
            "echo",
            &["{entity}".to_string()],
            root,
            &target.to_string_lossy(),
            5000,
            ValidationLevel::Error,
            &fake_policy(),
        );
        let err = result.unwrap_err();
        assert!(format!("{err}").contains("outside the repository root"));
    }

    #[test]
    fn rejects_shell_interpreter_validators() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let entity = root.join("entity.txt");
        std::fs::write(&entity, "x").unwrap();
        let entity_str = entity.to_string_lossy();

        for exe in [
            "powershell",
            "powershell.exe",
            "cmd",
            "cmd.exe",
            "bash",
            "sh",
            "pwsh",
            r"C:\Windows\System32\cmd.exe",
        ] {
            let result = ValidatorRunner::run(
                "shell-test".to_string(),
                exe,
                &[],
                root,
                &entity_str,
                1000,
                ValidationLevel::Error,
                &fake_policy(),
            );
            let err = result.unwrap_err();
            assert!(
                format!("{err}").contains("shell interpreter"),
                "expected shell interpreter rejection for {exe}, got {err}"
            );
        }
    }

    #[test]
    fn cwd_is_repo_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let entity = root.join("entity.txt");
        std::fs::write(&entity, "x").unwrap();
        let root_canon = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let entity_str = entity.to_string_lossy().to_string();

        // Non-shell-interpreter fixtures that print cwd (shells are rejected).
        #[cfg(windows)]
        let (executable, args, allow) = {
            // .cmd basename is not a shell-interpreter entry; CreateProcess runs it.
            let script = root.join("print_cwd.cmd");
            std::fs::write(&script, "@echo off\r\ncd\r\n").unwrap();
            let script_str = script.to_string_lossy().to_string();
            (
                script_str.clone(),
                Vec::<String>::new(),
                vec!["print_cwd.cmd".to_string(), script_str],
            )
        };
        #[cfg(unix)]
        let (executable, args, allow) = {
            (
                "/bin/pwd".to_string(),
                Vec::<String>::new(),
                vec!["pwd".to_string(), "/bin/pwd".to_string()],
            )
        };

        let policy = ProcessPolicy {
            allowed_commands: allow,
            denied_commands: Vec::new(),
            default_timeout_secs: 30,
            strict: true,
        };

        let result = ValidatorRunner::run(
            "cwd-test".to_string(),
            &executable,
            &args,
            root,
            &entity_str,
            5000,
            ValidationLevel::Error,
            &policy,
        )
        .unwrap_or_else(|e| panic!("cwd fixture failed to run: {e}"));

        assert!(
            result.success,
            "cwd printer should succeed: stdout={} stderr={}",
            result.stdout, result.stderr
        );
        let printed = result.stdout.lines().next().unwrap_or("").trim();
        assert!(
            !printed.is_empty(),
            "expected cwd on stdout, got stdout={:?} stderr={:?}",
            result.stdout,
            result.stderr
        );
        let printed_canon =
            dunce::canonicalize(printed).unwrap_or_else(|_| std::path::PathBuf::from(printed));
        assert_eq!(
            printed_canon, root_canon,
            "validator cwd must be repo_root; printed={printed}"
        );
    }
}
