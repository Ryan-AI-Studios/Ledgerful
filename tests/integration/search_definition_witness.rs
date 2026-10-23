//! Identifier-likely hybrid seats one definition witness and keeps later hits.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::common::{run_cli_env, setup_git_repo};

const TIMING_ENV: &[(&str, &str)] = &[("LEDGERFUL_TIMING_MIN_SPAN_MS", "0")];

const ENVELOPE_KEYS: &[&str] = &[
    "schemaVersion",
    "query",
    "mode",
    "limit",
    "truncated",
    "resultCount",
    "results",
    "searchIndexStatus",
    "semantic",
    "fallbackUsed",
];

const HIT_KEYS: &[&str] = &[
    "kind",
    "path",
    "line",
    "score",
    "content",
    "engine",
    "previewUnavailableReason",
];

const PKG_WIDGET: &str = "\
class Widget {
    constructor(name) {
        this.name = name;
    }
    widget_open(source) {
        return source;
    }
}
";

const WIDGET_DTS: &str = "\
export interface Widget {
    widget_open(source: Widget): void;
}
";

const WIDGET_TEST: &str = "\
test(\"opens\", () => {
    w.widget_open(source);
    w.widget_open(source);
    w.widget_open(source);
    constructor(name) {
        return name;
    }
});
";

const HELPER: &str = "\
function widget_open() {
    widget_open();
    widget_open();
}
";

const EXAMPLES: &str = "\
obj.widget_open(source);
obj.widget_open(source);
";

const KEEPER: &str = "\
pub fn keeper_slot() {
    let _ = 1;
}
";

const KEEPER_TEST: &str = "\
#[test]
fn keeper_slot() {
    keeper_slot();
    keeper_slot();
    keeper_slot();
}
";

fn run_search(root: &Path, args: &[&str]) -> (String, String, i32) {
    run_cli_env(root, args, TIMING_ENV)
}

fn parse_envelope(stdout: &str, stderr: &str, code: i32) -> Value {
    assert_eq!(
        code, 0,
        "search failed code {code}\nstdout={stdout}\nstderr={stderr}"
    );
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("envelope parse {err}\nstdout={stdout}\nstderr={stderr}"))
}

fn assert_known_keys(env: &Value) {
    let object = env.as_object().expect("envelope object");
    for key in object.keys() {
        assert!(
            ENVELOPE_KEYS.contains(&key.as_str()),
            "unexpected envelope key {key} in {env}"
        );
    }
    assert!(
        object.get("fallbackUsed").is_none(),
        "witness search must not set fallbackUsed: {env}"
    );
    for hit in env["results"].as_array().expect("results") {
        for key in hit.as_object().expect("hit").keys() {
            assert!(
                HIT_KEYS.contains(&key.as_str()),
                "unexpected hit key {key} in {hit}"
            );
        }
    }
}

fn norm_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn hit_paths_lines_kinds(env: &Value) -> Vec<(String, Option<u64>, String)> {
    env["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|hit| {
            (
                norm_path(hit["path"].as_str().expect("path")),
                hit.get("line").and_then(Value::as_u64),
                hit["kind"].as_str().expect("kind").to_string(),
            )
        })
        .collect()
}

fn write_fixture(root: &Path) {
    let files = [
        ("pkg/widget.js", PKG_WIDGET),
        ("types/widget.d.ts", WIDGET_DTS),
        ("tests/widget_open.test.js", WIDGET_TEST),
        ("tests/helper.js", HELPER),
        ("examples/use_widget.js", EXAMPLES),
        ("src/keeper.rs", KEEPER),
        ("tests/keeper.rs", KEEPER_TEST),
    ];
    for (rel, body) in files {
        let full = root.join(rel);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        fs::write(full, body).expect("write");
    }
}

fn assert_witness_shape(env: &Value, path_suffix: &str, content_sub: &str) {
    assert_eq!(env["schemaVersion"], 1);
    assert_eq!(env["mode"], "hybrid");
    assert_known_keys(env);
    let hits = env["results"].as_array().expect("results");
    assert!(
        hits.len() >= 2,
        "limit 5 keeps a later hit after the witness: {env}"
    );
    let first = &hits[0];
    let path = norm_path(first["path"].as_str().expect("path"));
    assert!(
        path.ends_with(path_suffix),
        "first path {path} should end with {path_suffix}: {env}"
    );
    let content = first["content"].as_str().expect("content");
    assert!(
        content.contains(content_sub),
        "first content {content:?} should contain {content_sub}: {env}"
    );
}

#[test]
fn search_definition_witness_orders_identifier_hybrid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);
    write_fixture(root);
    let (stdout, stderr, code) = run_search(root, &["init"]);
    assert_eq!(code, 0, "init failed\nstdout={stdout}\nstderr={stderr}");

    let (stdout, stderr, code) =
        run_search(root, &["search", "widget_open", "--json", "--limit", "5"]);
    let widget = parse_envelope(&stdout, &stderr, code);
    assert_witness_shape(&widget, "pkg/widget.js", "widget_open(source) {");
    let first = &widget["results"][0];
    assert_eq!(first["kind"], "regex_match");
    let score = first["score"].as_f64().expect("score");
    assert!(
        score >= 15.0,
        "pkg/widget.js boost is 15 or a kept higher score, got {score}: {widget}"
    );
    let later_call = widget["results"]
        .as_array()
        .expect("results")
        .iter()
        .skip(1)
        .any(|hit| {
            let path = norm_path(hit["path"].as_str().unwrap_or(""));
            path.contains("tests/") || path.contains("examples/")
        });
    assert!(later_call, "later test or example hit missing: {widget}");

    let (stdout, stderr, code) =
        run_search(root, &["search", "constructor", "--json", "--limit", "5"]);
    let constructor = parse_envelope(&stdout, &stderr, code);
    assert_witness_shape(&constructor, "pkg/widget.js", "constructor(name)");

    let (stdout, stderr, code) =
        run_search(root, &["search", "keeper_slot", "--json", "--limit", "5"]);
    let keeper = parse_envelope(&stdout, &stderr, code);
    assert_witness_shape(&keeper, "src/keeper.rs", "keeper_slot");
    let keeper_later = keeper["results"]
        .as_array()
        .expect("results")
        .iter()
        .skip(1)
        .any(|hit| norm_path(hit["path"].as_str().unwrap_or("")).ends_with("tests/keeper.rs"));
    assert!(
        keeper_later,
        "later keeper hit should be the test file: {keeper}"
    );

    let (stdout, stderr, code) =
        run_search(root, &["search", "widget open", "--json", "--limit", "5"]);
    let spaced = parse_envelope(&stdout, &stderr, code);
    assert_eq!(spaced["schemaVersion"], 1);
    assert_eq!(spaced["mode"], "bm25");
    assert_known_keys(&spaced);

    let (stdout, stderr, code) = run_search(
        root,
        &["search", "widget_open", "--regex", "--json", "--limit", "1"],
    );
    let regex_one = parse_envelope(&stdout, &stderr, code);
    assert_eq!(regex_one["schemaVersion"], 1);
    assert_eq!(regex_one["mode"], "regex");
    assert_eq!(regex_one["results"].as_array().map(Vec::len), Some(1));
    assert_known_keys(&regex_one);

    let (stdout, stderr, code) =
        run_search(root, &["search", "widget_open", "--json", "--limit", "5"]);
    let widget_again = parse_envelope(&stdout, &stderr, code);
    assert_eq!(
        hit_paths_lines_kinds(&widget),
        hit_paths_lines_kinds(&widget_again),
        "two processes must agree on path, line, and kind"
    );

    let (stdout, stderr, code) = run_search(root, &["search", "widget_open", "--limit", "5"]);
    assert_eq!(
        code, 0,
        "human search failed code {code}\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("[Search Mode: Hybrid]"),
        "human mode line missing: {stdout}"
    );
    let first_hit = stdout
        .lines()
        .find(|line| line.contains("[Regex]") || line.contains("[BM25]"));
    let first_hit = first_hit.expect("human hit line");
    assert!(
        first_hit.contains("pkg/widget.js"),
        "first human hit should name pkg/widget.js: {first_hit}\n{stdout}"
    );
}
