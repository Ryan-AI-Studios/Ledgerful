use ledgerful::commands::scan::execute_scan;
use ledgerful::state::layout::Layout;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

fn git_cmd(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn scan_clean_tree_reports_no_changes() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    let _guard = DirGuard::new(root);

    let result = execute_scan(false, false, false, None, None, None, None);
    assert!(result.is_ok());

    let layout = Layout::new(root.to_string_lossy().as_ref());
    let report = fs::read_to_string(layout.reports_dir().join("latest-scan.json")).unwrap();
    assert!(report.contains("\"isClean\": true"));
}

#[test]
fn scan_dirty_tree_reports_changed_files() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    // Add untracked file
    fs::write(root.join("untracked.txt"), "new").unwrap();

    // Modify existing file
    fs::write(root.join("initial.txt"), "modified").unwrap();

    // Stage a change
    fs::write(root.join("staged.txt"), "staged").unwrap();
    git_cmd(root, &["add", "staged.txt"]);

    let _guard = DirGuard::new(root);

    let result = execute_scan(false, false, false, None, None, None, None);
    assert!(result.is_ok());

    let layout = Layout::new(root.to_string_lossy().as_ref());
    let report = fs::read_to_string(layout.reports_dir().join("latest-scan.json")).unwrap();
    assert!(report.contains("initial.txt"));
    assert!(report.contains("untracked.txt"));
}

#[test]
fn scan_detached_head_reports_detached_state() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_sha = String::from_utf8(output.stdout).unwrap().trim().to_string();

    git_cmd(root, &["checkout", &head_sha]);

    let _guard = DirGuard::new(root);

    let result = execute_scan(false, false, false, None, None, None, None);
    assert!(result.is_ok());
}

#[test]
fn test_scan_impact_out_writes_json_without_json_flag() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    fs::write(root.join("initial.txt"), "modified").unwrap();

    let out_path = root.join("impact.json");
    let _guard = DirGuard::new(root);

    execute_scan(true, false, false, Some(out_path.clone()), None, None, None).unwrap();

    let content = fs::read_to_string(out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["schemaVersion"], "v1");
    assert!(parsed["changes"].is_array());
}

#[test]
fn test_scan_out_requires_impact() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    let _guard = DirGuard::new(root);
    let error = execute_scan(
        false,
        false,
        false,
        Some("out.json".into()),
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("--impact"),
        "expected impact requirement error, got {error:?}"
    );
}

#[test]
fn test_scan_json_requires_impact() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    let _guard = DirGuard::new(root);
    let error = execute_scan(false, false, true, None, None, None, None).unwrap_err();
    assert!(
        error.to_string().contains("--impact"),
        "expected impact requirement error, got {error:?}"
    );
}

#[test]
fn test_scan_summary_requires_impact() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    let _guard = DirGuard::new(root);
    let error = execute_scan(false, true, false, None, None, None, None).unwrap_err();
    assert!(
        error.to_string().contains("--impact"),
        "expected impact requirement error, got {error:?}"
    );
}

#[test]
fn test_scan_impact_excludes_tracked_ignored() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful/config.toml"),
        "[watch]\nignore_patterns = [\"ignored.rs\"]\n",
    )
    .unwrap();

    fs::write(root.join("ignored.rs"), "// ignored content").unwrap();
    git_cmd(root, &["add", "ignored.rs"]);
    git_cmd(root, &["commit", "-m", "add ignored"]);
    fs::write(root.join("ignored.rs"), "// modified ignored content").unwrap();

    fs::write(root.join("normal.rs"), "// normal content").unwrap();
    git_cmd(root, &["add", "normal.rs"]);
    git_cmd(root, &["commit", "-m", "add normal"]);
    fs::write(root.join("normal.rs"), "// modified normal content").unwrap();

    let _guard = DirGuard::new(root);

    let result = execute_scan(true, false, false, None, None, None, None);
    assert!(result.is_ok());

    let layout = Layout::new(root.to_string_lossy().as_ref());
    let report = fs::read_to_string(layout.reports_dir().join("latest-scan.json")).unwrap();
    assert!(
        !report.contains("ignored.rs"),
        "Report should not contain ignored.rs under impact"
    );
    assert!(
        report.contains("normal.rs"),
        "Report should contain normal.rs"
    );
}

#[test]
fn test_scan_impact_proactive_guidance_clean_tree() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Working tree is clean"),
        "Expected output to indicate clean tree, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("ledgerful ledger status"),
        "Clean-tree scan should not suggest ledger status, got: {}",
        stdout
    );
}

#[test]
fn scan_with_base_ref_emits_changed_files() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    // Create initial commit so HEAD~1 exists
    fs::write(root.join("base.txt"), "base content").unwrap();
    git_cmd(root, &["add", "base.txt"]);
    git_cmd(root, &["commit", "-m", "base commit"]);

    // Commit the file we want to detect
    fs::write(root.join("tracked.txt"), "tracked content").unwrap();
    git_cmd(root, &["add", "tracked.txt"]);
    git_cmd(root, &["commit", "-m", "add tracked file"]);

    let out_path = root.join("impact.json");
    let _guard = DirGuard::new(root);

    execute_scan(
        true,
        false,
        false,
        Some(out_path.clone()),
        Some("HEAD~1".to_string()),
        None,
        None,
    )
    .unwrap();

    let content = fs::read_to_string(out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let changes = parsed["changes"].as_array().unwrap();
    let paths: Vec<&str> = changes.iter().filter_map(|c| c["path"].as_str()).collect();
    assert!(
        paths.iter().any(|p| p.contains("tracked.txt")),
        "expected tracked.txt in changed_files, got: {:?}",
        paths
    );
}

#[test]
fn scan_with_base_ref_empty_when_no_diff() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    let out_path = root.join("impact.json");
    let _guard = DirGuard::new(root);

    // HEAD...HEAD produces no diff
    execute_scan(
        true,
        false,
        false,
        Some(out_path.clone()),
        Some("HEAD".to_string()),
        None,
        None,
    )
    .unwrap();

    let content = fs::read_to_string(out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(
        parsed["changes"].as_array().unwrap().is_empty(),
        "expected changes to be empty for HEAD...HEAD diff, got: {:?}",
        parsed["changes"]
    );
}

#[test]
fn scan_with_base_ref_detects_deleted_file() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    // Create initial commit with the file to be deleted
    fs::write(root.join("to_delete.rs"), "fn main() {}").unwrap();
    git_cmd(root, &["add", "to_delete.rs"]);
    git_cmd(root, &["commit", "-m", "initial"]);

    // Delete the file and commit
    fs::remove_file(root.join("to_delete.rs")).unwrap();
    git_cmd(root, &["rm", "to_delete.rs"]);
    git_cmd(root, &["commit", "-m", "delete file"]);

    let out_path = root.join("out.json");
    let _guard = DirGuard::new(root);

    execute_scan(
        true,
        false,
        true,
        Some(out_path.clone()),
        Some("HEAD~1".to_string()),
        None,
        None,
    )
    .unwrap();

    let content = fs::read_to_string(&out_path).unwrap();
    let packet: serde_json::Value = serde_json::from_str(&content).unwrap();
    let changes = packet["changes"].as_array().unwrap();
    assert!(!changes.is_empty(), "expected at least one changed file");
    let deleted = changes.iter().find(|c| {
        c["path"]
            .as_str()
            .map(|p| p.contains("to_delete"))
            .unwrap_or(false)
    });
    assert!(deleted.is_some(), "expected to_delete.rs in changes");
    let status = deleted.unwrap()["status"].as_str().unwrap_or("");
    assert_eq!(status, "Deleted");
}
