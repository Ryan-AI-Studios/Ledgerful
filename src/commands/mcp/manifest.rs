pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub schema_json: &'static str,
}

pub const INVENTORY: &[ToolDescriptor] = &[
    ToolDescriptor {
        name: "change_context",
        description: "Budgeted agent change packet: impact risk, capped readSet, doctor readiness, pending ledger, and greenfield changeHints/suggestedTests (schemaVersion 1). Prefer after doctor --json.",
        schema_json: r#"{
            "type": "object",
            "properties": {
                "detail": {
                    "type": "string",
                    "description": "minimal (default) or standard",
                    "default": "minimal"
                },
                "max_files": {
                    "type": "integer",
                    "description": "Cap on readSet length (default 20)",
                    "default": 20
                },
                "base_ref": {
                    "type": "string",
                    "description": "Git ref for structural impact/readSet/risk only; doctor and ledger stay present-tense"
                },
                "blast_depth": {
                    "type": "integer",
                    "description": "Structural blast hop depth (default 1; max 2)"
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Prospective paths (as if changed). Mutually exclusive with base_ref. Cap 50. In-memory only."
                },
                "include_governance": {
                    "type": "boolean",
                    "description": "Include process/governance temporal couplings in risk + readSet (pathMode=all). Default false (code mode).",
                    "default": false
                }
            }
        }"#,
    },
    ToolDescriptor {
        name: "scan",
        description: "Assess the impact and risk of uncommitted changes in the repository. MCP scan stays a full-impact dump without paths/include_governance in v1 — use CLI or change_context for prospective.",
        schema_json: r#"{"type": "object", "properties": {}}"#,
    },
    ToolDescriptor {
        name: "search",
        description: "High-precision regex and text discovery for code symbols. Returns a single JSON object (schemaVersion 1) with results[]. Refreshes the local index when stale; may take multi-seconds on large repos.",
        schema_json: r#"{
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query (regex supported)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return",
                    "default": 50
                }
            },
            "required": ["query"]
        }"#,
    },
    ToolDescriptor {
        name: "ask",
        description: "Conceptual and semantic natural language queries about the codebase. Uses a local model by default; MCP children run with LEDGERFUL_CLOUD_POLICY=forbidden (zero cloud egress) unless the host sets LEDGERFUL_MCP_ALLOW_CLOUD_EGRESS=1. Repo config cannot clear Forbidden.",
        schema_json: r#"{
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The natural language question"
                }
            },
            "required": ["query"]
        }"#,
    },
    ToolDescriptor {
        name: "ledger_status",
        description: "Get the current provenance status, including pending transactions and unaudited drift.",
        schema_json: r#"{"type": "object", "properties": {}}"#,
    },
    ToolDescriptor {
        name: "ledger_search",
        description: "Search the architectural history and transaction ledger. Default omits ROLLBACK entries; pass include_rollback to restore them ranked after non-rollback.",
        schema_json: r#"{
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "days": {
                    "type": "integer",
                    "description": "Limit search to the last N days",
                    "default": 30
                },
                "include_rollback": {
                    "type": "boolean",
                    "description": "Include ROLLBACK entries (omitted by default; ranked after non-rollback)",
                    "default": false
                }
            },
            "required": ["query"]
        }"#,
    },
    ToolDescriptor {
        name: "hotspots",
        description: "Identify brittle files with high change frequency or complexity. Returns an in-process hotspot array (not the CLI files[] envelope).",
        schema_json: r#"{
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of hotspots to return",
                    "default": 10
                }
            }
        }"#,
    },
    ToolDescriptor {
        name: "endpoints_changed",
        description: "List API endpoints affected by current changes. Tool text is the CLI endpoints --changed --json envelope (schemaVersion 1, results[]).",
        schema_json: r#"{"type": "object", "properties": {}}"#,
    },
    ToolDescriptor {
        name: "security_boundaries",
        description: "Inspect security policy boundaries and their risk status.",
        schema_json: r#"{"type": "object", "properties": {}}"#,
    },
    ToolDescriptor {
        name: "dead_code",
        description: "Identify likely unused functions and types based on graph reachability.",
        schema_json: r#"{
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of dead code findings to return",
                    "default": 50
                }
            }
        }"#,
    },
    ToolDescriptor {
        name: "verify_plan",
        description: "Predict the verification plan (test targets) for current changes.",
        schema_json: r#"{"type": "object", "properties": {}}"#,
    },
];

pub fn get_tool_count() -> usize {
    INVENTORY.len()
}
