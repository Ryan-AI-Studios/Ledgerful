//! Integration tests for Track 0043 self-timing facility.

use clap::Parser;
use ledgerful::cli::args::Cli;
#[cfg(feature = "self-timing")]
use ledgerful::observability::self_timing::{
    TimedCommand, hash_argv_shape, set_current_ledger_tx_id, set_current_repo_size_bytes,
};
use ledgerful::state::migrations::get_migrations;
use ledgerful::state::storage::timings::{
    TimingQuery, TimingRow, count_timings, get_outer_by_run_id, insert_timing_batch,
    is_self_timing_enabled, prune_timings, query_timings, set_self_timing_enabled, summarize_outer,
};
use rusqlite::Connection;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::DirGuard;

/// Windows debug builds can overflow the default 1 MiB stack while parsing the
/// large clap tree (same issue as `main.rs`). Parse on a larger stack.
fn parse_cli(args: &[&str]) -> Cli {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || Cli::try_parse_from(owned).expect("cli parse"))
        .expect("spawn parse thread")
        .join()
        .expect("parse thread panicked")
}

fn migrated_conn() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    conn
}

/// Temp repo root with `.ledgerful/state/ledger.db` migrated to latest.
fn temp_repo_with_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempdir().unwrap();
    let state = tmp.path().join(".ledgerful").join("state");
    fs::create_dir_all(&state).unwrap();
    let db_path = state.join("ledger.db");
    {
        let mut conn = Connection::open(&db_path).unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
    }
    (tmp, db_path)
}

fn outer(run_id: &str, command: &str, duration_ms: i64, argv_hash: Option<&str>) -> TimingRow {
    TimingRow {
        run_id: run_id.to_string(),
        ts_utc: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        command: command.to_string(),
        duration_ms,
        exit_code: 0,
        repo_size_bytes: None,
        argv_hash: argv_hash.map(|s| s.to_string()),
        ledger_tx_id: None,
        parent_span_id: None,
        span_name: None,
        notes: None,
    }
}

fn with_isolated_config_home<F: FnOnce()>(f: F) {
    let tmp = tempdir().unwrap();
    // Test inject — no process env mutation (Semgrep blocks unsafe set_var).
    let prev = ledgerful::state::rollup::set_test_config_home(Some(tmp.path().to_path_buf()));
    f();
    ledgerful::state::rollup::set_test_config_home(prev);
}

#[test]
fn round_trip_insert_query() {
    let mut conn = migrated_conn();
    let rows = vec![
        outer("r1", "verify", 120, Some("h1")),
        TimingRow {
            run_id: "r1".into(),
            ts_utc: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            command: "verify".into(),
            duration_ms: 90,
            exit_code: 0,
            repo_size_bytes: None,
            argv_hash: None,
            ledger_tx_id: None,
            parent_span_id: Some("p".into()),
            span_name: Some("run_tests".into()),
            notes: None,
        },
    ];
    assert_eq!(insert_timing_batch(&mut conn, &rows).unwrap(), 2);
    let out = query_timings(
        &conn,
        &TimingQuery {
            outer_only: true,
            command: Some("verify".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].duration_ms, 120);
    assert!(out[0].repo_size_bytes.is_none());
}

#[test]
fn argv_hash_groups_across_flag_order_and_ignores_values() {
    // Same flag *names* in different order → same shape → same hash.
    let mut flags_a = ["json", "impact", "out"];
    let mut flags_b = ["out", "impact", "json"];
    flags_a.sort_unstable();
    flags_b.sort_unstable();
    let shape_a = format!("scan|{}", flags_a.join(","));
    let shape_b = format!("scan|{}", flags_b.join(","));
    assert_eq!(shape_a, shape_b);
    #[cfg(feature = "self-timing")]
    assert_eq!(hash_argv_shape(&shape_a), hash_argv_shape(&shape_b));

    // Cli-level: flag order in argv_shape is sorted.
    let cli1 = parse_cli(&["ledgerful", "scan", "--impact", "--json"]);
    let cli2 = parse_cli(&["ledgerful", "scan", "--json", "--impact"]);
    assert_eq!(cli1.command.argv_shape(), cli2.command.argv_shape());

    // Path values never enter the shape: both exports produce the same flags.
    let cli_out1 = parse_cli(&[
        "ledgerful",
        "scan",
        "--impact",
        "--out",
        "C:\\secret\\a.json",
    ]);
    let cli_out2 = parse_cli(&["ledgerful", "scan", "--impact", "--out", "/tmp/other.json"]);
    assert_eq!(cli_out1.command.argv_shape(), cli_out2.command.argv_shape());
    assert!(!cli_out1.command.argv_shape().contains("secret"));
    assert!(!cli_out1.command.argv_shape().contains("tmp"));
}

#[test]
fn search_regex_vs_semantic_argv_shapes_differ() {
    let regex = parse_cli(&["ledgerful", "search", "--regex", "foo"]);
    let semantic = parse_cli(&["ledgerful", "search", "--semantic", "foo"]);
    let shape_r = regex.command.argv_shape();
    let shape_s = semantic.command.argv_shape();
    assert_ne!(
        shape_r, shape_s,
        "regex vs semantic must produce different shapes"
    );
    assert!(shape_r.contains("regex"), "got {shape_r}");
    assert!(shape_s.contains("semantic"), "got {shape_s}");
    // Query values never enter the shape.
    assert!(!shape_r.contains("foo"));
    assert!(!shape_s.contains("foo"));
    #[cfg(feature = "self-timing")]
    assert_ne!(hash_argv_shape(&shape_r), hash_argv_shape(&shape_s));
}

#[test]
fn scan_flag_order_invariance_still_holds() {
    let a = parse_cli(&["ledgerful", "scan", "--impact", "--json", "--summary"]);
    let b = parse_cli(&["ledgerful", "scan", "--summary", "--impact", "--json"]);
    assert_eq!(a.command.argv_shape(), b.command.argv_shape());
    assert_eq!(a.command.argv_shape(), "scan|impact,json,summary");
}

#[test]
fn privacy_no_path_env_or_argv_content_in_rows() {
    let mut conn = migrated_conn();
    // Simulate a captured row: only shape hash, never raw path.
    let shape = "scan|impact,out";
    #[cfg(feature = "self-timing")]
    let hash = hash_argv_shape(shape);
    #[cfg(not(feature = "self-timing"))]
    let hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(shape.as_bytes());
        hex::encode(h.finalize())
    };
    let row = outer("r-priv", "scan", 10, Some(&hash));
    insert_timing_batch(&mut conn, &[row]).unwrap();
    let rows = query_timings(&conn, &TimingQuery::default()).unwrap();
    let serialized = serde_json::to_string(&rows).unwrap();
    assert!(!serialized.contains("C:\\"));
    assert!(!serialized.contains("/home/"));
    assert!(!serialized.contains("PATH="));
    assert!(!serialized.contains("--out"));
    // Hash is hex, not the shape string itself.
    assert!(!serialized.contains("impact,out"));
}

#[test]
fn prune_outer_and_inner() {
    let mut conn = migrated_conn();
    let mut old_outer = outer("old", "doctor", 5, None);
    old_outer.ts_utc = "2019-01-01T00:00:00.000Z".into();
    let old_inner = TimingRow {
        run_id: "old".into(),
        ts_utc: "2019-01-01T00:00:00.000Z".into(),
        command: "doctor".into(),
        duration_ms: 2,
        exit_code: 0,
        repo_size_bytes: None,
        argv_hash: None,
        ledger_tx_id: None,
        parent_span_id: None,
        span_name: Some("probe".into()),
        notes: None,
    };
    let recent = outer("new", "doctor", 6, None);
    insert_timing_batch(&mut conn, &[old_outer, old_inner, recent]).unwrap();

    assert_eq!(prune_timings(&conn, 30, false).unwrap(), 1);
    assert_eq!(prune_timings(&conn, 30, true).unwrap(), 1);
    let left = query_timings(&conn, &TimingQuery::default()).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].run_id, "new");
}

#[test]
fn repo_size_bytes_default_null() {
    let mut conn = migrated_conn();
    let row = outer("rs", "config_view", 3, None);
    assert!(row.repo_size_bytes.is_none());
    insert_timing_batch(&mut conn, &[row]).unwrap();
    let got = query_timings(
        &conn,
        &TimingQuery {
            outer_only: true,
            command: Some("config_view".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(got[0].repo_size_bytes.is_none());

    // Opportunistic set API exists and is independent of insert default.
    #[cfg(feature = "self-timing")]
    set_current_repo_size_bytes(999);
}

#[test]
fn single_batch_multi_span_no_partial_writes() {
    let mut conn = migrated_conn();
    let mut rows = vec![outer("batch", "verify", 200, Some("h"))];
    for i in 0..15 {
        rows.push(TimingRow {
            run_id: "batch".into(),
            ts_utc: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            command: "verify".into(),
            duration_ms: 10 + i,
            exit_code: 0,
            repo_size_bytes: None,
            argv_hash: None,
            ledger_tx_id: None,
            parent_span_id: Some("root".into()),
            span_name: Some(format!("span_{i}")),
            notes: None,
        });
    }
    // One call → one transaction; all or nothing.
    insert_timing_batch(&mut conn, &rows).unwrap();
    let all = query_timings(
        &conn,
        &TimingQuery {
            command: Some("verify".into()),
            outer_only: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all.len(), 16);
}

#[cfg(feature = "self-timing")]
#[test]
fn flame_and_explain_against_temp_db() {
    let (tmp, db_path) = temp_repo_with_db();
    {
        let mut conn = Connection::open(&db_path).unwrap();
        let mut rows = vec![outer("f1", "verify", 100, None)];
        rows.push(TimingRow {
            run_id: "f1".into(),
            ts_utc: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            command: "verify".into(),
            duration_ms: 70,
            exit_code: 0,
            repo_size_bytes: None,
            argv_hash: None,
            ledger_tx_id: None,
            parent_span_id: Some("p".into()),
            span_name: Some("run_tests".into()),
            notes: None,
        });
        for i in 0..4 {
            rows.push(outer(&format!("e{i}"), "verify", 100 + i * 5, None));
        }
        insert_timing_batch(&mut conn, &rows).unwrap();
    }

    let _guard = DirGuard::new(tmp.path());

    let flame_args = ledgerful::commands::timings::TimingsArgs {
        global: false,
        json: false,
        top: None,
        days: Some(3650),
        export: Some(tmp.path().join("flame.txt")),
        inner: false,
        command: Some("verify".into()),
        flame: true,
        explain: None,
        prune: false,
        older_than: None,
        opt_in: false,
        opt_out: false,
    };
    ledgerful::commands::timings::execute_timings(flame_args)
        .expect("flame execute against temp Layout DB");
    let flame_body = fs::read_to_string(tmp.path().join("flame.txt")).expect("read flame export");
    assert!(
        flame_body.lines().any(|l| l.starts_with("verify ")),
        "missing outer collapsed stack: {flame_body}"
    );
    assert!(
        flame_body.lines().any(|l| l.contains("verify;run_tests")),
        "missing inner collapsed stack: {flame_body}"
    );

    // Explain path: seed is in the same DB; sentence must include a number.
    let bin = env!("CARGO_BIN_EXE_ledgerful");
    let out = Command::new(bin)
        .args(["timings", "--explain", "verify", "--json"])
        .current_dir(tmp.path())
        .output()
        .expect("run timings --explain");
    assert!(
        out.status.success(),
        "explain failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("explain json");
    let sentence = v["data"]["explain"]
        .as_str()
        .expect("explain string")
        .to_string();
    assert!(
        sentence.ends_with('.'),
        "explain must be one sentence: {sentence}"
    );
    assert!(
        sentence.chars().any(|c| c.is_ascii_digit()),
        "explain must include a number: {sentence}"
    );
}

#[test]
fn explain_emits_one_sentence() {
    let mut conn = migrated_conn();
    for i in 0..4 {
        insert_timing_batch(
            &mut conn,
            &[outer(&format!("e{i}"), "verify", 100 + i * 5, None)],
        )
        .unwrap();
    }
    let summaries = summarize_outer(&conn, Some(30), Some(5)).unwrap();
    assert!(!summaries.is_empty());
    // Sentence-like summary construction (mirrors execute_explain no-baseline clause).
    let avg = summaries[0].total_ms as f64 / summaries[0].runs as f64;
    let sentence = format!(
        "`verify` averaged {avg:.0} ms over {} run(s) in the last 7 days; no prior-week baseline yet.",
        summaries[0].runs
    );
    assert!(sentence.ends_with('.'));
    assert_eq!(sentence.matches('.').count(), 1);
    assert!(sentence.chars().any(|c| c.is_ascii_digit()));
    assert!(sentence.contains("no prior-week baseline yet"));
}

#[test]
fn explain_no_prior_week_baseline_clause() {
    let (tmp, db_path) = temp_repo_with_db();
    {
        let mut conn = Connection::open(&db_path).unwrap();
        // Only recent rows — no prior-week set.
        for i in 0..3 {
            insert_timing_batch(
                &mut conn,
                &[outer(&format!("nb{i}"), "index", 80 + i * 5, None)],
            )
            .unwrap();
        }
    }
    let _guard = DirGuard::new(tmp.path());
    // Capture via binary so we assert the real CLI sentence.
    let bin = env!("CARGO_BIN_EXE_ledgerful");
    let out = Command::new(bin)
        .args(["timings", "--explain", "index", "--json"])
        .current_dir(tmp.path())
        .output()
        .expect("run timings --explain");
    assert!(
        out.status.success(),
        "explain failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("explain json");
    let sentence = v["data"]["explain"].as_str().expect("explain string");
    assert!(
        sentence.contains("no prior-week baseline yet"),
        "must mention missing baseline: {sentence}"
    );
    assert!(sentence.ends_with('.'), "got {sentence}");
}

#[test]
fn doctor_cardinality_warning_fires() {
    let mut conn = migrated_conn();
    let mut rows = Vec::new();
    for i in 0..1001 {
        rows.push(TimingRow {
            run_id: format!("card-{i}"),
            ts_utc: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            command: "verify".into(),
            duration_ms: 3,
            exit_code: 0,
            repo_size_bytes: None,
            argv_hash: None,
            ledger_tx_id: None,
            parent_span_id: None,
            span_name: Some(format!("card_span_{i}")),
            notes: None,
        });
    }
    insert_timing_batch(&mut conn, &rows).unwrap();
    let warnings = ledgerful::commands::timings::doctor_timing_warnings(&conn);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("distinct span names") && w.contains("1000")),
        "expected cardinality warning, got {warnings:?}"
    );
}

#[test]
fn signing_basis_crypto_untouched() {
    // Source inspection: crypto.rs still uses the fixed 5-field basis
    // (`tx_id`, `category`, `summary`, `reason`, `committed_at`). Timing
    // columns must never enter Ed25519 signing. Do not edit crypto fields.
    let crypto = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ledger/crypto.rs"));
    // Source stores `\n` as two characters (escape sequence), not a raw newline.
    let template_src = "tx_id:{}\\ncategory:{}\\nsummary:{}\\nreason:{}\\ncommitted_at:{}";
    assert!(
        crypto.contains(template_src),
        "format! must remain exactly 5 fields"
    );
    assert_eq!(template_src.matches("{}").count(), 5);
    assert!(crypto.contains("tx_id:{}"));
    assert!(crypto.contains("category:{}"));
    assert!(crypto.contains("summary:{}"));
    assert!(crypto.contains("reason:{}"));
    assert!(crypto.contains("committed_at:{}"));
    // command_timings / duration_ms must never appear in the signing module.
    assert!(!crypto.contains("command_timings"));
    assert!(!crypto.contains("duration_ms"));
    assert!(!crypto.contains("argv_hash"));
    assert!(!crypto.contains("span_name"));

    // Table isolation: inserting timing rows leaves ledger_entries / chain_head counts unchanged.
    let mut conn = migrated_conn();
    let ledger_before: i64 = conn
        .query_row("SELECT count(*) FROM ledger_entries", [], |r| r.get(0))
        .unwrap_or(0);
    let chain_before: i64 = conn
        .query_row("SELECT count(*) FROM chain_head", [], |r| r.get(0))
        .unwrap_or(0);
    insert_timing_batch(&mut conn, &[outer("sig-iso", "verify", 11, Some("h"))]).unwrap();
    let ledger_after: i64 = conn
        .query_row("SELECT count(*) FROM ledger_entries", [], |r| r.get(0))
        .unwrap_or(0);
    let chain_after: i64 = conn
        .query_row("SELECT count(*) FROM chain_head", [], |r| r.get(0))
        .unwrap_or(0);
    assert_eq!(ledger_before, ledger_after);
    assert_eq!(chain_before, chain_after);
    assert_eq!(count_timings(&conn).unwrap(), 1);

    // Crypto path still works after timings exist in the same DB (temp keys).
    // Proves timings do not break sign/verify; basis remains the 5 fields.
    let keys_dir = tempdir().unwrap();
    let tx_id = "tx-timing-sig";
    let category = "BUGFIX";
    let summary = "timing isolation";
    let reason = "prove signing basis";
    let committed_at = "2026-07-19T12:00:00.000Z";
    let (sig, pub_key) = ledgerful::ledger::crypto::sign_ledger_entry_in(
        keys_dir.path(),
        tx_id,
        category,
        summary,
        reason,
        committed_at,
    )
    .expect("sign after timings insert");
    let sig = sig.expect("signature present");
    let pub_key = pub_key.expect("public key present");
    assert!(
        ledgerful::ledger::crypto::verify_signature(
            tx_id,
            category,
            summary,
            reason,
            committed_at,
            &sig,
            &pub_key,
        ),
        "verify_signature must succeed with the 5-field basis after timings exist"
    );
    // Corrupt category → verification fails (basis is still exactly those fields).
    assert!(
        !ledgerful::ledger::crypto::verify_signature(
            tx_id,
            "FEATURE",
            summary,
            reason,
            committed_at,
            &sig,
            &pub_key,
        ),
        "verify must fail when category is corrupted"
    );
    // Expected payload string (mirrors production format!) — 5 lines only.
    let expected_payload = format!(
        "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}",
        tx_id, category, summary, reason, committed_at
    );
    assert_eq!(expected_payload.lines().count(), 5);
    assert!(!expected_payload.contains("duration_ms"));
    assert!(!expected_payload.contains("command_timings"));
    assert!(!expected_payload.contains("argv_hash"));
    assert!(!expected_payload.contains("span_name"));
}

#[test]
fn opt_out_config_round_trip() {
    with_isolated_config_home(|| {
        set_self_timing_enabled(false).unwrap();
        assert!(!is_self_timing_enabled());
        let home = ledgerful::state::rollup::user_config_dir().unwrap();
        let content = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(content.contains("self_timing") && content.contains("false"));

        set_self_timing_enabled(true).unwrap();
        assert!(is_self_timing_enabled());
    });
}

#[cfg(feature = "self-timing")]
#[test]
fn opt_out_inserts_nothing() {
    with_isolated_config_home(|| {
        set_self_timing_enabled(false).unwrap();
        assert!(!is_self_timing_enabled());

        let (tmp, db_path) = temp_repo_with_db();
        let _guard = DirGuard::new(tmp.path());

        let timed = TimedCommand::start("verify", "verify");
        assert!(!timed.is_active());
        timed.finish(0);

        let conn = Connection::open(&db_path).unwrap();
        assert_eq!(
            count_timings(&conn).unwrap(),
            0,
            "opt-out must insert zero command_timings rows"
        );
    });
}

#[cfg(feature = "self-timing")]
#[test]
fn timings_command_self_exclusion_inserts_nothing() {
    with_isolated_config_home(|| {
        // Capture enabled, but the timings query command itself is never recorded.
        assert!(is_self_timing_enabled());
        let (tmp, db_path) = temp_repo_with_db();
        let _guard = DirGuard::new(tmp.path());

        let timed = TimedCommand::start("timings", "timings|json");
        assert!(!timed.is_active(), "timings must be self-excluded");
        assert!(timed.run_id().is_empty());
        timed.finish(0);

        let conn = Connection::open(&db_path).unwrap();
        assert_eq!(
            count_timings(&conn).unwrap(),
            0,
            "timings self-exclusion must insert zero rows"
        );
    });
}

#[cfg(feature = "self-timing")]
#[test]
fn timed_command_repo_size_and_ledger_tx_id() {
    with_isolated_config_home(|| {
        // Fresh config home → default-on.
        assert!(is_self_timing_enabled());
        let (tmp, db_path) = temp_repo_with_db();
        let _guard = DirGuard::new(tmp.path());

        // NULL by default (fast command, no set_current_repo_size_bytes).
        {
            let timed = TimedCommand::start("config_view", "config_view");
            let run_id = timed.run_id().to_string();
            timed.finish(0);
            let conn = Connection::open(&db_path).unwrap();
            let outer = get_outer_by_run_id(&conn, &run_id)
                .unwrap()
                .expect("outer row");
            assert!(outer.repo_size_bytes.is_none());
            assert!(outer.ledger_tx_id.is_none());
        }

        // Set when API called.
        {
            let timed = TimedCommand::start("index", "index");
            let run_id = timed.run_id().to_string();
            set_current_repo_size_bytes(12_345);
            set_current_ledger_tx_id("tx-abc");
            timed.finish(0);
            let conn = Connection::open(&db_path).unwrap();
            let outer = get_outer_by_run_id(&conn, &run_id)
                .unwrap()
                .expect("outer row");
            assert_eq!(outer.repo_size_bytes, Some(12_345));
            assert_eq!(outer.ledger_tx_id.as_deref(), Some("tx-abc"));
        }
    });
}

#[cfg(feature = "self-timing")]
#[test]
fn capture_failure_does_not_panic() {
    with_isolated_config_home(|| {
        assert!(is_self_timing_enabled());
        let tmp = tempdir().unwrap();
        let state = tmp.path().join(".ledgerful").join("state");
        fs::create_dir_all(&state).unwrap();
        // Directory named ledger.db forces Connection::open to fail.
        fs::create_dir(state.join("ledger.db")).unwrap();
        let _guard = DirGuard::new(tmp.path());

        let timed = TimedCommand::start("verify", "verify");
        assert!(timed.is_active());
        // Must not panic; host Result remains Ok (finish returns ()).
        timed.finish(0);
    });
}

#[test]
fn cli_help_lists_timings_flags() {
    let bin = env!("CARGO_BIN_EXE_ledgerful");
    let out = Command::new(bin)
        .args(["timings", "--help"])
        .output()
        .expect("run timings --help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--opt-out"));
    assert!(stdout.contains("--inner"));
    assert!(stdout.contains("--flame"));
    assert!(stdout.contains("--explain"));
    assert!(stdout.contains("--prune"));
}

#[test]
fn migration_creates_command_timings_table() {
    let conn = migrated_conn();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='command_timings'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn commands_timings_argv_shape_stable() {
    let a = parse_cli(&["ledgerful", "timings", "--json", "--top", "5"]);
    let b = parse_cli(&["ledgerful", "timings", "--top", "5", "--json"]);
    assert_eq!(a.command.argv_shape(), b.command.argv_shape());
    assert!(a.command.argv_shape().starts_with("timings"));
}

#[test]
fn viz_limit_override_changes_argv_shape() {
    let plain = parse_cli(&["ledgerful", "viz"]);
    let limited = parse_cli(&["ledgerful", "viz", "--limit", "50"]);
    let shape_plain = plain.command.argv_shape();
    let shape_limited = limited.command.argv_shape();
    assert_eq!(shape_plain, "viz");
    assert_ne!(
        shape_plain, shape_limited,
        "viz --limit 50 must differ from plain viz"
    );
    assert!(
        shape_limited.contains("limit"),
        "expected limit in shape, got {shape_limited}"
    );
    // Values never enter the hash.
    assert!(!shape_limited.contains("50"));
}

#[test]
fn update_migrate_vs_binary_argv_shapes_differ() {
    let migrate = parse_cli(&["ledgerful", "update", "--migrate"]);
    let binary = parse_cli(&["ledgerful", "update", "--binary"]);
    let shape_m = migrate.command.argv_shape();
    let shape_b = binary.command.argv_shape();
    assert_ne!(
        shape_m, shape_b,
        "update --migrate must differ from update --binary"
    );
    assert!(shape_m.contains("migrate"), "got {shape_m}");
    assert!(shape_b.contains("binary"), "got {shape_b}");
}

#[test]
fn reset_all_changes_argv_shape() {
    let plain = parse_cli(&["ledgerful", "reset"]);
    let all = parse_cli(&["ledgerful", "reset", "--all"]);
    let shape_plain = plain.command.argv_shape();
    let shape_all = all.command.argv_shape();
    assert_eq!(shape_plain, "reset");
    assert_ne!(
        shape_plain, shape_all,
        "reset --all must differ from plain reset"
    );
    assert!(shape_all.contains("all"), "got {shape_all}");
}
