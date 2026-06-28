pub const CREATE_NODE_TABLE: &str = ":create node { id: String => label: String, category: String, risk_score: Float, metadata: Json }";
pub const CREATE_EDGE_TABLE: &str = ":create edge { source: String, target: String, relation: String => confidence: Float, provenance_id: String }";
pub const CREATE_LEDGER_LINK_TABLE: &str =
    ":create ledger_link { node_id: String, ledger_id: String => interaction_type: String }";
pub const CREATE_LEDGER_ENTRY_TABLE: &str = ":create ledger_entry { id: Int => tx_id: String, category: String, entry_type: String, entity_normalized: String, change_type: String, summary: String, reason: String, committed_at: String, is_breaking: Bool, verification_status: String, trace_id: String, signature: String, public_key: String, risk: String, related_tickets: String }";
pub const CREATE_PROJECT_SYMBOL_TABLE: &str = ":create project_symbol { id: Int => file_path: String, qualified_name: String, symbol_name: String, symbol_kind: String, is_public: Bool, line_start: Int, line_end: Int }";
pub const CREATE_FTS_INDEX: &str =
    "::fts create node:fts_idx {extractor: label, tokenizer: Simple}";

pub const CREATE_TURN_TABLE: &str = ":create Turn { id: String => session_id: String, timestamp: String, project_id: String, summary: String, privacy_level: String }";
pub const CREATE_SESSION_TABLE: &str = ":create Session { id: String => project_id: String, started_at: String, ended_at: String, turn_count: Int, privacy_level: String }";
pub const CREATE_MEMORY_TABLE: &str = ":create Memory { id: String => source_turn_id: String, content: String, memory_type: String, privacy_level: String, created_at: String }";
pub const CREATE_DECISION_TABLE: &str = ":create Decision { id: String => title: String, context_field: String, decision_text: String, consequences: String, source_tx_id: String, timestamp: String }";

pub const GET_RELATIONS: &str = "::relations";

// K4: Service boundary and communication relations
pub const CREATE_SERVICE_ROOTS_TABLE: &str = ":create service_roots { name: String => dir_path: String, marker_kind: String, confidence: Float, last_indexed_at: String }";
pub const CREATE_SERVICE_DEPENDENCIES_TABLE: &str = ":create service_dependencies { caller_service: String, callee_service: String => pattern: String, call_kind: String, confidence: Float, last_indexed_at: String }";

pub fn node_count_query() -> &'static str {
    "?[count(id)] := *node{id}"
}

pub fn edge_count_query() -> &'static str {
    "?[count(source)] := *edge{source}"
}
