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
