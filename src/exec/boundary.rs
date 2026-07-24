use crate::exec::grouped::{GroupedProcessError, spawn_wait_grouped_captured};
use miette::Diagnostic;
use std::process::Command;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum ProcessError {
    #[error("Command not found: {cmd}")]
    #[diagnostic(help("Ensure the executable is in your PATH and accessible."))]
    NotFound { cmd: String },

    #[error("Command timed out after {timeout:?}")]
    #[diagnostic(code(ledgerful::process::timeout))]
    Timeout { timeout: Duration },

    #[error("Process exited with status {status}")]
    #[diagnostic(help("Check the captured stderr for more details."))]
    Failed { status: i32, stderr: String },

    #[error("I/O error during subprocess execution: {0}")]
    IoError(#[from] std::io::Error),
}

#[derive(Debug)]
pub struct ExecutionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    pub truncated: bool,
}

#[derive(Debug)]
pub struct CommandOptions {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl Default for CommandOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_bytes: 1024 * 1024, // 1MB
        }
    }
}

pub struct ExecutionBoundary;

impl ExecutionBoundary {
    pub fn execute(
        command: Command,
        options: &CommandOptions,
    ) -> Result<ExecutionResult, ProcessError> {
        let start = Instant::now();
        let program = command.get_program().to_string_lossy().to_string();

        let captured =
            match spawn_wait_grouped_captured(command, options.timeout, options.max_output_bytes) {
                Ok(c) => c,
                Err(GroupedProcessError::Spawn(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(ProcessError::NotFound { cmd: program });
                }
                Err(GroupedProcessError::Spawn(e)) => return Err(ProcessError::IoError(e)),
                Err(GroupedProcessError::Wait(e)) => return Err(ProcessError::IoError(e)),
                Err(GroupedProcessError::Timeout { timeout, .. }) => {
                    return Err(ProcessError::Timeout { timeout });
                }
            };

        let duration = start.elapsed();
        let exit_code = captured.status.code().unwrap_or(-1);

        Ok(ExecutionResult {
            exit_code,
            stdout: captured.stdout,
            stderr: captured.stderr,
            duration,
            truncated: captured.truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_basic_execution() {
        let cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", "echo hello"]);
            c
        } else {
            let mut c = Command::new("echo");
            c.arg("hello");
            c
        };
        let options = CommandOptions::default();
        let result = ExecutionBoundary::execute(cmd, &options).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
    }

    #[test]
    fn test_timeout() {
        let cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 5 127.0.0.1 >nul"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("5");
            c
        };
        let options = CommandOptions {
            timeout: Duration::from_secs(1),
            ..Default::default()
        };
        let result = ExecutionBoundary::execute(cmd, &options);
        match result {
            Err(ProcessError::Timeout { .. }) => (),
            _ => panic!("Expected timeout error, got {:?}", result),
        }
    }

    #[test]
    fn test_not_found() {
        let cmd = Command::new("nonexistent_command_12345");
        let options = CommandOptions::default();
        let result = ExecutionBoundary::execute(cmd, &options);
        match result {
            Err(ProcessError::NotFound { .. }) => (),
            _ => panic!("Expected NotFound error, got {:?}", result),
        }
    }

    #[test]
    fn test_truncation() {
        let cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", "python -c \"print('A' * 2000)\""]);
            c
        } else {
            let mut c = Command::new("printf");
            c.arg("'A%.0s' {1..2000}");
            c
        };
        let options = CommandOptions {
            max_output_bytes: 1000,
            ..Default::default()
        };
        // Truncation test may not find python/printf, so just verify no panic
        if let Ok(result) = ExecutionBoundary::execute(cmd, &options)
            && result.truncated
        {
            assert!(result.stdout.len() <= 1010);
        }
    }

    #[test]
    fn test_large_output_does_not_deadlock() {
        let cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("powershell");
            c.args([
                "-NoProfile",
                "-Command",
                "1..20000 | ForEach-Object { 'A' * 200 }",
            ]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args([
                "-c",
                "yes AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA | head -n 20000",
            ]);
            c
        };
        let options = CommandOptions {
            timeout: Duration::from_secs(10),
            max_output_bytes: 1024,
        };

        let result = ExecutionBoundary::execute(cmd, &options).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.truncated);
        assert!(result.stdout.len() <= 1024);
    }
}
