use serde_json::Value;
use std::fs;
use tempfile::tempdir;

use crate::common::{git_add_and_commit, run_cli, setup_git_repo};

fn write_ledger_fixtures(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::create_dir_all(root.join("tests/fixtures")).expect("fixtures dir");
    fs::write(root.join("src/ledger.rs"), "fn ledger_commit() {}\n").expect("product file");
    fs::write(
        root.join("tests/fixtures/checksum.sha256"),
        "ledger checksum body\n",
    )
    .expect("fixture file");
    git_add_and_commit(root, "ledger fixtures");
}

fn index_tantivy(root: &std::path::Path) {
    let (out, err, code) = run_cli(root, &["search", "hello", "--index"]);
    assert_eq!(code, 0, "search --index; stderr={err}; stdout={out}");
}

fn clap_limit_refuse_text(combined: &str) -> bool {
    combined.contains("invalid value") || combined.contains("is not in 1")
}

#[test]
fn cli_search_trigrams_led_ger_json_envelope() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);
    write_ledger_fixtures(root);
    index_tantivy(root);

    let (stdout, stderr, code) = run_cli(
        root,
        &["search-trigrams", "--json", "led", "ger", "--limit", "3"],
    );
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    assert!(
        stderr.trim().is_empty(),
        "success --json stderr must be empty: {stderr}"
    );
    let env: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    assert_eq!(env["schemaVersion"], 1);
    assert_eq!(env["kind"], "searchTrigrams");
    assert_eq!(
        env["query"],
        serde_json::json!(["led", "ger"]),
        "query is argv array not 0136 string"
    );
    assert_eq!(env["accepted"], serde_json::json!(["led", "ger"]));
    assert_eq!(env["rejected"], serde_json::json!([]));
    assert_eq!(env["limit"], 3);
    let result_count = env["resultCount"].as_u64().expect("resultCount");
    let total_matching = env["totalMatching"].as_u64().expect("totalMatching");
    assert_eq!(
        result_count,
        env["results"].as_array().expect("results").len() as u64
    );
    assert!(total_matching >= 2, "totalMatching={total_matching}");
    assert_eq!(env["truncated"], result_count < total_matching);
    assert!(
        env["documentCount"].as_u64().expect("documentCount") >= 2,
        "documentCount={}",
        env["documentCount"]
    );
    assert!(
        env.get("emptyReason").is_none(),
        "populated omits emptyReason: {env}"
    );
    assert_eq!(env["next"], "ledgerful search led ger");
    let paths: Vec<&str> = env["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|r| r["path"].as_str())
        .collect();
    assert!(
        paths.iter().any(|p| p.contains("src/ledger.rs")),
        "product path missing: {paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|p| p.contains("tests/fixtures/checksum.sha256")),
        "fixture path missing: {paths:?}"
    );
    for hit in env["results"].as_array().expect("results") {
        assert!(hit.get("line").is_none(), "no line: {hit}");
        assert!(hit.get("content").is_none(), "no content: {hit}");
        let score = hit["score"].as_f64().expect("score");
        assert!(score > 0.0 && score.is_finite(), "score={score}");
        let path = hit["path"].as_str().expect("path");
        assert!(!path.contains('\\'), "slash-normalized: {path}");
    }

    let (stdout_q, stderr_q, code_q) = run_cli(
        root,
        &[
            "--quiet",
            "search-trigrams",
            "--json",
            "led",
            "ger",
            "--limit",
            "3",
        ],
    );
    assert_eq!(code_q, 0, "stderr={stderr_q}; stdout={stdout_q}");
    assert!(
        stderr_q.trim().is_empty(),
        "--json --quiet stderr must be empty: {stderr_q}"
    );

    let (stdout1, stderr1, code1) = run_cli(
        root,
        &["search-trigrams", "--json", "led", "ger", "--limit", "1"],
    );
    assert_eq!(code1, 0, "stderr={stderr1}; stdout={stdout1}");
    let env1: Value = serde_json::from_str(stdout1.trim()).expect("limit1 envelope");
    assert_eq!(env1["resultCount"], 1);
    assert_eq!(env1["truncated"], true);
    assert!(
        env1["totalMatching"].as_u64().expect("totalMatching") >= 2,
        "totalMatching={}",
        env1["totalMatching"]
    );
}

#[test]
fn cli_search_trigrams_human_paths_on_stdout_diag_on_stderr() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);
    write_ledger_fixtures(root);
    index_tantivy(root);

    let (stdout, stderr, code) = run_cli(root, &["search-trigrams", "led", "ger", "--limit", "3"]);
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    assert!(
        stdout.contains("src/ledger.rs"),
        "product path on stdout: {stdout}"
    );
    assert!(
        stdout.contains("tests/fixtures/checksum.sha256"),
        "fixture path on stdout: {stdout}"
    );
    assert!(
        !stdout.contains("search-trigrams:"),
        "diagnostic must not be on stdout: {stdout}"
    );
    assert!(
        stderr.contains("search-trigrams:"),
        "diagnostic prefix: {stderr}"
    );
    assert!(
        stderr.contains("Ordinary search: ledgerful search led ger"),
        "runnable route: {stderr}"
    );
}

#[test]
fn cli_search_trigrams_empty_index_next_is_index() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "unindexed\n").expect("readme");
    git_add_and_commit(root, "unindexed readme");

    let (stdout, stderr, code) = run_cli(root, &["search-trigrams", "--json", "led", "ger"]);
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    assert!(stderr.trim().is_empty(), "json stderr: {stderr}");
    let env: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    assert_eq!(env["emptyReason"], "emptyIndex");
    assert_eq!(env["next"], "ledgerful index");
    assert_eq!(env["resultCount"], 0);
}

#[test]
fn cli_search_trigrams_empty_query_omits_next() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);

    let (stdout, stderr, code) = run_cli(root, &["search-trigrams", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    assert!(stderr.trim().is_empty(), "json stderr: {stderr}");
    let env: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    assert_eq!(env["emptyReason"], "emptyQuery");
    assert!(env.get("next").is_none(), "next omitted: {env}");
    assert_eq!(env["kind"], "searchTrigrams");
}

#[test]
fn cli_search_trigrams_limit_zero_rejected_by_clap() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);

    let (stdout, stderr, code) = run_cli(root, &["search-trigrams", "--limit", "0", "led"]);
    assert_eq!(code, 2, "limit 0 clap exit; stderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    let combined = format!("{stderr}{stdout}");
    assert!(clap_limit_refuse_text(&combined), "clap refuse: {combined}");

    let (stdout_j, stderr_j, code_j) =
        run_cli(root, &["search-trigrams", "--limit", "0", "--json", "led"]);
    assert_eq!(code_j, 2, "json limit 0 clap exit; stderr={stderr_j}");
    assert!(stdout_j.is_empty(), "json stdout={stdout_j}");
    let combined_j = format!("{stderr_j}{stdout_j}");
    assert!(
        clap_limit_refuse_text(&combined_j),
        "clap refuse json: {combined_j}"
    );
}

#[test]
fn cli_search_trigrams_limit_exceeds_cap_refuses() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);

    let (stdout, stderr, code) = run_cli(root, &["search-trigrams", "--limit", "5001", "led"]);
    assert_eq!(code, 2, "5001 clap exit; stderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    let combined = format!("{stderr}{stdout}");
    assert!(
        clap_limit_refuse_text(&combined) || combined.contains("5001"),
        "cap refuse: {combined}"
    );

    let (stdout_j, _stderr_j, code_j) = run_cli(
        root,
        &["search-trigrams", "--limit", "5001", "--json", "led"],
    );
    assert_eq!(code_j, 2, "json 5001 clap exit");
    assert!(stdout_j.is_empty(), "json stdout={stdout_j}");
}

#[test]
fn cli_search_trigrams_mixed_accept_reject_human() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);
    write_ledger_fixtures(root);
    index_tantivy(root);

    let (stdout, stderr, code) = run_cli(root, &["search-trigrams", "led", "ledger"]);
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    assert!(
        stdout.contains("src/ledger.rs") || !stdout.trim().is_empty(),
        "search still ran: stdout={stdout}"
    );
    assert!(
        stderr.contains(" rejected: ledger. Ordinary search:"),
        "stderr={stderr}"
    );
}

#[test]
fn cli_search_trigrams_no_matches_json() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src/hello.rs"), "fn hello_world() {}\n").expect("hello");
    git_add_and_commit(root, "hello only");
    index_tantivy(root);

    let (stdout, stderr, code) = run_cli(root, &["search-trigrams", "--json", "zzz", "qqq"]);
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    assert!(stderr.trim().is_empty(), "json stderr: {stderr}");
    let env: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    assert_eq!(env["emptyReason"], "noMatches");
    assert!(
        env["documentCount"].as_u64().expect("documentCount") > 0,
        "documentCount={}",
        env["documentCount"]
    );
    assert_eq!(env["next"], "ledgerful search zzz qqq");
    assert_eq!(env["resultCount"], 0);
}

#[test]
fn cli_search_trigrams_invalid_trigrams_omits_next() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);

    let (stdout, stderr, code) = run_cli(root, &["search-trigrams", "--json", "ledger"]);
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    assert!(stderr.trim().is_empty(), "json stderr: {stderr}");
    let env: Value = serde_json::from_str(stdout.trim()).expect("envelope");
    assert_eq!(env["emptyReason"], "invalidTrigrams");
    assert!(env.get("next").is_none(), "next omitted: {env}");
    assert_eq!(env["rejected"], serde_json::json!(["ledger"]));
}
