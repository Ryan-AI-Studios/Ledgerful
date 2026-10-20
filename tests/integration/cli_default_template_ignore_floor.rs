//! 0562: the effective `watch.ignore_patterns` always carries Ledgerful's own
//! internal-state entries, whatever source selected the config.
//!
//! Every oracle uses a synthetic repo, a synthetic home and an explicit child
//! environment; no host template is read, no provider call is made.

use crate::common::{git_add_and_commit, setup_git_repo};
use ledgerful::state::layout::{LEGACY_STATE_DIR, Layout, STATE_DIR};
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

/// The floor this track adds, derived from the product constants (no literal
/// retired brand in this file).
fn floor_entries() -> Vec<String> {
    vec![
        STATE_DIR.to_string(),
        format!("{STATE_DIR}/**"),
        LEGACY_STATE_DIR.to_string(),
        format!("{LEGACY_STATE_DIR}/**"),
    ]
}

/// Parseable stale template: an explicit list that omits internal state.
/// `target` is the marker proving this source was selected.
fn stale_template(extra: &[&str]) -> String {
    let mut entries = vec!["target", "target/**"];
    entries.extend_from_slice(extra);
    let quoted: Vec<String> = entries.iter().map(|e| format!("\"{e}\"")).collect();
    format!("[watch]\nignore_patterns = [{}]\n", quoted.join(", "))
}

fn setup_repo(root: &Path) {
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi\n").expect("write README");
    git_add_and_commit(root, "init");
}

/// Mirror `cli_change_context.rs`: an explicitly initialized storage home.
fn init_storage(root: &Path) {
    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().expect("state dir");
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).expect("init");
    storage.shutdown().expect("shutdown");
}

fn write_probe(root: &Path) {
    fs::write(root.join(STATE_DIR).join("state").join("probe.txt"), "state\n")
        .expect("write state probe");
}

fn write_home_template(home: &Path, contents: &str) -> PathBuf {
    let dir = home.join(STATE_DIR);
    fs::create_dir_all(&dir).expect("home state dir");
    let path = dir.join("default-config.toml");
    fs::write(&path, contents).expect("write home template");
    path
}

/// Private child builder: same isolation census as `common::isolate_cli_env`,
/// but with no default template pin so a home template can be selected.
fn run_child(
    dir: &Path,
    args: &[&str],
    home: &Path,
    extra: &[(&str, &str)],
) -> (String, String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    cmd.args(args).current_dir(dir);
    for key in crate::common::ISOLATED_REMOVED_ENV {
        cmd.env_remove(key);
    }
    cmd.env("USERPROFILE", home)
        .env("HOME", home)
        .env_remove("LEDGERFUL_DEFAULT_CONFIG");
    for (key, value) in extra {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn change_context_json(dir: &Path, home: &Path, extra: &[(&str, &str)]) -> serde_json::Value {
    let (stdout, stderr, code) = run_child(dir, &["change-context", "--json"], home, extra);
    assert_eq!(code, 0, "change-context failed: stderr={stderr}");
    serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON")
}

fn effective_ignore_patterns(dir: &Path, home: &Path, extra: &[(&str, &str)]) -> Vec<String> {
    let (stdout, stderr, code) = run_child(
        dir,
        &[
            "config",
            "view",
            "--json",
            "--section",
            "watch",
            "--key",
            "ignore_patterns",
        ],
        home,
        extra,
    );
    assert_eq!(code, 0, "config view failed: stderr={stderr}");
    serde_json::from_str(stdout.trim()).expect("ignore_patterns must be a JSON string array")
}

/// Self-certification: the fixture's `target` marker proves the stale template
/// was the selected source, and the floor is present in the effective list.
fn assert_selected_template_is_effective(
    dir: &Path,
    home: &Path,
    extra: &[(&str, &str)],
) -> Vec<String> {
    let list = effective_ignore_patterns(dir, home, extra);
    assert!(
        list.iter().any(|p| p == "target"),
        "stale template marker missing — fallback was selected: {list:?}"
    );
    for entry in floor_entries() {
        assert!(
            list.contains(&entry),
            "internal-state entry {entry:?} missing: {list:?}"
        );
    }
    list
}

fn parsed_builtin_ignore_patterns() -> Vec<String> {
    let config: ledgerful::config::Config =
        toml::from_str(ledgerful::config::defaults::DEFAULT_CONFIG).expect("builtin TOML");
    config.watch.ignore_patterns
}

#[test]
fn default_template_ignore_floor_hides_internal_state_dirt() {
    let repo = tempdir().expect("repo tempdir");
    setup_repo(repo.path());
    init_storage(repo.path());
    write_probe(repo.path());

    let home = tempdir().expect("home tempdir");
    write_home_template(home.path(), &stale_template(&[]));

    assert_selected_template_is_effective(repo.path(), home.path(), &[]);

    let json = change_context_json(repo.path(), home.path(), &[]);
    assert_eq!(json["status"], "empty", "{json}");
    assert_eq!(json["agentSummary"]["changed"]["total"], 0, "{json}");
}

#[test]
fn default_template_ignore_floor_keeps_real_source_dirt() {
    let repo = tempdir().expect("repo tempdir");
    setup_repo(repo.path());
    init_storage(repo.path());
    write_probe(repo.path());
    fs::create_dir_all(repo.path().join("src")).expect("mkdir src");
    fs::write(repo.path().join("src/new.rs"), "pub fn new() {}\n").expect("write source");

    let home = tempdir().expect("home tempdir");
    write_home_template(home.path(), &stale_template(&[]));

    assert_selected_template_is_effective(repo.path(), home.path(), &[]);

    let json = change_context_json(repo.path(), home.path(), &[]);
    assert_eq!(json["status"], "ready", "{json}");
    assert_eq!(json["agentSummary"]["changed"]["total"], 1, "{json}");
    let changed: Vec<String> = json["readSet"]
        .as_array()
        .expect("readSet")
        .iter()
        .filter(|entry| entry["reason"].as_str() == Some("changed"))
        .filter_map(|entry| entry["path"].as_str().map(str::to_string))
        .collect();
    assert_eq!(changed, vec!["src/new.rs".to_string()], "{json}");
}

#[test]
fn default_template_ignore_floor_project_config_precedence() {
    let repo = tempdir().expect("repo tempdir");
    setup_repo(repo.path());
    init_storage(repo.path());
    write_probe(repo.path());
    fs::write(
        repo.path().join(STATE_DIR).join("config.toml"),
        "[watch]\nignore_patterns = [\"project-marker.txt\"]\n",
    )
    .expect("write project config");

    let home = tempdir().expect("home tempdir");
    write_home_template(home.path(), &stale_template(&["home-marker.txt"]));

    let list = effective_ignore_patterns(repo.path(), home.path(), &[]);
    assert!(
        list.iter().any(|p| p == "project-marker.txt"),
        "project config must win: {list:?}"
    );
    assert!(
        !list.iter().any(|p| p == "home-marker.txt"),
        "home template must not be consulted: {list:?}"
    );
    for entry in floor_entries() {
        assert!(
            list.contains(&entry),
            "internal-state entry {entry:?} missing: {list:?}"
        );
    }

    let json = change_context_json(repo.path(), home.path(), &[]);
    assert_eq!(json["status"], "empty", "{json}");
    assert_eq!(json["agentSummary"]["changed"]["total"], 0, "{json}");
}

#[test]
fn default_template_ignore_floor_custom_entries_preserved() {
    let repo = tempdir().expect("repo tempdir");
    setup_repo(repo.path());
    init_storage(repo.path());
    write_probe(repo.path());
    fs::create_dir_all(repo.path().join("dist")).expect("mkdir dist");
    fs::write(repo.path().join("dist/out.bin"), "bin\n").expect("write dist artifact");

    let home = tempdir().expect("home tempdir");
    write_home_template(home.path(), &stale_template(&["dist/**"]));

    // `WatchConfig` has no `rename_all`, so the serialized key is the
    // snake_case `ignore_patterns`; a future camelCase rename of the config
    // model would silently break this oracle.
    let list = assert_selected_template_is_effective(repo.path(), home.path(), &[]);
    assert!(
        list.iter().any(|p| p == "dist/**"),
        "custom entry dropped: {list:?}"
    );

    let json = change_context_json(repo.path(), home.path(), &[]);
    assert_eq!(json["status"], "empty", "{json}");
    assert_eq!(json["agentSummary"]["changed"]["total"], 0, "{json}");
}

#[test]
fn default_template_ignore_floor_override_error_controls() {
    // (a) Override points at a directory: strict `config view` would exit 1, so
    // the warn-and-exit-0 contract is asserted through `change-context`.
    let repo = tempdir().expect("repo tempdir");
    setup_repo(repo.path());
    init_storage(repo.path());
    write_probe(repo.path());
    let home = tempdir().expect("home tempdir");
    let override_dir = tempdir().expect("override dir");
    let override_value = override_dir.path().to_string_lossy().into_owned();

    let (stdout, stderr, code) = run_child(
        repo.path(),
        &["change-context", "--json"],
        home.path(),
        &[("LEDGERFUL_DEFAULT_CONFIG", &override_value)],
    );
    assert_eq!(code, 0, "directory override must warn and exit 0: {stderr}");
    assert!(
        stderr.contains("config load failed; using defaults"),
        "expected the config-load warning: {stderr}"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON");
    assert_eq!(json["status"], "empty", "{json}");
    assert_eq!(json["agentSummary"]["changed"]["total"], 0, "{json}");

    // (b) Override points at a missing path: silent built-in fallback, floored.
    let missing = home.path().join("missing-template.toml");
    let missing_value = missing.to_string_lossy().into_owned();
    let list = effective_ignore_patterns(
        repo.path(),
        home.path(),
        &[("LEDGERFUL_DEFAULT_CONFIG", &missing_value)],
    );
    let mut expected = parsed_builtin_ignore_patterns();
    for entry in floor_entries() {
        if !expected.contains(&entry) {
            expected.push(entry);
        }
    }
    assert_eq!(list, expected, "missing override must use the floored built-in");

    let (_, stderr, code) = run_child(
        repo.path(),
        &["config", "view", "--json"],
        home.path(),
        &[("LEDGERFUL_DEFAULT_CONFIG", &missing_value)],
    );
    assert_eq!(code, 0, "missing override must not fail: {stderr}");
    assert!(
        !stderr.contains("config load failed"),
        "missing override is a designed fallback, not a warning: {stderr}"
    );
}

#[test]
fn default_template_ignore_floor_override_missing_path_suppresses_home_template() {
    let repo = tempdir().expect("repo tempdir");
    setup_repo(repo.path());
    init_storage(repo.path());

    let home = tempdir().expect("home tempdir");
    write_home_template(home.path(), &stale_template(&["home-marker.txt"]));

    let missing = home.path().join("missing-template.toml");
    let missing_value = missing.to_string_lossy().into_owned();
    let list = effective_ignore_patterns(
        repo.path(),
        home.path(),
        &[("LEDGERFUL_DEFAULT_CONFIG", &missing_value)],
    );
    assert!(
        !list.iter().any(|p| p == "home-marker.txt"),
        "a missing override must not read the home template: {list:?}"
    );
    for entry in floor_entries() {
        assert!(
            list.contains(&entry),
            "internal-state entry {entry:?} missing: {list:?}"
        );
    }
}

#[test]
fn default_template_ignore_floor_userprofile_shadows_home_template() {
    let repo = tempdir().expect("repo tempdir");
    setup_repo(repo.path());
    init_storage(repo.path());

    let userprofile = tempdir().expect("USERPROFILE tempdir");
    write_home_template(
        userprofile.path(),
        &stale_template(&["userprofile-marker.txt"]),
    );
    let home = tempdir().expect("HOME tempdir");
    write_home_template(home.path(), &stale_template(&["home-marker.txt"]));
    let home_value = home.path().to_string_lossy().into_owned();

    let list = assert_selected_template_is_effective(
        repo.path(),
        userprofile.path(),
        &[("HOME", &home_value)],
    );
    assert!(
        list.iter().any(|p| p == "userprofile-marker.txt"),
        "USERPROFILE template must win: {list:?}"
    );
    assert!(
        !list.iter().any(|p| p == "home-marker.txt"),
        "HOME template must be ignored: {list:?}"
    );
}
