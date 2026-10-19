use serde_json::Value;

use crate::commands::mcp::sanitize::{sanitize_mcp_content, sanitize_mcp_structured};

pub(super) fn error_response(msg: impl std::fmt::Display) -> Value {
    let mut final_msg = sanitize_mcp_content(&msg.to_string());
    if final_msg.contains("Failed to get layout")
        || final_msg.contains("Failed to discover git repository")
    {
        final_msg.push_str("\nHint: No .ledgerful directory found. Please run: ledgerful init");
    }
    serde_json::json!({
        "content": [{ "type": "text", "text": final_msg }],
        "isError": true
    })
}

pub(super) fn text_response(text: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": sanitize_mcp_content(text) }]
    })
}

pub(super) fn json_response<T: serde::Serialize>(data: &T) -> Value {
    match serde_json::to_string_pretty(data) {
        Ok(text) => serde_json::json!({
            "content": [{ "type": "text", "text": sanitize_mcp_structured(&text) }]
        }),
        Err(e) => error_response(format!("Failed to serialize response: {e}")),
    }
}

/// Wrap CLI `--json` stdout as structured MCP content (0552).
///
/// `stdout` is parsed as exactly one JSON document from its original bytes;
/// anything else (invalid UTF-8, truncated, NDJSON, BOM, trailing chatter such
/// as the subprocess truncation marker) is refused without echoing the body.
/// `tool` must be a trusted literal supplied by the handler.
pub(super) fn json_stdout_response(tool: &'static str, stdout: &[u8]) -> Value {
    match serde_json::from_slice::<Value>(stdout) {
        Ok(value) => json_response(&value),
        Err(_) => {
            // Metadata only: no stdout body, snippet, query, path or parser text.
            tracing::warn!(
                tool,
                stdout_len = stdout.len(),
                "MCP tool stdout was not a single JSON document"
            );
            error_response(format!("Failed to parse JSON tool output for {tool}"))
        }
    }
}
