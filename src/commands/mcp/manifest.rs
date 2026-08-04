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
                }
            }
        }"#,
    },
    ToolDescriptor {
        name: "scan",
        description: "Assess the impact and risk of uncommitted changes in the repository.",
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
        description: "Search the architectural history and transaction ledger.",
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
                }
            },
            "required": ["query"]
        }"#,
    },
    ToolDescriptor {
        name: "hotspots",
        description: "Identify brittle files with high change frequency or complexity.",
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
        description: "List API endpoints affected by current changes.",
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
