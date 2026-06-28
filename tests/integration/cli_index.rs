use camino::Utf8Path;
use ledgerful::commands::index::{IndexArgs, execute_index, execute_index_check};
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

/// Check mode on a missing index in an empty repo should report fresh
/// (no exit because there are no source files).
#[test]
fn test_index_check_missing_no_files() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    let result = execute_index_check(tmp.path(), 3, false, false);
    assert!(result.is_ok());
}

/// Check mode JSON output on an empty repo should return valid JSON.
#[test]
fn test_index_check_json_empty_repo() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    let result = execute_index_check(tmp.path(), 3, true, false);
    assert!(result.is_ok());
}

/// Semantic dry-run on a fresh repo should succeed and print a report.
#[test]
fn test_index_semantic_dry_run_smoke() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);

    let result = execute_index(IndexArgs {
        semantic_dry_run: Some(None),
        ..Default::default()
    });
    assert!(result.is_ok());
}

/// Docs mode with no config should gracefully skip.
#[test]
fn test_index_docs_no_config_skips() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    // ensure state dir exists so StorageManager::init can create ledger.db
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let result = execute_index(IndexArgs {
        docs: true,
        ..Default::default()
    });
    assert!(result.is_ok());
}

/// Mode-combination matrix: --semantic without --analyze-graph should
/// take the semantic standalone path (not the main pipeline).
#[test]
fn test_index_semantic_standalone_mode() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);

    // With no CozoDB storage this will fail at "CozoDB storage not initialized",
    // but that's expected and proves it entered the semantic path, not the main path.
    let result = execute_index(IndexArgs {
        semantic: true,
        analyze_graph: false,
        ..Default::default()
    });
    assert!(result.is_err());
}

/// Mode-combination matrix: --semantic --analyze-graph should fall through
/// to the main pipeline (not early-return from semantic standalone).
#[test]
fn test_index_semantic_with_analyze_graph_falls_through() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);

    // Ensure state dir exists so the main path gets past StorageManager::init.
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    // analyze_graph falls through to main path and completes successfully on
    // a minimal repo (unlike the semantic standalone path which needs CozoDB).
    let result = execute_index(IndexArgs {
        semantic: true,
        analyze_graph: true,
        ..Default::default()
    });
    assert!(result.is_ok(), "Main path should complete on minimal repo");
}

/// --auto-scip should gracefully fall back to native indexing if no toolchain
/// is found, instead of failing the entire command.
#[test]
#[allow(non_snake_case)]
fn test_index_auto_scip_graceful_fallback__slow() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}").unwrap();
    // Add a Cargo.toml so detection triggers for Rust
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"",
    )
    .unwrap();

    let _guard = DirGuard::from_utf8(root);

    // Ensure state dir exists so the main path gets past StorageManager::init.
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    // Even if rust-analyzer is missing, this should succeed by falling back.
    let result = execute_index(IndexArgs {
        auto_scip: true,
        ..Default::default()
    });
    assert!(
        result.is_ok(),
        "Auto-SCIP should fall back to native if binary is missing or generation fails"
    );
}
