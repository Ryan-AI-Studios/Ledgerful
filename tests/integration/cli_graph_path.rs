//! `ledgerful graph path` against a temp Cozo store.

#![allow(non_snake_case)]

use cozo::{DataValue, ScriptMutability};
use ledgerful::state::storage::StorageManager;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
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

fn node(id: &str) -> Value {
    json!([id, id, "file", 0.0, {}])
}

fn edge(source: &str, target: &str, relation: &str, confidence: f64, provenance: &str) -> Value {
    json!([source, target, relation, confidence, provenance])
}

fn run(dir: &Path, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run ledgerful");
    (
        output.stdout,
        output.stderr,
        output.status.code().unwrap_or(-1),
    )
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

fn assert_no_layer_words(body: &str) {
    for word in [
        "GraphLayer",
        "attribution_source",
        "exact",
        "derived",
        "heuristic",
    ] {
        assert!(!body.contains(word), "{word} leaked into {body}");
    }
}

#[test]
fn graph_path__direct_and_longer__one_hop_and_stderr_order() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a"), node("b"), node("c")],
        vec![
            edge("a", "c", "calls", 0.5, "direct"),
            edge("a", "b", "calls", 0.5, "ab"),
            edge("b", "c", "calls", 0.5, "bc"),
        ],
    );
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "path", "--from", "a", "--to", "c"]);
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        err,
        "source: Cozo nodes/edges\nfrom: a\nto: c\ndirection: source-to-target\npath: yes\nhops: 1\n"
    );
    let body = text(&stdout);
    assert_eq!(body, "1\ta\tcalls\tc\t0.5\tdirect\n");
    assert_no_layer_words(&body);
    assert_no_layer_words(&err);
}

#[test]
fn graph_path__only_two_hops__two_lines() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a"), node("b"), node("c")],
        vec![
            edge("a", "b", "calls", 0.5, "ab"),
            edge("b", "c", "imports", 0.25, "bc"),
        ],
    );
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "path", "--from", "a", "--to", "c"]);
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert!(err.ends_with("path: yes\nhops: 2\n"), "{err}");
    assert_eq!(
        text(&stdout),
        "1\ta\tcalls\tb\t0.5\tab\n2\tb\timports\tc\t0.25\tbc\n"
    );
}

#[test]
fn graph_path__reverse_without_edge__path_none() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a"), node("b")],
        vec![edge("a", "b", "calls", 0.5, "ab")],
    );
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "path", "--from", "b", "--to", "a"]);
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        err,
        "source: Cozo nodes/edges\nfrom: b\nto: a\ndirection: source-to-target\npath: none\n"
    );
    assert!(!err.contains("hops:"));
    assert!(stdout.is_empty(), "{}", text(&stdout));
}

#[test]
fn graph_path__same_id_with_self_edge__zero_hops() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a")],
        vec![edge("a", "a", "calls", 0.5, "self")],
    );
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "path", "--from", "a", "--to", "a"]);
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert!(err.ends_with("path: yes\nhops: 0\n"), "{err}");
    assert!(stdout.is_empty(), "{}", text(&stdout));
}

#[test]
fn graph_path__same_id_with_relation__ignores_edges() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a")],
        vec![edge("a", "a", "calls", 0.5, "self")],
    );
    let (stdout, stderr, code) = run(
        tmp.path(),
        &[
            "graph",
            "path",
            "--from",
            "a",
            "--to",
            "a",
            "--relation",
            "missing-rel",
        ],
    );
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert!(err.ends_with("path: yes\nhops: 0\n"), "{err}");
    assert!(stdout.is_empty(), "{}", text(&stdout));
}

#[test]
fn graph_path__missing_identity__exit_1() {
    let tmp = git_repo();
    seed(tmp.path(), vec![node("a")], Vec::new());
    let (stdout, stderr, code) = run(
        tmp.path(),
        &["graph", "path", "--from", "missing", "--to", "missing"],
    );
    let err = text(&stderr);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("entity not in graph: missing"), "{err}");
    assert!(!err.contains("path: yes"), "{err}");
    assert!(!err.contains("source: Cozo"), "{err}");
    assert!(stdout.is_empty(), "{}", text(&stdout));
}

#[test]
fn graph_path__missing_goal__names_that_id() {
    let tmp = git_repo();
    seed(tmp.path(), vec![node("a")], Vec::new());
    let (stdout, stderr, code) = run(
        tmp.path(),
        &["graph", "path", "--from", "a", "--to", "ghost"],
    );
    let err = text(&stderr);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("entity not in graph: ghost"), "{err}");
    assert!(!err.contains("path: yes"), "{err}");
    assert!(stdout.is_empty());
}

#[test]
fn graph_path__no_storage__storage_not_initialized() {
    let tmp = git_repo();
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "path", "--from", "a", "--to", "b"]);
    let err = text(&stderr);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("Storage not initialized"), "{err}");
    assert!(!text(&stdout).contains("source: Cozo"));
}

#[test]
fn graph_path__two_relations__calls_then_depends_on() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a"), node("b")],
        vec![
            edge("a", "b", "depends_on", 0.25, "d"),
            edge("a", "b", "calls", 0.5, "c"),
        ],
    );
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "path", "--from", "a", "--to", "b"]);
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert!(err.ends_with("hops: 1\n"), "{err}");
    assert_eq!(
        text(&stdout),
        "1\ta\tcalls\tb\t0.5\tc\n1\ta\tdepends_on\tb\t0.25\td\n"
    );
}

#[test]
fn graph_path__relation_filter__only_calls() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a"), node("b")],
        vec![
            edge("a", "b", "depends_on", 0.25, "d"),
            edge("a", "b", "calls", 0.5, "c"),
        ],
    );
    let (stdout, stderr, code) = run(
        tmp.path(),
        &[
            "graph",
            "path",
            "--from",
            "a",
            "--to",
            "b",
            "--relation",
            "calls",
        ],
    );
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("hops: 1"), "{err}");
    assert_eq!(text(&stdout), "1\ta\tcalls\tb\t0.5\tc\n");
}

#[test]
fn graph_path__relation_filter__changes_route() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a"), node("b"), node("d")],
        vec![
            edge("a", "d", "depends_on", 0.25, "direct"),
            edge("a", "b", "calls", 0.5, "ab"),
            edge("b", "d", "calls", 0.5, "bd"),
        ],
    );
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "path", "--from", "a", "--to", "d"]);
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert!(err.ends_with("hops: 1\n"), "{err}");
    assert_eq!(text(&stdout), "1\ta\tdepends_on\td\t0.25\tdirect\n");

    let (stdout, stderr, code) = run(
        tmp.path(),
        &[
            "graph",
            "path",
            "--from",
            "a",
            "--to",
            "d",
            "--relation",
            "calls",
        ],
    );
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    assert!(err.ends_with("hops: 2\n"), "{err}");
    let body = text(&stdout);
    assert_eq!(body, "1\ta\tcalls\tb\t0.5\tab\n2\tb\tcalls\td\t0.5\tbd\n");
    assert!(!body.contains("depends_on"), "{body}");
}

#[test]
fn graph_path__quote_in_id__exit_0() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a'b"), node("z")],
        vec![edge("a'b", "z", "calls", 0.5, "p")],
    );
    let (stdout, stderr, code) = run(tmp.path(), &["graph", "path", "--from", "a'b", "--to", "z"]);
    let err = text(&stderr);
    assert_eq!(code, 0, "{err}");
    let body = text(&stdout);
    assert!(body.contains("a'b"), "{body}");
    assert_eq!(body, "1\ta'b\tcalls\tz\t0.5\tp\n");
}

#[test]
fn graph_path__equal_paths__byte_identical_winner() {
    let tmp = git_repo();
    seed(
        tmp.path(),
        vec![node("a"), node("b"), node("c"), node("d")],
        vec![
            edge("a", "b", "calls", 0.5, "ab"),
            edge("b", "d", "calls", 0.5, "bd"),
            edge("a", "c", "calls", 0.5, "ac"),
            edge("c", "d", "calls", 0.5, "cd"),
        ],
    );
    let args = ["graph", "path", "--from", "a", "--to", "d"];
    let (first_out, first_err, first_code) = run(tmp.path(), &args);
    let (second_out, second_err, second_code) = run(tmp.path(), &args);
    assert_eq!(first_code, 0, "{}", text(&first_err));
    assert_eq!(second_code, 0, "{}", text(&second_err));
    assert_eq!(first_out, second_out);
    assert_eq!(first_err, second_err);
    assert_eq!(
        text(&first_out),
        "1\ta\tcalls\tb\t0.5\tab\n2\tb\tcalls\td\t0.5\tbd\n"
    );
    assert!(text(&first_err).ends_with("hops: 2\n"));
}

#[test]
fn graph_path__missing_flags__exit_2() {
    let tmp = git_repo();
    let (_, _, code) = run(tmp.path(), &["graph", "path"]);
    assert_eq!(code, 2);
}
