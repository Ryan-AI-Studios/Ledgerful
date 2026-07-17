use ledgerful::commands::scan::execute_scan;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};

fn git_cmd(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success());
}

fn run_pr_scan_json(dir: &std::path::Path, range: &str) -> (serde_json::Value, miette::Result<()>) {
    let _guard = DirGuard::new(dir);
    let out_path = dir.join("__pr_scan_output__.json");
    let result = execute_scan(
        false,
        false,
        false,
        Some(out_path.clone()),
        None,
        Some(range.into()),
        "json".into(),
    );
    let parsed = if out_path.exists() {
        let content = fs::read_to_string(&out_path).unwrap();
        serde_json::from_str(&content).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    (parsed, result)
}

#[test]
fn pr_scan_json_emits_schema_version_and_sorted_changes() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn f() {}").unwrap();
    git_add_and_commit(root, "add files");

    let (parsed, result) = run_pr_scan_json(root, "HEAD~1...HEAD");
    result.unwrap();

    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["baseRef"], "HEAD~1");
    assert_eq!(parsed["headRef"], "HEAD");
    assert!(parsed["headHash"].as_str().is_some());
    let changes = parsed["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    let paths: Vec<&str> = changes
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["Cargo.toml", "src/lib.rs"]);
    let risk = parsed["riskLevel"].as_str().unwrap();
    assert!(matches!(risk, "low" | "medium" | "high"));
}

#[test]
fn pr_scan_is_deterministic_except_generated_at() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "").unwrap();
    fs::write(root.join("src/b.rs"), "").unwrap();
    git_add_and_commit(root, "add files");

    let (first, _) = run_pr_scan_json(root, "HEAD~1...HEAD");
    let (second, _) = run_pr_scan_json(root, "HEAD~1...HEAD");

    // Remove volatile generatedAt before comparing.
    let mut a = first.clone();
    let mut b = second.clone();
    a.as_object_mut().unwrap().remove("generatedAt");
    b.as_object_mut().unwrap().remove("generatedAt");
    assert_eq!(
        a, b,
        "PR scan output should be deterministic except generatedAt"
    );
}

#[test]
fn pr_scan_risk_low_for_small_change_set() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/one.rs"), "").unwrap();
    git_add_and_commit(root, "add file");

    let (parsed, result) = run_pr_scan_json(root, "HEAD~1...HEAD");
    result.unwrap();

    assert_eq!(parsed["riskLevel"], "low");
    assert!(parsed["riskReasons"].as_array().unwrap().is_empty());
}

#[test]
fn pr_scan_risk_medium_for_ten_changes() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    fs::create_dir_all(root.join("src")).unwrap();
    for i in 0..10 {
        fs::write(root.join(format!("src/file{}.rs", i)), "").unwrap();
    }
    git_add_and_commit(root, "add files");

    let (parsed, result) = run_pr_scan_json(root, "HEAD~1...HEAD");
    result.unwrap();

    assert_eq!(parsed["riskLevel"], "medium");
    assert!(
        parsed["riskReasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().contains("10 files changed"))
    );
}

#[test]
fn pr_scan_risk_high_for_thirty_changes() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    fs::create_dir_all(root.join("src")).unwrap();
    for i in 0..30 {
        fs::write(root.join(format!("src/file{}.rs", i)), "").unwrap();
    }
    git_add_and_commit(root, "add files");

    let (parsed, result) = run_pr_scan_json(root, "HEAD~1...HEAD");
    result.unwrap();

    assert_eq!(parsed["riskLevel"], "high");
}

#[test]
fn pr_scan_risk_high_for_sensitive_path() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    fs::write(root.join("Cargo.toml"), "[package]").unwrap();
    git_add_and_commit(root, "add manifest");

    let (parsed, result) = run_pr_scan_json(root, "HEAD~1...HEAD");
    result.unwrap();

    assert_eq!(parsed["riskLevel"], "high");
    assert!(
        parsed["riskReasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().contains("sensitive path"))
    );
}

#[test]
fn pr_scan_detects_renamed_file() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("old.txt"), "old").unwrap();
    git_add_and_commit(root, "add old");

    fs::rename(root.join("old.txt"), root.join("new.txt")).unwrap();
    git_cmd(root, &["add", "-A"]);
    git_cmd(root, &["commit", "-m", "rename file"]);

    let (parsed, result) = run_pr_scan_json(root, "HEAD~1...HEAD");
    result.unwrap();

    let changes = parsed["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    let change = &changes[0];
    assert_eq!(change["changeType"], "renamed");
    assert_eq!(change["path"], "new.txt");
    assert_eq!(change["oldPath"], "old.txt");
}

#[test]
fn pr_scan_missing_base_commit_gives_fetch_depth_hint() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    let (parsed, result) = run_pr_scan_json(root, "nonexistent-ref-12345...HEAD");
    assert!(result.is_err(), "expected an error for missing base commit");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("fetch-depth: 0"),
        "expected fetch-depth hint, got: {err}"
    );
    assert_eq!(parsed, serde_json::Value::Null);
}

#[test]
fn pr_scan_with_impact_is_mutually_exclusive() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    let _guard = DirGuard::new(root);
    let result = execute_scan(
        true,
        false,
        false,
        None,
        None,
        Some("HEAD~1...HEAD".into()),
        "json".into(),
    );

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("mutually exclusive"),
        "expected mutual exclusion error, got: {err}"
    );
}

#[test]
fn pr_scan_json_out_writes_same_payload_to_file() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "").unwrap();
    git_add_and_commit(root, "add file");

    let out_path = root.join("pr-scan.json");
    {
        let _guard = DirGuard::new(root);
        execute_scan(
            false,
            false,
            false,
            Some(out_path.clone()),
            None,
            Some("HEAD~1...HEAD".into()),
            "json".into(),
        )
        .unwrap();
    }

    let file_content = fs::read_to_string(&out_path).unwrap();
    let file_parsed: serde_json::Value = serde_json::from_str(&file_content).unwrap();
    assert_eq!(file_parsed["baseRef"], "HEAD~1");
    assert_eq!(file_parsed["schemaVersion"], 1);
}

#[test]
#[serial_test::serial]
fn pr_scan_backward_compat_base_ref_impact_json_still_works() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "").unwrap();
    git_add_and_commit(root, "add file");

    let out_path = root.join("impact.json");
    {
        let _guard = DirGuard::new(root);
        execute_scan(
            true,
            false,
            true,
            Some(out_path.clone()),
            Some("HEAD~1".to_string()),
            None,
            "text".into(),
        )
        .unwrap();
    }

    let content = fs::read_to_string(&out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["schemaVersion"], "v1");
    assert!(parsed["changes"].is_array());
}

#[test]
fn pr_scan_rejects_empty_pr_range() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    let _guard = DirGuard::new(root);
    let result = execute_scan(
        false,
        false,
        false,
        None,
        None,
        Some("".into()),
        "json".into(),
    );

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("must not be empty"),
        "expected empty range error, got: {err}"
    );
}

#[test]
fn pr_scan_rejects_unknown_format() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    let _guard = DirGuard::new(root);
    let result = execute_scan(
        false,
        false,
        false,
        None,
        None,
        Some("main...HEAD".into()),
        "xml".into(),
    );

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unsupported --format"),
        "expected unsupported format error, got: {err}"
    );
}

#[test]
fn pr_scan_same_base_and_head_yields_empty_low_risk() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("base.txt"), "base content").unwrap();
    git_add_and_commit(root, "base commit");

    let (parsed, result) = run_pr_scan_json(root, "HEAD...HEAD");
    result.unwrap();

    assert_eq!(parsed["changeCount"], 0);
    assert_eq!(parsed["riskLevel"], "low");
    assert_eq!(parsed["treeClean"], true);
}

#[test]
#[serial_test::serial]
fn pr_scan_no_network_code_in_src() {
    use std::process::Command;

    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("rg")
        .args([
            "-n",
            "ureq|reqwest|tokio_tungstenite",
            repo_root.join("src").to_str().unwrap(),
        ])
        .output()
        .expect("ripgrep should be available");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Exclude pre-existing network code in unrelated modules (viz-server, LLM,
    // observability, etc.). This test verifies that the *scan --pr* code path
    // adds zero new network code.
    let scan_pr_related: Vec<&str> = stdout
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            (l.contains("src/commands/scan") || l.contains("src/commands/scan_pr"))
                && (l.contains("ureq") || l.contains("reqwest") || l.contains("tokio_tungstenite"))
        })
        .collect();
    assert!(
        scan_pr_related.is_empty(),
        "scan --pr code path must contain no network code, found: {:?}",
        scan_pr_related
    );
}
