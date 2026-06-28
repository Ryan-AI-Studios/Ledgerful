use ledgerful::bridge::model::{
    BridgeDirection, BridgePayload, BridgeRecord, deserialize_record, serialize_record,
};

#[test]
fn test_bridge_record_serialization() {
    let payload = BridgePayload::Hotspot {
        path: "src/lib.rs".to_string(),
        score: 0.9,
        reason: "high temporal coupling".to_string(),
        temporal_coupling: 0.0,
        failure_risk_probability: 0.0,
    };
    let record = BridgeRecord::new(
        BridgeDirection::Outbound,
        "test-project".to_string(),
        "hotspot_delta",
        payload,
    );

    let serialized = serialize_record(&record).unwrap();
    assert!(serialized.contains(r#""bridge_version":"0.3""#));
    assert!(serialized.contains(r#""direction":"outbound""#));
    assert!(serialized.contains(r#""project_id":"test-project""#));
    assert!(serialized.contains(r#""type":"Hotspot""#));
}

#[test]
fn test_bridge_record_deserialization() {
    let json = r#"{
        "bridge_version": "0.3",
        "direction": "inbound",
        "timestamp": "2026-05-19T12:00:00Z",
        "project_id": "test-project",
        "record_kind": "insight",
        "payload": {"type":"Insight","memory_id":"abc","relevance":0.8,"content":"refactor suggested"},
        "privacy": "Public"
    }"#;
    let record: BridgeRecord = deserialize_record(json).unwrap();

    assert_eq!(record.project_id, "test-project");
    if let BridgePayload::Insight { memory_id, .. } = record.payload {
        assert_eq!(memory_id, "abc");
    } else {
        panic!("Expected Insight variant");
    }
}
