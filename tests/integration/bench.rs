#![allow(non_snake_case)] // project test naming: feature__condition__expected
//! Smoke tests for `ledgerful bench` (0477).
//!
//! Parent cwd is the engine (`CARGO_MANIFEST_DIR`). `--fixture` is a temp git
//! repo. Do not `run_cli(fixture, …)` as the parent — that fails the sha gate.

use crate::common::{git_add_and_commit, setup_git_repo};
use rstest::rstest;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

fn engine_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn run_bench_parent(args: &[&str]) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(engine_dir())
        .env_remove("LEDGERFUL_STATE_DIR")
        .output()
        .expect("failed to run ledgerful bench");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn tiny_fixture(dir: &Path) {
    setup_git_repo(dir);
    fs::write(dir.join("lib.rs"), "pub fn hello() -> u8 { 1 }\n").expect("write fixture source");
    git_add_and_commit(dir, "init fixture");
}

#[test]
fn bench_smoke_or_unknown_sha_refuse() {
    let tmp = tempfile::tempdir().expect("tempdir");
    tiny_fixture(tmp.path());
    let out_json = tmp.path().join("bench.json");
    let fixture = tmp.path().to_string_lossy().into_owned();
    let output = out_json.to_string_lossy().into_owned();
    let (stdout, stderr, code) = run_bench_parent(&[
        "bench",
        "--fixture",
        &fixture,
        "--runs",
        "3",
        "--json",
        "--output",
        &output,
    ]);

    let embed = env!("LEDGERFUL_GIT_SHA");
    if embed == "unknown" {
        assert_eq!(code, 1, "unknown embed must refuse: stderr={stderr}");
        assert!(
            !stdout.contains("workloadBench"),
            "refuse must not emit workloadBench JSON: {stdout}"
        );
        return;
    }

    assert_eq!(
        code, 0,
        "bench smoke failed: stderr={stderr}\nstdout={stdout}"
    );
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("workloadBench JSON");
    assert_eq!(parsed["kind"], "workloadBench");
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["workload"], "index-cold");
    assert_eq!(parsed["n"], 3);
    assert_eq!(parsed["startState"], "cold");
    assert_eq!(parsed["comparable"], true);
    let samples = parsed["samplesMs"]
        .as_array()
        .expect("samplesMs array")
        .iter()
        .map(|v| v.as_i64().expect("sample i64"))
        .collect::<Vec<_>>();
    assert_eq!(samples.len(), 3);
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    assert_eq!(samples, sorted, "samplesMs must be sorted ascending");
    assert!(out_json.is_file(), " --output must write JSON");

    let (c_stdout, c_stderr, c_code) =
        run_bench_parent(&["bench", "--compare", &output, &output, "--json"]);
    assert_eq!(
        c_code, 0,
        "compare of two copies must succeed: stderr={c_stderr}\nstdout={c_stdout}"
    );
    let cmp: Value = serde_json::from_str(c_stdout.trim()).expect("compare JSON");
    assert_eq!(cmp["kind"], "workloadBenchCompare");
    assert_eq!(cmp["deltaP50Ms"], 0);
    assert_eq!(cmp["deltaP95Ms"], 0);
    assert_eq!(cmp["comparable"], true);

    let default_out = engine_dir()
        .join("output")
        .join("bench")
        .join(format!("{embed}-index-cold.json"));
    let (d_stdout, d_stderr, d_code) =
        run_bench_parent(&["bench", "--fixture", &fixture, "--runs", "1", "--json"]);
    assert_eq!(
        d_code, 0,
        "default output path run failed: stderr={d_stderr}\nstdout={d_stdout}"
    );
    assert!(
        default_out.is_file(),
        "default output must land under engine output/bench: {}",
        default_out.display()
    );

    let porcelain = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(engine_dir())
        .output()
        .expect("git status");
    assert!(porcelain.status.success());
    let tracked = String::from_utf8_lossy(&porcelain.stdout);
    assert!(
        !tracked
            .lines()
            .any(|l| l.starts_with("?? output/bench") || l.contains("output/bench")),
        "engine porcelain must not list default bench output as tracked: {tracked}"
    );
    let _ = fs::remove_file(&default_out);
}

#[test]
fn bench_child_ignores_inherited_state_dir() {
    let embed = env!("LEDGERFUL_GIT_SHA");
    if embed == "unknown" {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    tiny_fixture(tmp.path());
    let override_dir = tempfile::tempdir().expect("override state");
    let sentinel = override_dir.path().join("SENTINEL");
    fs::write(&sentinel, "keep").expect("write sentinel");
    let out_json = tmp.path().join("bench.json");
    let fixture = tmp.path().to_string_lossy().into_owned();
    let output = out_json.to_string_lossy().into_owned();
    let override_state = override_dir.path().to_string_lossy().into_owned();
    let cmd_out = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args([
            "bench",
            "--fixture",
            &fixture,
            "--runs",
            "1",
            "--json",
            "--output",
            &output,
        ])
        .current_dir(engine_dir())
        .env("LEDGERFUL_STATE_DIR", &override_state)
        .output()
        .expect("run bench with inherited STATE_DIR");
    let stdout = String::from_utf8_lossy(&cmd_out.stdout);
    let stderr = String::from_utf8_lossy(&cmd_out.stderr);
    assert_eq!(
        cmd_out.status.code().unwrap_or(-1),
        0,
        "bench must succeed with inherited STATE_DIR removed from child: stderr={stderr}\nstdout={stdout}"
    );
    assert_eq!(
        fs::read_to_string(&sentinel).expect("read sentinel"),
        "keep",
        "child must not write into inherited LEDGERFUL_STATE_DIR"
    );
    assert!(
        !override_dir.path().join("state").join("ledger.db").exists(),
        "inherited STATE_DIR must not receive the child index database"
    );
}

#[test]
fn bench_sha_mismatch_refuses_json() {
    let embed = env!("LEDGERFUL_GIT_SHA");
    if embed == "unknown" {
        return;
    }
    let engine_tmp = tempfile::tempdir().expect("engine tmp");
    tiny_fixture(engine_tmp.path());
    let fixture_tmp = tempfile::tempdir().expect("fixture tmp");
    tiny_fixture(fixture_tmp.path());
    let other = engine_tmp.path().to_string_lossy().into_owned();
    let fixture = fixture_tmp.path().to_string_lossy().into_owned();
    let (stdout, stderr, code) = run_bench_parent(&[
        "-C",
        &other,
        "bench",
        "--fixture",
        &fixture,
        "--runs",
        "1",
        "--json",
    ]);
    assert_eq!(code, 1, "sha mismatch must exit 1: stderr={stderr}");
    assert!(
        !stdout.contains("workloadBench"),
        "sha mismatch must not emit JSON: {stdout}"
    );
    assert!(
        stderr.contains("sha gate") || stderr.contains("embed"),
        "stderr should name the sha gate: {stderr}"
    );
}

#[test]
fn bench_refuses_engine_as_fixture() {
    let embed = env!("LEDGERFUL_GIT_SHA");
    if embed == "unknown" {
        return;
    }
    let engine = engine_dir().to_string_lossy().into_owned();
    let (stdout, stderr, code) =
        run_bench_parent(&["bench", "--fixture", &engine, "--runs", "1", "--json"]);
    assert_eq!(code, 1, "engine as --fixture must exit 1: stderr={stderr}");
    assert!(
        !stdout.contains("workloadBench"),
        "must not emit JSON when fixture is engine: {stdout}"
    );
}

fn write_workload_bench(path: &Path, os: &str, triple: &str, omit: Option<&str>) {
    let mut keys = vec![
        ("schemaVersion", "1"),
        ("kind", "workloadBench"),
        ("workload", "index-cold"),
        ("buildSha", "abc"),
        ("p50Ms", "10"),
        ("p95Ms", "20"),
        ("fixtureId", "1:abc"),
        ("os", os),
        ("targetTriple", triple),
    ];
    if let Some(drop) = omit {
        keys.retain(|(name, _)| *name != drop);
    }
    let body = keys
        .into_iter()
        .map(|(name, value)| {
            if name == "schemaVersion" || name == "p50Ms" || name == "p95Ms" {
                format!("\"{name}\":{value}")
            } else {
                format!("\"{name}\":\"{value}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    fs::write(path, format!("{{{body}}}\n")).expect("write bench json");
}

#[test]
fn bench_compare_refuses_os_or_target_triple_mismatch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = tmp.path().join("a.json");
    let b_os = tmp.path().join("b-os.json");
    let b_triple = tmp.path().join("b-triple.json");
    write_workload_bench(&a, "windows", "x86_64-pc-windows-msvc", None);
    write_workload_bench(&b_os, "linux", "x86_64-pc-windows-msvc", None);
    write_workload_bench(&b_triple, "windows", "aarch64-unknown-linux-gnu", None);
    let a_s = a.to_string_lossy().into_owned();
    let b_os_s = b_os.to_string_lossy().into_owned();
    let b_triple_s = b_triple.to_string_lossy().into_owned();

    let (stdout, stderr, code) = run_bench_parent(&["bench", "--compare", &a_s, &b_os_s, "--json"]);
    assert_eq!(code, 1, "os mismatch must exit 1: stderr={stderr}");
    assert!(
        stdout.trim().is_empty(),
        "os mismatch must write no JSON on stdout: {stdout}"
    );
    assert!(
        stderr.contains("refused (os)"),
        "stderr must name the os refusal: {stderr}"
    );

    let (stdout, stderr, code) =
        run_bench_parent(&["bench", "--compare", &a_s, &b_triple_s, "--json"]);
    assert_eq!(
        code, 1,
        "targetTriple mismatch must exit 1: stderr={stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "targetTriple mismatch must write no JSON on stdout: {stdout}"
    );
    assert!(
        stderr.contains("refused (targetTriple)"),
        "stderr must name the targetTriple refusal: {stderr}"
    );
}

#[rstest]
#[case("os")]
#[case("targetTriple")]
fn bench_compare_refuses_missing_required_field(#[case] omit: &str) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let full = tmp.path().join("full.json");
    let missing = tmp.path().join("missing.json");
    write_workload_bench(&full, "windows", "x86_64-pc-windows-msvc", None);
    write_workload_bench(&missing, "windows", "x86_64-pc-windows-msvc", Some(omit));
    let full_s = full.to_string_lossy().into_owned();
    let missing_s = missing.to_string_lossy().into_owned();
    let (stdout, stderr, code) =
        run_bench_parent(&["bench", "--compare", &full_s, &missing_s, "--json"]);
    assert_eq!(code, 1, "missing {omit} must exit 1: stderr={stderr}");
    assert!(
        stdout.trim().is_empty(),
        "missing {omit} must write no JSON on stdout: {stdout}"
    );
    assert!(
        stderr.contains("failed to parse"),
        "stderr must name the parse failure for {omit}: {stderr}"
    );
}

fn run_bench_parent_with_env(args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    command
        .args(args)
        .current_dir(engine_dir())
        .env_remove("LEDGERFUL_STATE_DIR");
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().expect("failed to run ledgerful bench");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn parse_receipt(stdout: &str) -> Value {
    serde_json::from_str(stdout.trim()).expect("workloadBench JSON")
}

#[test]
fn bench_warm_receipt__fresh_clone__separates_setup_and_records_identity() {
    if env!("LEDGERFUL_GIT_SHA") == "unknown" {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    tiny_fixture(tmp.path());
    let fixture = tmp.path().to_string_lossy().into_owned();
    let out_json = tmp.path().join("warm.json");
    let output = out_json.to_string_lossy().into_owned();

    let (stdout, stderr, code) = run_bench_parent(&[
        "bench",
        "--fixture",
        &fixture,
        "--workload",
        "status-warm",
        "--runs",
        "3",
        "--host-class",
        "local-windows-2026-11",
        "--json",
        "--output",
        &output,
    ]);
    assert_eq!(code, 0, "warm run failed: stderr={stderr}\nstdout={stdout}");

    let receipt = parse_receipt(&stdout);
    assert_eq!(receipt["kind"], "workloadBench");
    assert_eq!(receipt["workload"], "status-warm");
    assert_eq!(receipt["startState"], "warm");
    assert_eq!(receipt["n"], 3);
    assert_eq!(receipt["hostClass"], "local-windows-2026-11");
    assert_eq!(receipt["childArgs"][0], "ledger");
    assert_eq!(receipt["childArgs"][1], "status");
    assert!(
        receipt["setupMs"].as_i64().is_some_and(|ms| ms >= 0),
        "setupMs must be recorded: {receipt}"
    );
    assert!(
        receipt["warmupMs"].as_i64().is_some_and(|ms| ms >= 0),
        "warmupMs must be recorded: {receipt}"
    );
    assert_eq!(receipt["fixtureCommits"], 1);
    assert!(
        receipt["fixtureRootCommit"]
            .as_str()
            .is_some_and(|root| root.len() >= 7),
        "single-root fixture must record its root: {receipt}"
    );
    let fixture_id = receipt["fixtureId"].as_str().expect("fixtureId");
    let count = fixture_id.split_once(':').expect("count:hash").0;
    assert_eq!(receipt["fixtureFiles"].to_string(), count);

    let observed = receipt["observedMs"].as_array().expect("observedMs").len();
    assert_eq!(observed, 3);
    let samples = receipt["samplesMs"].as_array().expect("samplesMs");
    let mut sorted = samples
        .iter()
        .map(|v| v.as_i64().expect("i64"))
        .collect::<Vec<_>>();
    let observed_values = receipt["observedMs"]
        .as_array()
        .expect("observedMs")
        .iter()
        .map(|v| v.as_i64().expect("i64"))
        .collect::<Vec<_>>();
    let mut observed_sorted = observed_values;
    observed_sorted.sort_unstable();
    sorted.sort_unstable();
    assert_eq!(
        sorted, observed_sorted,
        "observedMs must be a permutation of samplesMs"
    );
    assert!(
        receipt["rssMeasuredSamples"].as_u64().unwrap_or(0) <= 3,
        "rss coverage cannot exceed n: {receipt}"
    );
}

#[test]
fn bench_warm_samples__keep_fixture_state__sentinel_survives() {
    if env!("LEDGERFUL_GIT_SHA") == "unknown" {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    tiny_fixture(tmp.path());
    let fixture = tmp.path().to_string_lossy().into_owned();
    let out_json = tmp.path().join("warm.json");
    let output = out_json.to_string_lossy().into_owned();

    let (_, stderr, code) = run_bench_parent(&[
        "bench",
        "--fixture",
        &fixture,
        "--workload",
        "status-warm",
        "--runs",
        "2",
        "--json",
        "--output",
        &output,
    ]);
    assert_eq!(code, 0, "warm run failed: stderr={stderr}");
    let sentinel = tmp.path().join(".ledgerful").join("SENTINEL");
    fs::write(&sentinel, "keep").expect("write sentinel");
    let (_, stderr, code) = run_bench_parent(&[
        "bench",
        "--fixture",
        &fixture,
        "--workload",
        "status-warm",
        "--runs",
        "1",
        "--json",
        "--output",
        &output,
    ]);
    assert_eq!(code, 0, "second warm run failed: stderr={stderr}");
    assert_eq!(
        fs::read_to_string(&sentinel).expect("sentinel survives"),
        "keep",
        "warm samples must not wipe the fixture .ledgerful"
    );
}

#[test]
fn bench_warm_run__adds_no_command_timings_rows() {
    if env!("LEDGERFUL_GIT_SHA") == "unknown" {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    tiny_fixture(tmp.path());
    let fixture = tmp.path().to_string_lossy().into_owned();
    let out_json = tmp.path().join("warm.json");
    let output = out_json.to_string_lossy().into_owned();
    let (_, stderr, code) = run_bench_parent(&[
        "bench",
        "--fixture",
        &fixture,
        "--workload",
        "status-warm",
        "--runs",
        "2",
        "--json",
        "--output",
        &output,
    ]);
    assert_eq!(code, 0, "warm run failed: stderr={stderr}");

    let (stdout, stderr, code) = run_bench_parent(&["-C", &fixture, "timings", "--days", "1"]);
    assert_eq!(code, 0, "timings read failed: stderr={stderr}");
    assert!(
        stdout.contains("No command timing rows"),
        "children must not write self-timing rows: {stdout}"
    );
}

#[test]
fn bench_warm_run__ambient_budget_overrides_do_not_change_the_run() {
    if env!("LEDGERFUL_GIT_SHA") == "unknown" {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    tiny_fixture(tmp.path());
    let fixture = tmp.path().to_string_lossy().into_owned();
    let out_json = tmp.path().join("warm.json");
    let output = out_json.to_string_lossy().into_owned();
    let (stdout, stderr, code) = run_bench_parent_with_env(
        &[
            "bench",
            "--fixture",
            &fixture,
            "--workload",
            "status-warm",
            "--runs",
            "2",
            "--json",
            "--output",
            &output,
        ],
        &[
            ("LEDGERFUL_HISTORY_BUDGET_SECS", "1"),
            ("LEDGERFUL_PROSPECTIVE_BUDGET_SECS", "1"),
            ("LEDGERFUL_REVIEW_BUDGET_SECS", "1"),
            ("LEDGERFUL_HOTSPOTS_OVERALL_BUDGET_SECS", "1"),
            ("LEDGERFUL_AUDIT_OVERALL_BUDGET_SECS", "1"),
            ("LEDGERFUL_DEPLOY_OVERALL_BUDGET_SECS", "1"),
            ("LEDGERFUL_BRIDGE_EXPORT_OVERALL_BUDGET_SECS", "1"),
        ],
    );
    assert_eq!(
        code, 0,
        "ambient budget overrides must not leak into the child: stderr={stderr}\nstdout={stdout}"
    );
    let receipt = parse_receipt(&stdout);
    assert_eq!(receipt["n"], 2);
}

#[test]
fn bench_index_cold__unborn_fixture__zero_commits_and_null_root() {
    if env!("LEDGERFUL_GIT_SHA") == "unknown" {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(tmp.path())
        .status()
        .expect("git init");
    assert!(status.success());
    fs::write(tmp.path().join("lib.rs"), "pub fn born() {}\n").expect("write source");
    let fixture = tmp.path().to_string_lossy().into_owned();
    let out_json = tmp.path().join("cold.json");
    let output = out_json.to_string_lossy().into_owned();

    let (stdout, stderr, code) = run_bench_parent(&[
        "bench",
        "--fixture",
        &fixture,
        "--runs",
        "1",
        "--json",
        "--output",
        &output,
    ]);
    assert_eq!(
        code, 0,
        "an unborn fixture must still run a cold sample: stderr={stderr}\nstdout={stdout}"
    );
    let receipt = parse_receipt(&stdout);
    assert_eq!(receipt["workload"], "index-cold");
    assert_eq!(receipt["startState"], "cold");
    assert_eq!(receipt["fixtureCommits"], 0);
    assert!(receipt["fixtureRootCommit"].is_null());
    assert!(receipt["setupMs"].is_null());
    assert!(receipt["warmupMs"].is_null());
}

#[test]
fn bench_index_cold__multi_root_fixture__null_root_but_commit_count() {
    if env!("LEDGERFUL_GIT_SHA") == "unknown" {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "bench@example.invalid"],
        vec!["config", "user.name", "bench"],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success());
    }
    fs::write(dir.join("a.rs"), "pub fn a() {}\n").expect("write a");
    for args in [vec!["add", "a.rs"], vec!["commit", "-m", "root a"]] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success());
    }
    let status = Command::new("git")
        .args(["checkout", "--orphan", "second"])
        .current_dir(dir)
        .status()
        .expect("git checkout");
    assert!(status.success());
    fs::write(dir.join("b.rs"), "pub fn b() {}\n").expect("write b");
    for args in [vec!["add", "b.rs"], vec!["commit", "-m", "root b"]] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success());
    }
    // Merge the unrelated root into the first branch so HEAD history has two roots.
    let first_branch = String::from_utf8_lossy(
        &Command::new("git")
            .args(["for-each-ref", "--format=%(refname:short)", "refs/heads"])
            .current_dir(dir)
            .output()
            .expect("list branches")
            .stdout,
    )
    .lines()
    .map(str::trim)
    .find(|name| *name != "second")
    .expect("first branch")
    .to_string();
    let status = Command::new("git")
        .args(["checkout", "--quiet", &first_branch])
        .current_dir(dir)
        .status()
        .expect("git checkout first");
    assert!(status.success());
    let status = Command::new("git")
        .args([
            "merge",
            "--allow-unrelated-histories",
            "--no-edit",
            "second",
        ])
        .current_dir(dir)
        .status()
        .expect("git merge");
    assert!(status.success());

    let fixture = dir.to_string_lossy().into_owned();
    let out_json = dir.join("cold.json");
    let output = out_json.to_string_lossy().into_owned();
    let (stdout, stderr, code) = run_bench_parent(&[
        "bench",
        "--fixture",
        &fixture,
        "--runs",
        "1",
        "--json",
        "--output",
        &output,
    ]);
    assert_eq!(
        code, 0,
        "multi-root fixture must still run: stderr={stderr}"
    );
    let receipt = parse_receipt(&stdout);
    assert_eq!(
        receipt["fixtureCommits"], 3,
        "two roots plus the merge commit"
    );
    assert!(receipt["fixtureRootCommit"].is_null());
}

fn write_unbudgeted_receipt(path: &Path, build_sha: &str) {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let body = format!(
        r#"{{"schemaVersion":1,"kind":"workloadBench","workload":"dummy-workload","buildSha":"{build_sha}","n":3,"samplesMs":[10,20,30],"observedMs":[10,20,30],"p50Ms":20,"p95Ms":30,"startState":"warm","fixtureId":"1:aaaaaaaaaaaa","fixtureFiles":1,"fixtureCommits":1,"fixtureRootCommit":"root","childArgs":["dummy"],"hostClass":"local-windows-2026-11","peakRssBytes":null,"rssTicks":0,"rssMeasuredSamples":0,"samplerWallMs":0,"setupMs":5,"warmupMs":5,"os":"{os}","targetTriple":"{}"}}"#,
        env!("TARGET")
    );
    fs::write(path, format!("{body}\n")).expect("write receipt");
}

#[test]
fn bench_check__unbudgeted_workload__exits_one_with_envelope() {
    let embed = env!("LEDGERFUL_GIT_SHA");
    let tmp = tempfile::tempdir().expect("tempdir");
    let receipt = tmp.path().join("receipt.json");
    write_unbudgeted_receipt(&receipt, if embed == "unknown" { "unknown" } else { embed });
    let receipt_arg = receipt.to_string_lossy().into_owned();
    let (stdout, stderr, code) = run_bench_parent(&["bench", "--json", "--check", &receipt_arg]);

    assert_eq!(
        code, 1,
        "a non-clean check must exit 1: stderr={stderr}\nstdout={stdout}"
    );
    if embed == "unknown" {
        assert!(
            stderr.contains("build sha unknown"),
            "unknown embed must refuse with a named reason: {stderr}"
        );
        assert!(
            stdout.trim().is_empty(),
            "a refusal writes no JSON: {stdout}"
        );
        return;
    }
    let packet: Value = serde_json::from_str(stdout.trim()).expect("perfBudgetCheck JSON");
    assert_eq!(packet["kind"], "perfBudgetCheck");
    assert_eq!(packet["schemaVersion"], 1);
    assert_eq!(packet["status"], "not-within-budget");
    assert_eq!(packet["results"][0]["status"], "unqualified-workload");
    assert!(
        stderr.contains("perf budget check failed"),
        "stderr must carry the roll-up reason: {stderr}"
    );
}

#[test]
fn bench_check__foreign_build__refuses_the_verdict() {
    let embed = env!("LEDGERFUL_GIT_SHA");
    if embed == "unknown" {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let receipt = tmp.path().join("receipt.json");
    write_unbudgeted_receipt(&receipt, "ffffffffffff");
    let receipt_arg = receipt.to_string_lossy().into_owned();
    let (stdout, stderr, code) = run_bench_parent(&["bench", "--json", "--check", &receipt_arg]);
    assert_eq!(code, 1, "a foreign build must not pass: stderr={stderr}");
    let packet: Value = serde_json::from_str(stdout.trim()).expect("perfBudgetCheck JSON");
    assert_eq!(packet["results"][0]["status"], "foreign-build");
}
