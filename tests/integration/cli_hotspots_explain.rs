use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::hotspots::compute_hotspot_explanation;
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use ledgerful::git::repo::open_repo;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

/// Regression test for CG-F16: `hotspots explain` used to report zeroed
/// complexity and frequency for a known hotspot because `HotspotQuery` was
/// built from `Default::default()`, leaving `commits` and `limit` at 0 (which
/// makes the git history walk break immediately and truncates results to
/// nothing) instead of routing through `exact_file` with real config values.
#[test]
fn test_hotspots_explain_reports_nonzero_metrics_for_known_hotspot() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn complex_fn(x: i32) -> i32 {\n    if x > 0 { x + 1 } else { x - 1 }\n}\n",
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    // Touch the file across several more commits so it accumulates real
    // git-history frequency (exercises the real `GixHistoryProvider`, not a
    // mock). This must happen before `execute_init` installs Ledgerful's own
    // git hooks, which would otherwise gate these commits on ledger state.
    for i in 1..=3 {
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "pub fn complex_fn(x: i32) -> i32 {{\n    if x > {i} {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit(root, &format!("touch {i}"));
    }

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    let repo_root = Utf8Path::from_path(root).unwrap();
    let repo = open_repo(repo_root.as_std_path()).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(&Layout::new(repo_root)).unwrap();

    let explanation = compute_hotspot_explanation(&storage, "src/lib.rs", &repo).unwrap();

    assert_eq!(explanation.normalized_entity, "src/lib.rs");
    assert!(
        explanation.complexity > 0,
        "expected non-zero complexity for an indexed function with a branch"
    );
    assert!(
        explanation.frequency > 0.0,
        "expected non-zero change frequency after 4 commits touching the file"
    );
    let breakdown = explanation
        .score_breakdown
        .as_ref()
        .expect("known hotspot must have a score breakdown");
    assert_eq!(
        explanation.complexity, breakdown.complexity as i32,
        "Metrics complexity must equal score-breakdown numerator when a hotspot row exists"
    );

    // Relative and absolute invocation forms must resolve identically.
    let absolute_entity = root.join("src").join("lib.rs");
    let explanation_abs =
        compute_hotspot_explanation(&storage, absolute_entity.to_str().unwrap(), &repo).unwrap();
    assert_eq!(
        explanation_abs.normalized_entity,
        explanation.normalized_entity
    );
    assert_eq!(explanation_abs.complexity, explanation.complexity);
    assert_eq!(explanation_abs.frequency, explanation.frequency);
}

#[test]
fn test_hotspots_explain_unknown_entity_returns_zero_without_error() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    let repo_root = Utf8Path::from_path(root).unwrap();
    let repo = open_repo(repo_root.as_std_path()).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(&Layout::new(repo_root)).unwrap();

    let explanation =
        compute_hotspot_explanation(&storage, "src/does_not_exist.rs", &repo).unwrap();

    assert_eq!(explanation.complexity, 0);
    assert_eq!(explanation.frequency, 0.0);
}

/// 0183-B3: explain complexity resolves `pkg.rs` → `pkg/mod.rs` when only the
/// latter is indexed with non-zero complexity metrics.
#[test]
fn test_hotspots_explain_complexity_resolves_file_form_to_mod_rs() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src/pkg")).unwrap();
    fs::write(
        root.join("src/pkg/mod.rs"),
        "pub fn complex_fn(x: i32) -> i32 {\n    if x > 0 { x + 1 } else { x - 1 }\n}\n",
    )
    .unwrap();
    git_add_and_commit(root, "initial");
    for i in 1..=3 {
        fs::write(
            root.join("src/pkg/mod.rs"),
            format!(
                "pub fn complex_fn(x: i32) -> i32 {{\n    if x > {i} {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit(root, &format!("touch {i}"));
    }

    // Hermetic seed: only mod.rs on project_files with explicit complexity.
    // No execute_init/index — complexity path is project_files SQL + B1 resolve.
    let repo_root = Utf8Path::from_path(root).unwrap();
    let layout = Layout::new(repo_root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) \
         VALUES (1, 'src/pkg/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols \
         (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
          cognitive_complexity, cyclomatic_complexity, last_indexed_at) \
         VALUES (1, 1, 'complex_fn', 'complex_fn', 'Function', 1, 12, 8, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    storage.shutdown().unwrap();

    let storage = StorageManager::open_read_only_sqlite_only(&layout).unwrap();
    let repo = open_repo(repo_root.as_std_path()).unwrap();

    let explanation = compute_hotspot_explanation(&storage, "src/pkg.rs", &repo).unwrap();
    assert_eq!(
        explanation.complexity, 12,
        "file-form path must resolve for project_files complexity"
    );

    // Exact path still works.
    let exact = compute_hotspot_explanation(&storage, "src/pkg/mod.rs", &repo).unwrap();
    assert_eq!(exact.complexity, 12);
}
