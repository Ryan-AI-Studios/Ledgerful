use crate::common::DirGuard;
use ledgerful::bridge::import::execute_import;
use ledgerful::bridge::model::{
    BridgeDirection, BridgePayload, BridgeRecord, SnapshotPayload, calculate_hash, serialize_record,
};
use ledgerful::impact::budget::{CompletenessFilter, HotspotProvenance, HotspotProvenanceSource};
use ledgerful::impact::hotspots::normalize_score;
use ledgerful::impact::packet::{Hotspot, ImpactPacket};
use ledgerful::state::layout::Layout;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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

fn temp_layout() -> (tempfile::TempDir, Layout, DirGuard) {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let guard = DirGuard::from_utf8(&root);
    let layout = Layout::new(&root);
    layout.ensure_state_dir().unwrap();
    (tmp, layout, guard)
}

fn write_packet(layout: &Layout, packet: &ImpactPacket) -> camino::Utf8PathBuf {
    let path = layout.reports_dir().join("latest-impact.json");
    fs::write(&path, serde_json::to_string_pretty(packet).unwrap()).unwrap();
    path
}

fn read_packet(path: &camino::Utf8Path) -> ImpactPacket {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn hotspot_line(path: &str, score: f64) -> String {
    serialize_record(&BridgeRecord::new(
        BridgeDirection::Inbound,
        "test-project".to_string(),
        "hotspot",
        BridgePayload::Hotspot {
            path: path.to_string(),
            score,
            reason: "imported".to_string(),
            temporal_coupling: 0.1,
            failure_risk_probability: 0.2,
        },
    ))
    .unwrap()
}

fn local_hotspot(path: &str, score: f32, complexity: i32) -> Hotspot {
    Hotspot {
        path: PathBuf::from(path),
        score,
        display_score: 1.25,
        complexity,
        frequency: 1.5,
        centrality: Some(4),
    }
}

fn scoped_packet(hotspot: Hotspot) -> ImpactPacket {
    ImpactPacket {
        tree_clean: false,
        hotspots: vec![hotspot],
        ..ImpactPacket::default()
    }
}

#[test]
fn test_bridge_import_hotspot_skips_path_absent_from_packet() {
    let (_tmp, layout, _guard) = temp_layout();
    let path = write_packet(
        &layout,
        &scoped_packet(local_hotspot("src/kept.rs", 0.2, 7)),
    );
    let in_path = layout.root.join("import.ndjson");
    fs::write(&in_path, hotspot_line("src/other.rs", 0.9)).unwrap();

    execute_import(in_path.to_string()).unwrap();

    let packet = read_packet(&path);
    assert_eq!(packet.hotspots.len(), 1);
    assert_eq!(packet.hotspots[0].path, PathBuf::from("src/kept.rs"));
    assert_eq!(packet.hotspots[0].score, 0.2);
    assert!(
        !packet
            .hotspots
            .iter()
            .any(|row| row.path.as_path() == Path::new("src/other.rs"))
    );
}

#[test]
fn test_bridge_import_hotspot_updates_score_inside_persisted_list() {
    let (_tmp, layout, _guard) = temp_layout();
    let path = write_packet(
        &layout,
        &scoped_packet(local_hotspot("src/kept.rs", 0.2, 7)),
    );
    let insight = r#"{"bridge_version":"0.3","direction":"inbound","timestamp":"2026-05-19T12:00:00Z","project_id":"test-project","record_kind":"insight","payload":{"type":"Insight","memory_id":"mem-123","relevance":0.95,"content":"Architecture note: Use trait-based dispatch for bridge providers."},"privacy":"Public"}"#;
    let ndjson = format!("{insight}\n{}\n", hotspot_line("src/kept.rs", 0.55));
    let in_path = layout.root.join("import.ndjson");
    fs::write(&in_path, ndjson).unwrap();

    execute_import(in_path.to_string()).unwrap();

    let packet = read_packet(&path);
    let stored = 0.55_f64 as f32;
    assert_eq!(packet.hotspots.len(), 1);
    assert_eq!(packet.hotspots[0].score, stored);
    assert_eq!(
        packet.hotspots[0].display_score,
        normalize_score(f64::from(stored)) as f32
    );
    assert_eq!(packet.hotspots[0].complexity, 7);
    assert_eq!(packet.hotspots[0].frequency, 1.5);
    assert_eq!(packet.hotspots[0].centrality, Some(4));
    assert!(
        packet
            .ai_insights
            .iter()
            .any(|row| row.memory_id == "mem-123")
    );
}

#[test]
fn test_bridge_import_hotspot_matches_backslash_and_dot_slash() {
    let (_tmp, layout, _guard) = temp_layout();
    let path = write_packet(
        &layout,
        &scoped_packet(local_hotspot("src/bridge/import.rs", 0.2, 3)),
    );
    let ndjson = format!(
        "{}\n{}\n",
        hotspot_line(r"src\bridge\import.rs", 0.4),
        hotspot_line("./src/bridge/import.rs", 0.7)
    );
    let in_path = layout.root.join("import.ndjson");
    fs::write(&in_path, ndjson).unwrap();

    execute_import(in_path.to_string()).unwrap();

    let packet = read_packet(&path);
    assert_eq!(packet.hotspots.len(), 1);
    assert_eq!(
        packet.hotspots[0].path,
        PathBuf::from("src/bridge/import.rs")
    );
    assert_eq!(packet.hotspots[0].score, 0.7_f64 as f32);
    assert_eq!(packet.hotspots[0].complexity, 3);
}

#[test]
fn test_bridge_import_hotspot_keeps_provenance_and_length() {
    let provenance = HotspotProvenance {
        source: HotspotProvenanceSource::Live,
        commits_requested: Some(50),
        limit: Some(1),
        filter: Some(CompletenessFilter::Unfiltered),
        head: Some("abc123".to_string()),
        ..HotspotProvenance::default()
    };
    let (_tmp, layout, _guard) = temp_layout();
    let mut packet = scoped_packet(local_hotspot("src/kept.rs", 0.2, 7));
    packet.hotspot_provenance = Some(provenance.clone());
    let path = write_packet(&layout, &packet);
    let in_path = layout.root.join("import.ndjson");
    let ndjson = format!(
        "{}\n{}\n",
        hotspot_line("src/kept.rs", 0.4),
        hotspot_line("src/outside.rs", 0.9)
    );
    fs::write(&in_path, ndjson).unwrap();

    execute_import(in_path.to_string()).unwrap();

    let updated = read_packet(&path);
    assert_eq!(updated.hotspots.len(), 1);
    assert_eq!(updated.hotspots[0].path, PathBuf::from("src/kept.rs"));
    assert_eq!(updated.hotspot_provenance, Some(provenance));

    let (_tmp_absent, layout_absent, _guard_absent) = temp_layout();
    let absent_path = write_packet(&layout_absent, &ImpactPacket::default());
    let absent_in = layout_absent.root.join("import.ndjson");
    fs::write(&absent_in, hotspot_line("src/outside.rs", 0.9)).unwrap();
    execute_import(absent_in.to_string()).unwrap();
    let absent = read_packet(&absent_path);
    assert!(absent.hotspots.is_empty());
    assert!(absent.hotspot_provenance.is_none());
}

#[test]
fn test_bridge_import_hotspot_on_clean_tree_does_not_merge() {
    let (_tmp, layout, _guard) = temp_layout();
    let packet = ImpactPacket {
        tree_clean: true,
        changes: Vec::new(),
        ..ImpactPacket::default()
    };
    ledgerful::state::reports::write_impact_report(&layout, &packet).unwrap();
    let latest_impact_path = layout.reports_dir().join("latest-impact.json");
    let before = fs::read_to_string(&latest_impact_path).unwrap();
    assert!(before.contains("clean_tree"), "{before}");

    let in_path = layout.root.join("import.ndjson");
    fs::write(&in_path, hotspot_line("src/kept.rs", 0.9)).unwrap();
    execute_import(in_path.to_string()).unwrap();

    let after = fs::read_to_string(&latest_impact_path).unwrap();
    assert!(after.contains("clean_tree"), "{after}");
    assert!(!after.contains("src/kept.rs"), "{after}");
    assert_eq!(after, before);
}

#[test]
fn test_bridge_import_snapshot_hotspots_stay_unmerged() {
    let (_tmp, layout, _guard) = temp_layout();
    let path = write_packet(
        &layout,
        &scoped_packet(local_hotspot("src/kept.rs", 0.2, 7)),
    );
    let record = BridgeRecord::new(
        BridgeDirection::Inbound,
        "test-project".to_string(),
        "snapshot",
        BridgePayload::Snapshot(Box::new(SnapshotPayload {
            project_id: "test-project".to_string(),
            impact: ImpactPacket::default(),
            hotspots: vec![local_hotspot("src/from-snapshot.rs", 0.9, 2)],
            ledger: Vec::new(),
            metadata: HashMap::new(),
            datasets: Vec::new(),
        })),
    );
    let in_path = layout.root.join("import.ndjson");
    fs::write(&in_path, serialize_record(&record).unwrap()).unwrap();

    execute_import(in_path.to_string()).unwrap();

    let packet = read_packet(&path);
    assert_eq!(packet.hotspots.len(), 1);
    assert_eq!(packet.hotspots[0].path, PathBuf::from("src/kept.rs"));
    assert_eq!(packet.hotspots[0].score, 0.2);
    assert!(
        !packet
            .hotspots
            .iter()
            .any(|row| row.path.as_path() == Path::new("src/from-snapshot.rs"))
    );
}

fn packet_with_rows(rows: &[(&str, f32)]) -> ImpactPacket {
    ImpactPacket {
        tree_clean: false,
        hotspots: rows
            .iter()
            .map(|(path, score)| local_hotspot(path, *score, 3))
            .collect(),
        ..ImpactPacket::default()
    }
}

#[test]
fn test_bridge_import_hotspot_ignores_negative_and_overflow_scores() {
    let (_tmp, layout, _guard) = temp_layout();
    let rows = [
        ("src/a.rs", 0.2_f32),
        ("src/b.rs", 0.3),
        ("src/c.rs", 0.4),
        ("src/d.rs", 0.5),
        ("src/e.rs", 0.6),
        ("src/f.rs", 0.7),
    ];
    let path = write_packet(&layout, &packet_with_rows(&rows));
    let ndjson = [
        hotspot_line("src/a.rs", -0.001),
        hotspot_line("src/b.rs", -0.5),
        hotspot_line("src/c.rs", -0.0005),
        hotspot_line("src/d.rs", -1e-50),
        hotspot_line("src/e.rs", 1e300),
        hotspot_line("src/f.rs", 0.9),
    ]
    .join("\n");
    let in_path = layout.root.join("import.ndjson");
    fs::write(&in_path, ndjson).unwrap();

    execute_import(in_path.to_string()).unwrap();

    // The packet must read back (a non-finite displayScore is written as null).
    let packet = read_packet(&path);
    assert_eq!(packet.hotspots.len(), 6);
    // `finalize` re-sorts rows, so look each one up by path.
    let by_path = |p: &str| {
        packet
            .hotspots
            .iter()
            .find(|h| h.path.as_path() == Path::new(p))
            .unwrap()
    };
    for (p, original) in rows.iter().take(5) {
        assert_eq!(by_path(p).score, *original, "{p} score");
        assert_eq!(by_path(p).display_score, 1.25, "{p} display");
    }
    let stored = 0.9_f64 as f32;
    assert_eq!(by_path("src/f.rs").score, stored);
    assert_eq!(
        by_path("src/f.rs").display_score,
        normalize_score(f64::from(stored)) as f32
    );
    assert!(packet.hotspots.iter().all(|h| h.display_score.is_finite()));
}

#[test]
fn test_bridge_import_hotspot_accepts_zero_and_negative_zero() {
    let (_tmp, layout, _guard) = temp_layout();
    let path = write_packet(
        &layout,
        &packet_with_rows(&[("src/a.rs", 0.2), ("src/b.rs", 0.3)]),
    );
    let ndjson = format!(
        "{}\n{}\n",
        hotspot_line("src/a.rs", 0.0),
        hotspot_line("src/b.rs", -0.0)
    );
    let in_path = layout.root.join("import.ndjson");
    fs::write(&in_path, ndjson).unwrap();

    execute_import(in_path.to_string()).unwrap();

    let packet = read_packet(&path);
    for h in &packet.hotspots {
        assert_eq!(h.score, 0.0);
        assert_eq!(h.display_score, 0.0);
    }
}

#[test]
fn test_bridge_packet_with_null_display_score_does_not_read_back() {
    let packet = packet_with_rows(&[("src/a.rs", f32::INFINITY)]);
    let mut value = serde_json::to_value(&packet).unwrap();
    value["hotspots"][0]["displayScore"] = serde_json::Value::Null;
    let err = serde_json::from_value::<ImpactPacket>(value).unwrap_err();
    assert!(err.to_string().contains("expected f32"), "{err}");
}
