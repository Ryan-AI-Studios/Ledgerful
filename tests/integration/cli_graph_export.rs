//! `ledgerful graph export` against a temp Cozo store.

#![allow(non_snake_case)]

use cozo::{DataValue, ScriptMutability};
use ledgerful::state::storage::StorageManager;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

use crate::common::setup_git_repo;

const PUT_NODES: &str = "?[id, label, category, risk_score, metadata] <- $batch :put node";
const PUT_EDGES: &str =
    "?[source, target, relation, confidence, provenance_id] <- $batch :put edge";

fn git_repo() -> TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    setup_git_repo(tmp.path());
    tmp
}

fn init_storage(root: &Path) -> StorageManager {
    let db = root.join(".ledgerful").join("state").join("ledger.db");
    fs::create_dir_all(db.parent().expect("state parent")).expect("state dir");
    StorageManager::init(&db).expect("init storage")
}

fn put_rows(storage: &StorageManager, script: &str, rows: Vec<Value>) {
    let cozo = storage.cozo().expect("cozo after init");
    let mut params = BTreeMap::new();
    params.insert("batch".to_string(), DataValue::from(Value::Array(rows)));
    cozo.run_script_with_params(script, params, ScriptMutability::Mutable)
        .unwrap_or_else(|err| panic!("put failed: {err:?}\n{script}"));
}

fn put_script(storage: &StorageManager, script: &str) {
    storage
        .cozo()
        .expect("cozo after init")
        .run_script(script)
        .unwrap_or_else(|err| panic!("script failed: {err:?}\n{script}"));
}

fn seed(root: &Path, nodes: Vec<Value>, edges: Vec<Value>) {
    let storage = init_storage(root);
    if !nodes.is_empty() {
        put_rows(&storage, PUT_NODES, nodes);
    }
    if !edges.is_empty() {
        put_rows(&storage, PUT_EDGES, edges);
    }
    storage.shutdown().expect("shutdown");
}

fn node(id: &str, label: &str, category: &str, risk: f64, metadata: Value) -> Value {
    json!([id, label, category, risk, metadata])
}

fn edge(source: &str, target: &str, relation: &str, confidence: f64, provenance: &str) -> Value {
    json!([source, target, relation, confidence, provenance])
}

fn run(dir: &Path, args: &[&str]) -> (Vec<u8>, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run ledgerful");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.stdout, stderr, output.status.code().unwrap_or(-1))
}

fn stdout_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("stdout is utf-8")
}

fn summary_has(stderr: &str, fragments: &[&str]) {
    let first = stderr.lines().next().unwrap_or("");
    for fragment in fragments {
        assert!(
            first.contains(fragment),
            "summary line missing {fragment:?}: {first}"
        );
    }
}

#[test]
fn graph_export__under_cap__counts_match_store() {
    let tmp = git_repo();
    let storage = init_storage(tmp.path());
    // The Json column rejects a bare null. parse_json('null') stores JSON null.
    put_script(
        &storage,
        "?[id, label, category, risk_score, metadata] := id = 'src/o\\'hare', label = 'O<H', category = 'file', risk_score = 0.0, metadata = parse_json('null')\n\
         ?[id, label, category, risk_score, metadata] := id = 'z', label = 'Z', category = 'symbol', risk_score = 1.0, metadata = parse_json('{}')\n\
         :put node",
    );
    put_rows(
        &storage,
        PUT_EDGES,
        vec![edge("src/o'hare", "z", "calls", 0.5, "prov-1")],
    );
    storage.shutdown().expect("shutdown");

    let (graphml, graphml_err, graphml_code) =
        run(tmp.path(), &["graph", "export", "--format", "graphml"]);
    assert_eq!(graphml_code, 0, "{graphml_err}");
    summary_has(
        &graphml_err,
        &[
            "source: Cozo nodes/edges",
            "format: graphml",
            "truncated: no",
            "emitted_nodes: 2",
            "emitted_edges: 1",
            "store_nodes: 2",
            "store_edges: 1",
        ],
    );
    assert!(
        !graphml_err.contains("capped"),
        "under cap must not print the cap line: {graphml_err}"
    );
    let body = stdout_text(&graphml);
    assert!(!body.contains('\r'));
    assert!(body.contains("edgedefault=\"directed\""));
    assert!(body.contains("<data key=\"node_id\">src/o'hare</data>"));
    assert!(body.contains("<data key=\"node_id\">z</data>"));
    assert!(body.contains("O&lt;H"));
    assert_eq!(body.matches("<data key=\"metadata\">").count(), 1);
    assert!(body.contains("<data key=\"metadata\">{}</data>"));
    assert!(body.contains("<data key=\"confidence\">0.5</data>"));
    assert!(body.contains("<data key=\"provenance_id\">prov-1</data>"));
    assert!(!body.contains("attribution_source"));
    assert!(!body.contains("exactness"));
    assert!(!body.contains("GraphLayer"));

    let (cypher, cypher_err, cypher_code) =
        run(tmp.path(), &["graph", "export", "--format", "cypher"]);
    assert_eq!(cypher_code, 0, "{cypher_err}");
    summary_has(&cypher_err, &["format: cypher", "truncated: no"]);
    assert!(!cypher_err.contains("capped"));
    let cypher_body = stdout_text(&cypher);
    assert!(cypher_body.contains("'src/o\\'hare'"));
    assert!(cypher_body.contains("n.risk_score = 0.0"));
    assert!(cypher_body.contains("n.risk_score = 1.0"));
    assert_eq!(cypher_body.matches("n.metadata").count(), 1);
    assert!(cypher_body.contains("r.confidence = 0.5"));
    assert!(cypher_body.contains("r.provenance_id = 'prov-1'"));
    assert!(cypher_body.contains("MERGE (s:file {id:"));
    assert!(cypher_body.contains("MERGE (t:symbol {id:"));
}

#[test]
fn graph_export__over_cap__stderr_and_file_say_yes() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![
            node("a", "a", "file", 0.0, json!({})),
            node("m", "m", "file", 0.0, json!({})),
            node("z", "z", "file", 0.0, json!({})),
        ],
        Vec::new(),
    );
    let (stdout, stderr, code) = run(
        tmp.path(),
        &["graph", "export", "--format", "graphml", "--limit", "1"],
    );
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<_> = stderr.lines().collect();
    assert!(lines.len() >= 2, "{stderr}");
    assert!(lines[0].contains("truncated: yes"));
    assert!(lines[0].contains("emitted_nodes: 1"));
    assert!(lines[0].contains("store_nodes: 3"));
    assert_eq!(lines[1], "truncated: graph export capped at limit 1");
    let body = stdout_text(&stdout);
    assert!(body.contains("<data key=\"truncated\">yes</data>"));
    assert!(body.contains("<data key=\"node_id\">a</data>"));
    assert!(!body.contains("<data key=\"node_id\">m</data>"));
    assert!(!body.contains("<data key=\"node_id\">z</data>"));
}

#[test]
fn graph_export__output_file__body_is_utf8() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a", "a", "file", 0.0, json!({}))],
        Vec::new(),
    );
    let (first, _, code) = run(tmp.path(), &["graph", "export", "--format", "graphml"]);
    assert_eq!(code, 0);
    let (second, _, code) = run(tmp.path(), &["graph", "export", "--format", "graphml"]);
    assert_eq!(code, 0);
    assert_eq!(first, second, "same fixture must be byte-identical");
    assert!(!first.contains(&b'\r'));

    let path: PathBuf = tmp.path().join("nested").join("out.graphml");
    let (stdout, stderr, code) = run(
        tmp.path(),
        &[
            "graph",
            "export",
            "--format",
            "graphml",
            "--output",
            path.to_str().expect("utf-8 path"),
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let wrote = stdout_text(&stdout);
    assert!(wrote.starts_with("wrote: "), "{wrote}");
    assert!(wrote.ends_with('\n'));
    let file_bytes = fs::read(&path).expect("output file");
    assert_eq!(file_bytes, first);
    assert!(path.is_file());
}

#[test]
fn graph_export__missing_entity__errors() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a", "a", "file", 0.0, json!({}))],
        Vec::new(),
    );
    let (stdout, stderr, code) = run(
        tmp.path(),
        &[
            "graph",
            "export",
            "--format",
            "graphml",
            "--entity",
            "missing-id",
        ],
    );
    assert_ne!(code, 0);
    assert!(
        stderr.contains("entity not in graph: missing-id"),
        "{stderr}"
    );
    assert!(!stdout_text(&stdout).contains("<graphml"));
}

#[test]
fn graph_export__does_not_init_state() {
    let tmp = git_repo();
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "export", "--format", "graphml"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("Storage not initialized"), "{stderr}");
    assert!(stderr.contains("ledgerful scan"), "{stderr}");
    assert!(
        !tmp.path().join(".ledgerful").exists(),
        "export must not create .ledgerful"
    );
    assert!(!stdout_text(&stdout).contains("<graphml"));
}

#[test]
fn graph_export__db_present_cozo_absent__viz_sentence() {
    let tmp = git_repo();
    let storage = init_storage(tmp.path());
    storage.shutdown().expect("shutdown");
    let cozo = tmp
        .path()
        .join(".ledgerful")
        .join("state")
        .join("ledger.cozo");
    assert!(cozo.exists(), "init creates ledger.cozo");
    if cozo.is_dir() {
        fs::remove_dir_all(&cozo).expect("remove cozo dir");
    } else {
        fs::remove_file(&cozo).expect("remove cozo file");
    }
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "export", "--format", "cypher"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("CozoDB not initialized. Run 'index' first."),
        "{stderr}"
    );
    assert!(!cozo.exists());
    assert!(!stdout_text(&stdout).contains("MERGE"));
}

#[test]
fn graph_export__entity_bounded_walk__depth_limits_and_traverses_bidirectional() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![
            node("a", "a", "file", 0.0, json!({})),
            node("b", "b", "file", 0.0, json!({})),
            node("c", "c", "file", 0.0, json!({})),
            node("in", "in", "file", 0.0, json!({})),
            node("z", "z", "file", 0.0, json!({})),
        ],
        vec![
            edge("in", "a", "links", 1.0, "p-in"),
            edge("a", "b", "calls", 1.0, "p-ab"),
            edge("b", "c", "next", 1.0, "p-bc"),
        ],
    );

    let (stdout, stderr, code) = run(
        tmp.path(),
        &[
            "graph", "export", "--format", "graphml", "--entity", "a", "--depth", "1",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    summary_has(
        &stderr,
        &[
            "truncated: no",
            "emitted_nodes: 3",
            "emitted_edges: 2",
            "store_nodes: 5",
            "store_edges: 3",
        ],
    );
    assert!(!stderr.contains("capped"), "{stderr}");
    let body = stdout_text(&stdout);
    assert!(body.contains("<data key=\"node_id\">a</data>"));
    assert!(body.contains("<data key=\"node_id\">b</data>"));
    assert!(body.contains("<data key=\"node_id\">in</data>"));
    assert!(!body.contains("<data key=\"node_id\">c</data>"));
    assert!(!body.contains("<data key=\"node_id\">z</data>"));
    assert!(body.contains("<data key=\"relation\">links</data>"));
    assert!(body.contains("<data key=\"relation\">calls</data>"));
    assert!(!body.contains("<data key=\"relation\">next</data>"));
    assert!(body.contains("<data key=\"truncated\">no</data>"));

    let (stdout, stderr, code) = run(
        tmp.path(),
        &[
            "graph", "export", "--format", "graphml", "--entity", "a", "--depth", "2", "--limit",
            "2",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<_> = stderr.lines().collect();
    assert!(lines[0].contains("truncated: yes"), "{stderr}");
    assert!(lines[0].contains("emitted_nodes: 2"), "{stderr}");
    assert_eq!(lines[1], "truncated: graph export capped at limit 2");
    let body = stdout_text(&stdout);
    assert!(body.contains("<data key=\"node_id\">a</data>"));
    assert!(body.contains("<data key=\"node_id\">b</data>"));
    assert!(!body.contains("<data key=\"node_id\">in</data>"));
    assert!(!body.contains("<data key=\"node_id\">c</data>"));
    assert!(body.contains("<data key=\"relation\">calls</data>"));
    assert!(!body.contains("<data key=\"relation\">links</data>"));
    assert!(!body.contains("<data key=\"relation\">next</data>"));
    assert!(body.contains("<data key=\"truncated\">yes</data>"));
}

#[test]
fn graph_export__depth_without_entity__returns_expected_error() {
    let tmp = git_repo();
    let storage = init_storage(tmp.path());
    storage.shutdown().expect("shutdown");
    let (stdout, stderr, code) = run(
        tmp.path(),
        &["graph", "export", "--format", "graphml", "--depth", "1"],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("--depth requires --entity"), "{stderr}");
    assert!(!stdout_text(&stdout).contains("<graphml"));
}

#[test]
fn graph_export__empty_store__success_with_zero_counts() {
    let tmp = git_repo();
    let storage = init_storage(tmp.path());
    storage.shutdown().expect("shutdown");
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "export", "--format", "graphml"]);
    assert_eq!(code, 0, "{stderr}");
    summary_has(
        &stderr,
        &[
            "truncated: no",
            "emitted_nodes: 0",
            "emitted_edges: 0",
            "store_nodes: 0",
            "store_edges: 0",
        ],
    );
    assert!(!stderr.contains("capped"), "{stderr}");
    let body = stdout_text(&stdout);
    assert!(body.contains("<graphml"));
    assert!(body.contains("edgedefault=\"directed\""));
    assert!(!body.contains("<node "));
    assert!(!body.contains("<edge "));
}
