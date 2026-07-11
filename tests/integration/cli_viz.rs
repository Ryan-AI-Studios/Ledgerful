use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::init::execute_init;
use ledgerful::commands::viz::execute_viz;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_viz_generates_html() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let out_path = root.join("output.html");
    let result = execute_viz(Some(out_path.clone()), 50, 3, None, "graph".to_string());
    assert!(result.is_ok());
    // Viz produces HTML output
    assert!(out_path.exists(), "viz output file should exist");
    let content = fs::read_to_string(&out_path).unwrap_or_default();
    assert!(
        content.contains("<!DOCTYPE html>") || content.contains("<html"),
        "viz output should contain HTML content"
    );
}
