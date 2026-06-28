use camino::Utf8Path;
use ledgerful::commands::watch::execute_watch;
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

#[test]
fn test_watch_invalid_config_fails_visibly() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);

    let state_dir = root.join(".ledgerful");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(state_dir.join("config.toml"), "[watch]\ndebounce_ms = 0\n").unwrap();

    let err = execute_watch(100, false, false).unwrap_err();
    assert!(format!("{err:?}").contains("debounce_ms"));
}
