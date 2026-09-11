//! 0207 — populated `--json` list commands emit a schemaVersion-1 object envelope.

use crate::common::{git_add_and_commit, run_cli, setup_git_repo};
use camino::Utf8Path;
use cozo::{DataValue, ScriptMutability};
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use std::collections::BTreeMap;
use std::fs;
use tempfile::tempdir;

fn init_indexed_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn envelope_probe(x: i32) -> i32 { x + 1 }\n",
    )
    .unwrap();
    git_add_and_commit(root, "initial");
    let (stdout, stderr, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");
    let (stdout, stderr, code) = run_cli(root, &["index", "--incremental"]);
    assert_eq!(
        code, 0,
        "index --incremental failed; stdout={stdout} stderr={stderr}"
    );
    tmp
}

fn parse_object(stdout: &str, label: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{label} stdout must parse as JSON: {e}\n{stdout}"));
    assert!(
        v.is_object(),
        "{label} must be an object envelope, got: {stdout}"
    );
    assert_eq!(
        v["schemaVersion"], 1,
        "{label} schemaVersion must be 1: {stdout}"
    );
    v
}

/// Plant five similar 3-d snippet vectors into the repo Cozo (same pattern as
/// `src/semantic/hotspots.rs` tests). Storage is dropped before the CLI binary
/// is invoked so it can open Cozo without a lock.
fn plant_similar_snippet_embeddings(root: &std::path::Path) {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let layout = Layout::new(repo_root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    {
        let cozo = storage
            .cozo()
            .unwrap_or_else(|| panic!("CozoDB storage not initialized for plant"));
        // Same :create then :put shape as `semantic::hotspots` tests.
        cozo.run_script(
            ":create snippet_embedding {file_path,name,line_offset=>embedding:<F32; 3>}",
        )
        .ok();
        let rows: [(&str, &str, Vec<f32>); 5] = [
            ("src/a.rs", "fn_a", vec![1.0, 0.00, 0.0]),
            ("src/b.rs", "fn_b", vec![1.0, 0.01, 0.0]),
            ("src/c.rs", "fn_c", vec![1.0, 0.02, 0.0]),
            ("src/d.rs", "fn_d", vec![1.0, 0.03, 0.0]),
            ("src/e.rs", "fn_e", vec![1.0, 0.04, 0.0]),
        ];
        for (file_path, name, embedding) in rows {
            let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
            let emb: Vec<f32> = embedding.iter().map(|x| x / norm).collect();
            let mut params = BTreeMap::new();
            params.insert(
                "data".to_string(),
                DataValue::from(vec![DataValue::List(Box::new(vec![
                    DataValue::from(file_path),
                    DataValue::from(name),
                    DataValue::from(0_i64),
                    DataValue::Vec(Box::new(cozo::Vector::F32(emb.into()))),
                ]))]),
            );
            cozo.run_script_with_params(
                "?[file_path, name, line_offset, embedding] <- $data :put snippet_embedding",
                params,
                ScriptMutability::Mutable,
            )
            .unwrap_or_else(|e| panic!("plant snippet_embedding {file_path}: {e}"));
        }
    }
    drop(storage);
}

#[test]
fn endpoints_json_is_object_envelope() {
    let tmp = init_indexed_repo();
    let (stdout, stderr, code) = run_cli(tmp.path(), &["endpoints", "--json"]);
    assert_eq!(code, 0, "endpoints --json; stderr={stderr}");
    let v = parse_object(&stdout, "endpoints --json");
    assert!(
        v["results"].is_array(),
        "endpoints --json must expose results[]: {stdout}"
    );
    assert!(
        v.get("resultCount").is_some(),
        "endpoints --json must include resultCount: {stdout}"
    );
    assert!(
        v.get("includeFixtures").is_some(),
        "endpoints --json must echo includeFixtures: {stdout}"
    );
    assert!(
        v.get("fixturesOmitted").is_some(),
        "endpoints --json must echo fixturesOmitted: {stdout}"
    );
}

#[test]
fn hotspots_json_is_object_envelope() {
    let tmp = init_indexed_repo();
    let (stdout, stderr, code) = run_cli(tmp.path(), &["hotspots", "--json", "--limit", "3"]);
    assert_eq!(code, 0, "hotspots --json; stderr={stderr}");
    let v = parse_object(&stdout, "hotspots --json");
    assert!(
        v["files"].is_array(),
        "hotspots --json must expose files[]: {stdout}"
    );
    assert_eq!(
        v["limit"], 3,
        "hotspots --json must echo the effective limit: {stdout}"
    );
    assert!(
        v.get("emptyReason").is_none(),
        "hotspots list envelope must not invent emptyReason: {stdout}"
    );
}

#[test]
fn hotspots_semantic_json_echoes_limit() {
    let tmp = init_indexed_repo();
    plant_similar_snippet_embeddings(tmp.path());
    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["hotspots", "--json", "--semantic", "--limit", "3"],
    );
    assert_eq!(
        code, 0,
        "hotspots --json --semantic --limit 3; stdout={stdout} stderr={stderr}"
    );
    let v = parse_object(&stdout, "hotspots --json --semantic");
    assert!(
        v["files"].is_array(),
        "hotspots --json --semantic must expose files[]: {stdout}"
    );
    assert_eq!(
        v["files"].as_array().map(Vec::len),
        Some(3),
        "hotspots --json --semantic must truncate files[] to --limit 3: {stdout}"
    );
    assert_eq!(
        v["limit"], 3,
        "hotspots --json --semantic must echo the effective limit: {stdout}"
    );
    assert!(
        v.get("emptyReason").is_none(),
        "hotspots --semantic envelope must not invent emptyReason: {stdout}"
    );
}

/// `index --incremental` does not create `snippet_embedding`. Missing relation
/// must fail-closed (`run_script?`), not `Ok([])`.
#[test]
fn hotspots_semantic_json_missing_relation_is_fail_closed() {
    let tmp = init_indexed_repo();
    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["hotspots", "--json", "--semantic", "--limit", "3"],
    );
    assert_ne!(
        code, 0,
        "missing snippet_embedding must fail-closed (non-zero); stdout={stdout} stderr={stderr}"
    );
}

fn assert_ci_json_envelope(root: &std::path::Path, sub: &str) {
    let (stdout, stderr, code) = run_cli(root, &["ci", sub, "--json"]);
    assert_eq!(code, 0, "ci {sub} --json; stderr={stderr}");
    let v = parse_object(&stdout, &format!("ci {sub} --json"));
    let gates = v["gates"]
        .as_array()
        .unwrap_or_else(|| panic!("ci {sub} --json must expose gates[], got: {stdout}"));
    assert_eq!(
        v["resultCount"].as_u64().unwrap_or(u64::MAX),
        gates.len() as u64,
        "ci {sub} --json resultCount must match gates length: {stdout}"
    );
    assert!(
        v.get("emptyReason").is_none(),
        "ci catalog must not invent emptyReason, got: {stdout}"
    );
}

#[test]
fn ci_list_json_is_object_envelope() {
    let tmp = init_indexed_repo();
    assert_ci_json_envelope(tmp.path(), "list");
}

#[test]
fn ci_diff_json_is_object_envelope() {
    let tmp = init_indexed_repo();
    assert_ci_json_envelope(tmp.path(), "diff");
}

fn assert_services_json_envelope(root: &std::path::Path, sub: &str) {
    let (stdout, stderr, code) = run_cli(root, &["services", sub, "--json"]);
    assert_eq!(code, 0, "services {sub} --json; stderr={stderr}");
    let v = parse_object(&stdout, &format!("services {sub} --json"));
    let results = v["results"]
        .as_array()
        .unwrap_or_else(|| panic!("services {sub} --json must expose results[], got: {stdout}"));
    assert_eq!(
        v["resultCount"].as_u64().unwrap_or(u64::MAX),
        results.len() as u64,
        "services {sub} --json resultCount must match results length: {stdout}"
    );
    assert_eq!(
        v["emptyReason"].as_str(),
        Some("disabledByConfig"),
        "gated empty catalog must keep emptyReason disabledByConfig: {stdout}"
    );
}

#[test]
fn services_diff_json_is_object_envelope() {
    let tmp = init_indexed_repo();
    assert_services_json_envelope(tmp.path(), "diff");
}

#[test]
fn services_list_json_is_object_envelope() {
    let tmp = init_indexed_repo();
    assert_services_json_envelope(tmp.path(), "list");
}

fn seed_data_model_rows(root: &std::path::Path, product: bool, fixture: bool) {
    let root_utf8 = Utf8Path::from_path(root).expect("utf8 root");
    let layout = Layout::new(root_utf8);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    let conn = storage.get_connection();
    if product {
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/models/user.rs', 'Rust', 'hash_dm_prod', 80, '2026-09-11T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
             VALUES ('UserRow', ?1, 'Rust', 'SCHEMA', 0.9, '-> <persistence>', '2026-09-11T00:00:00Z')",
            [file_id],
        )
        .unwrap();
    }
    if fixture {
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('tests/fixtures/go_sample/pkg/user.go', 'go', 'hash_dm_fix', 80, '2026-09-11T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
             VALUES ('User', ?1, 'go', 'STRUCT', 1.0, 'json tags', '2026-09-11T00:00:00Z')",
            [file_id],
        )
        .unwrap();
    }
    storage.shutdown().unwrap();
}

#[test]
fn data_models_list_json_echoes_fixture_flags() {
    let tmp = init_indexed_repo();
    seed_data_model_rows(tmp.path(), true, true);

    let (stdout, stderr, code) = run_cli(tmp.path(), &["data-models", "list", "--json"]);
    assert_eq!(code, 0, "data-models list --json; stderr={stderr}");
    let v = parse_object(&stdout, "data-models list --json");
    assert_eq!(v["includeFixtures"], false);
    assert_eq!(v["fixturesOmitted"], 1);
    assert_eq!(v["resultCount"], 1);
    assert_eq!(v["models"][0]["name"], "UserRow");
    assert_eq!(v["models"][0]["fieldImpact"], "unsupported");
    assert!(v.get("fieldImpact").is_none());

    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["data-models", "list", "--include-fixtures", "--json"],
    );
    assert_eq!(
        code, 0,
        "data-models list --include-fixtures --json; stderr={stderr}"
    );
    let with = parse_object(&stdout, "data-models list --include-fixtures --json");
    assert_eq!(with["includeFixtures"], true);
    assert_eq!(with["fixturesOmitted"], 0);
    assert_eq!(with["resultCount"], 2);
}

#[test]
fn data_models_list_json_post_omit_empty_is_no_matches() {
    let tmp = init_indexed_repo();
    seed_data_model_rows(tmp.path(), false, true);

    let (stdout, stderr, code) = run_cli(tmp.path(), &["data-models", "list", "--json"]);
    assert_eq!(
        code, 0,
        "data-models list --json post-omit; stderr={stderr}"
    );
    let v = parse_object(&stdout, "data-models list post-omit empty");
    assert_eq!(v["includeFixtures"], false);
    assert_eq!(v["fixturesOmitted"], 1);
    assert_eq!(v["resultCount"], 0);
    assert_eq!(v["emptyReason"], "noMatches");
    assert_ne!(v["emptyReason"], "noIndexedData");
    assert!(v.get("fieldImpact").is_none());
    assert!(
        v["message"]
            .as_str()
            .unwrap_or("")
            .contains("No product data models indexed")
    );
}
