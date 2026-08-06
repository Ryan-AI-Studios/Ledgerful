use camino::Utf8Path;
use ledgerful::commands::index::{IndexArgs, execute_index};
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, run_cli, setup_git_repo};

/// DoD-1: healthy `index --check` prints non-empty human stdout and exits 0.
/// Uses subprocess because the live path may call process::exit on other branches.
#[test]
fn test_index_check_healthy_prints_status() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn check_target() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    // Live path: `index --check` (not the deleted execute_index_check).
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["index", "--check"]);
    assert_eq!(code, 0, "healthy check must exit 0; stdout={stdout}");
    assert!(
        !stdout.trim().is_empty(),
        "DoD-1: healthy check must print non-empty stdout, got empty"
    );
    assert!(
        stdout.contains("Index")
            || stdout.contains("up to date")
            || stdout.contains("Files indexed"),
        "expected human status text, got: {stdout}"
    );
}

/// DoD-3: `index --check --json` entire stdout parses as a single JSON document.
#[test]
fn test_index_check_json_is_pure() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn check_json_target() {}",
    )
    .unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    let (stdout, stderr, code) = run_cli(tmp.path(), &["index", "--check", "--json"]);
    assert_eq!(code, 0, "check --json must exit 0; stdout={stdout}");
    // Assert by parsing the *entire* stdout — absence greps pass on empty output.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("DoD-3: entire stdout must parse as JSON: {e}; stdout={stdout}");
    });
    assert!(parsed.is_object(), "expected JSON object, got {parsed}");
    // 0149 DoD-4: success path must not print human Info on stderr.
    assert!(
        stderr.trim().is_empty(),
        "index --check --json success must have empty stderr, got: {stderr:?}"
    );
}

/// DoD-2: `index --check --strict` on a stale index prints reason on stderr and exits 1.
///
/// Must use `run_cli` (subprocess): the live path calls `process::exit(1)`, which
/// would kill an in-process test binary. Do not "simplify" this back in-process.
#[test]
fn test_index_check_strict_stale_exits_with_reason() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn strict_target() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    // Make index stale by editing a source file after indexing.
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn strict_target() {}\npub fn extra() {}",
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(tmp.path(), &["index", "--check", "--strict"]);
    assert_eq!(
        code, 1,
        "DoD-2: strict stale must exit 1; stdout={stdout}; stderr={stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "strict failure reason belongs on stderr, not stdout: {stdout}"
    );
    assert!(
        !stderr.trim().is_empty(),
        "DoD-2: strict stale must print reason on stderr"
    );
    let lower = stderr.to_lowercase();
    assert!(
        lower.contains("stale") || lower.contains("strict"),
        "stderr must name staleness/strict: {stderr}"
    );
}

/// 0128: content-dirty age-fresh index must never report FreshPopulated with
/// positive stale_files; assessment.stale_files == top-level stale_files.
#[test]
fn test_index_check_json_content_stale_not_fresh_populated() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn content_stale_target() {}",
    )
    .unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    // Edit after index → content-hash drift while last_indexed_at is today.
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn content_stale_target() {}\npub fn after_edit() {}",
    )
    .unwrap();

    let (stdout, _stderr, code) = run_cli(tmp.path(), &["index", "--check", "--json"]);
    assert_eq!(
        code, 0,
        "check --json (non-strict) must exit 0; stdout={stdout}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("entire stdout must parse as JSON: {e}; stdout={stdout}");
    });

    let top_stale = parsed["stale_files"]
        .as_u64()
        .expect("top-level stale_files");
    assert!(
        top_stale > 0,
        "edited file must produce content drift: {parsed}"
    );

    let assessment = parsed["assessment"]
        .as_object()
        .expect("assessment object present");
    let state = assessment["state"].as_str().unwrap_or("");
    assert_ne!(
        state, "FreshPopulated",
        "DoD-1: never FreshPopulated with stale_files > 0: {parsed}"
    );
    assert!(
        state == "ContentStalePopulated" || state.contains("Stale") || state.contains("Content"),
        "expected ContentStalePopulated (or stale variant), got {state}: {parsed}"
    );
    let assess_stale = assessment["stale_files"]
        .as_u64()
        .expect("assessment.stale_files");
    assert_eq!(
        assess_stale, top_stale,
        "DoD-2: assessment.stale_files must equal top-level stale_files"
    );
}

/// 0128: clean age-fresh index reports up to date with stale_files 0.
#[test]
fn test_index_check_json_clean_is_fresh_populated_zero_stale() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn clean_target() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    let (stdout, _stderr, code) = run_cli(tmp.path(), &["index", "--check", "--json"]);
    assert_eq!(code, 0, "check --json must exit 0; stdout={stdout}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("entire stdout must parse as JSON: {e}; stdout={stdout}");
    });

    assert_eq!(
        parsed["stale_files"].as_u64().unwrap_or(999),
        0,
        "clean tree must have top-level stale_files 0: {parsed}"
    );
    let assessment = &parsed["assessment"];
    assert_eq!(
        assessment["state"].as_str().unwrap_or(""),
        "FreshPopulated",
        "clean age-fresh must be FreshPopulated: {parsed}"
    );
    assert_eq!(
        assessment["stale_files"].as_u64().unwrap_or(999),
        0,
        "assessment.stale_files must be 0 when clean: {parsed}"
    );
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
