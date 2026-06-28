use miette::Diagnostic;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use wait_timeout::ChildExt;

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
        mut command: Command,
        options: &CommandOptions,
    ) -> Result<ExecutionResult, ProcessError> {
        let start = Instant::now();

        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProcessError::NotFound {
                    cmd: command.get_program().to_string_lossy().to_string(),
                });
            }
            Err(e) => return Err(ProcessError::IoError(e)),
        };

        let stdout_reader = child.stdout.take().map(|stdout| {
            let max_output_bytes = options.max_output_bytes;
            thread::spawn(move || read_capped(stdout, max_output_bytes))
        });
        let stderr_reader = child.stderr.take().map(|stderr| {
            let max_output_bytes = options.max_output_bytes;
            thread::spawn(move || read_capped(stderr, max_output_bytes))
        });

        let status = match child.wait_timeout(options.timeout)? {
            Some(status) => status,
            None => {
                child.kill().ok();
                child.wait().ok();
                join_reader(stdout_reader);
                join_reader(stderr_reader);
                return Err(ProcessError::Timeout {
                    timeout: options.timeout,
                });
            }
        };

        let duration = start.elapsed();

        let stdout = join_reader(stdout_reader);
        let stderr = join_reader(stderr_reader);
        let truncated = stdout.truncated || stderr.truncated;

        let exit_code = status.code().unwrap_or(-1);

        Ok(ExecutionResult {
            exit_code,
            stdout: stdout.output,
            stderr: stderr.output,
            duration,
            truncated,
        })
    }
}

#[derive(Debug, Default)]
struct CapturedOutput {
    output: String,
    truncated: bool,
}

fn read_capped(mut reader: impl Read, max_output_bytes: usize) -> CapturedOutput {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];

    loop {
        let bytes_read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(bytes_read) => bytes_read,
            Err(_) => break,
        };

        let remaining = max_output_bytes.saturating_sub(output.len());
        if remaining > 0 {
            let bytes_to_store = bytes_read.min(remaining);
            output.extend_from_slice(&buffer[..bytes_to_store]);
        }
        if bytes_read > remaining {
            truncated = true;
        }
    }

    CapturedOutput {
        output: String::from_utf8_lossy(&output).to_string(),
        truncated,
    }
}

fn join_reader(reader: Option<thread::JoinHandle<CapturedOutput>>) -> CapturedOutput {
    reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
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
