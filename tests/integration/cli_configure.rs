//! 0324 L-02: clap `--apply a,b` is two tokens at the binary parse layer.

use crate::common::{git_add_and_commit, setup_git_repo};
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

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
fn configure_apply_comma_list_refuses_second_token_not_joined_literal() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "# temp\n").unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    let init = Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    insert_http_route(root);

    let session = "t-0324-l02";
    let out = Command::new(exe)
        .args(["configure", "--apply", "coverage.global,coverage.services"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env("LEDGERFUL_SESSION_ID", session)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "joined apply must refuse; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "validation refuse must not emit stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("coverage.services"),
        "clap must split the second token; stderr={stderr}"
    );
    assert!(
        !stderr.contains("`coverage.global,coverage.services`"),
        "must not treat the comma list as one id; stderr={stderr}"
    );

    let layout = Layout::new(root.to_string_lossy().as_ref());
    assert!(
        !layout.cli_session_file_for_id(session).exists(),
        "validation refuse must not write cookie"
    );
    let cfg = ledgerful::config::load::load_config(&layout).unwrap();
    assert!(
        !cfg.coverage.enabled,
        "fail-closed: no writes on mixed apply"
    );
}
