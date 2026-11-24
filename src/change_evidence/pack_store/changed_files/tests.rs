use super::{ChangedFileCompleteness, ChangedFileKind, ChangedFileLimits, Observation, TreePaths};
use crate::change_evidence::{buffer::MemoryBudget, git::exact_oid};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("missing-fixture-config"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", "Fixture Operator")
        .env("GIT_COMMITTER_NAME", "Fixture Operator")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}: {:?}", args, result.stderr);
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

#[test]
fn changed_files_omit_only_changes_with_unrepresentable_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(
        root,
        &["init", "--quiet", "--object-format=sha1", "--template="],
    );
    fs::create_dir(root.join("docs folder")).unwrap();
    fs::write(root.join("docs folder/guide.md"), "first guide\n").unwrap();
    fs::write(root.join("café.txt"), "unique note\n").unwrap();
    fs::write(root.join("readme.md"), "first readme\n").unwrap();
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "first"]);
    let parent = exact_oid(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();

    fs::write(root.join("readme.md"), "second readme\n").unwrap();
    git(root, &["commit", "--quiet", "-am", "second"]);
    let middle = exact_oid(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();

    fs::write(root.join("docs folder/guide.md"), "second guide\n").unwrap();
    fs::write(root.join("readme.md"), "third readme\n").unwrap();
    fs::rename(root.join("café.txt"), root.join("note.txt")).unwrap();
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "third"]);
    let head = exact_oid(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();

    let interrupted = AtomicBool::new(false);
    let memory = MemoryBudget::new(4 * 1024 * 1024);
    let mut observation = Observation::open(
        root,
        Instant::now() + Duration::from_secs(10),
        &interrupted,
        4 * 1024 * 1024,
        10 * 1024 * 1024,
        &memory,
    )
    .unwrap();
    let limits = ChangedFileLimits {
        rows: 1,
        path_bytes: 100,
    };

    let complete = observation
        .observe_changed_files(middle, parent, limits)
        .unwrap();
    assert_eq!(complete.completeness, ChangedFileCompleteness::Complete);
    assert_eq!(complete.files()[0].path(), "readme.md");

    let partial = observation
        .observe_changed_files(head, middle, limits)
        .unwrap();
    assert_eq!(
        partial.completeness,
        ChangedFileCompleteness::Partial {
            projection_limited: false,
            paths_omitted: true,
            rewrites_unresolved: false,
        }
    );
    assert_eq!(partial.total, Some(3));
    assert_eq!(partial.omitted, Some(2));
    assert_eq!(partial.files().len(), 1);
    assert_eq!(partial.files()[0].kind, ChangedFileKind::Modified);
    assert_eq!(partial.files()[0].path(), "readme.md");

    let pair = observation.observe_commit_pair(head, middle).unwrap();
    assert_eq!(
        observation
            .walk_verified_tree(pair.head_tree, TreePaths::Strict)
            .err(),
        Some("unsupported-tree-path")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn changed_files_omit_case_aliases_and_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(
        root,
        &["init", "--quiet", "--object-format=sha1", "--template="],
    );
    git(root, &["config", "core.ignorecase", "false"]);
    fs::create_dir(root.join("Docs")).unwrap();
    fs::create_dir(root.join("docs")).unwrap();
    fs::write(root.join("Docs/guide.md"), "first guide\n").unwrap();
    fs::write(root.join("docs/notes.md"), "first notes\n").unwrap();
    fs::write(root.join("readme.md"), "first readme\n").unwrap();
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "first"]);
    let parent = exact_oid(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();

    fs::write(root.join("readme.md"), "second readme\n").unwrap();
    git(root, &["commit", "--quiet", "-am", "second"]);
    let middle = exact_oid(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();

    fs::write(root.join("Docs/guide.md"), "second guide\n").unwrap();
    fs::write(root.join("docs/notes.md"), "second notes\n").unwrap();
    fs::write(root.join("readme.md"), "third readme\n").unwrap();
    git(root, &["commit", "--quiet", "-am", "third"]);
    let head = exact_oid(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();

    let interrupted = AtomicBool::new(false);
    let memory = MemoryBudget::new(4 * 1024 * 1024);
    let mut observation = Observation::open(
        root,
        Instant::now() + Duration::from_secs(10),
        &interrupted,
        4 * 1024 * 1024,
        10 * 1024 * 1024,
        &memory,
    )
    .unwrap();
    let limits = ChangedFileLimits {
        rows: 10,
        path_bytes: 100,
    };

    let complete = observation
        .observe_changed_files(middle, parent, limits)
        .unwrap();
    assert!(complete.head_has_unsupported_checkout_path);
    assert_eq!(complete.completeness, ChangedFileCompleteness::Complete);
    assert_eq!(complete.files()[0].path(), "readme.md");

    let partial = observation
        .observe_changed_files(head, middle, limits)
        .unwrap();
    assert!(partial.head_has_unsupported_checkout_path);
    assert_eq!(
        partial.completeness,
        ChangedFileCompleteness::Partial {
            projection_limited: false,
            paths_omitted: true,
            rewrites_unresolved: false,
        }
    );
    assert_eq!(partial.total, Some(3));
    assert_eq!(partial.omitted, Some(2));
    assert_eq!(partial.files().len(), 1);
    assert_eq!(partial.files()[0].path(), "readme.md");

    let pair = observation.observe_commit_pair(head, middle).unwrap();
    assert_eq!(
        observation
            .walk_verified_tree(pair.head_tree, TreePaths::Strict)
            .err(),
        Some("tree-path-alias")
    );
}
