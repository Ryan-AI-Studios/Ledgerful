//! MCP tool dispatch barrel. Handlers, spawn, and envelope live in sibling modules.

use serde_json::Value;

mod envelope;
mod handlers;
mod spawn;

#[cfg(test)]
mod tests;

pub use spawn::McpToolError;

use envelope::error_response;
use handlers::{
    handle_ask, handle_change_context, handle_dead_code, handle_endpoints_changed, handle_hotspots,
    handle_ledger_search, handle_ledger_status, handle_scan, handle_search,
    handle_security_boundaries, handle_verify_plan,
};

type ToolHandler = fn(Value) -> Value;

/// Name → handler table aligned with [`crate::commands::mcp::INVENTORY`].
/// Sorted by name so a missing inventory entry is a table test failure, not a
/// three-file merge conflict.
static TOOL_HANDLERS: &[(&str, ToolHandler)] = &[
    ("ask", handle_ask),
    ("change_context", handle_change_context),
    ("dead_code", handle_dead_code),
    ("endpoints_changed", handle_endpoints_changed),
    ("hotspots", handle_hotspots),
    ("ledger_search", handle_ledger_search),
    ("ledger_status", handle_ledger_status),
    ("scan", handle_scan),
    ("search", handle_search),
    ("security_boundaries", handle_security_boundaries),
    ("verify_plan", handle_verify_plan),
];

fn handler_for_tool(name: &str) -> Option<ToolHandler> {
    TOOL_HANDLERS
        .iter()
        .find_map(|(n, handler)| (*n == name).then_some(*handler))
}

pub fn dispatch_tool(name: &str, params: Value) -> Value {
    match handler_for_tool(name) {
        Some(handler) => handler(params),
        None => error_response(McpToolError::UnknownTool {
            name: name.to_string(),
        }),
    }
}
