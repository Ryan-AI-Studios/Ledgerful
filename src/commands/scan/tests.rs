use super::execute::{
    changes_include_observability_config, compute_pr_scan_affected_flows,
    compute_pr_scan_test_gaps, graph_is_missing_or_stale, maybe_auto_analyze_graph,
    should_print_scan_report_honesty, should_skip_auto_analyze_graph, test_hooks,
};
use super::git::{is_missing_base_commit_error, parse_pr_range, resolve_commit_oid};
use super::validate::{
    validate_blast_depth_requires_impact, validate_mode_requires_impact, validate_scan_args,
    validate_timeout_requires_impact,
};
use crate::cli::args::ScanImpactMode;
use crate::git::{ChangeType, FileChange, RepoSnapshot};
use crate::state::layout::Layout;
use crate::state::migrations::get_migrations;
use crate::state::storage::StorageManager;
use chrono::Utc;
use rusqlite::Connection;
use std::path::PathBuf;

#[test]
fn scan_pr_human_uses_range_label_not_working_tree() {
    let src = include_str!("execute.rs");
    assert!(
        src.contains("\"Range:\""),
        "PR human summary must label treeClean as Range"
    );
    assert!(
        !src.contains("\"Working tree:\""),
        "PR human summary must not label range emptiness as Working tree"
    );
}

/// 0174: scan RO honesty must not prefix machine stdout.
#[test]
fn scan_report_honesty_human_only_gate() {
    assert!(should_print_scan_report_honesty(false, false));
    assert!(!should_print_scan_report_honesty(true, false));
    assert!(!should_print_scan_report_honesty(false, true));
    assert!(!should_print_scan_report_honesty(true, true));
}

#[test]
fn resolve_commit_oid_rejects_option_like_ref() {
    let err = resolve_commit_oid(std::path::Path::new("."), "--output=evil")
        .expect_err("option-like ref must fail before git option parse");
    let msg = format!("{err}");
    assert!(
        msg.contains("must not start with") || msg.contains("option-like"),
        "unexpected error: {msg}"
    );
}

#[test]
fn resolve_commit_oid_rejects_empty() {
    assert!(resolve_commit_oid(std::path::Path::new("."), "   ").is_err());
}

#[test]
fn observability_config_patterns_match_expected_files() {
    let changes = vec![
        FileChange {
            path: PathBuf::from("observability/OpenSLO.yaml"),
            change_type: ChangeType::Modified,
            is_staged: true,
        },
        FileChange {
            path: PathBuf::from("config/otel-collector.yaml"),
            change_type: ChangeType::Modified,
            is_staged: true,
        },
    ];
    assert!(changes_include_observability_config(&changes));

    let non_obs_changes = vec![FileChange {
        path: PathBuf::from("src/main.rs"),
        change_type: ChangeType::Modified,
        is_staged: true,
    }];
    assert!(!changes_include_observability_config(&non_obs_changes));
}

#[test]
fn graph_staleness_detects_empty_storage() {
    let conn = Connection::open_in_memory().unwrap();
    let mut conn = conn;
    get_migrations().to_latest(&mut conn).unwrap();
    let storage = StorageManager::init_from_conn(conn);

    assert!(graph_is_missing_or_stale(&storage, u64::MAX));
}

/// 0034 / 0189 DoD-3: empty changes never reach write-mode graph analysis.
#[test]
fn maybe_auto_analyze_graph_empty_changes_is_noop() {
    use crate::config::model::Config;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let root =
        camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp path");
    let layout = Layout::new(&root);
    let config = Config::default();

    let opened = maybe_auto_analyze_graph(&[], tmp.path(), &config, &layout)
        .expect("empty changes must be a no-op");
    assert!(
        opened.is_none(),
        "empty changes must not open write storage"
    );
    assert!(
        !layout.state_subdir().join("ledger.db").exists(),
        "empty changes must not open write storage / create ledger.db"
    );
}

/// 0189 DoD-3: obs + StalePopulated + non-empty changes still extract once (Run).
#[test]
fn maybe_auto_analyze_graph_stale_populated_obs_change_runs_once() {
    use crate::commands::index::{IndexArgs, execute_index};
    use crate::config::model::Config;
    use crate::tests::DirGuard;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let root =
        camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp path");

    let git_init = std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .expect("git init");
    assert!(
        git_init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&git_init.stderr)
    );
    for (key, value) in [("user.name", "Test"), ("user.email", "test@test.com")] {
        let cfg = std::process::Command::new("git")
            .args(["config", key, value])
            .current_dir(tmp.path())
            .output()
            .unwrap_or_else(|_| panic!("git config {key}"));
        assert!(cfg.status.success(), "git config {key} failed");
    }

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src").join("lib.rs"),
        "fn helper() {}\nfn caller() { helper(); }\n",
    )
    .unwrap();

    let layout = Layout::new(&root);
    layout.ensure_state_dir().unwrap();
    let _guard = DirGuard::new(tmp.path());
    execute_index(IndexArgs {
        full: true,
        ..Default::default()
    })
    .expect("index --full");

    let db_path = layout.state_subdir().join("ledger.db");
    let n = {
        let conn = Connection::open(db_path.as_std_path()).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM structural_edges", [], |r| r.get(0))
            .unwrap();
        assert!(n > 0, "fixture must produce native edges, got {n}");
        let extra_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_symbols WHERE symbol_name = 'extra_0189'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            extra_before, 0,
            "extra_0189 must be absent after the initial --full index"
        );
        let stale_at = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        conn.execute("UPDATE project_files SET last_indexed_at = ?1", [&stale_at])
            .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO index_metadata (key, value) VALUES ('last_indexed_at', ?1)",
            [&stale_at],
        )
        .unwrap();
        n
    };

    // Mutate after --full so a no-op SqliteExtractPolicy::Run cannot pass.
    std::fs::write(
        root.join("src").join("lib.rs"),
        "fn helper() {}\nfn caller() { helper(); }\nfn extra_0189() { helper(); }\n",
    )
    .unwrap();

    std::fs::create_dir_all(root.join("observability")).unwrap();
    std::fs::write(
        root.join("observability").join("slo.yaml"),
        "slo: fixture\n",
    )
    .unwrap();

    let config = Config::default();
    let storage = StorageManager::open_read_only(&layout).expect("open indexed storage");
    assert!(
        graph_is_missing_or_stale(&storage, config.index.stale_threshold_days),
        "backdated index must be StalePopulated so scan takes the Run path"
    );

    let changes = vec![FileChange {
        path: PathBuf::from("observability/slo.yaml"),
        change_type: ChangeType::Modified,
        is_staged: true,
    }];

    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(BufWriter(buf.clone()))
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        maybe_auto_analyze_graph(&changes, tmp.path(), &config, &layout)
            .expect("obs + stale populated must run graph analysis");
    });
    drop(storage);

    let (n_after, extra_after) = {
        let conn = Connection::open(db_path.as_std_path()).unwrap();
        let n_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM structural_edges", [], |r| r.get(0))
            .unwrap();
        let extra_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_symbols WHERE symbol_name = 'extra_0189'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        (n_after, extra_after)
    };
    assert!(
        n_after > 0,
        "scan Run must leave native edges, got {n_after}"
    );
    assert!(
        n_after < 2 * n.max(2),
        "scan Run must land one builder pass, not 2× (before={n} after={n_after})"
    );
    assert!(
        extra_after > 0,
        "scan Run must extract extra_0189; a no-op SqliteExtractPolicy::Run leaves this 0"
    );

    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let hits = logs.matches("Call graph build complete").count();
    assert_eq!(
        hits, 1,
        "expected exactly one Call graph build complete during scan Run, got {hits}: {logs}"
    );
}

#[test]
fn graph_freshness_respects_threshold() {
    let conn = Connection::open_in_memory().unwrap();
    let mut conn = conn;
    get_migrations().to_latest(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES (?1, ?2, ?3)",
        ("src/lib.rs", "OK", Utc::now().to_rfc3339()),
    )
    .unwrap();
    let storage = StorageManager::init_from_conn(conn);

    assert!(!graph_is_missing_or_stale(&storage, 3));
}

#[test]
fn pr_test_gaps_unavailable_without_db_does_not_create_state() {
    use crate::impact::enrichment::test_gaps::TestGapsStatus;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    // Do NOT create .ledgerful or ledger.db
    let snapshot = RepoSnapshot {
        head_hash: Some("abc".into()),
        branch_name: Some("feature".into()),
        is_clean: false,
        changes: vec![FileChange {
            path: PathBuf::from("src/lib.rs"),
            change_type: ChangeType::Modified,
            is_staged: true,
        }],
    };
    let gaps = compute_pr_scan_test_gaps(&layout, &snapshot);
    assert_eq!(gaps.status, TestGapsStatus::Unavailable);
    // Soft-open must not create .ledgerful
    assert!(
        !layout.state_dir.exists(),
        ".ledgerful must not be created by PR soft-open"
    );
    assert!(!layout.state_subdir().join("ledger.db").exists());
}

#[test]
fn pr_affected_flows_unavailable_without_db_does_not_create_state() {
    use crate::impact::enrichment::affected_flows::AffectedFlowsStatus;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    let snapshot = RepoSnapshot {
        head_hash: Some("abc".into()),
        branch_name: Some("feature".into()),
        is_clean: false,
        changes: vec![FileChange {
            path: PathBuf::from("src/lib.rs"),
            change_type: ChangeType::Modified,
            is_staged: true,
        }],
    };
    let flows = compute_pr_scan_affected_flows(&layout, &snapshot);
    assert_eq!(flows.status, AffectedFlowsStatus::Unavailable);
    assert!(
        !layout.state_dir.exists(),
        ".ledgerful must not be created by PR soft-open"
    );
    assert!(!layout.state_subdir().join("ledger.db").exists());
}

#[test]
fn parse_pr_range_three_dot() {
    let (base, head, git_range) = parse_pr_range("main...HEAD").unwrap();
    assert_eq!(base, "main");
    assert_eq!(head, "HEAD");
    assert_eq!(git_range, "main...HEAD");
}

#[test]
fn parse_pr_range_two_dot_normalizes_to_three_dot() {
    let (base, head, git_range) = parse_pr_range("main..HEAD").unwrap();
    assert_eq!(base, "main");
    assert_eq!(head, "HEAD");
    assert_eq!(git_range, "main...HEAD");
}

#[test]
fn parse_pr_range_bare_base_defaults_head_to_three_dot() {
    let (base, head, git_range) = parse_pr_range("main").unwrap();
    assert_eq!(base, "main");
    assert_eq!(head, "HEAD");
    assert_eq!(git_range, "main...HEAD");
}

#[test]
fn parse_pr_range_rejects_empty_base() {
    let err = parse_pr_range("...HEAD").unwrap_err().to_string();
    assert!(err.contains("empty base ref"));
}

#[test]
fn parse_pr_range_rejects_empty_head() {
    let err = parse_pr_range("main..").unwrap_err().to_string();
    assert!(err.contains("empty head ref"));
}

#[test]
fn parse_pr_range_rejects_empty_range() {
    let err = parse_pr_range("").unwrap_err().to_string();
    assert!(err.contains("must not be empty"));
}

#[test]
fn is_missing_base_commit_error_detects_known_phrases() {
    assert!(is_missing_base_commit_error(
        "fatal: Not a valid object name main"
    ));
    assert!(is_missing_base_commit_error("unknown revision: main"));
    assert!(is_missing_base_commit_error("bad revision 'main'"));
    assert!(is_missing_base_commit_error("does not exist: 'main'"));
    assert!(!is_missing_base_commit_error("some other git failure"));
}

#[test]
fn blast_depth_requires_impact_flag() {
    // Silent no-op banned (codex R1 P2 / 0106 DoD-9).
    let err = validate_blast_depth_requires_impact(false, &None, Some(2))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("--impact"),
        "expected require --impact, got {err}"
    );
    assert!(validate_blast_depth_requires_impact(true, &None, Some(2)).is_ok());
    assert!(validate_blast_depth_requires_impact(false, &None, None).is_ok());
    let pr_err = validate_blast_depth_requires_impact(false, &Some("main...HEAD".into()), Some(2))
        .unwrap_err()
        .to_string();
    assert!(
        pr_err.contains("--pr") || pr_err.contains("impact"),
        "expected pr rejection, got {pr_err}"
    );
}

#[test]
fn mode_docs_requires_impact_before_gitscan() {
    let err = validate_mode_requires_impact(false, Some(ScanImpactMode::Docs))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("--impact"),
        "expected --mode reject to mention --impact, got {err}"
    );
    assert!(validate_mode_requires_impact(true, Some(ScanImpactMode::Docs)).is_ok());
    assert!(validate_mode_requires_impact(false, None).is_ok());
}

#[test]
fn timeout_requires_impact() {
    let err = validate_timeout_requires_impact(false, Some(25))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("--impact"),
        "expected --timeout reject to mention --impact, got {err}"
    );
    assert!(validate_timeout_requires_impact(true, Some(25)).is_ok());
    assert!(validate_timeout_requires_impact(false, None).is_ok());
}

#[test]
#[allow(non_snake_case)]
fn scan_timeout__without_impact__errors() {
    timeout_requires_impact();
}

#[test]
fn scan_timeout_or_prospective_skips_auto_graph() {
    let src = include_str!("execute.rs");
    assert!(
        src.contains("should_skip_auto_analyze_graph(prospective, resolved_overall_budget)"),
        "timed/prospective scan must skip auto-graph via the resolved-budget helper"
    );
    assert!(
        !src.contains("if prospective || timeout.is_some_and(|s| s > 0)"),
        "auto-graph skip must not depend on the clap flag literal alone"
    );
}

#[test]
#[allow(non_snake_case)]
fn should_skip_auto_analyze_graph__prospective_or_resolved_budget() {
    assert!(should_skip_auto_analyze_graph(true, None));
    assert!(should_skip_auto_analyze_graph(true, Some(0)));
    assert!(should_skip_auto_analyze_graph(false, Some(25)));
    assert!(should_skip_auto_analyze_graph(false, Some(15)));
    assert!(!should_skip_auto_analyze_graph(false, None));
    assert!(!should_skip_auto_analyze_graph(false, Some(0)));
}

#[test]
fn scan_working_tree_persist_arm_is_not_gated_on_timeout_is_some() {
    let src = include_str!("execute.rs");
    assert!(
        src.contains("should_skip_persist"),
        "working-tree / base_ref must stay on the persist-aware arm"
    );
    let persist_at = src
        .find("should_skip_persist")
        .expect("persist-aware arm must call should_skip_persist");
    let after = src.get(persist_at..).unwrap_or("");
    assert!(
        !after.contains("execute_impact_silent"),
        "silent execute_impact_silent* fallback must be gone after the persist-aware arm"
    );
    assert!(
        !src.contains("if timeout.is_some()"),
        "persist-aware arm must not be gated on clap timeout.is_some()"
    );
}

#[test]
fn scan_timeout_path_keeps_blast_depth_warning() {
    let src = include_str!("execute.rs");
    assert!(
        !src.contains("let _ = crate::impact::enrichment::blast::apply_cli_blast_depth"),
        "timeout path must not discard blast-depth warning"
    );
    assert!(
        src.contains("if let Some(note) = depth_note"),
        "timeout path must append blast-depth note to analysis_warnings"
    );
}

#[test]
fn docs_mode_paths_fixtures_auto_detect() {
    use crate::impact::lead::should_auto_detect_docs_mode;
    assert!(should_auto_detect_docs_mode([
        "docs/agent-output-contract.md"
    ]));
    assert!(should_auto_detect_docs_mode(["conductor.md"]));
    assert!(
        !should_auto_detect_docs_mode(["src/lib.rs", "docs/installation.md"]),
        "mixed src+docs must not enter docs mode"
    );
}

#[test]
fn json_out_ok_without_impact_summary_still_requires_impact() {
    // 0180: bare --json / --out allowed (gitScan); --summary still requires --impact.
    assert!(
        validate_scan_args(&None, &None, &None, false, false, true, &None).is_ok(),
        "json without impact must be allowed (gitScan)"
    );
    assert!(
        validate_scan_args(
            &None,
            &None,
            &None,
            false,
            false,
            false,
            &Some(std::path::PathBuf::from("out.json"))
        )
        .is_ok(),
        "out without impact must be allowed (gitScan file)"
    );
    let summary_err = validate_scan_args(&None, &None, &None, false, true, false, &None)
        .unwrap_err()
        .to_string();
    assert!(
        summary_err.contains("--summary") && summary_err.contains("--impact"),
        "expected summary requires impact, got {summary_err}"
    );
    assert!(
        !summary_err.contains("--format json") && !summary_err.contains("scan --pr"),
        "summary reject must not tip PR format, got {summary_err}"
    );
    assert!(
        validate_scan_args(&None, &None, &None, true, false, true, &None).is_ok(),
        "json with impact must be allowed"
    );
}

#[test]
fn scan_git_json_envelope_keys() {
    use crate::state::reports::{ScanGitJson, ScanReport};
    let report = ScanReport {
        head_hash: Some("abc".into()),
        branch_name: Some("main".into()),
        is_clean: true,
        changes: vec![],
        diff_summaries: vec![],
    };
    let env = ScanGitJson::from_report(&report);
    let v = serde_json::to_value(&env).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "gitScan");
    assert_eq!(v["isClean"], true);
    assert!(v["changes"].as_array().unwrap().is_empty());
    assert!(v["diffSummaries"].as_array().unwrap().is_empty());
}

/// Mirrors scan --impact --paths prospective branch: in-memory only, no
/// `write_impact_report` / `write_scan_report` clobber (0173-G).
#[test]
fn scan_prospective_impact_path_does_not_clobber_latest_impact() {
    use crate::commands::impact::{
        build_prospective_snapshot, compute_impact_from_snapshot_in_memory_with_mode,
        parse_prospective_paths,
    };
    use crate::state::reports::{
        LATEST_IMPACT_REPORT, LATEST_SCAN_REPORT, ScanReport, write_impact_report,
        write_scan_report,
    };
    use crate::state::storage::StorageManager;
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(root)
        .output()
        .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/exists.rs"), "fn x() {}").unwrap();
    fs::write(root.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(root)
        .output()
        .unwrap();

    let utf8 = camino::Utf8Path::from_path(root).unwrap();
    let layout = Layout::new(utf8);
    layout.ensure_state_dir().unwrap();
    let seed = crate::impact::packet::ImpactPacket {
        schema_version: "v1".to_string(),
        head_hash: Some("SEED_MARKER_0173_SCAN".to_string()),
        risk_reasons: vec!["seed-scan-do-not-clobber".to_string()],
        ..Default::default()
    };
    write_impact_report(&layout, &seed).unwrap();
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let before = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert!(before.contains("SEED_MARKER_0173_SCAN"));

    // Seed latest-scan.json with a marker; prospective must not clobber it.
    let scan_seed = ScanReport::from_snapshot(
        &RepoSnapshot {
            head_hash: Some("SEED_SCAN_0173".into()),
            branch_name: Some("main".into()),
            is_clean: true,
            changes: vec![],
        },
        vec![],
    );
    write_scan_report(&layout, &scan_seed).unwrap();
    let scan_path = layout.reports_dir().join(LATEST_SCAN_REPORT);
    let scan_before = fs::read_to_string(scan_path.as_std_path()).unwrap();
    assert!(scan_before.contains("SEED_SCAN_0173"));

    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = crate::config::model::Config::default();
    let parsed = parse_prospective_paths(&["src/exists.rs".into()]).unwrap();
    let snap = build_prospective_snapshot(root, &parsed).unwrap();
    // Same SoT as scan.rs prospective branch (no write_impact_report).
    let packet = compute_impact_from_snapshot_in_memory_with_mode(
        &storage,
        &config,
        root,
        snap,
        false,
        "prospective",
        parsed,
    )
    .unwrap();
    assert_eq!(packet.analysis_mode, "prospective");
    assert!(!packet.changes.is_empty());

    let after = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert_eq!(
        before, after,
        "scan prospective path must not rewrite latest-impact.json"
    );
    // Policy: prospective skips write_scan_report (execute_scan_with_opts).
    // Assert seed still present after in-memory compute (no accidental write helper).
    let scan_after = fs::read_to_string(scan_path.as_std_path()).unwrap();
    assert_eq!(
        scan_before, scan_after,
        "scan prospective path must not rewrite latest-scan.json"
    );
    let _ = storage.shutdown();
}

/// Dispatch proof for 0227 mtime freeze lives in
/// `tests/integration/cli_scan.rs` (`scan_docs_mode_does_not_clobber_latest_impact_mtime`
/// and auto-detect sibling). Overlay-only would not catch a write regression.

#[test]
fn prospective_snapshot_roots_paths_at_repo_root_not_cwd_subdir() {
    use crate::commands::impact::{build_prospective_snapshot, parse_prospective_paths};
    use crate::git::ChangeType;
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(root)
        .output()
        .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("src/exists.rs"), "fn x() {}").unwrap();
    fs::write(root.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(root)
        .output()
        .unwrap();

    let parsed = parse_prospective_paths(&["src/exists.rs".into()]).unwrap();
    // Resolve against repo root even if a subdir exists (caller must pass root).
    let snap = build_prospective_snapshot(root, &parsed).unwrap();
    assert_eq!(snap.changes.len(), 1);
    assert_eq!(snap.changes[0].change_type, ChangeType::Modified);
    // Wrong root (nested subdir) would mark the same path as Added/missing.
    let wrong = build_prospective_snapshot(&root.join("nested"), &parsed).unwrap();
    assert_eq!(wrong.changes[0].change_type, ChangeType::Added);
}

fn hermetic_dirty_scan_repo() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    use crate::state::reports::{LATEST_IMPACT_REPORT, write_impact_report};
    use std::fs;

    let dir = tempfile::tempdir().expect("tempdir");
    // Nest the git root so federated `scan_siblings` walks this empty parent
    // instead of the process Temp directory (38s+ on a busy Windows Temp).
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).expect("nested repo");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&root)
        .output()
        .expect("git init");
    for (key, value) in [("user.email", "t@t.com"), ("user.name", "T")] {
        let cfg = std::process::Command::new("git")
            .args(["config", key, value])
            .current_dir(&root)
            .output()
            .unwrap_or_else(|_| panic!("git config {key}"));
        assert!(cfg.status.success(), "git config {key} failed");
    }
    fs::create_dir_all(root.join("src")).expect("mkdir");
    fs::write(root.join("src/exists.rs"), "fn x() {}").expect("write");
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&root)
        .output()
        .expect("add");
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&root)
        .output()
        .expect("commit");
    fs::write(root.join("src/exists.rs"), "fn x() { 1 }").expect("dirty");

    let utf8 = camino::Utf8Path::from_path(&root).expect("utf8");
    let layout = Layout::new(utf8);
    layout.ensure_state_dir().expect("state");
    fs::write(
        layout.config_file(),
        "[hotspots]\nhistory_budget_secs = 1\n[federation]\nscan_timeout_secs = 1\nscan_file_budget = 1\n",
    )
    .expect("destress config");
    let seed = crate::impact::packet::ImpactPacket {
        schema_version: "v1".to_string(),
        head_hash: Some("SEED_MARKER_0424_SCAN_PERSIST".to_string()),
        risk_reasons: vec!["seed-scan-persist-do-not-clobber".to_string()],
        ..Default::default()
    };
    write_impact_report(&layout, &seed).expect("seed report");
    let report_path = layout
        .reports_dir()
        .join(LATEST_IMPACT_REPORT)
        .as_std_path()
        .to_path_buf();
    (dir, root, report_path)
}

#[test]
#[allow(non_snake_case)]
#[serial_test::serial(cwd)]
fn execute_scan_with_opts__omitted_timeout_and_cancel__does_not_write_latest_impact() {
    use crate::commands::scan::execute_scan_with_opts;
    use crate::tests::DirGuard;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let (_dir, root, report_path) = hermetic_dirty_scan_repo();
    let before = fs::read_to_string(&report_path).expect("read before");
    assert!(before.contains("SEED_MARKER_0424_SCAN_PERSIST"));

    let _cwd = DirGuard::new(&root);
    test_hooks::set_cancel(Some(Arc::new(AtomicBool::new(true))));
    let out = root.join("scan-out-0424-cancel.json");
    execute_scan_with_opts(
        true,
        false,
        true,
        Some(out),
        None,
        None,
        None,
        None,
        Vec::new(),
        false,
        None,
        false,
        None,
    )
    .expect("omitted timeout + cancel");
    test_hooks::set_cancel(None);

    let after = fs::read_to_string(&report_path).expect("read after");
    assert_eq!(
        before, after,
        "omitted --timeout with injected cancel must not rewrite latest-impact.json"
    );
}

#[test]
#[allow(non_snake_case)]
#[serial_test::serial(cwd)]
#[serial_test::serial(env)]
fn execute_scan_with_opts__timeout_zero_without_cancel__may_persist_latest_impact() {
    use crate::commands::helpers::{LEDGERFUL_STATE_DIR_ENV, get_layout};
    use crate::commands::scan::execute_scan_with_opts;
    use crate::config::load::load_config;
    use crate::impact::budget::HISTORY_BUDGET_ENV;
    use crate::tests::DirGuard;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    let (_dir, root, report_path) = hermetic_dirty_scan_repo();
    let before = fs::read_to_string(&report_path).expect("read before");
    assert!(before.contains("SEED_MARKER_0424_SCAN_PERSIST"));

    let _cwd = DirGuard::new(&root);
    let _state = TempEnv::remove(LEDGERFUL_STATE_DIR_ENV);
    let _hist = TempEnv::set(HISTORY_BUDGET_ENV, "1");
    let _offline = TempEnv::set("LEDGERFUL_NO_NETWORK", "1");
    let layout = get_layout().expect("temp layout");
    let loaded = load_config(&layout).expect("destress config");
    assert_eq!(
        loaded.hotspots.history_budget_secs, 1,
        "hermetic destress config must load"
    );
    assert_eq!(loaded.federation.scan_timeout_secs, 1);
    test_hooks::set_cancel(Some(Arc::new(AtomicBool::new(false))));
    let out = root.join("scan-out-0424-persist.json");
    execute_scan_with_opts(
        true,
        false,
        true,
        Some(out),
        None,
        None,
        None,
        None,
        Vec::new(),
        false,
        None,
        false,
        Some(0),
    )
    .expect("timeout 0 without cancel");

    let after = fs::read_to_string(&report_path).expect("read after");
    assert_ne!(
        before, after,
        "timeout 0 without cancel must be allowed to persist latest-impact.json"
    );
    assert!(
        !after.contains("SEED_MARKER_0424_SCAN_PERSIST"),
        "persisted packet must replace the 0424 seed marker"
    );
}
