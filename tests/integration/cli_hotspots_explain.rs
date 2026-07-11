use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::hotspots::compute_hotspot_explanation;
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use ledgerful::git::repo::open_repo;
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
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();

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
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();

    let explanation =
        compute_hotspot_explanation(&storage, "src/does_not_exist.rs", &repo).unwrap();

    assert_eq!(explanation.complexity, 0);
    assert_eq!(explanation.frequency, 0.0);
}
