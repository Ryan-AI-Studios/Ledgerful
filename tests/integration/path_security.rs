#![allow(non_snake_case)]

use camino::Utf8PathBuf;
use ledgerful::state::layout::Layout;
use ledgerful::util::path::{ensure_path_within_root, normalize_relative_path};
use path_clean::PathClean;
use proptest::prelude::*;
use std::path::Path;

/// Helper to verify that a returned relative path, when joined with the repo root
/// and cleaned, still starts with the repo root.
fn assert_returned_path_stays_within_root(repo_root: &Path, input: &str, relative: &str) {
    let resolved = repo_root.join(relative).clean();
    assert!(
        resolved.starts_with(repo_root),
        "Security violation: input '{}' resolved to '{}', which escaped repo root '{}'",
        input,
        resolved.display(),
        repo_root.display()
    );
}

/// Property: `normalize_relative_path` must never return a relative path that resolves
/// outside the repo root for any string input. Errors are expected for malformed/escaping inputs.
fn prop_path_never_escapes_root(repo_root: &Path, input: &str) {
    match normalize_relative_path(repo_root, input) {
        Ok(relative) => {
            assert_returned_path_stays_within_root(repo_root, input, &relative);
        }
        Err(err) => {
            assert!(
                err.contains("outside the repository root") || err.contains("Security violation"),
                "Expected an escaping-related error for '{}', got: {}",
                input,
                err
            );
        }
    }
}

fn run_path_never_escapes_root_strategy(strategy: impl Strategy<Value = String>) {
    let repo_root = tempfile::tempdir().unwrap();
    proptest!(ProptestConfig::with_cases(256), |(input in strategy)| {
        prop_path_never_escapes_root(repo_root.path(), &input);
    });
}

#[test]
fn normalize_relative_path__random_ascii__never_escapes_root() {
    run_path_never_escapes_root_strategy("[ -~]{0,128}");
}

#[test]
fn normalize_relative_path__dotdot_sequences__never_escapes_root() {
    let prefix_strategy = "[a-zA-Z0-9_]{0,10}";
    let suffix_strategy = "[a-zA-Z0-9_/.\\\\\\\\]{0,20}";
    let repo_root = tempfile::tempdir().unwrap();
    proptest!(
        ProptestConfig::with_cases(256),
        |(prefix in prefix_strategy, depth in 1usize..8, suffix in suffix_strategy)| {
            let traversal = (0..depth).map(|_| "../").collect::<String>();
            let input = format!("{}{}{}", prefix, traversal, suffix);
            prop_path_never_escapes_root(repo_root.path(), &input);
        }
    );
}

#[test]
fn normalize_relative_path__percent_encoded_traversal__never_escapes_root() {
    let prefix_strategy = "[a-zA-Z0-9_]{0,10}";
    let encoded_strategy = "%2e%2e%2f|%2e%2e/|%2e%2e%2f|..%2f|%2e%2e%5c|%2e%2e\\\\\\\\";
    let suffix_strategy = "[a-zA-Z0-9_/.\\\\\\\\]{0,20}";
    let repo_root = tempfile::tempdir().unwrap();
    proptest!(
        ProptestConfig::with_cases(256),
        |(prefix in prefix_strategy, encoded in encoded_strategy, repeats in 1usize..6, suffix in suffix_strategy)| {
            let traversal = encoded.repeat(repeats);
            let input = format!("{}{}{}", prefix, traversal, suffix);
            prop_path_never_escapes_root(repo_root.path(), &input);
        }
    );
}

#[test]
fn normalize_relative_path__mixed_separators__never_escapes_root() {
    let segment_strategy = "[a-zA-Z0-9_.]{1,15}";
    let separator_strategy = "/|\\\\\\\\";
    let repo_root = tempfile::tempdir().unwrap();
    proptest!(
        ProptestConfig::with_cases(256),
        |(segments in proptest::collection::vec(segment_strategy, 1..8), separators in proptest::collection::vec(separator_strategy, 1..8))| {
            let mut input = String::new();
            for (i, segment) in segments.iter().enumerate() {
                if i > 0 {
                    let sep_index = (i - 1) % separators.len();
                    input.push_str(&separators[sep_index]);
                }
                input.push_str(segment);
            }
            prop_path_never_escapes_root(repo_root.path(), &input);
        }
    );
}

#[test]
fn normalize_relative_path__null_bytes__never_escapes_root() {
    let before_strategy = "[a-zA-Z0-9_/.\\\\\\\\]{0,30}";
    let after_strategy = "[a-zA-Z0-9_/.\\\\\\\\]{0,30}";
    let repo_root = tempfile::tempdir().unwrap();
    proptest!(
        ProptestConfig::with_cases(256),
        |(before in before_strategy, after in after_strategy)| {
            let mut input = before;
            input.push('\0');
            input.push_str(&after);
            prop_path_never_escapes_root(repo_root.path(), &input);
        }
    );
}

#[test]
fn normalize_relative_path__long_components__never_escapes_root() {
    let component_strategy = "[a-zA-Z0-9_]{200,300}";
    let tail_strategy = "[a-zA-Z0-9_/.\\\\\\\\]{0,40}";
    let repo_root = tempfile::tempdir().unwrap();
    proptest!(
        ProptestConfig::with_cases(256),
        |(component in component_strategy, tail in tail_strategy)| {
            let input = format!("{}{}", component, tail);
            prop_path_never_escapes_root(repo_root.path(), &input);
        }
    );
}

#[test]
fn normalize_relative_path__absolute_paths__never_escapes_root() {
    let unix_strategy = "(/|/etc/|/home/|/var/)[a-zA-Z0-9_/.]{0,40}";
    let windows_strategy = "(C:\\\\|D:\\\\|\\\\\\\\server\\\\share\\\\)[a-zA-Z0-9_\\\\.]{0,40}";
    let repo_root = tempfile::tempdir().unwrap();
    proptest!(
        ProptestConfig::with_cases(256),
        |(unix in unix_strategy, windows in windows_strategy)| {
            prop_path_never_escapes_root(repo_root.path(), &unix);
            prop_path_never_escapes_root(repo_root.path(), &windows);
        }
    );
}

#[test]
fn normalize_relative_path__empty_string__never_escapes_root() {
    let repo_root = tempfile::tempdir().unwrap();
    prop_path_never_escapes_root(repo_root.path(), "");
}

#[cfg(unix)]
#[test]
fn ensure_path_within_root__symlink_escape__rejected() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    std::fs::create_dir(&repo_root).unwrap();

    let outside_dir = temp.path().join("outside");
    std::fs::create_dir(&outside_dir).unwrap();
    let outside_file = outside_dir.join("secret.txt");
    std::fs::write(&outside_file, "outside").unwrap();

    let symlink_path = repo_root.join("link");
    std::os::unix::fs::symlink(&outside_file, &symlink_path).unwrap();

    let result = ensure_path_within_root(&repo_root, &symlink_path);
    assert!(
        result.is_err(),
        "Expected symlink escape to be rejected, but got Ok"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("symlink") || err.contains("outside"),
        "Expected symlink/outside error, got: {}",
        err
    );
}

#[cfg(unix)]
#[test]
fn ensure_path_within_root__normal_file_inside_root__accepted() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    std::fs::create_dir(&repo_root).unwrap();

    let inside_file = repo_root.join("normal.txt");
    std::fs::write(&inside_file, "contents").unwrap();

    assert!(
        ensure_path_within_root(&repo_root, &inside_file).is_ok(),
        "Expected normal file inside root to be accepted"
    );
}

#[cfg(windows)]
#[test]
fn ensure_path_within_root__symlink_escape__skips_if_unprivileged() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    std::fs::create_dir(&repo_root).unwrap();

    let outside_dir = temp.path().join("outside");
    std::fs::create_dir(&outside_dir).unwrap();
    let outside_file = outside_dir.join("secret.txt");
    std::fs::write(&outside_file, "outside").unwrap();

    let symlink_path = repo_root.join("link");
    if std::os::windows::fs::symlink_file(&outside_file, &symlink_path).is_err() {
        // Symlink creation requires elevated privileges on many Windows configs.
        return;
    }

    let result = ensure_path_within_root(&repo_root, &symlink_path);
    assert!(
        result.is_err(),
        "Expected symlink escape to be rejected, but got Ok"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("symlink") || err.contains("outside"),
        "Expected symlink/outside error, got: {}",
        err
    );
}

#[cfg(windows)]
#[test]
fn ensure_path_within_root__normal_file_inside_root__accepted() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    std::fs::create_dir(&repo_root).unwrap();

    let inside_file = repo_root.join("normal.txt");
    std::fs::write(&inside_file, "contents").unwrap();

    assert!(
        ensure_path_within_root(&repo_root, &inside_file).is_ok(),
        "Expected normal file inside root to be accepted"
    );
}

#[test]
fn normalize_root__dotdot_within_tempdir__resolves_without_dotdot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let input = Utf8PathBuf::from_path_buf(root.join("a").join("..").join("b")).unwrap();
    let layout = Layout::new(&input);
    let root_str = layout.root.as_str();

    assert!(
        !root_str.contains(".."),
        "Expected Layout::new to normalize .. components, but root still contains '..': {}",
        root_str
    );

    // The resulting root should still be inside the tempdir (not escape it).
    assert!(
        layout.root.as_std_path().starts_with(root),
        "Layout root escaped tempdir: {}",
        root_str
    );
}
