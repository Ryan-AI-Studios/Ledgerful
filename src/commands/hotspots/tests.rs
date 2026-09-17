use super::explain::complexity_for_entity_path;
use super::list::{
    latest_hotspot_history_timestamp, list_hotspot_json, live_list_provenance, omitted_docs_footer,
    omitted_hotspots_footer, omitted_vendor_hotspots_footer, wrap_hotspots_list_json,
    wrap_hotspots_list_json_with_completeness,
};
use super::trend::{
    TrendMode, TrendRow, build_trend_summary, compute_history_available, format_trend_ts,
    render_hotspot_trend_table, render_trend_summary_table, resolve_trend_mode,
    trend_entries_provenance, trend_file_json, trend_summary_provenance,
};
use crate::impact::budget::{
    CompletenessFilter, HotspotProvenance, HotspotProvenanceSource, format_provenance_footer,
};
use crate::impact::hotspots::normalize_score;

fn row(path: &str, at: &str, score: f64, hash: &str) -> TrendRow {
    TrendRow {
        file_path: path.to_string(),
        recorded_at: at.to_string(),
        score,
        commit_hash: Some(hash.to_string()),
    }
}

#[test]
fn omitted_hotspots_footer_zero_is_none() {
    assert_eq!(omitted_hotspots_footer(0), None);
}

#[test]
fn omitted_hotspots_footer_n_names_include_tests_flag() {
    assert_eq!(
        omitted_hotspots_footer(3).as_deref(),
        Some("3 test/example files omitted; --include tests")
    );
}

#[test]
fn omitted_docs_footer_zero_is_none() {
    assert_eq!(omitted_docs_footer(0), None);
}

#[test]
fn omitted_docs_footer_n_names_include_docs_flag() {
    assert_eq!(
        omitted_docs_footer(2).as_deref(),
        Some("2 documentation files omitted; --include docs")
    );
}

#[test]
fn omitted_vendor_hotspots_footer_zero_is_none() {
    assert_eq!(omitted_vendor_hotspots_footer(0), None);
}

#[test]
fn omitted_vendor_hotspots_footer_n_names_include_vendor_flag() {
    assert_eq!(
        omitted_vendor_hotspots_footer(3).as_deref(),
        Some("3 vendored files omitted; --include vendor")
    );
}

#[test]
fn wrap_hotspots_list_json_truncates_and_echoes_limit() {
    let items: Vec<serde_json::Value> = (0..5).map(|i| serde_json::json!({ "i": i })).collect();
    let output = wrap_hotspots_list_json(items, 3);
    assert_eq!(output["schemaVersion"], 1);
    assert_eq!(output["limit"], 3);
    assert_eq!(output["files"].as_array().map(Vec::len), Some(3));
    assert!(
        output.get("emptyReason").is_none(),
        "envelope must not invent emptyReason: {output}"
    );
}

#[test]
fn wrap_hotspots_list_json_empty_files_is_schema_version_1() {
    let output = wrap_hotspots_list_json(Vec::<serde_json::Value>::new(), 10);
    assert_eq!(output["schemaVersion"], 1);
    assert_eq!(output["limit"], 10);
    assert_eq!(output["files"].as_array().map(Vec::len), Some(0));
}

#[test]
fn history_walk_complete_omits_completeness() {
    let output = wrap_hotspots_list_json(vec![serde_json::json!({"path": "src/a.rs"})], 10);
    assert_eq!(output["schemaVersion"], 1);
    assert!(
        output.get("completeness").is_none(),
        "complete walk must omit completeness: {output}"
    );
}

#[test]
fn hotspots_json_budget_emits_one_document() {
    use crate::impact::budget::{CompletenessFilter, HistoryWalkStop, completeness_for_walk};

    let completeness = completeness_for_walk(
        HistoryWalkStop::Budget,
        500,
        3,
        None,
        CompletenessFilter::Default,
        Some("abc".to_string()),
        Some(45),
    )
    .expect("budget completeness");
    let output = wrap_hotspots_list_json_with_completeness(
        vec![serde_json::json!({"path": "src/a.rs"})],
        10,
        Some(&completeness),
        None,
    );
    assert_eq!(output["schemaVersion"], 1);
    assert_eq!(output["limit"], 10);
    assert!(output.get("files").is_some(), "{output}");
    assert_eq!(output["completeness"]["stop"], "budget");
    assert!(
        output["completeness"]["commitsWalked"].as_u64().unwrap()
            < output["completeness"]["commitsRequested"].as_u64().unwrap()
    );
    let encoded = serde_json::to_string(&output).expect("one document");
    let parsed: serde_json::Value = serde_json::from_str(&encoded).expect("single JSON value");
    assert!(
        parsed.is_object(),
        "stdout must be one JSON object: {parsed}"
    );
}

#[test]
fn same_state_complete_walks_are_byte_identical() {
    let files = vec![
        serde_json::json!({"path": "src/b.rs", "score": 0.2}),
        serde_json::json!({"path": "src/a.rs", "score": 0.9}),
    ];
    let a = serde_json::to_string(&wrap_hotspots_list_json(files.clone(), 10)).unwrap();
    let b = serde_json::to_string(&wrap_hotspots_list_json(files, 10)).unwrap();
    assert_eq!(a, b, "complete same-key wraps must be byte-identical");
}

#[test]
fn format_trend_ts_parses_rfc3339() {
    assert_eq!(
        format_trend_ts("2026-06-21T15:00:00+00:00"),
        "2026-06-21 15:00 UTC"
    );
}

#[test]
fn format_trend_ts_falls_back_to_input_on_bad_timestamp() {
    assert_eq!(format_trend_ts("not-a-date"), "not-a-date");
}

#[test]
fn render_hotspot_trend_table_uses_premium_framing() {
    let rows = vec![TrendRow {
        file_path: "src/lib.rs".to_string(),
        recorded_at: "2026-06-21T15:00:00+00:00".to_string(),
        score: 1.2345,
        commit_hash: Some("abc".to_string()),
    }];
    let rendered = render_hotspot_trend_table(&rows);
    assert!(
        rendered.contains('╭') || rendered.contains('+'),
        "expected premium table border (utf8 rounded or ascii +), got:\n{rendered}"
    );
    assert!(
        rendered.contains("Timestamp") && rendered.contains("File") && rendered.contains("Display"),
        "expected Display header, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("Score"),
        "bare Score header is the honesty bug: {rendered}"
    );
    assert!(
        rendered.contains("src/lib.rs"),
        "expected row content, got:\n{rendered}"
    );
    assert!(
        rendered.contains("7.119"),
        "expected normalized display_score in table (ln_1p of 1.2345*1000 = 7.119), got:\n{rendered}"
    );
}

#[test]
fn trend_display_score_matches_hotspots_normalization() {
    // Raw score 0.0043 previously appeared as a tiny raw value in the trend
    // table; after normalization it should match the `hotspots` display scale.
    let raw = 0.0043_f64;
    let expected = normalize_score(raw);
    let row = TrendRow {
        file_path: "src/lib.rs".to_string(),
        recorded_at: "2026-06-21T15:00:00+00:00".to_string(),
        score: raw,
        commit_hash: Some("abc".to_string()),
    };
    let rendered = render_hotspot_trend_table(std::slice::from_ref(&row));
    assert!(
        rendered.contains(&format!("{:.3}", expected)),
        "expected trend table score {:.3} to match hotspots normalization of raw {raw}, got:\n{rendered}",
        expected
    );

    // JSON shape must include both raw score and computed display_score.
    let entries_json = serde_json::json!({
        "file_path": row.file_path,
        "recorded_at": row.recorded_at,
        "score": row.score,
        "display_score": normalize_score(row.score),
        "commit_hash": row.commit_hash,
    });
    assert_eq!(
        entries_json["display_score"].as_f64().unwrap(),
        expected,
        "JSON display_score must equal hotspots normalization"
    );
    assert!(
        (entries_json["score"].as_f64().unwrap() - raw).abs() < f64::EPSILON,
        "JSON score must remain the raw value"
    );
}

#[test]
fn build_trend_summary_ranks_by_latest_score_path_tiebreak() {
    // Shuffled input order must not affect ranking.
    let rows = vec![
        row("b.rs", "2026-01-02T00:00:00Z", 1.0, "c2"),
        row("a.rs", "2026-01-01T00:00:00Z", 5.0, "c1"),
        row("a.rs", "2026-01-03T00:00:00Z", 2.0, "c3"), // latest for a = 2.0
        row("b.rs", "2026-01-03T00:00:00Z", 2.0, "c3"), // latest for b = 2.0 (tie → a first)
        row("c.rs", "2026-01-03T00:00:00Z", 9.0, "c3"), // highest
    ];
    let summary = build_trend_summary(&rows, 10);
    assert_eq!(summary.total_files, 3);
    assert_eq!(summary.total_entries, 5);
    assert_eq!(summary.snapshot_count, 3);
    assert!(!summary.truncated);
    let paths: Vec<&str> = summary.files.iter().map(|f| f.file_path.as_str()).collect();
    assert_eq!(paths, vec!["c.rs", "a.rs", "b.rs"]);
    assert!((summary.files[0].latest_score - 9.0).abs() < f64::EPSILON);
    assert!((summary.files[1].latest_score - 2.0).abs() < f64::EPSILON);
    assert!((summary.files[2].latest_score - 2.0).abs() < f64::EPSILON);
}

#[test]
fn build_trend_summary_nan_sorts_lowest_deterministic() {
    let rows = vec![
        row("nan.rs", "2026-01-02T00:00:00Z", f64::NAN, "c2"),
        row("inf.rs", "2026-01-02T00:00:00Z", f64::INFINITY, "c2"),
        row("ok.rs", "2026-01-02T00:00:00Z", 1.0, "c2"),
        row(
            "neg_inf.rs",
            "2026-01-02T00:00:00Z",
            f64::NEG_INFINITY,
            "c2",
        ),
    ];
    let summary = build_trend_summary(&rows, 10);
    let paths: Vec<&str> = summary.files.iter().map(|f| f.file_path.as_str()).collect();
    // Finite 1.0 ranks first; non-finite map to NEG_INFINITY → path ASC among them.
    assert_eq!(paths[0], "ok.rs");
    assert_eq!(&paths[1..], &["inf.rs", "nan.rs", "neg_inf.rs"]);
    // Same input twice → same order (determinism; avoid f64 NaN PartialEq).
    let again = build_trend_summary(&rows, 10);
    let paths_again: Vec<&str> = again.files.iter().map(|f| f.file_path.as_str()).collect();
    assert_eq!(paths, paths_again);
}

#[test]
fn build_trend_summary_single_sample_omits_prior_and_delta() {
    let rows = vec![row("solo.rs", "2026-01-01T00:00:00Z", 3.0, "c1")];
    let summary = build_trend_summary(&rows, 20);
    assert_eq!(summary.files.len(), 1);
    assert_eq!(summary.files[0].sample_count, 1);
    assert!(summary.files[0].prior_display_score.is_none());
    assert!(summary.files[0].delta.is_none());
    let json = trend_file_json(&summary.files[0]);
    assert!(json.get("priorDisplayScore").is_none());
    assert!(json.get("delta").is_none());
}

#[test]
fn build_trend_summary_prior_is_window_scoped_previous_sample() {
    let rows = vec![
        row("f.rs", "2026-01-01T00:00:00Z", 1.0, "c1"),
        row("f.rs", "2026-01-02T00:00:00Z", 2.0, "c2"),
        row("f.rs", "2026-01-03T00:00:00Z", 4.0, "c3"),
    ];
    let summary = build_trend_summary(&rows, 5);
    let f = &summary.files[0];
    assert_eq!(f.sample_count, 3);
    assert!((f.latest_score - 4.0).abs() < f64::EPSILON);
    let expected_prior = normalize_score(2.0);
    let expected_delta = normalize_score(4.0) - expected_prior;
    assert_eq!(f.prior_display_score, Some(expected_prior));
    assert_eq!(f.delta, Some(expected_delta));
    assert_eq!(f.commit_hash.as_deref(), Some("c3"));
}

#[test]
fn build_trend_summary_truncates_to_limit() {
    let rows: Vec<TrendRow> = (0..5)
        .map(|i| {
            row(
                &format!("f{i}.rs"),
                "2026-01-01T00:00:00Z",
                f64::from(i),
                "c1",
            )
        })
        .collect();
    let summary = build_trend_summary(&rows, 2);
    assert_eq!(summary.total_files, 5);
    assert_eq!(summary.files.len(), 2);
    assert!(summary.truncated);
    assert_eq!(summary.limit, 2);
    // Highest scores: f4, f3
    assert_eq!(summary.files[0].file_path, "f4.rs");
    assert_eq!(summary.files[1].file_path, "f3.rs");
}

#[test]
fn history_available_true_when_entries_non_empty() {
    // H1: never false while trend rows exist, even if history_known_present is false.
    assert!(compute_history_available(false, 10));
    assert!(compute_history_available(true, 0));
    assert!(!compute_history_available(false, 0));
}

#[test]
fn resolve_trend_mode_precedence_entity_over_all_over_summary() {
    assert_eq!(
        resolve_trend_mode(&Some("a.rs".into()), true, 5),
        TrendMode::Entity
    );
    assert_eq!(resolve_trend_mode(&None, true, 5), TrendMode::Full);
    assert_eq!(
        resolve_trend_mode(&None, false, 5),
        TrendMode::Summary { limit: 5 }
    );
}

#[test]
fn render_trend_summary_table_has_display_header_and_em_dash_prior() {
    let summary = build_trend_summary(
        &[row("solo.rs", "2026-06-21T15:00:00+00:00", 1.0, "abc")],
        20,
    );
    let rendered = render_trend_summary_table(&summary);
    assert!(
        rendered.contains("Display") && rendered.contains("Prior"),
        "expected summary headers, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("Score"),
        "bare Score header is the honesty bug: {rendered}"
    );
    // Style-aware: Utf8 uses Δ / —; Ascii uses Delta / -
    assert!(
        (rendered.contains('Δ') && rendered.contains('—'))
            || (rendered.contains("Delta") && rendered.contains('-')),
        "expected delta header + missing prior marker, got:\n{rendered}"
    );
    assert!(
        rendered.contains("solo.rs"),
        "expected file path, got:\n{rendered}"
    );
}

/// 0183-B3: explain complexity resolves `pkg.rs` → `pkg/mod.rs` when only
/// the latter is indexed (project_files SQL).
#[test]
fn complexity_resolves_file_form_to_mod_rs() {
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    let mut conn = Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) \
         VALUES (1, 'src/pkg/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols \
         (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
          cognitive_complexity, cyclomatic_complexity, last_indexed_at) \
         VALUES (1, 1, 'f', 'f', 'Function', 1, 12, 8, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // Raw path would miss project_files without resolve.
    let raw: i32 = conn
        .query_row(
            "SELECT MAX(IFNULL(cognitive_complexity, 0), IFNULL(cyclomatic_complexity, 0)) \
             FROM project_symbols ps JOIN project_files pf ON ps.file_id = pf.id \
             WHERE pf.file_path = ?1",
            ["src/pkg.rs"],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert_eq!(raw, 0);

    let resolved = complexity_for_entity_path(&conn, "src/pkg.rs").unwrap();
    assert_eq!(resolved, 12);
}

#[test]
fn complexity_ambiguous_refuses() {
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    let mut conn = Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (1, 'src/a/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (2, 'src/b/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    let err = complexity_for_entity_path(&conn, "mod.rs").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("more specific path"), "{msg}");
    assert!(msg.contains("src/a/mod.rs"), "{msg}");
}

/// 0210-C: nested MAX across symbols, not the first `project_symbols` row.
/// Seed order is load-bearing: cog/cyc 3 first would win on the old 2-arg
/// scalar `MAX` + `query_row`.
#[test]
fn complexity_two_symbols_uses_max() {
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    let mut conn = Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) \
         VALUES (1, 'src/file.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols \
         (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
          cognitive_complexity, cyclomatic_complexity, last_indexed_at) \
         VALUES (1, 1, 'first', 'first', 'Function', 1, 3, 3, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols \
         (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
          cognitive_complexity, cyclomatic_complexity, last_indexed_at) \
         VALUES (2, 1, 'second', 'second', 'Function', 1, 12, 8, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let resolved = complexity_for_entity_path(&conn, "src/file.rs").unwrap();
    assert_eq!(resolved, 12);
}

fn live_prov(
    commits: u64,
    days: Option<u64>,
    limit: u64,
    filter: CompletenessFilter,
) -> HotspotProvenance {
    HotspotProvenance {
        source: HotspotProvenanceSource::Live,
        commits_requested: Some(commits),
        days_requested: days,
        limit: Some(limit),
        filter: Some(filter),
        head: Some("abc".to_string()),
        ..HotspotProvenance::default()
    }
}

#[test]
fn list_json_emits_provenance_on_complete_walk() {
    let provenance = live_prov(500, None, 10, CompletenessFilter::Default);
    let output = wrap_hotspots_list_json_with_completeness(
        vec![serde_json::json!({"path": "src/a.rs"})],
        10,
        None,
        Some(&provenance),
    );
    assert_eq!(output["schemaVersion"], 1);
    assert_eq!(output["limit"], 10);
    assert!(output.get("completeness").is_none(), "{output}");
    assert_eq!(output["provenance"]["source"], "live");
    assert_eq!(output["provenance"]["filter"], "default");
    assert_eq!(output["provenance"]["commitsRequested"], 500);
}

#[test]
fn list_json_emits_provenance_on_empty_files() {
    let provenance = live_prov(50, Some(30), 5, CompletenessFilter::Session);
    let output = wrap_hotspots_list_json_with_completeness(
        Vec::<serde_json::Value>::new(),
        5,
        None,
        Some(&provenance),
    );
    assert_eq!(output["files"].as_array().map(Vec::len), Some(0));
    assert_eq!(output["provenance"]["source"], "live");
}

#[test]
fn list_json_omits_days_requested_without_days_flag() {
    let provenance = live_prov(500, None, 10, CompletenessFilter::Default);
    let output = wrap_hotspots_list_json_with_completeness(
        vec![serde_json::json!({"path": "src/a.rs"})],
        10,
        None,
        Some(&provenance),
    );
    assert!(
        output["provenance"].get("daysRequested").is_none(),
        "{output}"
    );
}

#[test]
fn semantic_json_omits_provenance() {
    let output = wrap_hotspots_list_json(vec![serde_json::json!({"path": "dup.rs"})], 10);
    assert!(output.get("provenance").is_none(), "{output}");
    assert!(output.get("completeness").is_none(), "{output}");
    assert!(
        output["files"][0].get("presence").is_none(),
        "semantic wrap must not invent presence: {output}"
    );
}

#[test]
fn format_provenance_footer_exact_strings() {
    let live = live_prov(500, None, 10, CompletenessFilter::Default);
    assert_eq!(
        format_provenance_footer(&live),
        "Window: 500 commits · Filter: default · Source: live"
    );
    let live_days = live_prov(50, Some(30), 5, CompletenessFilter::Session);
    assert_eq!(
        format_provenance_footer(&live_days),
        "Window: 50 commits · 30 days · Filter: session · Source: live"
    );
    let summary = trend_summary_provenance(30, 20, &[]);
    assert_eq!(
        format_provenance_footer(&summary),
        "Window: 30 days · Limit: 20 · Source: trends · Delta: displayScore"
    );
    let full = trend_entries_provenance(14, &[]);
    assert_eq!(
        format_provenance_footer(&full),
        "Window: 14 days · Source: trends"
    );
}

#[test]
fn trend_json_labels_delta_unit_display_score() {
    let rows = vec![
        row("f.rs", "2026-01-01T00:00:00Z", 0.1, "c1"),
        row("f.rs", "2026-01-02T00:00:00Z", 0.2, "c2"),
    ];
    let summary = build_trend_summary(&rows, 5);
    assert!((summary.files[0].latest_score - 0.2).abs() < f64::EPSILON);
    assert!(summary.files[0].latest_score > 0.0 && summary.files[0].latest_score <= 1.0);
    assert!(summary.files[0].delta.is_some());
    let provenance = trend_summary_provenance(30, summary.limit as u64, &rows);
    let v = serde_json::to_value(&provenance).expect("json");
    assert_eq!(v["source"], "trends");
    assert_eq!(v["deltaUnit"], "displayScore");
    assert_eq!(v["firstRecordedAt"], "2026-01-01T00:00:00Z");
    assert_eq!(v["lastRecordedAt"], "2026-01-02T00:00:00Z");
    assert!(v.get("filter").is_none());
    assert!(v.get("head").is_none());
    assert!(v.get("commitsRequested").is_none());

    let solo = vec![row("solo.rs", "2026-01-01T00:00:00Z", 0.3, "c1")];
    let solo_summary = build_trend_summary(&solo, 5);
    assert!(solo_summary.files[0].delta.is_none());
}

#[test]
fn trend_json_empty_omits_extrema() {
    let provenance = trend_summary_provenance(30, 20, &[]);
    let v = serde_json::to_value(&provenance).expect("json");
    assert_eq!(v["source"], "trends");
    assert!(v.get("firstRecordedAt").is_none(), "{v}");
    assert!(v.get("lastRecordedAt").is_none(), "{v}");
}

#[test]
fn trend_full_entity_omit_limit_filter_head_delta_unit() {
    let rows = vec![row("f.rs", "2026-01-01T00:00:00Z", 0.1, "c1")];
    let provenance = trend_entries_provenance(30, &rows);
    let v = serde_json::to_value(&provenance).expect("json");
    assert_eq!(v["source"], "trends");
    assert!(v.get("limit").is_none(), "{v}");
    assert!(v.get("filter").is_none(), "{v}");
    assert!(v.get("head").is_none(), "{v}");
    assert!(v.get("deltaUnit").is_none(), "{v}");
    assert_eq!(v["firstRecordedAt"], "2026-01-01T00:00:00Z");
}

#[test]
fn list_json_snapshot_age_when_history_exists() {
    use crate::impact::hotspots::HotspotQuery;
    use crate::state::layout::Layout;
    use crate::state::storage::StorageManager;
    use chrono::{TimeZone, Utc};

    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    assert!(latest_hotspot_history_timestamp(&storage).is_none());

    let query = HotspotQuery {
        commits: 500,
        limit: 10,
        ..HotspotQuery::default()
    };
    let empty = live_list_provenance(&query, None, None, None, Utc::now());
    let empty_json = serde_json::to_value(&empty).expect("json");
    assert!(empty_json.get("snapshotAt").is_none(), "{empty_json}");
    assert!(empty_json.get("snapshotAgeSecs").is_none(), "{empty_json}");

    storage
        .get_connection()
        .execute(
            "INSERT INTO hotspot_history (file_path, score, display_score, complexity, frequency, timestamp) \
             VALUES ('src/a.rs', 0.1, 1.0, 1, 1.0, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    let ts = latest_hotspot_history_timestamp(&storage).expect("row");
    assert_eq!(ts, "2026-01-01T00:00:00Z");
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap();
    let with_age = live_list_provenance(&query, None, None, Some(ts), now);
    let v = serde_json::to_value(&with_age).expect("json");
    assert_eq!(v["snapshotAt"], "2026-01-01T00:00:00Z");
    assert!(v["snapshotAgeSecs"].as_u64().expect("age") >= 3600);
    let _ = storage.shutdown();
}

#[test]
fn presence_historical_on_deleted_git_path() {
    use crate::impact::packet::Hotspot;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    for (k, v) in [("user.email", "t@t.com"), ("user.name", "T")] {
        assert!(
            Command::new("git")
                .args(["config", k, v])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(dir.join("stay.rs"), "fn stay() {}\n").unwrap();
    fs::write(dir.join("gone.rs"), "fn gone() {}\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    fs::remove_file(dir.join("gone.rs")).unwrap();
    assert!(
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "delete gone"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );

    let repo = crate::git::repo::open_repo(dir).expect("open");
    let gone = Hotspot {
        path: PathBuf::from("gone.rs"),
        score: 0.2,
        display_score: 1.0,
        complexity: 1,
        frequency: 1.0,
        centrality: None,
    };
    let stay = Hotspot {
        path: PathBuf::from("stay.rs"),
        score: 0.3,
        display_score: 1.1,
        complexity: 1,
        frequency: 1.0,
        centrality: None,
    };
    let gone_json = list_hotspot_json(&repo, &gone);
    let stay_json = list_hotspot_json(&repo, &stay);
    assert_eq!(gone_json["presence"], "historical");
    assert!(stay_json.get("presence").is_none(), "{stay_json}");
    assert_eq!(gone_json["path"], "gone.rs");
}

#[test]
fn presence_omitted_when_head_unborn() {
    use crate::impact::packet::Hotspot;
    use std::path::PathBuf;
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    let repo = crate::git::repo::open_repo(dir).expect("unborn");
    let hotspot = Hotspot {
        path: PathBuf::from("gone.rs"),
        score: 0.2,
        display_score: 1.0,
        complexity: 1,
        frequency: 1.0,
        centrality: None,
    };
    let json = list_hotspot_json(&repo, &hotspot);
    assert!(
        json.get("presence").is_none(),
        "unborn HEAD must omit presence, not mark historical: {json}"
    );
}

#[test]
fn entity_prefix_still_git_history_starts_with() {
    use crate::impact::hotspots::{HotspotQuery, calculate_hotspots_detailed};
    use crate::impact::temporal::GixHistoryProvider;
    use crate::state::layout::Layout;
    use crate::state::storage::StorageManager;
    use std::fs;
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    for (k, v) in [("user.email", "t@t.com"), ("user.name", "T")] {
        assert!(
            Command::new("git")
                .args(["config", k, v])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("other")).unwrap();
    fs::write(dir.join("src/lib.rs"), "fn src() {}\n").unwrap();
    fs::write(dir.join("other/out.rs"), "fn other() {}\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );

    let root = camino::Utf8Path::from_path(dir).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let repo = crate::git::repo::open_repo(dir).expect("open");
    let provider = GixHistoryProvider::new(&repo);
    let query = HotspotQuery {
        commits: 50,
        limit: 10,
        dir_filter: Some("src/".to_string()),
        ..HotspotQuery::default()
    };
    let calc_filters = calculate_hotspots_detailed(
        &storage,
        &provider,
        &HotspotQuery {
            commits: 50,
            limit: 10,
            dir_filters: vec!["src/".to_string()],
            ..HotspotQuery::default()
        },
    )
    .expect("dir_filters calc");
    assert!(
        calc_filters.hotspots.iter().all(|h| h
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .starts_with("src/")),
        "dir_filters must crawl-filter like dir_filter: {:?}",
        calc_filters
            .hotspots
            .iter()
            .map(|h| h.path.clone())
            .collect::<Vec<_>>()
    );
    let calc = calculate_hotspots_detailed(&storage, &provider, &query).expect("calc");
    assert!(
        calc.hotspots.iter().all(|h| h
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .starts_with("src/")),
        "dir_filter must stay git-history starts_with: {:?}",
        calc.hotspots
            .iter()
            .map(|h| h.path.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        calc.hotspots
            .iter()
            .all(|h| !h.path.to_string_lossy().contains("other/")),
        "must not remap via project_files: {:?}",
        calc.hotspots
            .iter()
            .map(|h| h.path.clone())
            .collect::<Vec<_>>()
    );
    let _ = storage.shutdown();
}

#[test]
#[allow(non_snake_case)]
fn dir_filters_multi_prefix__does_not_false_empty_outside_top_n() {
    use crate::impact::hotspots::{HotspotQuery, calculate_hotspots_detailed};
    use crate::impact::temporal::GixHistoryProvider;
    use crate::state::layout::Layout;
    use crate::state::storage::StorageManager;
    use std::fs;
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    for (k, v) in [("user.email", "t@t.com"), ("user.name", "T")] {
        assert!(
            Command::new("git")
                .args(["config", k, v])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("docs")).unwrap();
    fs::create_dir_all(dir.join("tests")).unwrap();
    for i in 0..8 {
        fs::write(
            dir.join(format!("tests/noise{i}.rs")),
            format!("fn n{i}() {{}}\n"),
        )
        .unwrap();
    }
    assert!(
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "noise"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    fs::write(dir.join("src/lib.rs"), "fn src() {}\n").unwrap();
    fs::write(dir.join("docs/note.md"), "# note\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "scoped"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );

    let root = camino::Utf8Path::from_path(dir).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let repo = crate::git::repo::open_repo(dir).expect("open");
    let provider = GixHistoryProvider::new(&repo);
    let calc = calculate_hotspots_detailed(
        &storage,
        &provider,
        &HotspotQuery {
            commits: 50,
            limit: 3,
            dir_filters: vec!["src/".to_string(), "docs/".to_string()],
            ..HotspotQuery::default()
        },
    )
    .expect("multi-prefix calc");
    let paths: Vec<String> = calc
        .hotspots
        .iter()
        .map(|h| h.path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(
        paths.iter().any(|p| p.starts_with("src/")),
        "src/ must survive in-engine filter when outside global top-N: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.starts_with("docs/")),
        "docs/ must survive in-engine filter when outside global top-N: {paths:?}"
    );
    assert!(
        paths.iter().all(|p| !p.starts_with("tests/")),
        "tests/ must stay excluded: {paths:?}"
    );
    let _ = storage.shutdown();
}

fn budget_storage() -> (tempfile::TempDir, crate::state::storage::StorageManager) {
    use crate::state::layout::Layout;
    use crate::state::storage::StorageManager;

    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    (tmp, storage)
}

fn insert_history(
    storage: &crate::state::storage::StorageManager,
    path: &str,
    score: f64,
    timestamp: &str,
) {
    storage
        .get_connection()
        .execute(
            "INSERT INTO hotspot_history (file_path, score, display_score, complexity, frequency, timestamp) \
             VALUES (?1, ?2, 1.0, 1, 1.0, ?3)",
            rusqlite::params![path, score, timestamp],
        )
        .unwrap();
}

#[test]
fn budget_in_range_score_is_ok() {
    use super::budget::{BudgetStatus, ThresholdSource, evaluate_hotspot_budget};
    use chrono::{TimeZone, Utc};

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/a.rs", 0.1, "2026-01-01T00:00:00Z");
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap();
    let report = evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, now).unwrap();
    assert_eq!(report.status, BudgetStatus::Ok);
    assert_eq!(report.evaluated, 1);
    assert!(report.violations.is_empty());
    assert_eq!(report.score_unit, "score");
    assert_eq!(report.threshold, Some(0.5));
    assert_eq!(report.threshold_source, Some(ThresholdSource::Cli));
    let _ = storage.shutdown();
}

#[test]
fn budget_over_threshold_is_violation() {
    use super::budget::{BudgetStatus, evaluate_hotspot_budget, format_budget_human};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/hot.rs", 0.9, "2026-01-01T00:00:00Z");
    let report =
        evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::Violation);
    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].path, "src/hot.rs");
    assert!((report.violations[0].score - 0.9).abs() < f64::EPSILON);
    assert!((report.violations[0].threshold - 0.5).abs() < f64::EPSILON);
    assert!(report.next.is_none(), "{report:?}");
    let human = format_budget_human(&report);
    assert!(human.contains("exceeds budget"), "{human}");
    let _ = storage.shutdown();
}

#[test]
fn budget_empty_history_is_no_data_not_ok() {
    use super::budget::{BudgetStatus, evaluate_hotspot_budget, format_budget_human};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    let report =
        evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::NoData);
    assert_eq!(report.evaluated, 0);
    let human = format_budget_human(&report);
    assert!(human.contains("No hotspot_history snapshot"), "{human}");
    assert!(!human.contains("within budget"), "{human}");
    let _ = storage.shutdown();
}

#[test]
fn budget_json_empty_names_dataset_and_snapshot_next() {
    use super::budget::{BudgetEmptyReason, BudgetStatus, evaluate_hotspot_budget};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    let report =
        evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::NoData);
    assert_eq!(report.dataset, "hotspot_history");
    assert_eq!(report.empty_reason, Some(BudgetEmptyReason::NoSnapshot));
    assert_eq!(
        report.next.as_deref(),
        Some("ledgerful hotspots --snapshot")
    );
    let v = serde_json::to_value(&report).expect("json");
    assert_eq!(v["dataset"], "hotspot_history");
    assert_eq!(v["emptyReason"], "noSnapshot");
    assert_eq!(v["next"], "ledgerful hotspots --snapshot");
    assert!(v.get("schemaVersion").is_none(), "{v}");
    assert!(v.get("provenance").is_none(), "{v}");
    let _ = storage.shutdown();
}

#[test]
fn budget_human_no_snapshot_keeps_snapshot_command() {
    use super::budget::{evaluate_hotspot_budget, format_budget_human};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    let report =
        evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, Utc::now()).unwrap();
    let human = format_budget_human(&report);
    assert!(human.contains("No hotspot_history snapshot"), "{human}");
    assert!(human.contains("ledgerful hotspots --snapshot"), "{human}");
    let _ = storage.shutdown();
}

#[test]
fn budget_populated_ok_omits_next_and_empty_reason() {
    use super::budget::evaluate_hotspot_budget;
    use chrono::{TimeZone, Utc};

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/a.rs", 0.1, "2026-01-01T00:00:00Z");
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap();
    let report = evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, now).unwrap();
    assert_eq!(report.dataset, "hotspot_history");
    assert!(report.next.is_none(), "{report:?}");
    assert!(report.empty_reason.is_none(), "{report:?}");
    let v = serde_json::to_value(&report).expect("json");
    assert_eq!(v["dataset"], "hotspot_history");
    assert!(v.get("next").is_none(), "{v}");
    assert!(v.get("emptyReason").is_none(), "{v}");
    assert_eq!(v["snapshotAt"], "2026-01-01T00:00:00Z");
    assert!(v["snapshotAgeSecs"].as_u64().expect("age") >= 3600);
    let _ = storage.shutdown();
}

#[test]
fn budget_stale_snapshot_age_is_informational() {
    use super::budget::{BudgetStatus, evaluate_hotspot_budget};
    use chrono::{TimeZone, Utc};

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/a.rs", 0.1, "2026-01-01T00:00:00Z");
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap();
    let report = evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, now).unwrap();
    assert_eq!(report.status, BudgetStatus::Ok);
    assert!(report.snapshot_age_secs.expect("age") >= 3600);
    let _ = storage.shutdown();
}

#[test]
fn classify_persisted_score_non_finite_is_skipped() {
    use super::budget::{PersistedScoreClass, classify_persisted_score};

    assert_eq!(classify_persisted_score(0.5), PersistedScoreClass::Finite);
    assert_eq!(
        classify_persisted_score(f64::NAN),
        PersistedScoreClass::NonFinite
    );
    assert_eq!(
        classify_persisted_score(f64::INFINITY),
        PersistedScoreClass::NonFinite
    );
    assert_eq!(
        classify_persisted_score(f64::NEG_INFINITY),
        PersistedScoreClass::NonFinite
    );
}

#[test]
fn budget_all_non_finite_report_human_and_json_direct() {
    use super::budget::{BudgetEmptyReason, BudgetReport, BudgetStatus, format_budget_human};

    let report = BudgetReport {
        status: BudgetStatus::NoData,
        dataset: "hotspot_history",
        score_unit: "score",
        threshold: Some(0.5),
        threshold_source: None,
        evaluated: 0,
        violations: Vec::new(),
        snapshot_at: Some("2026-01-01T00:00:00Z".to_string()),
        snapshot_age_secs: Some(3600),
        head: None,
        legacy_score_count: 0,
        skipped_non_finite: 2,
        empty_reason: Some(BudgetEmptyReason::AllNonFinite),
        next: None,
    };
    let human = format_budget_human(&report);
    assert!(
        human.contains(
            "Latest hotspot_history snapshot has no finite scores (skippedNonFinite=2). Not a pass."
        ),
        "{human}"
    );
    assert!(
        human.contains("Evaluated: 0 (snapshot 2026-01-01T00:00:00Z age 3600s)"),
        "{human}"
    );
    assert!(!human.contains("No hotspot_history snapshot"), "{human}");
    let v = serde_json::to_value(&report).expect("json");
    assert_eq!(v["emptyReason"], "allNonFinite");
    assert_eq!(v["dataset"], "hotspot_history");
    assert!(v.get("next").is_none(), "{v}");
}

#[test]
fn budget_all_non_finite_via_inf_bind() {
    use super::budget::{BudgetEmptyReason, BudgetStatus, evaluate_hotspot_budget};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    let inserted = storage.get_connection().execute(
        "INSERT INTO hotspot_history (file_path, score, display_score, complexity, frequency, timestamp) \
         VALUES (?1, ?2, 1.0, 1, 1.0, ?3)",
        rusqlite::params!["src/inf.rs", f64::INFINITY, "2026-01-01T00:00:00Z"],
    );
    match inserted {
        Ok(_) => {
            let report =
                evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, Utc::now())
                    .unwrap();
            assert_eq!(report.status, BudgetStatus::NoData);
            assert_eq!(report.empty_reason, Some(BudgetEmptyReason::AllNonFinite));
            assert!(report.next.is_none(), "{report:?}");
            assert!(report.skipped_non_finite >= 1);
        }
        Err(err) => {
            eprintln!("inf bind skipped: {err}");
        }
    }
    let _ = storage.shutdown();
}

#[test]
fn budget_json_echoes_unit_threshold_evaluated() {
    use super::budget::{ThresholdSource, evaluate_hotspot_budget};
    use chrono::{TimeZone, Utc};

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/a.rs", 0.1, "2026-01-01T00:00:00Z");
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap();
    let report = evaluate_hotspot_budget(&storage, None, None, false, None, now).unwrap();
    assert_eq!(report.threshold, Some(0.5));
    assert_eq!(report.threshold_source, Some(ThresholdSource::Default));
    assert_eq!(report.score_unit, "score");
    let v = serde_json::to_value(&report).expect("json");
    assert_eq!(v["scoreUnit"], "score");
    assert!((v["threshold"].as_f64().unwrap() - 0.5).abs() < f64::EPSILON);
    assert_eq!(v["thresholdSource"], "default");
    assert_eq!(v["snapshotAt"], "2026-01-01T00:00:00Z");
    assert!(v["snapshotAgeSecs"].as_u64().expect("age") >= 3600);
    let _ = storage.shutdown();
}

#[test]
fn budget_fail_without_threshold_is_not_configured() {
    use super::budget::{
        BudgetStatus, evaluate_hotspot_budget, format_budget_human, request_fail_exit,
    };
    use chrono::Utc;

    let _ = crate::output::requested_exit::take_requested_exit_code();
    let (_tmp, storage) = budget_storage();
    let report = evaluate_hotspot_budget(&storage, None, None, true, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::NotConfigured);
    assert!(report.threshold.is_none());
    assert_eq!(report.dataset, "hotspot_history");
    assert!(report.empty_reason.is_none(), "{report:?}");
    assert!(report.next.is_none(), "{report:?}");
    let v = serde_json::to_value(&report).expect("json");
    assert_eq!(v["dataset"], "hotspot_history");
    assert!(v.get("emptyReason").is_none(), "{v}");
    assert!(v.get("next").is_none(), "{v}");
    let human = format_budget_human(&report);
    assert!(human.contains("--fail requires --threshold"), "{human}");
    assert!(request_fail_exit(&report, true).is_err());
    assert_eq!(
        crate::output::requested_exit::take_requested_exit_code(),
        Some(1)
    );
    insert_history(&storage, "src/a.rs", 0.1, "2026-01-01T00:00:00Z");
    let with_rows = evaluate_hotspot_budget(&storage, None, None, true, None, Utc::now()).unwrap();
    assert_eq!(with_rows.status, BudgetStatus::NotConfigured);
    assert!(with_rows.empty_reason.is_none(), "{with_rows:?}");
    assert!(with_rows.next.is_none(), "{with_rows:?}");
    let _ = crate::output::requested_exit::take_requested_exit_code();
    let _ = storage.shutdown();
}

#[test]
fn budget_fail_violation_requests_exit_1() {
    use super::budget::{BudgetStatus, evaluate_hotspot_budget, request_fail_exit};
    use chrono::Utc;

    let _ = crate::output::requested_exit::take_requested_exit_code();
    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/hot.rs", 0.9, "2026-01-01T00:00:00Z");
    let report =
        evaluate_hotspot_budget(&storage, Some(0.5), None, true, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::Violation);
    assert!(request_fail_exit(&report, true).is_err());
    assert_eq!(
        crate::output::requested_exit::take_requested_exit_code(),
        Some(1)
    );
    assert!(request_fail_exit(&report, false).is_ok());
    assert_eq!(
        crate::output::requested_exit::take_requested_exit_code(),
        None
    );
    let _ = storage.shutdown();
}

#[test]
fn budget_fail_no_data_requests_exit_1() {
    use super::budget::{BudgetStatus, evaluate_hotspot_budget, request_fail_exit};
    use chrono::Utc;

    let _ = crate::output::requested_exit::take_requested_exit_code();
    let (_tmp, storage) = budget_storage();
    let report =
        evaluate_hotspot_budget(&storage, Some(0.5), None, true, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::NoData);
    assert!(request_fail_exit(&report, true).is_err());
    assert_eq!(
        crate::output::requested_exit::take_requested_exit_code(),
        Some(1)
    );
    let _ = crate::output::requested_exit::take_requested_exit_code();
    let _ = storage.shutdown();
}

#[test]
fn budget_legacy_score_gt_one_counted() {
    use super::budget::{BudgetStatus, evaluate_hotspot_budget, format_budget_human};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/old.rs", 12.0, "2026-01-01T00:00:00Z");
    let report =
        evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::Violation);
    assert_eq!(report.legacy_score_count, 1);
    let human = format_budget_human(&report);
    assert!(human.contains("score > 1"), "{human}");
    let v = serde_json::to_value(&report).expect("json");
    assert_eq!(v["legacyScoreCount"], 1);
    let _ = storage.shutdown();
}

#[test]
fn budget_threshold_cli_overrides_config() {
    use super::budget::{BudgetStatus, ThresholdSource, evaluate_hotspot_budget};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/a.rs", 0.5, "2026-01-01T00:00:00Z");
    let report =
        evaluate_hotspot_budget(&storage, Some(0.8), Some(0.2), false, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::Ok);
    assert_eq!(report.threshold, Some(0.8));
    assert_eq!(report.threshold_source, Some(ThresholdSource::Cli));
    let _ = storage.shutdown();
}

#[test]
fn budget_history_budget_secs_is_not_score_threshold() {
    use super::budget::{ThresholdSource, evaluate_hotspot_budget};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/a.rs", 0.1, "2026-01-01T00:00:00Z");
    let mut config = crate::config::model::Config::default();
    config.hotspots.history_budget_secs = 5;
    let report = evaluate_hotspot_budget(
        &storage,
        None,
        config.hotspots.budget_threshold,
        false,
        None,
        Utc::now(),
    )
    .unwrap();
    assert_eq!(report.threshold, Some(0.5));
    assert_eq!(report.threshold_source, Some(ThresholdSource::Default));
    let _ = storage.shutdown();
}

#[test]
fn budget_equal_threshold_is_ok() {
    use super::budget::{BudgetStatus, evaluate_hotspot_budget};
    use chrono::Utc;

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/a.rs", 0.5, "2026-01-01T00:00:00Z");
    let report =
        evaluate_hotspot_budget(&storage, Some(0.5), None, false, None, Utc::now()).unwrap();
    assert_eq!(report.status, BudgetStatus::Ok);
    let _ = storage.shutdown();
}

#[test]
fn parse_budget_threshold_rejects_out_of_range() {
    use crate::cli::parse_budget_threshold;

    assert!(parse_budget_threshold("1.1").is_err());
    assert!(parse_budget_threshold("-0.1").is_err());
    assert!(parse_budget_threshold("nan").is_err());
    assert!((parse_budget_threshold("0").unwrap() - 0.0).abs() < f64::EPSILON);
    assert!((parse_budget_threshold("1").unwrap() - 1.0).abs() < f64::EPSILON);
    assert!((parse_budget_threshold("0.5").unwrap() - 0.5).abs() < f64::EPSILON);
}

#[test]
fn list_session_trend_omit_score_unit() {
    use super::budget::evaluate_hotspot_budget;
    use crate::impact::hotspots::HotspotQuery;
    use chrono::Utc;

    let list = wrap_hotspots_list_json(vec![serde_json::json!({"path": "src/a.rs"})], 10);
    assert!(list.get("scoreUnit").is_none(), "{list}");
    let live = serde_json::to_value(live_list_provenance(
        &HotspotQuery::default(),
        None,
        None,
        None,
        Utc::now(),
    ))
    .expect("live");
    assert!(live.get("scoreUnit").is_none(), "{live}");
    let trend = serde_json::to_value(trend_summary_provenance(30, 20, &[])).expect("trend");
    assert!(trend.get("scoreUnit").is_none(), "{trend}");
    let full_trend = serde_json::to_value(trend_entries_provenance(30, &[])).expect("entries");
    assert!(full_trend.get("scoreUnit").is_none(), "{full_trend}");

    let (_tmp, storage) = budget_storage();
    insert_history(&storage, "src/a.rs", 0.1, "2026-01-01T00:00:00Z");
    let budget = evaluate_hotspot_budget(&storage, None, None, false, None, Utc::now()).unwrap();
    let budget_json = serde_json::to_value(&budget).expect("budget");
    assert_eq!(budget_json["scoreUnit"], "score");
    assert!(budget_json.get("schemaVersion").is_none(), "{budget_json}");
    assert!(budget_json.get("provenance").is_none(), "{budget_json}");
    let _ = storage.shutdown();
}

#[test]
#[allow(non_snake_case)]
fn hotspots_config_default__overall_25_history_45() {
    let config = crate::config::model::Config::default();
    assert_eq!(config.hotspots.overall_budget_secs, 25);
    assert_eq!(config.hotspots.history_budget_secs, 45);
}

#[test]
#[allow(non_snake_case)]
fn hotspots_list__completeness_history_only_copy_omits_scope() {
    use crate::impact::budget::{CompletenessFilter, CompletenessScope, HistoryWalkStop};
    let c = super::list::list_completeness_after_walk(
        HistoryWalkStop::Budget,
        500,
        3,
        None,
        CompletenessFilter::Default,
        Some("abc".into()),
        45,
        None,
        25,
    )
    .expect("history completeness");
    assert!(c.scope.is_none() || c.scope != Some(CompletenessScope::Overall));
    assert_eq!(c.budget_secs, Some(45));
}

#[test]
#[allow(non_snake_case)]
fn hotspots_list__overall_stop_not_overwritten_by_later_history_object() {
    use crate::impact::budget::{
        CompletenessFilter, CompletenessScope, CompletenessStop, HistoryWalkStop,
        overall_deadline_fired,
    };
    let expired = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap_or_else(std::time::Instant::now);
    assert!(overall_deadline_fired(Some(expired)));
    let c = super::list::list_completeness_after_walk(
        HistoryWalkStop::Budget,
        500,
        3,
        None,
        CompletenessFilter::Default,
        Some("abc".into()),
        45,
        Some(expired),
        25,
    )
    .expect("overall");
    assert_eq!(c.scope, Some(CompletenessScope::Overall));
    assert_eq!(c.stop, CompletenessStop::Budget);
    assert_eq!(c.stage.as_deref(), Some("hotspots"));
    assert_eq!(c.budget_secs, Some(25));
}

#[test]
#[allow(non_snake_case)]
fn hotspots_explain_json_envelope_kind() {
    use crate::impact::budget::{CompletenessStop, completeness_for_overall};
    let c = completeness_for_overall(CompletenessStop::Budget, Some(25), "git");
    let v = super::explain::explanation_json_envelope(
        "src/lib.rs",
        1,
        0.0,
        None,
        Vec::new(),
        Some("temporal couplings untrusted: overall budget".into()),
        Some(&c),
    );
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "hotspotExplanation");
    assert_eq!(v["entity"], "src/lib.rs");
    assert_eq!(v["completeness"]["scope"], "overall");
    assert_eq!(v["completeness"]["stage"], "git");
    assert_eq!(
        v["couplingsWarning"],
        "temporal couplings untrusted: overall budget"
    );
}

#[test]
#[allow(non_snake_case)]
#[serial_test::serial(cwd)]
fn hotspots_list__overall_expired__emits_json_with_scope_overall() {
    use super::{HotspotRunOpts, execute_hotspots_with_opts};
    use crate::cli::HotspotArgs;
    use crate::tests::DirGuard;
    use std::fs;
    use std::process::Command;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(root)
        .status()
        .unwrap();
    fs::write(root.join("README.md"), "one\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "first"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    let _guard = DirGuard::new(root);
    let mut buf = Vec::new();
    let args = HotspotArgs {
        json: true,
        limit: Some(5),
        ..Default::default()
    };
    let expired = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    execute_hotspots_with_opts(
        args,
        HotspotRunOpts {
            overall_deadline_override: Some(expired),
            ..Default::default()
        },
        Some(&mut buf),
    )
    .expect("emit");
    let stdout = String::from_utf8_lossy(&buf);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect(&stdout);
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["completeness"]["scope"], "overall");
    assert_eq!(v["completeness"]["stage"], "storage");
    assert_eq!(v["files"].as_array().map(Vec::len), Some(0));
}

#[test]
#[allow(non_snake_case)]
#[serial_test::serial(cwd)]
fn hotspots_explain__overall_expired__emits_json_kind_hotspot_explanation() {
    use super::{HotspotRunOpts, execute_hotspots_with_opts};
    use crate::cli::{HotspotArgs, HotspotSubcommands};
    use crate::tests::DirGuard;
    use std::fs;
    use std::process::Command;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(root)
        .status()
        .unwrap();
    fs::write(root.join("README.md"), "one\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "first"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    let _guard = DirGuard::new(root);
    let mut buf = Vec::new();
    let args = HotspotArgs {
        command: Some(HotspotSubcommands::Explain {
            entity: "README.md".into(),
            json: true,
        }),
        ..Default::default()
    };
    let expired = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    execute_hotspots_with_opts(
        args,
        HotspotRunOpts {
            overall_deadline_override: Some(expired),
            ..Default::default()
        },
        Some(&mut buf),
    )
    .expect("emit");
    let stdout = String::from_utf8_lossy(&buf);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect(&stdout);
    assert_eq!(v["kind"], "hotspotExplanation");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["completeness"]["scope"], "overall");
    assert_eq!(
        v["couplingsWarning"],
        "temporal couplings untrusted: overall budget"
    );
}

#[test]
#[allow(non_snake_case)]
fn hotspots_cli_timeout__does_not_write_history_budget_secs() {
    let mut config = crate::config::model::Config::default();
    crate::impact::budget::apply_resolved_history_budget(&mut config, None);
    assert_eq!(config.hotspots.history_budget_secs, 45);
    assert_eq!(
        crate::impact::budget::resolve_hotspots_overall_budget_secs(Some(5), 25),
        5
    );
    assert_eq!(config.hotspots.history_budget_secs, 45);
}

#[test]
#[allow(non_snake_case)]
fn hotspots_timeout_zero__disables_overall_wall_clock() {
    assert_eq!(
        crate::impact::budget::resolve_hotspots_overall_budget_secs(Some(0), 25),
        0
    );
}

#[test]
#[allow(non_snake_case)]
fn hotspots_list__omitted_timeout__uses_overall_default_not_history_only() {
    assert_eq!(
        crate::impact::budget::resolve_hotspots_overall_budget_secs(None, 25),
        25
    );
    assert_eq!(
        crate::impact::budget::resolve_history_budget_secs(None, 45),
        45
    );
}

#[test]
#[allow(non_snake_case)]
#[serial_test::serial(cwd)]
fn hotspots_semantic__overall_expired__emits_empty_files_stage_semantic() {
    use super::{HotspotRunOpts, execute_hotspots_with_opts};
    use crate::cli::HotspotArgs;
    use crate::tests::DirGuard;
    use std::fs;
    use std::process::Command;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(root)
        .status()
        .unwrap();
    fs::write(root.join("README.md"), "one\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "first"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    let _guard = DirGuard::new(root);
    let mut buf = Vec::new();
    let args = HotspotArgs {
        json: true,
        semantic: true,
        ..Default::default()
    };
    let expired = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    execute_hotspots_with_opts(
        args,
        HotspotRunOpts {
            overall_deadline_override: Some(expired),
            ..Default::default()
        },
        Some(&mut buf),
    )
    .expect("emit");
    let stdout = String::from_utf8_lossy(&buf);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect(&stdout);
    assert_eq!(v["completeness"]["stage"], "semantic");
    assert_eq!(v["completeness"]["scope"], "overall");
    assert_eq!(v["files"].as_array().map(Vec::len), Some(0));
}
