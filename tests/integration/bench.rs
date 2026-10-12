//! Smoke tests for `ledgerful bench` (0477).
//!
//! Parent cwd is the engine (`CARGO_MANIFEST_DIR`). `--fixture` is a temp git
//! repo. Do not `run_cli(fixture, …)` as the parent — that fails the sha gate.

use crate::common::{git_add_and_commit, setup_git_repo};
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
