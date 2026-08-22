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
