use crate::bridge::model::{BridgeRecord, deserialize_record};
use crate::config::load::load_config;
use crate::state::layout::Layout;
use crate::util::query::sanitize_fts5_query;
use miette::{IntoDiagnostic, Result};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

fn provider_command() -> String {
    let current_dir = std::env::current_dir()
        .into_diagnostic()
        .unwrap_or_default();
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    load_config(&layout)
        .map(|c| c.bridge.provider_command)
        .unwrap_or_else(|_| "ai-brains".to_string())
}

pub fn query_external_cli(query: &str) -> Result<Vec<BridgeRecord>> {
    // CR3: Increased from 800ms to 2000ms to prevent false timeouts on loaded systems.
    let timeout = Duration::from_millis(2000);

    let command_name = provider_command();

    // 0073 / RT-A3: reject evil repo-config provider_command before spawn
    // (basename allowlist: ai-brains / ai-brains.exe only).
    if let Err(e) = crate::bridge::allowlist::check_bridge_provider_command(&command_name) {
        tracing::warn!(
            "Bridge provider_command '{}' denied by allowlist before spawn: {}. \
             Only ai-brains is permitted (0073).",
            command_name,
            e
        );
        return Ok(Vec::new());
    }

    let mut child = match Command::new(&command_name)
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
            return Ok(Vec::new());
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
            return Ok(Vec::new());
        }
        Err(e) => {
            tracing::warn!(
                "Error waiting for bridge provider '{}': {}",
                command_name,
                e
            );
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Vec::new());
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
        return Ok(Vec::new());
    }

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }

    let mut records = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match deserialize_record(line) {
            Ok(record) => records.push(record),
            Err(e) => {
                tracing::warn!("Failed to parse bridge provider record: {}", e);
            }
        }
    }

    Ok(records)
}
