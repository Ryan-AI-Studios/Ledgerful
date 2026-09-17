use crate::bridge::allowlist::check_bridge_provider_command;
use crate::util::query::sanitize_fts5_query;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Debug, Clone)]
pub(crate) enum CliRun {
    AllowlistDenied { command: String },
    SpawnErr { command: String },
    Timeout { command: String },
    NonZero { command: String, message: String },
    WaitErr { command: String, message: String },
    Stdout { command: String, stdout: String },
}

pub(crate) fn query_external_cli(query: &str, timeout: Duration, command_name: &str) -> CliRun {
    if check_bridge_provider_command(command_name).is_err() {
        tracing::warn!(
            "Bridge provider_command '{}' denied by allowlist before spawn. \
             Only ai-brains is permitted (0073).",
            command_name
        );
        return CliRun::AllowlistDenied {
            command: command_name.to_string(),
        };
    }

    let mut child = match Command::new(command_name)
        .args([
            "sync",
            "query",
            &sanitize_fts5_query(query),
            "--format",
            "ndjson",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Failed to spawn bridge provider '{}': {}. Bridge provider integration is degraded.",
                command_name,
                e
            );
            return CliRun::SpawnErr {
                command: command_name.to_string(),
            };
        }
    };

    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            tracing::warn!(
                "Bridge provider '{}' query timed out. Killing process.",
                command_name
            );
            let _ = child.kill();
            let _ = child.wait();
            return CliRun::Timeout {
                command: command_name.to_string(),
            };
        }
        Err(e) => {
            tracing::warn!(
                "Error waiting for bridge provider '{}': {}",
                command_name,
                e
            );
            let _ = child.kill();
            let _ = child.wait();
            return CliRun::WaitErr {
                command: command_name.to_string(),
                message: e.to_string(),
            };
        }
    };

    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut stderr);
        }
        tracing::warn!(
            "Bridge provider '{}' returned error: {}. Bridge provider integration is degraded.",
            command_name,
            stderr
        );
        return CliRun::NonZero {
            command: command_name.to_string(),
            message: stderr,
        };
    }

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }

    CliRun::Stdout {
        command: command_name.to_string(),
        stdout,
    }
}
