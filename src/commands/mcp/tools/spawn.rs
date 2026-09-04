use std::process::{Command, Stdio};
use std::time::Duration;

pub(super) const MCP_TOOL_TIMEOUT_SECS: u64 = 120;
/// Child `ask --timeout` so product messaging fires before the parent kill (M4).
/// Must stay strictly under [`MCP_TOOL_TIMEOUT_SECS`].
pub(super) const MCP_ASK_CHILD_TIMEOUT_SECS: u64 = 110;
pub(super) const MCP_ASK_CHILD_TIMEOUT_FLAG: &str = "110";
const MCP_SUBPROCESS_OUTPUT_MAX: usize = 4 * 1024 * 1024;

/// Internal classification of MCP tool failures. Rendered through
/// [`error_response`] as the existing `{content, isError}` envelope — not a
/// JSON-RPC error object or `structuredContent`.
#[derive(Debug, thiserror::Error)]
pub enum McpToolError {
    #[error("Process policy denied ledgerful self-spawn: {0}")]
    Policy(String),
    #[error("Failed to spawn ledgerful tool: {0}")]
    Spawn(String),
    #[error("ledgerful tool timed out after {} seconds", MCP_TOOL_TIMEOUT_SECS)]
    Timeout,
    #[error("Error waiting for ledgerful tool: {0}")]
    Wait(String),
    #[error("Failed to get layout: {0}")]
    Layout(String),
    #[error("{0}")]
    InvalidParams(String),
    #[error("Tool {name} not implemented yet.")]
    UnknownTool { name: String },
    #[error("Failed to read ledgerful tool output: {stderr}")]
    ChildFailed { stdout: String, stderr: String },
    #[error("{0}")]
    Other(String),
}

fn get_ledgerful_exe() -> std::path::PathBuf {
    // Legitimate: re-exec this binary for MCP tool subprocesses.
    // nosemgrep: rust.lang.security.current-exe.current-exe
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ledgerful"))
}

pub(super) fn run_ledgerful_tool<I, S>(args: I) -> Result<std::process::Output, McpToolError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let exe = get_ledgerful_exe();
    let exe_str = exe.to_string_lossy().to_string();

    crate::platform::process_policy::check_policy(
        &exe_str,
        &crate::platform::process_policy::ProcessPolicy {
            allowed_commands: vec![exe_str.clone()],
            denied_commands: Vec::new(),
            default_timeout_secs: MCP_TOOL_TIMEOUT_SECS,
            strict: true,
        },
    )
    .map_err(|e| McpToolError::Policy(e.to_string()))?;

    // 0073: every MCP tool child inherits Forbidden cloud policy (unless host
    // LEDGERFUL_MCP_ALLOW_CLOUD_EGRESS) + NON_INTERACTIVE so cloud fallbacks
    // and interactive degrade→Gemini cannot run. When allow-cloud is set,
    // explicitly remove LEDGERFUL_CLOUD_POLICY so an inherited Forbidden
    // marker cannot stick on the child.
    let mut command = Command::new(&exe);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in crate::local_model::cloud_policy::mcp_tool_spawn_env() {
        command.env(key, value);
    }
    for key in crate::local_model::cloud_policy::mcp_tool_spawn_env_removes() {
        command.env_remove(key);
    }

    let mut child = command
        .spawn()
        .map_err(|e| McpToolError::Spawn(e.to_string()))?;

    let timeout = Duration::from_secs(MCP_TOOL_TIMEOUT_SECS);
    let status = match wait_timeout::ChildExt::wait_timeout(&mut child, timeout)
        .map_err(|e| McpToolError::Wait(e.to_string()))?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            return Err(McpToolError::Timeout);
        }
    };

    child
        .wait_with_output()
        .map(|mut output| {
            output.status = status;
            if output.stdout.len() > MCP_SUBPROCESS_OUTPUT_MAX {
                let boundary = String::from_utf8_lossy(&output.stdout)
                    .floor_char_boundary(MCP_SUBPROCESS_OUTPUT_MAX)
                    .min(output.stdout.len());
                let mut truncated = output.stdout[..boundary].to_vec();
                truncated.extend_from_slice(b"\n[...subprocess output truncated...]");
                output.stdout = truncated;
            }
            if output.stderr.len() > MCP_SUBPROCESS_OUTPUT_MAX {
                let boundary = String::from_utf8_lossy(&output.stderr)
                    .floor_char_boundary(MCP_SUBPROCESS_OUTPUT_MAX)
                    .min(output.stderr.len());
                let mut truncated = output.stderr[..boundary].to_vec();
                truncated.extend_from_slice(b"\n[...subprocess stderr truncated...]");
                output.stderr = truncated;
            }
            output
        })
        .map_err(|e| McpToolError::Other(format!("Failed to read ledgerful tool output: {e}")))
}
