use camino::Utf8PathBuf;
use ledgerful::git::GitError;
use ledgerful::impact::hotspots::{HotspotQuery, calculate_hotspots, calculate_hotspots_detailed};

fn query(limit: usize) -> HotspotQuery {
    HotspotQuery {
        commits: 10,
        limit,
        ..Default::default()
    }
}
use ledgerful::impact::temporal::{CommitFileSet, HistoryProvider};
use ledgerful::state::storage::StorageManager;
use std::collections::HashSet;
use tempfile::tempdir;

struct MockHistoryProvider {
    history: Vec<CommitFileSet>,
}

impl HistoryProvider for MockHistoryProvider {
    fn get_history(
        &self,
        _max_commits: usize,
        _max_days: Option<u64>,
        _since: Option<String>,
        _all_parents: bool,
    ) -> Result<Vec<CommitFileSet>, GitError> {
        Ok(self.history.clone())
    }
}

#[test]
fn test_hotspots_use_normalized_multiplication_and_path_tie_breaking() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    insert_snapshot(&storage);
    insert_complexity(&storage, "src/a.rs", 10);
    insert_complexity(&storage, "src/b.rs", 20);
    insert_complexity(&storage, "src/c.rs", 20);

    let history = MockHistoryProvider {
        history: vec![
            commit(&["src/a.rs", "src/b.rs", "src/c.rs"]),
            commit(&["src/a.rs", "src/b.rs", "src/c.rs"]),
            commit(&["src/a.rs"]),
        ],
    };

    let hotspots = calculate_hotspots(&storage, &history, &query(10)).unwrap();

    assert_eq!(hotspots[0].path.to_string_lossy(), "src/b.rs");
    assert_eq!(hotspots[1].path.to_string_lossy(), "src/c.rs");
    assert_eq!(hotspots[0].score, hotspots[1].score);
    assert!(hotspots[0].score > hotspots[2].score);
}

#[test]
fn test_hotspots_apply_directory_and_language_filters() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    insert_snapshot(&storage);
    insert_complexity(&storage, "src/a.rs", 10);
    insert_complexity(&storage, "tests/a.rs", 10);
    insert_complexity(&storage, "src/a.py", 10);

    let history = MockHistoryProvider {
        history: vec![commit(&["src/a.rs", "tests/a.rs", "src/a.py"])],
    };

    let hotspots = calculate_hotspots(
        &storage,
        &history,
        &HotspotQuery {
            commits: 10,
            limit: 10,
            dir_filter: Some("src/".to_string()),
            lang_filter: Some("rs".to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(hotspots.len(), 1);
    assert_eq!(hotspots[0].path.to_string_lossy(), "src/a.rs");
}

#[test]
fn test_hotspots_are_json_serializable() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    insert_snapshot(&storage);
    insert_complexity(&storage, "src/a.rs", 10);

    let history = MockHistoryProvider {
        history: vec![commit(&["src/a.rs"])],
    };

    let hotspots = calculate_hotspots(&storage, &history, &query(10)).unwrap();
    let json = serde_json::to_string(&hotspots).unwrap();

    assert!(json.contains("src/a.rs"));
    assert!(json.contains("score"));
}

#[test]
fn test_hotspots_propagate_malformed_sqlite_rows() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    insert_snapshot(&storage);
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO symbols (snapshot_id, file_path, symbol_name, symbol_kind, is_public, cognitive_complexity, cyclomatic_complexity)
         VALUES (1, 'src/a.rs', 'a', 'Function', 1, 'bad', 0)",
        [],
    )
    .unwrap();

    let history = MockHistoryProvider {
        history: vec![commit(&["src/a.rs"])],
    };

    let error = calculate_hotspots(&storage, &history, &query(10)).unwrap_err();
    assert!(format!("{error:?}").contains("Invalid column type"));
}

#[test]
fn test_hotspot_score_is_finite_when_all_complexity_is_zero() {
    // Regression: max_comp=0 used to produce 0/0=NaN, breaking JSON serialization
    // and causing verify to fail to load the impact packet.
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    insert_snapshot(&storage);
    // No complexity rows inserted — all files get complexity=0 from the fallback

    let history = MockHistoryProvider {
        history: vec![commit(&["README.md", "docs/guide.md"])],
    };

    let hotspots = calculate_hotspots(&storage, &history, &query(10)).unwrap();
    assert!(!hotspots.is_empty());
    for h in &hotspots {
        assert!(
            h.score.is_finite(),
            "score should be 0.0, not NaN: got {:?} for {}",
            h.score,
            h.path.display()
        );
        assert_eq!(h.score, 0.0);
    }

    // Must round-trip through JSON without null scores
    let json = serde_json::to_string(&hotspots).unwrap();
    assert!(
        !json.contains("null"),
        "JSON should not contain null scores"
    );
    let decoded: Vec<ledgerful::impact::packet::Hotspot> = serde_json::from_str(&json).unwrap();
    for h in &decoded {
        assert!(h.score.is_finite());
    }
}

/// DoD-3 frozen unfiltered `score` JSON (sorted path → score) for the
/// `src/lib.rs` + `src/foo.rs` + `tests/foo.rs` + `examples/x.rs` fixture.
/// Captured against pre-0222 `calculate_hotspots` math (exclude_test_paths
/// default false). `--include tests` must keep these bytes.
const UNFILTERED_FIXTURE_SCORE_JSON: &str = concat!(
    r#"{"examples/x.rs":0.20000000298023224,"src/foo.rs":0.07500000298023224,"#,
    r#""src/lib.rs":0.20000000298023224,"tests/foo.rs":1.0}"#
);

fn agent_default_fixture(
    storage: &StorageManager,
) -> (MockHistoryProvider, HotspotQuery, HotspotQuery) {
    insert_snapshot(storage);
    insert_complexity(storage, "tests/foo.rs", 80);
    insert_complexity(storage, "src/lib.rs", 20);
    insert_complexity(storage, "src/foo.rs", 10);
    insert_complexity(storage, "examples/x.rs", 40);

    let history = MockHistoryProvider {
        history: vec![
            commit(&["tests/foo.rs", "src/lib.rs", "src/foo.rs", "examples/x.rs"]),
            commit(&["tests/foo.rs", "src/lib.rs", "src/foo.rs", "examples/x.rs"]),
            commit(&["tests/foo.rs", "src/lib.rs", "src/foo.rs"]),
            commit(&["tests/foo.rs", "src/lib.rs"]),
            commit(&["tests/foo.rs"]),
        ],
    };
    let include_tests = HotspotQuery {
        commits: 10,
        limit: 10,
        exclude_test_paths: false,
        ..Default::default()
    };
    let default_cli = HotspotQuery {
        commits: 10,
        limit: 10,
        exclude_test_paths: true,
        ..Default::default()
    };
    (history, include_tests, default_cli)
}

fn scores_json(hotspots: &[ledgerful::impact::packet::Hotspot]) -> String {
    let mut pairs: Vec<(String, serde_json::Value)> = hotspots
        .iter()
        .map(|h| {
            (
                h.path.to_string_lossy().replace('\\', "/"),
                serde_json::json!(h.score),
            )
        })
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut map = serde_json::Map::new();
    for (path, score) in pairs {
        map.insert(path, score);
    }
    serde_json::Value::Object(map).to_string()
}

fn paths_of(hotspots: &[ledgerful::impact::packet::Hotspot]) -> Vec<String> {
    hotspots
        .iter()
        .map(|h| h.path.to_string_lossy().replace('\\', "/"))
        .collect()
}

#[test]
fn hotspots_cli_default_fixture_excludes_tests_examples_ranks_src_lib() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    let (history, _, default_cli) = agent_default_fixture(&storage);

    let calculated = calculate_hotspots_detailed(&storage, &history, &default_cli).unwrap();
    let paths = paths_of(&calculated.hotspots);

    assert_eq!(paths[0], "src/lib.rs");
    assert!(
        paths.iter().any(|p| p == "src/foo.rs"),
        "src/foo.rs (#[cfg(test)] production path) must still rank: {paths:?}"
    );
    assert!(
        paths
            .iter()
            .all(|p| !p.starts_with("tests/") && !p.starts_with("examples/")),
        "default must not list tests/ or examples/: {paths:?}"
    );
    assert_eq!(calculated.omitted_test_paths, 2);
}

#[test]
fn hotspots_include_tests_fixture_ranks_test_first_and_keeps_unfiltered_scores() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    let (history, include_tests, _) = agent_default_fixture(&storage);

    let hotspots = calculate_hotspots(&storage, &history, &include_tests).unwrap();
    let paths = paths_of(&hotspots);
    assert_eq!(paths[0], "tests/foo.rs");
    assert!(paths.iter().any(|p| p == "examples/x.rs"));
    assert_eq!(
        scores_json(&hotspots),
        UNFILTERED_FIXTURE_SCORE_JSON,
        "unfiltered score bytes must match pre-change capture"
    );
    assert!(
        !serde_json::to_string(&hotspots)
            .unwrap()
            .contains("scoreUnit"),
        "must not add scoreUnit on Hotspot"
    );
}

#[test]
fn hotspots_exclude_before_max_comp_does_not_crush_production_c_norm() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    let (history, include_tests, default_cli) = agent_default_fixture(&storage);

    let unfiltered = calculate_hotspots(&storage, &history, &include_tests).unwrap();
    let filtered = calculate_hotspots(&storage, &history, &default_cli).unwrap();

    let unfiltered_lib = unfiltered
        .iter()
        .find(|h| h.path.to_string_lossy().replace('\\', "/") == "src/lib.rs")
        .unwrap();
    let filtered_lib = filtered
        .iter()
        .find(|h| h.path.to_string_lossy().replace('\\', "/") == "src/lib.rs")
        .unwrap();

    assert_eq!(unfiltered_lib.score, 0.2);
    assert_eq!(filtered_lib.score, 1.0);
}

#[test]
fn test_hotspot_score_null_deserializes_as_zero_for_backward_compat() {
    // Regression: packets written before the NaN fix have "score":null.
    // The custom deserializer should read null as 0.0 so verify doesn't crash.
    let json = r#"[{"path":"src/lib.rs","score":null,"complexity":0,"frequency":3}]"#;
    let hotspots: Vec<ledgerful::impact::packet::Hotspot> = serde_json::from_str(json).unwrap();
    assert_eq!(hotspots[0].score, 0.0);
    assert!(hotspots[0].score.is_finite());
}

fn insert_complexity(storage: &StorageManager, file_path: &str, complexity: i32) {
    storage
        .get_connection()
        .execute(
            "INSERT INTO symbols (snapshot_id, file_path, symbol_name, symbol_kind, is_public, cognitive_complexity, cyclomatic_complexity)
             VALUES (1, ?1, 'symbol', 'Function', 1, ?2, ?2)",
            (file_path, complexity),
        )
        .unwrap();
}

fn insert_snapshot(storage: &StorageManager) {
    storage
        .get_connection()
        .execute(
            "INSERT INTO snapshots (id, timestamp, is_clean, packet_json) VALUES (1, '2026-01-01T00:00:00Z', 0, '{}')",
            [],
        )
        .unwrap();
}

fn commit(paths: &[&str]) -> CommitFileSet {
    CommitFileSet {
        files: paths.iter().map(Utf8PathBuf::from).collect::<HashSet<_>>(),
        is_merge: false,
    }
}
