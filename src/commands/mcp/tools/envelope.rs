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
