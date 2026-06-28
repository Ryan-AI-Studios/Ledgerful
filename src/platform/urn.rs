use crate::state::graph_kinds::NodeKind;

pub fn build_urn(kind: NodeKind, identifier: &str) -> String {
    // Normalize identifier: replace backslashes with forward slashes for cross-platform stability
    let normalized = identifier.replace('\\', "/");
    format!("urn:ledgerful:{}:{}", kind, normalized)
}
