use crate::bridge::model::{BridgeRecord, deserialize_record};
use crate::util::query::sanitize_fts5_query;
use miette::Result;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

pub fn query_external_cli(query: &str) -> Result<Vec<BridgeRecord>> {
    // CR3: Increased from 800ms to 2000ms to prevent false timeouts on loaded systems.
    let timeout = Duration::from_millis(2000);

    let mut child = match Command::new("ai-brains")
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
                "Failed to spawn ai-brains CLI: {}. AI-Brains integration is degraded.",
                e
            );
            return Ok(Vec::new());
        }
    };

    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            tracing::warn!("ai-brains query timed out. Killing process.");
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Vec::new());
        }
        Err(e) => {
            tracing::warn!("Error waiting for ai-brains: {}", e);
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
            "ai-brains CLI returned error: {}. AI-Brains integration is degraded.",
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
                tracing::warn!("Failed to parse ai-brains record: {}", e);
            }
        }
    }

    Ok(records)
}
