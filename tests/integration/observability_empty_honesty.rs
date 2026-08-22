//! 0215 — observability empty taxonomy honesty
//! (`noMatches` / disk-without-ingest `noIndexedData` / `cleanDiff` / dirty).

use serial_test::serial;

use crate::common::{DirGuard, git_add_and_commit, git_add_and_commit_if_dirty, setup_git_repo};
use ledgerful::commands::init::execute_init;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const OLD_LIE: &str =
    "No observability data found. Run `ledgerful index --analyze-graph` to populate.";

fn fixture_slo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("observability")
        .join("dogfood_slo.yaml")
}

fn new_git_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}\n").unwrap();
    git_add_and_commit(root, "initial");
    tmp
}

fn copy_openslo_fixture(root: &Path) {
    let src = fixture_slo();
    assert!(src.is_file(), "dogfood SLO fixture must exist at {src:?}");
    let dest_dir = root.join("observability");
    fs::create_dir_all(&dest_dir).unwrap();
    fs::copy(&src, dest_dir.join("dogfood_slo.yaml")).unwrap();
}

fn run_cli(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(dir)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("stdout was not JSON: {e}\n{stdout}"))
}

/// DoD-1 / 0215-A6 / 0215-B: populated graph, no `observability/` → `noMatches`.
#[test]
#[serial(cwd)]
fn test_engine_shaped_populated_graph_no_yaml_is_no_matches() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let (stdout, stderr, code) = run_cli(root, &["observability", "diff", "--json"]);
    assert_eq!(
        code, 0,
        "observability diff --json must succeed; stdout={stdout} stderr={stderr}"
    );
    let v = parse_json(&stdout);
    assert_eq!(
        v["emptyReason"].as_str(),
        Some("noMatches"),
        "engine-main / sparse no-YAML must be noMatches, got: {v}"
    );
    let message = v["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("observability/"),
        "NoMatches message must name observability/, got: {message}"
    );
    assert_ne!(
        message, OLD_LIE,
        "must not claim the graph is missing as the sole next step"
    );
    assert!(
        !message.contains("has not been built yet"),
        "must not claim the graph is unbuilt, got: {message}"
    );
    assert_eq!(
        v["indexedCount"].as_u64(),
        Some(0),
        "empty indexed==0 object must report indexedCount 0, got: {v}"
    );

    let (human, stderr, code) = run_cli(root, &["observability", "diff"]);
    assert_eq!(
        code, 0,
        "observability diff must succeed; stdout={human} stderr={stderr}"
    );
    assert!(
        !human.contains(OLD_LIE),
        "human stdout must not contain the analyze-graph-only lie, got: {human}"
    );
    assert!(
        !human.contains("0 other observability signal(s) not impacted"),
        "footer must be absent when indexed==0, got: {human}"
    );
    assert!(
        human.contains("observability/"),
        "human next-step must name observability/, got: {human}"
    );

    let (stdout, stderr, code) = run_cli(root, &["observability", "coverage", "--json"]);
    assert_eq!(
        code, 0,
        "observability coverage --json must succeed; stdout={stdout} stderr={stderr}"
    );
    let v = parse_json(&stdout);
    assert_eq!(
        v["emptyReason"].as_str(),
        Some("noMatches"),
        "coverage JSON on populated graph without YAML must be noMatches, got: {v}"
    );
    assert_ne!(
        v["emptyReason"].as_str(),
        Some("cleanDiff"),
        "coverage must never emit CleanDiff, got: {v}"
    );

    // 0215-C: human coverage Note is graph/OpenSLO, not the old SQLite LOG-pattern lie.
    // `run_cli` sets LEDGERFUL_NON_INTERACTIVE=1 so DX1 degrades read-only.
    let (human, stderr, code) = run_cli(root, &["observability", "coverage"]);
    assert_eq!(
        code, 0,
        "observability coverage must succeed non-interactively; stdout={human} stderr={stderr}"
    );
    assert!(
        human.contains("not source-code LOG patterns"),
        "coverage Note must distinguish graph nodes from LOG patterns, got: {human}"
    );
    assert!(
        !human.contains("stored in SQLite"),
        "coverage Note must not claim LOG patterns stored in SQLite, got: {human}"
    );
    assert!(
        !human.contains("shown in 'observability diff'"),
        "coverage Note must not claim LOG patterns are shown in observability diff, got: {human}"
    );
}

/// DoD-2 disk-without-ingest: YAML on disk, skip `--analyze-graph` → `noIndexedData`.
#[test]
#[serial(cwd)]
fn test_disk_without_ingest_is_no_indexed_data() {
    let tmp = new_git_repo();
    let root = tmp.path();
    copy_openslo_fixture(root);
    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let (stdout, stderr, code) = run_cli(root, &["observability", "diff", "--json"]);
    assert_eq!(
        code, 0,
        "observability diff --json must succeed; stdout={stdout} stderr={stderr}"
    );
    let v = parse_json(&stdout);
    assert_eq!(
        v["emptyReason"].as_str(),
        Some("noIndexedData"),
        "disk-present uningested OpenSLO must be noIndexedData, got: {v}"
    );
    let message = v["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("OpenSLO files on disk but not in the graph"),
        "disk-without-ingest must use the disk-first sentence, got: {message}"
    );
    assert_eq!(
        v["indexedCount"].as_u64(),
        Some(0),
        "uningested OpenSLO is not in the graph, got: {v}"
    );
    assert_ne!(
        message, OLD_LIE,
        "must not use the analyze-graph-only lie that ignores disk YAML, got: {message}"
    );
    assert!(
        message.contains("index --analyze-graph"),
        "disk-without-ingest message must name index --analyze-graph, got: {message}"
    );
    assert!(
        !message.to_lowercase().contains("add observability"),
        "disk-present YAML must not say add observability/ as if missing, got: {message}"
    );
    assert!(
        !message.contains("add them under 'observability/'"),
        "disk-present YAML must not say add YAML, got: {message}"
    );
}

/// 0215-A4 / 0146: ingested + committed clean tree → `cleanDiff` + `indexedCount >= 1`.
#[test]
#[serial(cwd)]
fn test_ingested_clean_tree_is_clean_diff() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    git_add_and_commit_if_dirty(root, "commit init artifacts");

    copy_openslo_fixture(root);
    git_add_and_commit(root, "add OpenSLO fixture");

    let (stdout, stderr, code) = run_cli(root, &["index", "--analyze-graph"]);
    assert_eq!(
        code, 0,
        "index --analyze-graph must succeed; stdout={stdout} stderr={stderr}"
    );

    let report_path = root
        .join(".ledgerful")
        .join("reports")
        .join("latest-impact.json");
    fs::create_dir_all(report_path.parent().expect("reports parent")).expect("mkdir reports");
    let seed = r#"{"schemaVersion":"v1","headHash":"SEED_MARKER_0215_A4","riskReasons":["seed-do-not-clobber"]}"#;
    fs::write(&report_path, seed).expect("seed latest-impact.json");
    let before = fs::read(&report_path).expect("read seeded report");

    let (stdout, stderr, code) = run_cli(root, &["observability", "diff", "--json"]);
    assert_eq!(
        code, 0,
        "observability diff --json must succeed; stdout={stdout} stderr={stderr}"
    );
    let v = parse_json(&stdout);
    assert_eq!(
        v["emptyReason"].as_str(),
        Some("cleanDiff"),
        "ingested clean tree must be cleanDiff, got: {v}"
    );
    let indexed = v["indexedCount"].as_u64().unwrap_or(0);
    assert!(
        indexed >= 1,
        "ingested OpenSLO must yield indexedCount >= 1, got: {v}"
    );

    let (human, stderr, code) = run_cli(root, &["observability", "diff"]);
    assert_eq!(
        code, 0,
        "observability diff must succeed; stdout={human} stderr={stderr}"
    );
    assert!(
        human.contains("No observability signals impacted by current diff."),
        "human CleanDiff copy missing, got: {human}"
    );

    assert!(
        report_path.is_file(),
        "seeded latest-impact.json must still exist after filter CLI"
    );
    let after = fs::read(&report_path).expect("read report after filter CLI");
    assert_eq!(
        before, after,
        "observability diff must not rewrite latest-impact.json \
         (filter path must not call execute_impact_silent / write_impact_report)"
    );

    fs::remove_file(&report_path).expect("remove seeded report");
    assert!(!report_path.exists(), "precondition: report absent");
    let (stdout, stderr, code) = run_cli(root, &["observability", "diff", "--json"]);
    assert_eq!(
        code, 0,
        "second clean-tree run must succeed; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !report_path.exists(),
        "observability diff must not create latest-impact.json when absent"
    );
}

/// Dirty YAML (after commit-init): non-empty `changed`, no `emptyReason`.
#[test]
#[serial(cwd)]
fn test_dirty_yaml_is_non_empty_changed() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    git_add_and_commit_if_dirty(root, "commit init artifacts");

    copy_openslo_fixture(root);
    git_add_and_commit(root, "add OpenSLO fixture");

    let (stdout, stderr, code) = run_cli(root, &["index", "--analyze-graph"]);
    assert_eq!(
        code, 0,
        "index --analyze-graph must succeed; stdout={stdout} stderr={stderr}"
    );

    let yaml_path = root.join("observability").join("dogfood_slo.yaml");
    let mut yaml = fs::read_to_string(&yaml_path).unwrap();
    yaml.push('\n');
    fs::write(&yaml_path, yaml).unwrap();

    let (stdout, stderr, code) = run_cli(root, &["observability", "diff", "--json"]);
    assert_eq!(
        code, 0,
        "observability diff --json must succeed; stdout={stdout} stderr={stderr}"
    );
    let v = parse_json(&stdout);
    let changed = v["changed"]
        .as_array()
        .unwrap_or_else(|| panic!("expected changed array, got: {v}"));
    assert!(
        !changed.is_empty(),
        "dirty OpenSLO YAML must produce non-empty changed, got: {v}"
    );
    assert!(
        changed.iter().any(|item| matches!(
            item["category"].as_str(),
            Some("slo") | Some("metric") | Some("alert")
        )),
        "dirty changed items must include an slo/metric/alert category, got: {v}"
    );
    assert!(
        v.get("emptyReason").is_none(),
        "non-empty changed must not include emptyReason, got: {v}"
    );
}
