use super::execute::{
    changes_include_observability_config, compute_pr_scan_affected_flows,
    compute_pr_scan_test_gaps, graph_is_missing_or_stale, maybe_auto_analyze_graph,
    should_print_scan_report_honesty,
};
use super::git::{is_missing_base_commit_error, parse_pr_range, resolve_commit_oid};
use super::validate::{
    validate_blast_depth_requires_impact, validate_mode_requires_impact, validate_scan_args,
};
use crate::cli::args::ScanImpactMode;
use crate::git::{ChangeType, FileChange, RepoSnapshot};
use crate::state::layout::Layout;
use crate::state::migrations::get_migrations;
use crate::state::storage::StorageManager;
use chrono::Utc;
use rusqlite::Connection;
use std::path::PathBuf;

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
    let conn = Connection::open_in_memory().unwrap();
    let mut conn = conn;
    get_migrations().to_latest(&mut conn).unwrap();
    let storage = StorageManager::init_from_conn(conn);
    let config = Config::default();

    maybe_auto_analyze_graph(&[], &storage, tmp.path(), &config, &layout)
        .expect("empty changes must be a no-op");
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
        maybe_auto_analyze_graph(&changes, &storage, tmp.path(), &config, &layout)
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
