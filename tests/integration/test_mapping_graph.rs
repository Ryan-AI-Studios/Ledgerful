use ledgerful::platform::urn::build_urn;
use ledgerful::state::graph_kinds::{EdgeKind, NodeKind};
use ledgerful::state::storage_cozo::CozoStorage;
use ledgerful::state::storage_cozo::GraphEdge;
use ledgerful::state::storage_cozo::GraphNode;
use serde_json::json;

#[test]
fn test_entity_to_test_link() {
    let storage = CozoStorage::new_in_memory().expect("Failed to create in-memory storage");

    // Create a symbol node
    let symbol_id = build_urn(NodeKind::Symbol, "src/lib.rs:my_func");
    let symbol_node = GraphNode {
        id: symbol_id.clone(),
        label: "my_func".to_string(),
        category: NodeKind::Symbol,
        risk_score: 0.1,
        metadata: Some(json!({"file": "src/lib.rs"})),
    };

    // Create a test node
    let test_id = build_urn(NodeKind::Test, "tests/test_lib.rs:test_my_func");
    let test_node = GraphNode {
        id: test_id.clone(),
        label: "test_my_func".to_string(),
        category: NodeKind::Test,
        risk_score: 0.0,
        metadata: Some(json!({
            "file": "tests/test_lib.rs",
            "test_kind": "unit",
            "confidence": 0.9,
            "flakiness": 0.0
        })),
    };

    storage
        .put_node_batch(&[symbol_node, test_node])
        .expect("Failed to put nodes");

    // Create a Validates edge
    let edge = GraphEdge {
        source: test_id.clone(),
        target: symbol_id.clone(),
        relation: EdgeKind::Validates,
        confidence: 0.9,
        provenance_id: "test_mapping".to_string(),
    };

    storage
        .put_edge_batch(&[edge])
        .expect("Failed to put edges");

    // Verify reachability
    let script = format!(
        "?[test_id, symbol_id] := *edge{{source: test_id, target: symbol_id, relation: 'validates'}}, test_id == '{}'",
        test_id
    );
    let res = storage.run_script(&script).expect("Failed to run script");
    assert_eq!(res.rows.len(), 1);

    let row_test_id = match &res.rows[0][0] {
        cozo::DataValue::Str(s) => s.to_string(),
        _ => panic!("Expected string for test_id"),
    };
    let row_symbol_id = match &res.rows[0][1] {
        cozo::DataValue::Str(s) => s.to_string(),
        _ => panic!("Expected string for symbol_id"),
    };

    assert_eq!(row_test_id, test_id);
    assert_eq!(row_symbol_id, symbol_id);
}
