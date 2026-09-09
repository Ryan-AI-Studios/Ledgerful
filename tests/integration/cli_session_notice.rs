//! Track 0300: session-once notices. Printer tests bind §8 names to real CLI
//! empty arms (agy-B-01 / agy-M-03). Doctor never writes the cookie. Session
//! writes only when `configChecklist` has gated/empty rows (0301).

use crate::common::{DirGuard, git_add_and_commit, non_interactive, run_cli, setup_git_repo};
use ledgerful::commands::init::execute_init;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

const MARKER: &str = "Already shown this session.";

fn init_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi").unwrap();
    git_add_and_commit(root, "init");
    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();
    tmp
}

fn init_cli_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi").unwrap();
    git_add_and_commit(root, "init");
    let _guard = DirGuard::new(root);
    let _env = non_interactive();
    execute_init(false, false).unwrap();
    tmp
}

fn run_cli_ni(dir: &Path, args: &[&str]) -> (String, String, i32) {
    run_cli_ni_env(dir, args, &[])
}

fn run_cli_ni_env(dir: &Path, args: &[&str], extra: &[(&str, &str)]) -> (String, String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    cmd.args(args)
        .current_dir(dir)
        .env("LEDGERFUL_NON_INTERACTIVE", "1");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn parse_json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout was not JSON: {e}\n{stdout}"))
}

#[test]
#[serial(cwd)]
fn session_notice_not_written_by_session() {
    let tmp = init_repo();
    let root = tmp.path();
    let layout = Layout::new(root.to_string_lossy().as_ref());
    let _guard = DirGuard::new(root);
    for _ in 0..2 {
        let (stdout, stderr, code) = run_cli(root, &["session", "--json"]);
        assert_eq!(code, 0, "stderr={stderr}");
        let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("session json");
        assert_eq!(json["kind"], "session");
        assert!(json.get("sessionNotices").is_none());
    }
    assert!(
        !layout.cli_session_file().exists(),
        "session --json on a no-gap fixture must not create cli-session.json"
    );
}

#[test]
#[serial(cwd)]
fn session_notice_not_written_by_doctor() {
    let tmp = init_repo();
    let root = tmp.path();
    let layout = Layout::new(root.to_string_lossy().as_ref());
    let _guard = DirGuard::new(root);
    for _ in 0..2 {
        let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
        assert_eq!(code, 0, "stderr={stderr}");
        let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("doctor json");
        assert_eq!(json["schemaVersion"], 1);
        assert!(json.get("sessionNotices").is_none());
    }
    assert!(
        !layout.cli_session_file().exists(),
        "doctor --json must not create cli-session.json"
    );
}

#[test]
#[serial(cwd)]
fn session_notice_second_human_services_collapses() {
    let tmp = init_cli_repo();
    let root = tmp.path();
    let (first, err, code) = run_cli_ni(root, &["services", "diff"]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        first.contains("config set coverage.enabled=true"),
        "first human must be full hint, got: {first}"
    );
    assert!(
        !first.contains(MARKER),
        "first must not collapse, got: {first}"
    );

    let (second, err, code) = run_cli_ni(root, &["services", "diff"]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        second.contains(MARKER),
        "second human must contain marker, got: {second}"
    );
    assert!(
        !second.contains("config set"),
        "second human must omit config set, got: {second}"
    );
}

#[test]
#[serial(cwd)]
fn session_notice_second_json_keeps_message() {
    let tmp = init_cli_repo();
    let root = tmp.path();
    let (first, err, code) = run_cli_ni(root, &["services", "diff", "--json"]);
    assert_eq!(code, 0, "stderr={err}");
    let first = parse_json(&first);
    assert_eq!(first["schemaVersion"], 1);
    assert!(first.get("sessionNotices").is_none());
    let message = first["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("config set coverage.enabled=true"),
        "first JSON message must keep hint, got: {message}"
    );

    let (second, err, code) = run_cli_ni(root, &["services", "diff", "--json"]);
    assert_eq!(code, 0, "stderr={err}");
    let second = parse_json(&second);
    assert_eq!(second["schemaVersion"], 1);
    assert_eq!(second["message"], message);
    assert_eq!(second["sessionNotices"]["coverage.global"], "already_shown");
}

#[test]
#[serial(cwd)]
fn session_notice_observability_coverage_human_collapses() {
    let tmp = init_cli_repo();
    let root = tmp.path();
    let (first, err, code) = run_cli_ni(root, &["observability", "coverage"]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        first.contains("index --analyze-graph"),
        "first coverage human must include analyze-graph, got: {first}"
    );
    assert!(
        !first.contains("Would you like to generate"),
        "non-interactive must skip prompt_yes_no print, got: {first}"
    );

    let (second, err, code) = run_cli_ni(root, &["observability", "coverage"]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(second.contains(MARKER), "got: {second}");
    assert!(
        !second.contains("index --analyze-graph"),
        "second must omit analyze-graph guidance, got: {second}"
    );
    assert!(
        !second.contains("Would you like to generate"),
        "second must not prompt, got: {second}"
    );
}

#[test]
#[serial(cwd)]
fn session_notice_observability_coverage_json_keeps_message() {
    let tmp = init_cli_repo();
    let root = tmp.path();
    let (first, err, code) = run_cli_ni(root, &["observability", "coverage", "--json"]);
    assert_eq!(code, 0, "stderr={err}");
    let first = parse_json(&first);
    let message = first["message"].as_str().unwrap_or_default().to_string();
    assert!(
        message.contains("index --analyze-graph") || message.contains("observability/"),
        "got: {message}"
    );
    assert!(first.get("sessionNotices").is_none());

    let (second, err, code) = run_cli_ni(root, &["observability", "coverage", "--json"]);
    assert_eq!(code, 0, "stderr={err}");
    let second = parse_json(&second);
    assert_eq!(second["message"], message);
    assert_eq!(
        second["sessionNotices"]["observability.empty"],
        "already_shown"
    );
}

#[test]
#[serial(cwd)]
fn session_notice_cross_command_services_then_surfaces() {
    let tmp = init_cli_repo();
    let root = tmp.path();
    let (svc, err, code) = run_cli_ni(root, &["services", "diff"]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        svc.contains("config set coverage.enabled=true"),
        "got: {svc}"
    );

    let (surfaces, err, code) = run_cli_ni(root, &["surfaces"]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        surfaces.contains(MARKER),
        "services+deploy Next must collapse, got: {surfaces}"
    );
    assert!(
        !surfaces.contains("config set"),
        "gated Next must not reprint config set after services, got: {surfaces}"
    );
    assert!(
        surfaces.contains("analyze-graph"),
        "observability Next must stay full until its first emit, got: {surfaces}"
    );
}

#[test]
#[serial(cwd)]
fn session_notice_surfaces_next_unchanged() {
    let tmp = init_cli_repo();
    let root = tmp.path();
    let (first, err, code) = run_cli_ni(root, &["surfaces", "--json"]);
    assert_eq!(code, 0, "stderr={err}");
    let first = parse_json(&first);
    assert!(first.get("sessionNotices").is_none());
    let services_next = first["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .find(|s| s["id"] == "services")
        .expect("services")["next"]
        .clone();

    let (second, err, code) = run_cli_ni(root, &["surfaces", "--json"]);
    assert_eq!(code, 0, "stderr={err}");
    let second = parse_json(&second);
    let services_next_2 = second["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .find(|s| s["id"] == "services")
        .expect("services")["next"]
        .clone();
    assert_eq!(services_next_2, services_next);
    assert_eq!(
        services_next_2,
        "ledgerful config set coverage.enabled=true"
    );
    assert_eq!(second["sessionNotices"]["coverage.global"], "already_shown");
}

#[test]
#[serial(cwd)]
fn session_notice_services_specific_gate_collapses() {
    let tmp = init_cli_repo();
    let root = tmp.path();
    let (out, err, code) = run_cli_ni(root, &["config", "set", "coverage.enabled=true"]);
    assert_eq!(code, 0, "config set enabled stderr={err} stdout={out}");
    let (out, err, code) = run_cli_ni(root, &["config", "set", "coverage.services.enabled=false"]);
    assert_eq!(code, 0, "config set services stderr={err} stdout={out}");

    let (first, err, code) = run_cli_ni(root, &["services", "diff", "--json"]);
    assert_eq!(code, 0, "stderr={err}");
    let first = parse_json(&first);
    let message = first["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("coverage.services.enabled"),
        "got: {message}"
    );
    assert!(first.get("sessionNotices").is_none());

    let (second, err, code) = run_cli_ni(root, &["services", "diff", "--json"]);
    assert_eq!(code, 0, "stderr={err}");
    let second = parse_json(&second);
    assert_eq!(second["message"], message);
    assert_eq!(
        second["sessionNotices"]["coverage.services"],
        "already_shown"
    );
    assert!(second["sessionNotices"].get("coverage.global").is_none());
}

#[test]
#[serial(cwd)]
fn session_notice_env_id_isolates() {
    let tmp = init_cli_repo();
    let root = tmp.path();
    let layout = Layout::new(root.to_string_lossy().as_ref());
    let (first, err, code) = run_cli_ni_env(
        root,
        &["services", "diff"],
        &[("LEDGERFUL_SESSION_ID", "agent-a")],
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        first.contains("config set coverage.enabled=true"),
        "got: {first}"
    );

    let (other, err, code) = run_cli_ni_env(
        root,
        &["services", "diff"],
        &[("LEDGERFUL_SESSION_ID", "agent-b")],
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        other.contains("config set coverage.enabled=true"),
        "agent-b must still see full hint, got: {other}"
    );
    assert!(!layout.cli_session_file().is_file());
    assert!(layout.cli_session_file_for_id("agent-a").is_file());
    assert!(layout.cli_session_file_for_id("agent-b").is_file());
}

fn insert_http_route(root: &Path) {
    let layout = Layout::new(root.to_string_lossy().as_ref());
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (1, 'src/api.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, last_indexed_at) \
         VALUES ('GET', '/api/probe', 'handler', 1, 'axum', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    storage.shutdown().unwrap();
}

#[test]
#[serial(cwd)]
fn session_human_does_not_write_cookie_when_gaps() {
    let tmp = init_repo();
    let root = tmp.path();
    insert_http_route(root);
    let layout = Layout::new(root.to_string_lossy().as_ref());
    let _guard = DirGuard::new(root);

    let (stdout, stderr, code) = run_cli(root, &["session"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        10,
        "human session must stay 10 lines, got {}: {stdout}",
        lines.len()
    );
    assert!(
        !layout.cli_session_file().exists(),
        "human session must not write cli-session.json on a gap fixture"
    );

    let (stdout, stderr, code) = run_cli(root, &["session", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json = parse_json(&stdout);
    assert_eq!(json["kind"], "session");
    let checklist = json["configChecklist"].as_array().expect("configChecklist");
    assert!(
        checklist.iter().any(|i| i["id"] == "coverage.global"),
        "gap fixture must include coverage.global: {checklist:?}"
    );
    assert!(
        layout.cli_session_file().exists(),
        "session --json may persist the cookie when gaps exist"
    );
}
