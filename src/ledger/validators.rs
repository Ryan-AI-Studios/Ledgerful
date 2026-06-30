use crate::ledger::enforcement::ValidationLevel;
use crate::ledger::error::LedgerError;
use crate::platform::process_policy::{ProcessPolicy, check_policy};
use miette::Result;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

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

        let mut child = Command::new(executable)
            .args(&processed_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                LedgerError::Validation(format!("Failed to start validator '{}': {}", name, e))
            })?;

        let timeout = Duration::from_millis(timeout_ms);
        let status = match child.wait_timeout(timeout).map_err(|e| {
            LedgerError::Validation(format!("Error waiting for validator '{}': {}", name, e))
        })? {
            Some(status) => status,
            None => {
                child.kill().ok();
                return Ok(ValidationResult {
                    name,
                    success: false,
                    exit_code: None,
                    stdout: "".to_string(),
                    stderr: "Validator timed out".to_string(),
                    level,
                });
            }
        };

        let output = child.wait_with_output().map_err(|e| {
            LedgerError::Validation(format!("Failed to read validator output '{}': {}", name, e))
        })?;

        Ok(ValidationResult {
            name,
            success: status.success(),
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            level,
        })
    }
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

        let executable = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "echo"
        };
        let args = if cfg!(target_os = "windows") {
            vec!["/C".to_string(), "echo".to_string(), "{entity}".to_string()]
        } else {
            vec!["{entity}".to_string()]
        };
        let result = ValidatorRunner::run(
            "echo-entity".to_string(),
            executable,
            &args,
            root,
            &root.join("src").join("main.rs").to_string_lossy(),
            5000,
            ValidationLevel::Error,
            &fake_policy(),
        )
        .unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("main.rs"));
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
}
