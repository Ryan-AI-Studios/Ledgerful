#![allow(non_snake_case)]

use crate::common::{DirGuard, git_add_and_commit, run_cli, setup_git_repo};
use ledgerful::commands::init::execute_init;
use ledgerful::commands::viz::execute_viz;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

fn init_repo() -> (tempfile::TempDir, DirGuard) {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    setup_git_repo(&root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(&root, "initial");
    let guard = DirGuard::new(&root);
    execute_init(false, false).unwrap();
    (tmp, guard)
}

#[test]
fn test_viz_generates_html() {
    let (_tmp, _guard) = init_repo();
    let out_path = std::env::current_dir().unwrap().join("output.html");
    let result = execute_viz(Some(out_path.clone()), 50, 3, None, "graph".to_string());
    assert!(result.is_ok());
    assert!(out_path.exists(), "viz output file should exist");
    let content = fs::read_to_string(&out_path).unwrap_or_default();
    assert!(
        content.contains("<!DOCTYPE html>") || content.contains("<html"),
        "viz output should contain HTML content"
    );
}

#[test]
fn viz_graph__offline_banner_and_a11y() {
    let (_tmp, _guard) = init_repo();
    let out_path = std::env::current_dir().unwrap().join("graph.html");
    execute_viz(Some(out_path.clone()), 50, 2, None, "graph".to_string()).unwrap();
    let content = fs::read_to_string(&out_path).unwrap();
    assert!(!content.contains("unpkg.com"));
    assert!(!content.contains("src=\"https://"));
    assert!(content.contains("source: Cozo nodes/edges"));
    assert!(content.contains("limit: 50"));
    assert!(content.contains("truncated: no"));
    assert!(content.contains("asset: vis-network 10.1.2"));
    assert!(content.contains("name=\"viewport\""));
    assert!(content.contains("max-width: 720px"));
    assert!(content.contains("tabindex=\"0\""));
    assert!(content.contains("id=\"evidence\""));
}

#[test]
fn viz_services__gated_declared_overlay() {
    let (_tmp, _guard) = init_repo();
    let root = std::env::current_dir().unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        r#"
[coverage]
enabled = false

[[services.definitions]]
name = "billing-api"
root = "src/billing"
"#,
    )
    .unwrap();
    let out_path = root.join("services.html");
    execute_viz(Some(out_path.clone()), 50, 2, None, "services".to_string()).unwrap();
    let content = fs::read_to_string(&out_path).unwrap();
    assert!(!content.contains("unpkg.com"));
    assert!(content.contains("source: declared overlay; inference gated"));
    assert!(content.contains("\"dir_path\":\"src/billing\""));
    assert!(content.contains("\"marker_kind\":\"DECLARED\""));
    assert!(content.contains("\"confidence\":1.0"));
    assert!(content.contains("billing-api"));
    assert!(content.contains("name=\"viewport\""));
    assert!(content.contains("max-width: 720px"));
    assert!(!content.contains("Run <code>ledgerful index</code> first"));
}

#[test]
fn viz_services__gated_empty_next_is_not_index_only() {
    let (_tmp, _guard) = init_repo();
    let out_path = std::env::current_dir().unwrap().join("services-empty.html");
    execute_viz(Some(out_path.clone()), 50, 2, None, "services".to_string()).unwrap();
    let content = fs::read_to_string(&out_path).unwrap();
    assert!(content.contains("source: declared overlay; inference gated"));
    assert!(
        content.contains("coverage.enabled") || content.contains("gated"),
        "empty gated copy must mention gate, not index-only"
    );
    assert!(!content.contains("Run <code>ledgerful index</code> first"));
}

#[test]
fn viz_services__gated_writes_html_without_cozo() {
    let (_tmp, _guard) = init_repo();
    let root = std::env::current_dir().unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        r#"
[coverage]
enabled = false

[[services.definitions]]
name = "billing-api"
root = "src/billing"
"#,
    )
    .unwrap();
    let state = root.join(".ledgerful").join("state");
    fs::remove_dir_all(&state).unwrap();
    fs::write(&state, "not-a-state-dir").unwrap();
    let out_path = root.join("services-no-cozo.html");
    execute_viz(Some(out_path.clone()), 50, 2, None, "services".to_string()).unwrap();
    let content = fs::read_to_string(&out_path).unwrap();
    assert!(content.contains("source: declared overlay; inference gated"));
    assert!(content.contains("billing-api"));
    assert!(!content.contains("not shown"));
}

#[test]
fn viz_services__gated_leftover_persist_not_shown() {
    let (_tmp, _guard) = init_repo();
    let root = std::env::current_dir().unwrap();
    let storage = StorageManager::init(root.join(".ledgerful/state/ledger.db").as_path()).unwrap();
    storage
        .cozo()
        .expect("cozo after init")
        .run_script(
            "?[name, dir_path, marker_kind, confidence, last_indexed_at] <- [['leftover-svc', 'src/old', 'DIRECTORY', 0.9, '2026-01-01T00:00:00Z']] :put service_roots",
        )
        .unwrap();
    storage.shutdown().unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        r#"
[coverage]
enabled = false

[[services.definitions]]
name = "billing-api"
root = "src/billing"
"#,
    )
    .unwrap();
    let out_path = root.join("services-leftover.html");
    execute_viz(Some(out_path.clone()), 50, 2, None, "services".to_string()).unwrap();
    let content = fs::read_to_string(&out_path).unwrap();
    assert!(content.contains("source: declared overlay; inference gated"));
    assert!(content.contains("persisted 1 not shown"));
    assert!(content.contains("billing-api"));
    assert!(!content.contains("leftover-svc"));
}

#[test]
fn viz_services__enabled_uses_persisted_roots() {
    let (_tmp, _guard) = init_repo();
    let root = std::env::current_dir().unwrap();
    let storage = StorageManager::init(root.join(".ledgerful/state/ledger.db").as_path()).unwrap();
    storage
        .cozo()
        .expect("cozo after init")
        .run_script(
            "?[name, dir_path, marker_kind, confidence, last_indexed_at] <- [['persisted-api', 'src/persist', 'DIRECTORY', 0.8, '2026-01-01T00:00:00Z']] :put service_roots",
        )
        .unwrap();
    storage.shutdown().unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        r#"
[coverage]
enabled = true
[coverage.services]
enabled = true
"#,
    )
    .unwrap();
    let out_path = root.join("services-enabled.html");
    execute_viz(Some(out_path.clone()), 50, 2, None, "services".to_string()).unwrap();
    let content = fs::read_to_string(&out_path).unwrap();
    assert!(content.contains("source: persisted service_roots"));
    assert!(content.contains("persisted-api"));
    assert!(content.contains("src/persist"));
    assert!(!content.contains("inference gated"));
}

#[test]
fn viz_graph__entity_and_limit_plus_one_truncation() {
    let (_tmp, _guard) = init_repo();
    let root = std::env::current_dir().unwrap();
    let storage = StorageManager::init(root.join(".ledgerful/state/ledger.db").as_path()).unwrap();
    let cozo = storage.cozo().expect("cozo after init");
    cozo.run_script(
        "?[id, label, category, risk_score, metadata] <- [ \
            ['alpha', 'alpha', 'file', 0.1, '{}'], \
            ['beta', 'beta', 'file', 0.2, '{}'], \
            ['gamma', 'gamma', 'file', 0.3, '{}'] \
         ] :put node",
    )
    .unwrap();
    cozo.run_script(
        "?[source, target, relation, confidence, provenance_id] <- [ \
            ['alpha', 'beta', 'calls', 1.0, ''], \
            ['gamma', 'gamma', 'self', 1.0, ''] \
         ] :put edge",
    )
    .unwrap();
    storage.shutdown().unwrap();

    let entity_path = root.join("graph-entity.html");
    execute_viz(
        Some(entity_path.clone()),
        50,
        1,
        Some("alpha".to_string()),
        "graph".to_string(),
    )
    .unwrap();
    let entity_html = fs::read_to_string(&entity_path).unwrap();
    assert!(entity_html.contains("source: Cozo nodes/edges"));
    assert!(entity_html.contains("\"id\":\"alpha\""));
    assert!(entity_html.contains("\"id\":\"beta\""));
    assert!(!entity_html.contains("\"id\":\"gamma\""));
    assert!(entity_html.contains("\"from\":\"alpha\""));
    assert!(entity_html.contains("\"to\":\"beta\""));
    assert!(
        entity_html.contains("communities: scoped"),
        "entity neighborhood must run Louvain on the retrieved id set"
    );

    let trunc_path = root.join("graph-truncated.html");
    execute_viz(Some(trunc_path.clone()), 1, 2, None, "graph".to_string()).unwrap();
    let trunc_html = fs::read_to_string(&trunc_path).unwrap();
    assert!(trunc_html.contains("limit: 1"));
    assert!(trunc_html.contains("truncated: yes"));

    let storage = StorageManager::init(root.join(".ledgerful/state/ledger.db").as_path()).unwrap();
    let node_count = storage
        .cozo()
        .expect("cozo after seed")
        .run_script("?[id] := *node{id}")
        .unwrap()
        .rows
        .len();
    storage.shutdown().unwrap();
    assert!(node_count >= 3, "seeded fixture must have nodes");
    let exact_path = root.join("graph-exact.html");
    execute_viz(
        Some(exact_path.clone()),
        node_count,
        2,
        None,
        "graph".to_string(),
    )
    .unwrap();
    let exact_html = fs::read_to_string(&exact_path).unwrap();
    assert!(exact_html.contains(&format!("limit: {node_count}")));
    assert!(exact_html.contains("truncated: no"));
}

#[test]
fn viz_graph_stdout_prints_drawn() {
    let (tmp, _guard) = init_repo();
    let root = tmp.path();
    let storage = StorageManager::init(root.join(".ledgerful/state/ledger.db").as_path()).unwrap();
    let cozo = storage.cozo().expect("cozo after init");
    cozo.run_script(
        "?[id, label, category, risk_score, metadata] <- [ \
            ['alpha', 'alpha', 'file', 0.1, '{}'], \
            ['beta', 'beta', 'file', 0.2, '{}'], \
            ['gamma', 'gamma', 'file', 0.3, '{}'] \
         ] :put node",
    )
    .unwrap();
    cozo.run_script(
        "?[source, target, relation, confidence, provenance_id] <- [ \
            ['alpha', 'beta', 'calls', 1.0, ''], \
            ['gamma', 'gamma', 'self', 1.0, ''] \
         ] :put edge",
    )
    .unwrap();
    storage.shutdown().unwrap();

    let out_path = root.join("drawn.html");
    let out_str = out_path.to_str().expect("utf8 viz output");
    let (stdout, stderr, code) = run_cli(root, &["viz", "--output", out_str, "--limit", "50"]);
    assert_eq!(code, 0, "viz graph failed: {stderr} {stdout}");
    assert!(
        stdout.contains("source: Cozo nodes/edges"),
        "missing banner: {stdout}"
    );
    assert!(
        stdout.contains("Drawn:") && !stdout.contains("Drawn: 0 nodes, 0 edges"),
        "seeded graph must print a non-zero Drawn line: {stdout}"
    );
    assert!(
        stdout.contains("2 edges"),
        "seeded two both-ends edges: {stdout}"
    );
}

#[test]
fn viz_services_stdout_omits_drawn() {
    let (tmp, _guard) = init_repo();
    let root = tmp.path();
    let out_path = root.join("services-drawn.html");
    let out_str = out_path.to_str().expect("utf8 services output");
    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "viz", "--view", "services", "--output", out_str, "--limit", "50",
        ],
    );
    assert_eq!(code, 0, "viz services failed: {stderr} {stdout}");
    assert!(
        !stdout.contains("Drawn:"),
        "services stdout must not print graph Drawn: {stdout}"
    );
}
