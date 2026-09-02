use super::emit::format_human;
use super::packet::*;
use super::*;
use crate::config::model::Config;
use crate::impact::packet::ImpactPacket;
use crate::state::layout::Layout;
use crate::state::reports::{
    CleanTreeTombstone, LATEST_IMPACT_REPORT, LatestImpactReport, write_clean_tree_tombstone,
    write_impact_report,
};
use crate::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

fn init_git_repo(dir: &std::path::Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .expect("git email");
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .expect("git name");
    fs::write(dir.join("README.md"), "hi").expect("readme");
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .expect("git commit");
}

fn head_sha(dir: &std::path::Path) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn classify_impact_cache_none_is_absent() {
    let cache = classify_impact_cache(None, Some("abc"));
    assert!(!cache.present);
    assert!(!cache.valid_for_head);
    assert!(!cache.tree_clean);
}

#[test]
fn classify_impact_cache_packet_mismatched_head() {
    let packet = ImpactPacket {
        head_hash: Some("DEADBEEF".to_string()),
        ..Default::default()
    };
    let report = LatestImpactReport::Packet(Box::new(packet));
    let cache = classify_impact_cache(Some(&report), Some("abc123"));
    assert!(cache.present);
    assert!(!cache.valid_for_head);
    assert!(!cache.tree_clean);
}

#[test]
fn classify_impact_cache_cleantree_same_head() {
    let tombstone = CleanTreeTombstone {
        status: CleanTreeTombstone::STATUS.to_string(),
        head_hash: Some("abc123".to_string()),
        branch_name: Some("main".to_string()),
        schema_version: "v1".to_string(),
        tree_clean: true,
        timestamp_utc: "2026-01-01T00:00:00Z".to_string(),
        changes: Vec::new(),
    };
    let report = LatestImpactReport::CleanTree(tombstone);
    let cache = classify_impact_cache(Some(&report), Some("abc123"));
    assert!(cache.present);
    assert!(cache.valid_for_head);
    assert!(cache.tree_clean);
}

#[test]
fn cap_dirty_paths_preserves_true_count() {
    let paths: Vec<String> = (0..7).map(|i| format!("src/{i}.rs")).collect();
    let (capped, count) = cap_dirty_paths(paths);
    assert_eq!(count, 7);
    assert_eq!(capped.len(), SESSION_DIRTY_PATH_CAP);
}

#[test]
fn human_summary_is_ten_lines_and_not_json() {
    let envelope = SessionEnvelope::default();
    let text = format_human(&envelope);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 10, "human summary must be 10 lines: {text}");
    assert!(text.starts_with("Ledgerful session"));
    assert!(
        serde_json::from_str::<serde_json::Value>(&text).is_err(),
        "human stdout must not parse as JSON: {text}"
    );
}

#[test]
fn envelope_json_has_frozen_fields_no_warn_action() {
    let envelope = SessionEnvelope::default();
    let v = serde_json::to_value(&envelope).expect("serialize");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "session");
    assert!(v.get("git").is_some());
    assert!(v.get("ledger").is_some());
    assert!(v.get("doctor").is_some());
    assert!(v.get("changeContext").is_some());
    assert!(v.get("hotspots").is_some());
    assert!(v.get("impactCache").is_some());
    assert!(v.get("next").is_some());
    assert!(v["doctor"].get("warnAction").is_none());
    assert!(v["ledger"]["collisions"].is_array());
    assert_eq!(v["hotspots"]["excludedTests"], true);
}

#[test]
fn session_does_not_rewrite_latest_impact() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    let seed = ImpactPacket {
        schema_version: "v1".to_string(),
        head_hash: Some("SEED_MARKER_0224".to_string()),
        risk_reasons: vec!["seed-reason-do-not-clobber".to_string()],
        ..Default::default()
    };
    write_impact_report(&layout, &seed).unwrap();

    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let before = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert!(before.contains("SEED_MARKER_0224"));

    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    let envelope = build_session(&layout, &storage, &config).unwrap();
    assert!(
        envelope.impact_cache.present,
        "seeded packet must be present"
    );
    assert!(
        !envelope.impact_cache.valid_for_head,
        "mismatched seed head must not be validForHead"
    );
    assert!(!envelope.impact_cache.tree_clean);

    let after = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert_eq!(before, after, "session must not rewrite latest-impact.json");
    let _ = storage.shutdown();
}

#[test]
fn session_cleantree_same_head_valid_for_head() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);
    let live = head_sha(dir);

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    write_clean_tree_tombstone(&layout, Some(live.clone()), Some("master".to_string())).unwrap();

    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    let envelope = build_session(&layout, &storage, &config).unwrap();
    assert!(envelope.impact_cache.present);
    assert!(envelope.impact_cache.tree_clean);
    assert!(
        envelope.impact_cache.valid_for_head,
        "CleanTree same-head must be validForHead; head={live} envelope={:?}",
        envelope.impact_cache
    );
    let _ = storage.shutdown();
}

#[test]
fn session_pending_and_dirty_emits_collisions() {
    use crate::ledger::{Category, TransactionManager, TransactionRequest};

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);

    fs::create_dir_all(dir.join("crates").join("dedupe-chrome")).unwrap();
    fs::write(
        dir.join("crates").join("dedupe-chrome").join("foo.rs"),
        "fn x() {}\n",
    )
    .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let mut storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    {
        let mut mgr = TransactionManager::new(&mut storage, dir.to_path_buf(), Config::default());
        mgr.start_change(TransactionRequest {
            category: Category::Feature,
            entity: "crates/dedupe-chrome".to_string(),
            planned_action: Some("chrome work".to_string()),
            ..Default::default()
        })
        .unwrap();
    }

    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    assert!(
        envelope.ledger.pending_count >= 1,
        "pending must be visible: {:?}",
        envelope.ledger
    );
    assert!(
        envelope.git.dirty_count >= 1,
        "dirty must be visible: {:?}",
        envelope.git
    );
    assert!(
        !envelope.ledger.collisions.is_empty(),
        "pending chrome + dirty chrome path must collide: {:?}",
        envelope.ledger.collisions
    );
    assert_eq!(
        envelope.change_context.status, "ready",
        "pending+dirty is ready"
    );
    let _ = storage.shutdown();
}

#[test]
fn session_max_files_passthrough_is_honest() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    for name in ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"] {
        fs::write(dir.join("src").join(name), "pub fn x() {}\n").unwrap();
    }

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    assert!(
        envelope.change_context.read_set.len() <= SESSION_MAX_FILES,
        "readSet must honor max_files=5: {:?}",
        envelope.change_context.read_set
    );
    if envelope.change_context.read_set_total_candidates > SESSION_MAX_FILES {
        assert!(
            envelope.change_context.read_set_capped,
            "capped must pass through from the max_files=5 packet"
        );
    }
    let _ = storage.shutdown();
}

#[test]
fn session_git_unavailable_warns_in_next() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    assert!(
        envelope
            .next
            .iter()
            .any(|s| s.contains("git repository unavailable")),
        "git failure must appear in next, not a silent empty git section: {:?}",
        envelope.next
    );
    assert!(envelope.git.head.is_empty());
    assert_eq!(envelope.git.dirty_count, 0);
    let _ = storage.shutdown();
}
