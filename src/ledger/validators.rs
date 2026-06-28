use crate::ledger::enforcement::ValidationLevel;
use crate::ledger::error::LedgerError;
use crate::platform::process_policy::{ProcessPolicy, check_policy};
use miette::Result;
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
    pub fn run(
        name: String,
        executable: &str,
        args: &[String],
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
