//! Ranked and regex search must return the fixture hits. Wall-clock limits
//! live in the ignored release gate, not in this CI test (0415).

use std::fs;
use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use crate::common::{run_cli_env, setup_git_repo};

const FIXTURE: &str = "\
pub struct TantivySearchEngine;
pub fn search_gate() {}
let alpha_beta = 1;
let a = b;
";

const TIMING_ENV: &[(&str, &str)] = &[("LEDGERFUL_TIMING_MIN_SPAN_MS", "0")];

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

fn path_suffix_ok(path: &str) -> bool {
    path.replace('\\', "/").ends_with("src/lib.rs")
}

fn assert_hit(env: &Value, kind: &str, content_sub: &str) {
    let hits = env["results"].as_array().expect("results array");
    let matched = hits.iter().any(|hit| {
        hit["kind"].as_str() == Some(kind)
            && hit["path"].as_str().is_some_and(path_suffix_ok)
            && hit["line"].as_u64().is_some()
            && hit["content"]
                .as_str()
                .is_some_and(|content| content.contains(content_sub))
    });
    assert!(
        matched,
        "missing {kind} hit containing {content_sub}: {env}"
    );
}

fn assert_no_index_status(env: &Value) {
    assert!(
        env.get("searchIndexStatus").is_none(),
        "warm search must omit searchIndexStatus: {env}"
    );
}

#[test]
fn search_ranked_and_regex_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), FIXTURE).unwrap();
    let (stdout, stderr, code) = run_search(root, &["init"]);
    assert_eq!(code, 0, "init failed\nstdout={stdout}\nstderr={stderr}");

    let (stdout, stderr, code) =
        run_search(root, &["search", "alpha beta", "--json", "--limit", "10"]);
    let _prewarm = parse_envelope(&stdout, &stderr, code);

    let (stdout, stderr, code) =
        run_search(root, &["search", "alpha beta", "--json", "--limit", "10"]);
    let bm25 = parse_envelope(&stdout, &stderr, code);
    assert_eq!(bm25["schemaVersion"], 1);
    assert_eq!(bm25["mode"], "bm25");
    assert_eq!(bm25["truncated"], false);
    assert_eq!(bm25["resultCount"], 1);
    assert_no_index_status(&bm25);
    assert_hit(&bm25, "bm25_match", "alpha");

    let (stdout, stderr, code) = run_search(
        root,
        &[
            "search",
            "pub fn.*\\{",
            "--regex",
            "--json",
            "--limit",
            "10",
        ],
    );
    let regex_filtered = parse_envelope(&stdout, &stderr, code);
    assert_eq!(regex_filtered["schemaVersion"], 1);
    assert_eq!(regex_filtered["mode"], "regex");
    assert_no_index_status(&regex_filtered);
    assert_hit(&regex_filtered, "regex_match", "search_gate");

    let (stdout, stderr, code) = run_search(
        root,
        &["search", "a = b", "--regex", "--json", "--limit", "10"],
    );
    let regex_all_paths = parse_envelope(&stdout, &stderr, code);
    assert_eq!(regex_all_paths["schemaVersion"], 1);
    assert_eq!(regex_all_paths["mode"], "regex");
    assert_no_index_status(&regex_all_paths);
    assert_hit(&regex_all_paths, "regex_match", "a = b");

    let (stdout, stderr, code) = run_search(
        root,
        &["search", "TantivySearchEngine", "--json", "--limit", "10"],
    );
    let hybrid = parse_envelope(&stdout, &stderr, code);
    assert_eq!(hybrid["schemaVersion"], 1);
    assert_eq!(hybrid["mode"], "hybrid");
    assert_no_index_status(&hybrid);
    let hybrid_hit = hybrid["results"]
        .as_array()
        .expect("results")
        .iter()
        .any(|hit| hit["path"].as_str().is_some_and(path_suffix_ok));
    assert!(hybrid_hit, "hybrid hit missing: {hybrid}");

    let (stdout, stderr, code) = run_search(
        root,
        &[
            "search",
            "zzzzqqqqnomatchtokennn yyyywwwwabsentphraseee",
            "--json",
            "--limit",
            "10",
        ],
    );
    let empty = parse_envelope(&stdout, &stderr, code);
    assert_eq!(empty["schemaVersion"], 1);
    assert_eq!(empty["resultCount"], 0);
    assert_eq!(empty["results"].as_array().map(Vec::len), Some(0));
    assert_no_index_status(&empty);

    let (stdout, stderr, code) =
        run_search(root, &["search", "[", "--regex", "--json", "--limit", "10"]);
    assert_eq!(
        code, 1,
        "bad regex must exit 1\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stderr.trim().is_empty(),
        "bad regex stderr empty\nstdout={stdout}"
    );

    let (stdout, stderr, code) = run_search(
        root,
        &["timings", "--inner", "--command", "search", "--json"],
    );
    assert_eq!(
        code, 0,
        "fixture timings failed\nstdout={stdout}\nstderr={stderr}"
    );
    let timings: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("timings json {err}\nstdout={stdout}\nstderr={stderr}"));
    let has_lexical = timings["data"].as_array().is_some_and(|rows| {
        rows.iter()
            .any(|row| row["span_name"].as_str() == Some("lexical_query"))
    });
    assert!(
        has_lexical,
        "fixture worktree timings missing lexical_query: {timings}"
    );
}

#[ignore = "0415 release CLI cold sample; cargo test --release --test integration -- --ignored"]
#[test]
fn search_perf_cli_cold_sample() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), FIXTURE).unwrap();
    let (stdout, stderr, code) = run_search(root, &["init"]);
    assert_eq!(code, 0, "init failed\nstdout={stdout}\nstderr={stderr}");

    let (stdout, stderr, code) =
        run_search(root, &["search", "alpha beta", "--json", "--limit", "10"]);
    let _prewarm = parse_envelope(&stdout, &stderr, code);

    let started = Instant::now();
    let (stdout, stderr, code) =
        run_search(root, &["search", "alpha beta", "--json", "--limit", "10"]);
    let cli_cold_ms = started.elapsed().as_millis();
    let env = parse_envelope(&stdout, &stderr, code);
    assert_hit(&env, "bm25_match", "alpha");
    println!("{{\"schemaVersion\":1,\"kind\":\"searchPerfCli\",\"cliColdMs\":{cli_cold_ms}}}");
}
