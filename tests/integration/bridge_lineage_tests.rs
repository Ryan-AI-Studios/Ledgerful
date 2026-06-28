use ledgerful::bridge::model::{BridgeDirection, BridgePayload, BridgeRecord, calculate_hash};

#[test]
fn test_bridge_record_chaining() {
    let payload = BridgePayload::Query {
        text: "test".to_string(),
    };
    let r1 = BridgeRecord::new(
        BridgeDirection::Outbound,
        "p1".to_string(),
        "query",
        payload,
    );

    let h1 = calculate_hash(&r1);

    let payload2 = BridgePayload::Query {
        text: "test2".to_string(),
    };
    let mut r2 = BridgeRecord::new(
        BridgeDirection::Outbound,
        "p1".to_string(),
        "query",
        payload2,
    );
    r2.parent_hash = Some(h1.clone());

    let h2 = calculate_hash(&r2);
    assert_ne!(h1, h2);
}
