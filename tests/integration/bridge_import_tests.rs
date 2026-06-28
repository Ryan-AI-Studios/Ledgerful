use crate::common::DirGuard;
use ledgerful::bridge::import::execute_import;
use ledgerful::bridge::model::{BridgeRecord, calculate_hash};
use ledgerful::impact::packet::ImpactPacket;
use ledgerful::state::layout::Layout;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_bridge_import_enrichment() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // Create a dummy latest-impact.json
    let dummy_packet = ImpactPacket::default();
    let dummy_json = serde_json::to_string_pretty(&dummy_packet).unwrap();
    let latest_impact_path = layout.reports_dir().join("latest-impact.json");
    fs::write(&latest_impact_path, dummy_json).unwrap();

    let in_path = root.join("import.ndjson");
    let insight = r#"{"bridge_version":"0.3","direction":"inbound","timestamp":"2026-05-19T12:00:00Z","project_id":"test-project","record_kind":"insight","payload":{"type":"Insight","memory_id":"mem-123","relevance":0.95,"content":"Architecture note: Use trait-based dispatch for bridge providers."},"privacy":"Public"}"#;
    fs::write(&in_path, insight).unwrap();

    // Call execute_import directly
    let res = execute_import(in_path.to_string());
    assert!(res.is_ok(), "execute_import failed: {:?}", res);

    // Read the updated latest-impact.json
    let updated_content = fs::read_to_string(&latest_impact_path).unwrap();
    assert!(
        updated_content.contains("mem-123"),
        "mem-123 not found in updated content: {}",
        updated_content
    );
    assert!(
        updated_content.contains("trait-based dispatch"),
        "content not found in updated content: {}",
        updated_content
    );
}

#[test]
fn test_bridge_import_lineage_bootstrap() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // Create a dummy latest-impact.json
    let dummy_packet = ImpactPacket::default();
    let dummy_json = serde_json::to_string_pretty(&dummy_packet).unwrap();
    let latest_impact_path = layout.reports_dir().join("latest-impact.json");
    fs::write(&latest_impact_path, dummy_json).unwrap();

    // Construct record 1 with parent_hash: null (None)
    let in_path = root.join("import.ndjson");
    let record1 = r#"{"bridge_version":"0.3","direction":"inbound","timestamp":"2026-05-19T12:00:00Z","project_id":"test-project","record_kind":"insight","payload":{"type":"Insight","memory_id":"mem-1","relevance":0.95,"content":"Initial bootstrapped record"},"privacy":"Public"}"#;

    // De-serialize to compute hash for record 2
    let parsed1: BridgeRecord = serde_json::from_str(record1).unwrap();
    let h1 = calculate_hash(&parsed1);

    // Construct record 2 matching record 1's hash
    let record2 = format!(
        r#"{{"bridge_version":"0.3","direction":"inbound","timestamp":"2026-05-19T12:01:00Z","parent_hash":"{}","project_id":"test-project","record_kind":"insight","payload":{{"type":"Insight","memory_id":"mem-2","relevance":0.95,"content":"Valid second record"}},"privacy":"Public"}}"#,
        h1
    );

    // Construct record 3 with invalid parent hash
    let record3 = r#"{"bridge_version":"0.3","direction":"inbound","timestamp":"2026-05-19T12:02:00Z","parent_hash":"invalid_hash","project_id":"test-project","record_kind":"insight","payload":{"type":"Insight","memory_id":"mem-3","relevance":0.95,"content":"Invalid third record"},"privacy":"Public"}"#;

    let ndjson = format!("{}\n{}\n{}\n", record1, record2, record3);
    fs::write(&in_path, ndjson).unwrap();

    // Call execute_import directly
    let res = execute_import(in_path.to_string());
    assert!(res.is_ok(), "execute_import failed: {:?}", res);

    // Read the updated latest-impact.json to verify which records were imported
    let updated_content = fs::read_to_string(&latest_impact_path).unwrap();

    // mem-1 and mem-2 should be present because parent_hash: null starts the chain and record 2 matches
    assert!(updated_content.contains("mem-1"), "mem-1 not found");
    assert!(updated_content.contains("mem-2"), "mem-2 not found");

    // mem-3 should NOT be present because parent_hash was invalid and rejected
    assert!(!updated_content.contains("mem-3"), "mem-3 was not rejected");
}

/// Regression for CG-F18: a clean-tree tombstone used to be blindly
/// deserialized as a full `ImpactPacket`, which fails (missing required
/// fields like `riskLevel`) and previously made `execute_import` error out
/// entirely instead of importing the other record types.
#[test]
fn test_bridge_import_handles_clean_tree_tombstone_without_erroring() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    let packet = ImpactPacket {
        tree_clean: true,
        changes: Vec::new(),
        ..ImpactPacket::default()
    };
    ledgerful::state::reports::write_impact_report(&layout, &packet).unwrap();

    let latest_impact_path = layout.reports_dir().join("latest-impact.json");
    let written = fs::read_to_string(&latest_impact_path).unwrap();
    assert!(
        written.contains("clean_tree"),
        "expected a clean-tree tombstone to be on disk before import: {written}"
    );

    let in_path = root.join("import.ndjson");
    let insight = r#"{"bridge_version":"0.3","direction":"inbound","timestamp":"2026-05-19T12:00:00Z","project_id":"test-project","record_kind":"insight","payload":{"type":"Insight","memory_id":"mem-123","relevance":0.95,"content":"Architecture note: Use trait-based dispatch for bridge providers."},"privacy":"Public"}"#;
    fs::write(&in_path, insight).unwrap();

    let res = execute_import(in_path.to_string());
    assert!(
        res.is_ok(),
        "execute_import must not error on a clean-tree tombstone: {:?}",
        res
    );

    // The tombstone must survive untouched -- it must not be silently
    // overwritten with a synthetic ImpactPacket, nor corrupted.
    let after = fs::read_to_string(&latest_impact_path).unwrap();
    assert!(
        after.contains("clean_tree"),
        "tombstone should remain on disk: {after}"
    );
}
