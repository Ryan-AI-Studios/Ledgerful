use crate::commands::CommandError;
use crate::exec::{CommandOptions, ExecutionBoundary, ExecutionResult, ProcessError};
use crate::platform::process_policy::{ProcessPolicy, check_policy};
use crate::verify::plan::VerificationStep;
use miette::{IntoDiagnostic, Result};
use std::env;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Direct,
    Shell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStep {
    pub display_command: String,
    pub executable: String,
    pub args: Vec<String>,
    pub timeout_secs: u64,
    pub description: String,
    pub execution_mode: ExecutionMode,
}

pub fn prepare_manual_step(step: &VerificationStep) -> PreparedStep {
    shell_step(step)
}

pub fn prepare_rule_step(step: &VerificationStep) -> Result<PreparedStep> {
    if step.shell {
        return Ok(shell_step(step));
    }

    if contains_shell_metacharacters(&step.command) {
        return Err(CommandError::Verify(format!(
            "Command contains shell metacharacters but 'shell' is false. \
             Set shell: true in the step configuration if shell features are required: {}",
            step.command
        ))
        .into());
    }

    match split_command_string(&step.command) {
        Some(tokens) if !tokens.is_empty() => Ok(PreparedStep {
            display_command: step.command.clone(),
            executable: tokens[0].clone(),
            args: tokens[1..].to_vec(),
            timeout_secs: step.timeout_secs,
            description: step.description.clone(),
            execution_mode: ExecutionMode::Direct,
        }),
        _ => Err(CommandError::Verify(format!(
            "Unable to parse command into argv tokens: {}",
            step.command
        ))
        .into()),
    }
}

pub fn execute_step(step: &PreparedStep, policy: &ProcessPolicy) -> Result<ExecutionResult> {
    check_policy(&step.executable, policy).into_diagnostic()?;

    let mut command = Command::new(&step.executable);
    command.args(&step.args);
    command.stdin(Stdio::null());
    command
        .current_dir(env::current_dir().into_diagnostic()?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let options = CommandOptions {
        timeout: Duration::from_secs(step.timeout_secs),
        ..Default::default()
    };

    match ExecutionBoundary::execute(command, &options) {
        Ok(result) => {
            if step.execution_mode == ExecutionMode::Shell && looks_like_command_not_found(&result)
            {
                let hint = fallback_install_hint(&step.display_command);
                return Err(CommandError::Verify(format!(
                    "Command not found via shell fallback: {}{}",
                    step.display_command, hint
                ))
                .into());
            }
            Ok(result)
        }
        Err(ProcessError::Timeout { timeout }) => {
            Err(CommandError::Verify(format!("Timed out after {:?}", timeout)).into())
        }
        Err(ProcessError::NotFound { cmd }) => {
            let hint = fallback_install_hint(&cmd);
            Err(CommandError::Verify(format!("Command not found: {}{}", cmd, hint)).into())
        }
        Err(ProcessError::Failed { status, stderr }) => Err(CommandError::Verify(format!(
            "Process exited with status {}: {}",
            status, stderr
        ))
        .into()),
        Err(e) => Err(e.into()),
    }
}

fn fallback_install_hint(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if lower.contains("nextest") {
        "\nHint: You can install nextest via 'cargo install cargo-nextest' or visit https://nexte.st".to_string()
    } else if lower.contains("cargo") {
        "\nHint: Verify that Rust/Cargo is installed. Visit https://rustup.rs to set up the toolchain.".to_string()
    } else if lower.contains("npm") {
        "\nHint: Verify Node.js/NPM is installed. Visit https://nodejs.org to set up Node."
            .to_string()
    } else if lower.contains("python") || lower.contains("pytest") || lower.contains("pip") {
        "\nHint: Verify Python and your virtual environment are active and on your PATH."
            .to_string()
    } else if lower.contains("make") {
        "\nHint: Install make (e.g. 'choco install make' on Windows, or 'brew install make' on macOS).".to_string()
    } else {
        "\nHint: Double check that the executable is installed and available on your PATH environment variable.".to_string()
    }
}

fn shell_step(step: &VerificationStep) -> PreparedStep {
    if cfg!(target_os = "windows") {
        PreparedStep {
            display_command: step.command.clone(),
            executable: "cmd".to_string(),
            args: vec!["/C".to_string(), step.command.clone()],
            timeout_secs: step.timeout_secs,
            description: step.description.clone(),
            execution_mode: ExecutionMode::Shell,
        }
    } else {
        PreparedStep {
            display_command: step.command.clone(),
            executable: "sh".to_string(),
            args: vec!["-c".to_string(), step.command.clone()],
            timeout_secs: step.timeout_secs,
            description: step.description.clone(),
            execution_mode: ExecutionMode::Shell,
        }
    }
}

fn contains_shell_metacharacters(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            '|' | '&' | ';' | '>' | '<' | '(' | ')' | '$' | '*' | '?' | '{' | '}' | '\n'
        )
    })
}

fn split_command_string(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    for ch in command.chars() {
        match quote {
            Some(active_quote) if ch == active_quote => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }

    if quote.is_some() {
        return None;
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

fn looks_like_command_not_found(result: &ExecutionResult) -> bool {
    let stderr = result.stderr.to_ascii_lowercase();
    let stdout = result.stdout.to_ascii_lowercase();
    result.exit_code != 0
        && (stderr.contains("not recognized as an internal or external command")
            || stderr.contains("command not found")
            || (result.exit_code == 127 && stderr.contains("not found"))
            || stdout.contains("not recognized as an internal or external command")
            || stdout.contains("command not found")
            || (result.exit_code == 127 && stdout.contains("not found")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_step(command: &str, timeout_secs: u64) -> VerificationStep {
        VerificationStep {
            description: "test".to_string(),
            command: command.to_string(),
            timeout_secs,
            shell: false,
        }
    }

    #[test]
    fn prepare_rule_step_uses_direct_execution_for_simple_commands() {
        let step = base_step("cargo test -j 1 --all-features -- --test-threads=1", 5);
        let prepared = prepare_rule_step(&step).unwrap();

        assert_eq!(prepared.execution_mode, ExecutionMode::Direct);
        assert_eq!(prepared.executable, "cargo");
        assert_eq!(
            prepared.args,
            vec![
                "test",
                "-j",
                "1",
                "--all-features",
                "--",
                "--test-threads=1"
            ]
        );
    }

    #[test]
    fn prepare_rule_step_rejects_shell_syntax_when_shell_false() {
        let step = base_step("echo hello | sort", 5);
        let err = prepare_rule_step(&step).unwrap_err();
        let err_text = format!("{err:?}");
        assert!(
            err_text.contains("shell"),
            "expected shell error, got {err_text}"
        );
    }

    #[test]
    fn prepare_rule_step_uses_shell_when_shell_true() {
        let step = VerificationStep {
            description: "test".to_string(),
            command: "echo hello | sort".to_string(),
            timeout_secs: 5,
            shell: true,
        };
        let prepared = prepare_rule_step(&step).unwrap();

        assert_eq!(prepared.execution_mode, ExecutionMode::Shell);
        if cfg!(target_os = "windows") {
            assert_eq!(prepared.executable, "cmd");
            assert_eq!(prepared.args, vec!["/C", "echo hello | sort"]);
        } else {
            assert_eq!(prepared.executable, "sh");
            assert_eq!(prepared.args, vec!["-c", "echo hello | sort"]);
        }
    }

    #[test]
    fn execute_step_denies_blocked_commands() {
        let prepared = PreparedStep {
            display_command: "cargo test".to_string(),
            executable: "cargo".to_string(),
            args: vec!["test".to_string()],
            timeout_secs: 5,
            description: "test".to_string(),
            execution_mode: ExecutionMode::Direct,
        };
        let policy = ProcessPolicy {
            denied_commands: vec!["cargo".to_string()],
            ..ProcessPolicy::default()
        };

        let err = execute_step(&prepared, &policy).unwrap_err();
        assert!(format!("{err:?}").contains("denied"));
    }

    #[test]
    fn execute_step_direct_process_succeeds() {
        let (executable, args) = if cfg!(target_os = "windows") {
            (
                "cmd".to_string(),
                vec!["/C".to_string(), "echo direct-ok".to_string()],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), "printf direct-ok".to_string()],
            )
        };
        let prepared = PreparedStep {
            display_command: "direct echo".to_string(),
            executable,
            args,
            timeout_secs: 10,
            description: "test".to_string(),
            execution_mode: ExecutionMode::Direct,
        };

        let result = execute_step(&prepared, &ProcessPolicy::default()).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("direct-ok"));
    }

    #[test]
    fn execute_step_manual_shell_succeeds() {
        let mut step = base_step("echo hello", 5);
        step.shell = true;
        let prepared = prepare_manual_step(&step);
        let result = execute_step(&prepared, &ProcessPolicy::default()).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.to_ascii_lowercase().contains("hello"));
    }

    #[test]
    fn shell_exit_127_not_found_is_normalized() {
        let result = ExecutionResult {
            exit_code: 127,
            stdout: String::new(),
            stderr: "sh: 1: missing_tool: not found".to_string(),
            duration: Duration::from_millis(1),
            truncated: false,
        };

        assert!(looks_like_command_not_found(&result));
    }

    #[test]
    fn execute_step_timeout_errors() {
        let prepared = if cfg!(target_os = "windows") {
            PreparedStep {
                display_command: "ping -n 10 127.0.0.1".to_string(),
                executable: "ping".to_string(),
                args: vec!["-n".to_string(), "10".to_string(), "127.0.0.1".to_string()],
                timeout_secs: 1,
                description: "timeout".to_string(),
                execution_mode: ExecutionMode::Direct,
            }
        } else {
            PreparedStep {
                display_command: "sleep 10".to_string(),
                executable: "sleep".to_string(),
                args: vec!["10".to_string()],
                timeout_secs: 1,
                description: "timeout".to_string(),
                execution_mode: ExecutionMode::Direct,
            }
        };

        let err = execute_step(&prepared, &ProcessPolicy::default()).unwrap_err();
        assert!(format!("{err:?}").contains("Timed out"));
    }
}
