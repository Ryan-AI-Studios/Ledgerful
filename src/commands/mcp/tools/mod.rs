//! MCP tool dispatch barrel. Handlers, spawn, and envelope live in sibling modules.

use serde_json::Value;
use std::path::Path;

mod envelope;
mod handlers;
mod spawn;

#[cfg(test)]
mod tests;

pub use handlers::{build_verify_plan_preview, frame_verify_plan_preview};
pub use spawn::McpToolError;

use envelope::error_response;
use handlers::{
    handle_architecture_propose, handle_ask, handle_change_context, handle_dead_code,
    handle_endpoints_changed, handle_hotspots, handle_ledger_search, handle_ledger_status,
    handle_scan, handle_search, handle_security_boundaries, handle_verify_plan,
};

type ToolHandler = fn(Value, &Path) -> Value;

/// Name → handler table aligned with [`crate::commands::mcp::INVENTORY`].
/// Sorted by name so a missing inventory entry is a table test failure, not a
/// three-file merge conflict.
static TOOL_HANDLERS: &[(&str, ToolHandler)] = &[
    ("architecture_propose", handle_architecture_propose),
    ("ask", |params, _| handle_ask(params)),
    ("change_context", |params, _| handle_change_context(params)),
    ("dead_code", |params, _| handle_dead_code(params)),
    ("endpoints_changed", |params, _| {
        handle_endpoints_changed(params)
    }),
    ("hotspots", |params, _| handle_hotspots(params)),
    ("ledger_search", |params, _| handle_ledger_search(params)),
    ("ledger_status", |params, _| handle_ledger_status(params)),
    ("scan", |params, _| handle_scan(params)),
    ("search", |params, _| handle_search(params)),
    ("security_boundaries", |params, _| {
        handle_security_boundaries(params)
    }),
    ("verify_plan", |params, _| handle_verify_plan(params)),
];

fn handler_for_tool(name: &str) -> Option<ToolHandler> {
    TOOL_HANDLERS
        .iter()
        .find_map(|(n, handler)| (*n == name).then_some(*handler))
}

pub fn dispatch_tool(name: &str, params: Value) -> Value {
    dispatch_tool_from(name, params, Path::new("."))
}

pub(super) fn dispatch_tool_from(name: &str, params: Value, proposal_cwd: &Path) -> Value {
    match handler_for_tool(name) {
        Some(handler) => handler(params, proposal_cwd),
        None => error_response(McpToolError::UnknownTool {
            name: name.to_string(),
        }),
    }
}
