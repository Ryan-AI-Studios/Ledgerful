use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::init::execute_init;
use ledgerful::commands::update::execute_update;
use std::fs;
use tempfile::tempdir;

#[test]
fn update_dry_run_reports_available_version() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false).unwrap();

    // dry_run=true with --migrate should only print what would be done
    let result = execute_update(true, false, false, false, false, true, false);
    assert!(result.is_ok());
}
