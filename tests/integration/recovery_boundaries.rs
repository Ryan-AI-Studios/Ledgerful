//! Doctor and reset diagnostics on disposable git repos.
//! `schemaVersion` stays 1. No new doctor code and no `index_health` key.
//!
//! Names keep the repo's `feature__condition__result` form.
#![allow(non_snake_case)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use camino::Utf8PathBuf;
use ledgerful::config::model::Config;
use ledgerful::ledger::{
    Category, ChangeType, CommitRequest, TransactionManager, TransactionRequest,
};
use ledgerful::search::TantivySearchEngine;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;

use crate::common::{TempEnv, isolate_cli_env};

fn home_beside_repo(temp_root: &Path) -> PathBuf {
    let home = temp_root.join("home");
    fs::create_dir_all(&home).expect("synthetic home beside the git repo");
    home
}

fn assert_home_beside_repo(temp_root: &Path, repo: &Path) {
    assert!(
        temp_root.join("home").is_dir(),
        "home is beside the git repo"
    );
    assert!(
        !repo.join("home").exists(),
        "home is not inside the git repo"
    );
}

#[allow(dead_code)]
struct HomeGuard {
    userprofile: TempEnv,
    home: TempEnv,
}

impl HomeGuard {
    fn new(temp_root: &Path) -> Self {
        let home = home_beside_repo(temp_root);
        let home_s = home.to_string_lossy().into_owned();
        Self {
            userprofile: TempEnv::set("USERPROFILE", &home_s),
            home: TempEnv::set("HOME", &home_s),
        }
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git available");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("src dir");
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "t@example.com"]);
    git(root, &["config", "user.name", "t"]);
    let _ = Command::new("git")
        .args(["config", "core.hooksPath", ".git/hooks"])
        .current_dir(root)
        .output();
    fs::write(root.join("src").join("probe.rs"), "fn probe() {}\n").expect("probe");
    git(root, &["add", "src/probe.rs"]);
    git(root, &["commit", "-m", "init"]);
}

fn cli(temp_root: &Path, repo: &Path, args: &[&str]) -> (String, String, i32) {
    let home = home_beside_repo(temp_root);
    let repo_s = repo.to_string_lossy().into_owned();
    let mut owned = vec!["-C".to_string(), repo_s];
    owned.extend(args.iter().copied().map(str::to_string));
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    cmd.args(&refs).current_dir(repo);
    isolate_cli_env(&mut cmd, &home);
    let output = cmd.output().expect("failed to run ledgerful");
    assert_home_beside_repo(temp_root, repo);
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn seed_one_ledger_row(temp_root: &Path, repo: &Path) {
    let _home = HomeGuard::new(temp_root);
    assert_home_beside_repo(temp_root, repo);
    let utf8 = Utf8PathBuf::from_path_buf(repo.to_path_buf()).expect("utf-8 repo");
    let layout = Layout::new(&utf8);
    layout.ensure_state_dir().expect("state dir");
    let mut storage = StorageManager::init_with_layout(&layout).expect("init");
    let mut tx = TransactionManager::new(&mut storage, repo.to_path_buf(), Config::default());
    let tx_id = tx
        .start_change(TransactionRequest {
            category: Category::Chore,
            entity: "src/probe.rs".to_string(),
            planned_action: Some("seed one committed ledger row".to_string()),
            ..Default::default()
        })
        .expect("start_change");
    tx.commit_change(
        tx_id,
        CommitRequest {
            change_type: ChangeType::Modify,
            summary: "seed one committed ledger row".to_string(),
            reason: "recovery boundary oracle".to_string(),
            ..Default::default()
        },
        false,
    )
    .expect("commit_change");
    storage.shutdown().expect("shutdown before a CLI child");
    assert_home_beside_repo(temp_root, repo);
}

fn ledger_count(root: &Path) -> i64 {
    let db = root.join(".ledgerful").join("state").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open ledger.db");
    conn.query_row("SELECT COUNT(*) FROM ledger_entries", [], |row| row.get(0))
        .expect("ledger_entries count")
}

fn search_index_dir(root: &Path) -> PathBuf {
    root.join(".ledgerful").join("search_index")
}

fn store_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let dir = search_index_dir(root);
    for entry in fs::read_dir(&dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display())) {
        let entry = entry.expect("index entry");
        if entry.file_name().to_string_lossy().ends_with(".store") {
            files.push(entry.path());
        }
    }
    files.sort();
    files
}

fn doctor_json(temp_root: &Path, repo: &Path) -> (serde_json::Value, String, i32) {
    let (stdout, stderr, code) = cli(temp_root, repo, &["doctor", "--json"]);
    let value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("doctor --json: {err}\nstdout={stdout}\nstderr={stderr}"));
    (value, stderr, code)
}

fn finding_codes(value: &serde_json::Value) -> Vec<String> {
    value["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["code"].as_str().map(str::to_string))
        .collect()
}

fn index_repo(temp_root: &Path, repo: &Path) {
    let (stdout, stderr, code) = cli(temp_root, repo, &["index"]);
    assert_eq!(code, 0, "index failed\nstdout={stdout}\nstderr={stderr}");
    assert!(
        !store_files(repo).is_empty(),
        "index must leave a Tantivy .store file"
    );
    assert!(
        repo.join(".ledgerful")
            .join("state")
            .join("ledger.cozo")
            .exists(),
        "index must leave ledger.cozo"
    );
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundaries__missing_segment__search_load_failed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("repo");
    init_repo(&root);
    seed_one_ledger_row(tmp.path(), &root);
    index_repo(tmp.path(), &root);
    assert_eq!(ledger_count(&root), 1, "index does not add a ledger row");
    for path in store_files(&root) {
        fs::remove_file(&path).expect("delete .store after index exited");
    }
    assert!(store_files(&root).is_empty(), "every .store file is gone");
    let (value, stderr, code) = doctor_json(tmp.path(), &root);
    assert_eq!(code, 0, "doctor --json exit; stderr={stderr}");
    assert_eq!(value["schemaVersion"], 1, "schemaVersion stays 1");
    assert!(
        value.get("index_health").is_none(),
        "doctor --json has no index_health field"
    );
    let codes = finding_codes(&value);
    assert!(
        codes.iter().any(|code| code == "search-load-failed"),
        "missing .store is search-load-failed; codes={codes:?} stderr={stderr}"
    );
    assert!(
        !codes.iter().any(|code| code == "search-corrupt"),
        "this test does not accept search-corrupt; codes={codes:?}"
    );
    assert_eq!(ledger_count(&root), 1, "ledger_entries count is exactly 1");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundaries__cleared_index__search_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("repo");
    init_repo(&root);
    seed_one_ledger_row(tmp.path(), &root);
    index_repo(tmp.path(), &root);
    {
        let engine = TantivySearchEngine::open_or_create(&search_index_dir(&root)).expect("open");
        assert!(
            engine.document_count() > 0,
            "index left at least one document"
        );
        assert!(
            engine.segment_count().expect("segment_count") > 0,
            "index left at least one segment"
        );
        assert!(!store_files(&root).is_empty(), "a .store file exists");
        engine.clear().expect("clear");
    }
    let (value, stderr, code) = doctor_json(tmp.path(), &root);
    assert_eq!(code, 0, "doctor --json exit; stderr={stderr}");
    assert_eq!(value["schemaVersion"], 1, "schemaVersion stays 1");
    assert!(
        value.get("index_health").is_none(),
        "doctor --json has no index_health field"
    );
    let codes = finding_codes(&value);
    assert!(
        codes.iter().any(|code| code == "search-empty"),
        "cleared index is search-empty; codes={codes:?} stderr={stderr}"
    );
    assert_eq!(ledger_count(&root), 1, "ledger_entries count is exactly 1");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundaries__default_reset__ledger_file_remains() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("repo");
    init_repo(&root);
    seed_one_ledger_row(tmp.path(), &root);
    index_repo(tmp.path(), &root);
    let (stdout, stderr, code) = cli(tmp.path(), &root, &["reset"]);
    assert_eq!(code, 0, "default reset\nstdout={stdout}\nstderr={stderr}");
    assert!(
        root.join(".ledgerful")
            .join("state")
            .join("ledger.db")
            .exists(),
        "ledger.db remains"
    );
    assert_eq!(ledger_count(&root), 1, "ledger_entries count is exactly 1");
    assert!(search_index_dir(&root).exists(), "search_index remains");
    assert!(
        root.join(".ledgerful")
            .join("state")
            .join("ledger.cozo")
            .exists(),
        "ledger.cozo remains"
    );
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundaries__include_ledger_without_yes__refuses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("repo");
    init_repo(&root);
    let utf8 = Utf8PathBuf::from_path_buf(root.clone()).expect("utf-8 repo");
    let layout = Layout::new(&utf8);
    layout.ensure_state_dir().expect("state dir");
    let _home = HomeGuard::new(tmp.path());
    let storage = StorageManager::init_with_layout(&layout).expect("init");
    storage.shutdown().expect("shutdown");
    let db = root.join(".ledgerful").join("state").join("ledger.db");
    assert!(db.exists(), "ledger.db exists before the refusal");
    let (stdout, stderr, code) = cli(tmp.path(), &root, &["reset", "--include-ledger"]);
    assert_ne!(
        code, 0,
        "reset --include-ledger without --yes must refuse\nstdout={stdout}"
    );
    assert!(
        stderr.contains("Re-run with '--yes'"),
        "stderr must contain the confirmation hint; stderr={stderr} stdout={stdout}"
    );
    assert!(db.exists(), "ledger.db still exists");
}
