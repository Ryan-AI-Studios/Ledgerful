use super::emit::format_human;
use super::packet::*;
use super::*;
use crate::config::model::Config;
use crate::impact::packet::{Hotspot, ImpactPacket};
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
fn config_checklist_human_session_stays_ten_lines() {
    use crate::config::checklist::{ChecklistStatus, ConfigChecklistItem};
    let mut envelope = SessionEnvelope::default();
    envelope.config_checklist.push(ConfigChecklistItem {
        id: "coverage.global".to_string(),
        status: ChecklistStatus::Gated,
        applicable: true,
        next: "ledgerful config set coverage.enabled=true".to_string(),
        already_shown: false,
        apply_arg: Some("coverage.global".to_string()),
    });
    let text = format_human(&envelope);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 10, "human summary must stay 10 lines: {text}");
    assert!(!text.contains("configChecklist"));
    assert!(!text.contains("coverage.global"));
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
    assert!(v["configChecklist"].is_array());
    assert!(v.get("sessionNotices").is_none());
    assert!(v.get("warnAction").is_none());
    assert!(v["doctor"].get("warnAction").is_none());
    assert!(v["ledger"]["collisions"].is_array());
    assert_eq!(v["hotspots"]["excludedTests"], true);
    assert_eq!(v["hotspots"]["provenance"]["source"], "live");
    assert_eq!(v["hotspots"]["provenance"]["filter"], "session");
    assert_eq!(v["hotspots"]["provenance"]["daysRequested"], 30);
    assert_eq!(v["hotspots"]["provenance"]["limit"], 5);
}

#[test]
fn session_hotspot_file_from_copies_stored_display_score_not_recomputed_ln() {
    let hotspot = Hotspot {
        path: std::path::PathBuf::from("src\\lib.rs"),
        score: 0.02,
        display_score: 9.99,
        complexity: 1,
        frequency: 1.0,
        centrality: None,
    };
    let file = session_hotspot_file_from(&hotspot);
    assert_eq!(file.path, "src/lib.rs");
    assert!((file.score - 0.02).abs() < 1e-5);
    assert!(
        (file.display_score - 9.99).abs() < 1e-5,
        "must copy stored display_score, not normalize_score(score): {}",
        file.display_score
    );
    assert!(file.presence.is_none());
    assert!((file.score - file.display_score).abs() > 1.0);
    let v = serde_json::to_value(&file).expect("serialize");
    assert!(v.get("scoreUnit").is_none(), "no scoreUnit: {v}");
    assert!(
        v.get("displayScore").is_some(),
        "additive displayScore: {v}"
    );
    let score = v["score"].as_f64().expect("score f64");
    assert!(
        (0.0..=1.0).contains(&score),
        "session score is 0-1: {score}"
    );
}

#[test]
fn session_hotspot_file_with_head_omits_presence_when_head_unborn() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir)
        .output()
        .expect("git init");
    let repo = crate::git::repo::open_repo(dir).expect("unborn");
    let hotspot = Hotspot {
        path: std::path::PathBuf::from("gone.rs"),
        score: 0.2,
        display_score: 1.0,
        complexity: 1,
        frequency: 1.0,
        centrality: None,
    };
    let file = session_hotspot_file_with_head(&hotspot, &repo);
    assert!(
        file.presence.is_none(),
        "unborn HEAD must omit presence, not mark historical"
    );
    let v = serde_json::to_value(&file).expect("serialize");
    assert!(v.get("presence").is_none(), "{v}");
}

#[test]
fn session_hotspot_file_with_head_historical_on_deleted_path() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    init_git_repo(dir);
    fs::write(dir.join("gone.rs"), "fn gone() {}\n").expect("gone");
    fs::write(dir.join("stay.rs"), "fn stay() {}\n").expect("stay");
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("add stay/gone");
    std::process::Command::new("git")
        .args(["commit", "-m", "add files"])
        .current_dir(dir)
        .output()
        .expect("commit add");
    fs::remove_file(dir.join("gone.rs")).expect("delete gone");
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("stage delete");
    std::process::Command::new("git")
        .args(["commit", "-m", "delete gone"])
        .current_dir(dir)
        .output()
        .expect("commit delete");

    let repo = crate::git::repo::open_repo(dir).expect("open");
    let gone = Hotspot {
        path: std::path::PathBuf::from("gone.rs"),
        score: 0.2,
        display_score: 1.0,
        complexity: 1,
        frequency: 1.0,
        centrality: None,
    };
    let stay = Hotspot {
        path: std::path::PathBuf::from("stay.rs"),
        score: 0.3,
        display_score: 1.1,
        complexity: 1,
        frequency: 1.0,
        centrality: None,
    };
    let gone_file = session_hotspot_file_with_head(&gone, &repo);
    let stay_file = session_hotspot_file_with_head(&stay, &repo);
    assert_eq!(gone_file.presence.as_deref(), Some("historical"));
    assert!(stay_file.presence.is_none());
}

#[test]
fn session_hotspot_file_deserializes_missing_display_score_to_zero() {
    let file: SessionHotspotFile = serde_json::from_value(serde_json::json!({
        "path": "a.rs",
        "score": 0.02
    }))
    .expect("older envelope without displayScore");
    assert_eq!(file.path, "a.rs");
    assert!((file.score - 0.02).abs() < 1e-5);
    assert_eq!(file.display_score, 0.0);
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
    assert!(
        envelope
            .next
            .iter()
            .all(|n| !n.contains("config set") && !n.contains("configChecklist")),
        "session.next must stay structural: {:?}",
        envelope.next
    );
    let _ = storage.shutdown();
}

#[test]
fn config_checklist_does_not_mutate_session_next() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src").join("api.rs"), "fn handler() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "api"])
        .current_dir(dir)
        .output()
        .expect("git commit");

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES \
         (1, 'src/api.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, last_indexed_at) \
         VALUES ('GET', '/api/probe', 'handler', 1, 'axum', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    assert!(
        envelope
            .config_checklist
            .iter()
            .any(|i| i.id == "coverage.global"),
        "product route must emit coverage.global: {:?}",
        envelope
            .config_checklist
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        envelope
            .next
            .iter()
            .all(|n| !n.contains("config set") && !n.contains("Declare [services]")),
        "checklist must not mutate session.next: {:?}",
        envelope.next
    );
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
fn session_watch_ignored_claude_file_is_not_dirty() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);

    fs::write(dir.join(".gitignore"), ".claude/\n").unwrap();
    std::process::Command::new("git")
        .args(["add", ".gitignore"])
        .current_dir(dir)
        .output()
        .expect("git add gitignore");
    std::process::Command::new("git")
        .args(["commit", "-m", "gitignore"])
        .current_dir(dir)
        .output()
        .expect("git commit gitignore");

    // Untracked *file* named .claude (not a directory — a populated dir
    // matching gitignore `.claude/` is ignored at the gix walk).
    fs::write(dir.join(".claude"), "shim\n").unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    assert_eq!(
        envelope.git.dirty_count, 0,
        "watch-ignored .claude file must not count as session dirt: {:?}",
        envelope.git
    );
    assert!(
        envelope.git.dirty_paths.is_empty(),
        "dirtyPaths must be empty: {:?}",
        envelope.git.dirty_paths
    );

    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src").join("foo.rs"), "pub fn x() {}\n").unwrap();
    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    assert!(
        envelope.git.dirty_count >= 1,
        "src dirt must still count: {:?}",
        envelope.git
    );
    assert!(
        envelope.git.dirty_paths.iter().any(|p| p == "src/foo.rs"),
        "dirtyPaths must contain src/foo.rs: {:?}",
        envelope.git.dirty_paths
    );
    assert!(
        !envelope.git.dirty_paths.iter().any(|p| p == ".claude"),
        ".claude must stay omitted: {:?}",
        envelope.git.dirty_paths
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

#[test]
fn session_hotspots_exclude_vendor_paths() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);

    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("vendor")).unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    fs::write(dir.join("vendor/lib.rs"), "pub fn v() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("git add vendor");
    std::process::Command::new("git")
        .args(["commit", "-m", "src and vendor"])
        .current_dir(dir)
        .output()
        .expect("git commit vendor");

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage
        .get_connection()
        .execute(
            "INSERT INTO snapshots (id, timestamp, is_clean, packet_json) VALUES (1, '2026-01-01T00:00:00Z', 0, '{}')",
            [],
        )
        .unwrap();
    storage
        .get_connection()
        .execute(
            "INSERT INTO symbols (snapshot_id, file_path, symbol_name, symbol_kind, is_public, cognitive_complexity, cyclomatic_complexity)
             VALUES (1, 'src/lib.rs', 'a', 'Function', 1, 3, 3)",
            [],
        )
        .unwrap();
    storage
        .get_connection()
        .execute(
            "INSERT INTO symbols (snapshot_id, file_path, symbol_name, symbol_kind, is_public, cognitive_complexity, cyclomatic_complexity)
             VALUES (1, 'vendor/lib.rs', 'v', 'Function', 1, 204, 204)",
            [],
        )
        .unwrap();

    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    let paths: Vec<&str> = envelope
        .hotspots
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert!(
        paths.contains(&"src/lib.rs"),
        "session must still list first-party src/lib.rs: {paths:?}"
    );
    assert!(
        paths.iter().all(|p| !p.contains("vendor/")),
        "session must omit vendor/: {paths:?}"
    );
    let _ = storage.shutdown();
}

#[test]
fn session_skips_git_history_enrichment() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src").join("dirty.rs"), "pub fn x() {}\n").unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let seed = ImpactPacket {
        schema_version: "v1".to_string(),
        head_hash: Some("SEED_MARKER_0308".to_string()),
        ..Default::default()
    };
    write_impact_report(&layout, &seed).unwrap();
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let before = fs::read_to_string(report_path.as_std_path()).unwrap();

    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    crate::impact::budget::test_hooks::reset_walk_count();
    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    let walks = crate::impact::budget::test_hooks::walk_count();
    assert_eq!(
        walks, 1,
        "dirty session must call get_history exactly once (collect_hotspots); got {walks}"
    );
    assert!(envelope.change_context.read_set.len() <= SESSION_MAX_FILES);
    if let Some(c) = &envelope.hotspots.completeness {
        assert_eq!(c.filter, crate::impact::budget::CompletenessFilter::Session);
    }
    let after = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert_eq!(before, after, "session must not rewrite latest-impact.json");
    let _ = storage.shutdown();
}

#[test]
fn session_json_emits_provenance_keeps_ten_line_human() {
    let envelope = SessionEnvelope::default();
    let v = serde_json::to_value(&envelope).expect("json");
    assert_eq!(v["hotspots"]["provenance"]["filter"], "session");
    assert!(
        v["hotspots"]["provenance"]["commitsRequested"]
            .as_u64()
            .unwrap()
            <= 50
    );
    assert_eq!(v["hotspots"]["provenance"]["daysRequested"], 30);
    assert_eq!(v["hotspots"]["provenance"]["limit"], 5);
    assert!(v["hotspots"]["provenance"].get("snapshotAt").is_none());
    let text = format_human(&envelope);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 10, "human summary must stay 10 lines: {text}");
    assert!(
        serde_json::from_str::<serde_json::Value>(&text).is_err(),
        "human stdout must not parse as JSON: {text}"
    );
}

#[test]
fn session_omits_snapshot_age_keys() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    init_git_repo(dir);
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage
        .get_connection()
        .execute(
            "INSERT INTO hotspot_history (file_path, score, display_score, complexity, frequency, timestamp) \
             VALUES ('src/a.rs', 0.1, 1.0, 1, 1.0, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    let envelope = build_session(&layout, &storage, &Config::default()).unwrap();
    let v = serde_json::to_value(&envelope.hotspots.provenance).expect("json");
    assert_eq!(v["source"], "live");
    assert_eq!(v["filter"], "session");
    assert!(v.get("snapshotAt").is_none(), "{v}");
    assert!(v.get("snapshotAgeSecs").is_none(), "{v}");
    let _ = storage.shutdown();
}

#[test]
fn same_query_session_and_list_ranks_match() {
    use crate::impact::hotspots::{HotspotQuery, calculate_hotspots_detailed};
    use crate::impact::temporal::GixHistoryProvider;

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    init_git_repo(dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "src"])
        .current_dir(dir)
        .output()
        .expect("git commit");

    let root = camino::Utf8Path::from_path(dir).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let repo = crate::git::repo::open_repo(dir).expect("open");
    let provider = GixHistoryProvider::new(&repo);
    let query = HotspotQuery {
        commits: 50,
        days: Some(30),
        limit: 5,
        exclude_test_paths: true,
        exclude_vendor_paths: true,
        ..HotspotQuery::default()
    };
    let a = calculate_hotspots_detailed(&storage, &provider, &query).expect("first");
    let b = calculate_hotspots_detailed(&storage, &provider, &query).expect("second");
    let ranks_a: Vec<(String, i32)> = a
        .hotspots
        .iter()
        .map(|h| {
            (
                h.path.to_string_lossy().replace('\\', "/"),
                (h.score * 1_000_000.0).round() as i32,
            )
        })
        .collect();
    let ranks_b: Vec<(String, i32)> = b
        .hotspots
        .iter()
        .map(|h| {
            (
                h.path.to_string_lossy().replace('\\', "/"),
                (h.score * 1_000_000.0).round() as i32,
            )
        })
        .collect();
    assert_eq!(
        ranks_a, ranks_b,
        "same HotspotQuery must yield same path+score"
    );
    let _ = storage.shutdown();
}

#[test]
fn unmatched_windows_differ_and_are_labeled() {
    use crate::impact::budget::{CompletenessFilter, HotspotProvenance, HotspotProvenanceSource};

    let session = SessionEnvelope::default();
    let session_v = serde_json::to_value(&session.hotspots.provenance).expect("session");
    let list = HotspotProvenance {
        source: HotspotProvenanceSource::Live,
        commits_requested: Some(500),
        days_requested: None,
        limit: Some(10),
        filter: Some(CompletenessFilter::Default),
        ..HotspotProvenance::default()
    };
    let list_v = serde_json::to_value(&list).expect("list");
    assert_ne!(session_v["commitsRequested"], list_v["commitsRequested"]);
    assert_eq!(session_v["filter"], "session");
    assert_eq!(list_v["filter"], "default");
    assert_eq!(session_v["daysRequested"], 30);
    assert!(list_v.get("daysRequested").is_none());
}
