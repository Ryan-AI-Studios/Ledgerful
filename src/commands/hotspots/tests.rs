use super::explain::complexity_for_entity_path;
use super::list::{omitted_hotspots_footer, wrap_hotspots_list_json};
use super::trend::{
    TrendMode, TrendRow, build_trend_summary, compute_history_available, format_trend_ts,
    render_hotspot_trend_table, render_trend_summary_table, resolve_trend_mode, trend_file_json,
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
        rendered.contains("Timestamp") && rendered.contains("File") && rendered.contains("Score"),
        "expected headers, got:\n{rendered}"
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
fn render_trend_summary_table_has_score_header_and_em_dash_prior() {
    let summary = build_trend_summary(
        &[row("solo.rs", "2026-06-21T15:00:00+00:00", 1.0, "abc")],
        20,
    );
    let rendered = render_trend_summary_table(&summary);
    assert!(
        rendered.contains("Score") && rendered.contains("Prior"),
        "expected summary headers, got:\n{rendered}"
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
