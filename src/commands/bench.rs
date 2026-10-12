//! Live CLI workload harness (`ledgerful bench`).
//!
//! Two directories: the parent stays on the engine checkout (sha gate);
//! the child is `current_exe()` plus `-C <fixture>`. Not `timings`, not
//! `criterion`. Child rows never enter `command_timings`.

use crate::cli::args::BenchCliArgs;
use crate::exec::grouped::{GroupedProcessError, spawn_wait_grouped_captured};
use crate::state::layout::{LEDGERFUL_STATE_DIR_ENV, Layout};
use crate::state::storage::timings::percentile_sorted;
use camino::Utf8Path;
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Child wall budget per sample (spec §5.3).
const CHILD_TIMEOUT: Duration = Duration::from_secs(120);
const CHILD_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const KIND_WORKLOAD_BENCH: &str = "workloadBench";
const KIND_COMPARE: &str = "workloadBenchCompare";
const SCHEMA_VERSION: u32 = 1;

pub fn execute_bench(args: BenchCliArgs) -> Result<()> {
    if !args.compare.is_empty() {
        return execute_compare(&args);
    }
    let fixture = args
        .fixture
        .as_ref()
        .ok_or_else(|| miette!("--fixture PATH is required unless --compare"))?;
    execute_run(&args, fixture)
}

fn execute_run(args: &BenchCliArgs, fixture: &Path) -> Result<()> {
    let layout = crate::commands::helpers::get_layout_or_cwd_if_not_git()?;
    let embed = env!("LEDGERFUL_GIT_SHA");
    let git_head = git_short_head(layout.root.as_std_path())?;
    if !sha_matches(embed, &git_head) {
        return Err(miette!(
            "bench sha gate refused: embed {embed} != engine HEAD {git_head} (or embed is unknown)"
        ));
    }

    let engine_canon = dunce::canonicalize(layout.root.as_std_path()).into_diagnostic()?;
    let fixture_canon = dunce::canonicalize(fixture).into_diagnostic()?;
    if fixture_canon == engine_canon {
        return Err(miette!(
            "--fixture must not be the engine checkout (refusing wipe of engine .ledgerful)"
        ));
    }
    if !fixture_canon.join(".git").exists() {
        return Err(miette!(
            "--fixture must be a git repository (missing .git): {}",
            fixture.display()
        ));
    }

    // Legitimate: re-exec this sha-gated binary as the fixture child.
    // nosemgrep: rust.lang.security.current-exe.current-exe
    let exe = std::env::current_exe().into_diagnostic()?;
    let fixture_id = fixture_id_for(&fixture_canon)?;
    let mut samples_ms: Vec<i64> = Vec::with_capacity(args.runs as usize);
    for _ in 0..args.runs {
        prepare_cold_fixture(&fixture_canon, &engine_canon)?;
        let mut cmd = Command::new(&exe);
        cmd.arg("-C")
            .arg(&fixture_canon)
            .args(args.workload.child_args())
            .stdin(Stdio::null())
            .env_remove(LEDGERFUL_STATE_DIR_ENV);
        let started = Instant::now();
        match spawn_wait_grouped_captured(cmd, CHILD_TIMEOUT, CHILD_MAX_OUTPUT_BYTES) {
            Ok(captured) => {
                if !captured.status.success() {
                    eprint!("{}", captured.stderr);
                    let _ = io::stderr().flush();
                    return Err(miette!(
                        "bench child exited non-zero; no workloadBench JSON written"
                    ));
                }
                let ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
                samples_ms.push(ms);
            }
            Err(GroupedProcessError::Timeout {
                timeout,
                detail,
                stderr,
            }) => {
                eprint!("{stderr}");
                let _ = io::stderr().flush();
                return Err(miette!(
                    "bench child timed out after {timeout:?} ({detail}); no workloadBench JSON written"
                ));
            }
            Err(err) => {
                return Err(miette!("bench child spawn failed: {err}"));
            }
        }
    }

    samples_ms.sort_unstable();
    let n = samples_ms.len();
    let p50 = percentile_sorted(&samples_ms, 50);
    let p95 = percentile_sorted(&samples_ms, 95);
    let os = os_name();
    let target_triple = env!("TARGET");
    let comparable = n >= 3;
    let report = WorkloadBenchReport {
        schema_version: SCHEMA_VERSION,
        kind: KIND_WORKLOAD_BENCH,
        workload: args.workload.as_str(),
        build_sha: embed,
        git_head: git_head.as_str(),
        n,
        samples_ms: &samples_ms,
        p50_ms: p50,
        p95_ms: p95,
        start_state: args.workload.start_state(),
        fixture_id: fixture_id.as_str(),
        os,
        target_triple,
        comparable,
    };

    let output_path = resolve_output_path(
        args.output.as_deref(),
        layout.root.as_std_path(),
        embed,
        args.workload.as_str(),
    );
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).into_diagnostic()?;
    }
    let json = serde_json::to_string_pretty(&report).into_diagnostic()?;
    fs::write(&output_path, format!("{json}\n")).into_diagnostic()?;

    if args.json {
        println!("{json}");
    } else {
        print_human_run(&report, &output_path);
    }
    Ok(())
}

fn execute_compare(args: &BenchCliArgs) -> Result<()> {
    if args.compare.len() != 2 {
        return Err(miette!("--compare requires exactly two JSON paths"));
    }
    let a_path = &args.compare[0];
    let b_path = &args.compare[1];
    let a = read_bench_file(a_path)?;
    let b = read_bench_file(b_path)?;
    if let Some(reason) = compare_mismatch_reason(&a, &b) {
        return Err(miette!(
            "bench --compare refused ({reason}); no workloadBenchCompare JSON written"
        ));
    }
    let envelope = WorkloadBenchCompare {
        schema_version: SCHEMA_VERSION,
        kind: KIND_COMPARE,
        a_path: a_path.display().to_string(),
        b_path: b_path.display().to_string(),
        a_build_sha: a.build_sha.clone(),
        b_build_sha: b.build_sha.clone(),
        delta_p50_ms: b.p50_ms - a.p50_ms,
        delta_p95_ms: b.p95_ms - a.p95_ms,
        comparable: true,
    };
    let json = serde_json::to_string_pretty(&envelope).into_diagnostic()?;
    if args.json {
        println!("{json}");
    } else {
        println!("kind            {KIND_COMPARE}");
        println!("aPath           {}", envelope.a_path);
        println!("bPath           {}", envelope.b_path);
        println!("aBuildSha       {}", envelope.a_build_sha);
        println!("bBuildSha       {}", envelope.b_build_sha);
        println!("aP50Ms          {}", a.p50_ms);
        println!("aP95Ms          {}", a.p95_ms);
        println!("bP50Ms          {}", b.p50_ms);
        println!("bP95Ms          {}", b.p95_ms);
        println!("deltaP50Ms      {}", envelope.delta_p50_ms);
        println!("deltaP95Ms      {}", envelope.delta_p95_ms);
        println!("comparable      {}", envelope.comparable);
    }
    Ok(())
}

/// Embed must equal engine HEAD and must not be the build.rs fail-soft token.
pub(crate) fn sha_matches(embed: &str, git_head: &str) -> bool {
    embed != "unknown" && embed == git_head
}

fn git_short_head(engine_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(engine_root)
        .output()
        .into_diagnostic()?;
    if !output.status.success() {
        return Err(miette!(
            "git rev-parse --short=12 HEAD failed in {}: {}",
            engine_root.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn prepare_cold_fixture(fixture_canon: &Path, engine_canon: &Path) -> Result<()> {
    if fixture_canon == engine_canon {
        return Err(miette!("refusing to wipe engine .ledgerful"));
    }
    if !fixture_canon.join(".git").exists() {
        return Err(miette!(
            "refusing wipe: fixture is not a git repository: {}",
            fixture_canon.display()
        ));
    }
    let state = fixture_canon.join(".ledgerful");
    if state.exists() {
        fs::remove_dir_all(&state).into_diagnostic()?;
    }
    // Empty dirs only — sqlite cannot create `ledger.db` without `state/`.
    // This is still cold: no index rows survive the wipe.
    let root = Utf8Path::from_path(fixture_canon)
        .ok_or_else(|| miette!("fixture path is not UTF-8: {}", fixture_canon.display()))?;
    Layout::new(root).ensure_state_dir()?;
    Ok(())
}

fn fixture_id_for(fixture: &Path) -> Result<String> {
    let mut paths = Vec::new();
    collect_relative_paths(fixture, fixture, &mut paths)?;
    paths.sort();
    let mut hasher = Sha256::new();
    for p in &paths {
        hasher.update(p.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    let hex = hex::encode(digest);
    let short = hex.get(..12).unwrap_or(hex.as_str());
    Ok(format!("{}:{}", paths.len(), short))
}

fn is_fixture_leaf(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() || meta.is_file() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

fn collect_relative_paths(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .into_diagnostic()?
        .collect::<io::Result<Vec<_>>>()
        .into_diagnostic()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == ".git" || name == ".ledgerful" {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| miette!("fixture scan failed for {}: {e}", path.display()))?;
        // Do not follow directory symlinks / reparse points (cycles or
        // paths outside the fixture). Record the link itself as a leaf.
        if is_fixture_leaf(&meta) {
            let rel = path.strip_prefix(root).into_diagnostic()?;
            out.push(rel.to_string_lossy().replace('\\', "/"));
            continue;
        }
        if meta.is_dir() {
            collect_relative_paths(root, &path, out)?;
        }
    }
    Ok(())
}

fn resolve_output_path(
    requested: Option<&Path>,
    engine_root: &Path,
    build_sha: &str,
    workload: &str,
) -> PathBuf {
    match requested {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => engine_root.join(p),
        None => engine_root
            .join("output")
            .join("bench")
            .join(format!("{build_sha}-{workload}.json")),
    }
}

fn os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn print_human_run(report: &WorkloadBenchReport<'_>, output_path: &Path) {
    println!("kind            {}", report.kind);
    println!("workload        {}", report.workload);
    println!("buildSha        {}", report.build_sha);
    println!("gitHead         {}", report.git_head);
    println!("n               {}", report.n);
    println!("p50Ms           {}", report.p50_ms);
    println!("p95Ms           {}", report.p95_ms);
    println!("startState      {}", report.start_state);
    println!("fixtureId       {}", report.fixture_id);
    println!("os              {}", report.os);
    println!("targetTriple    {}", report.target_triple);
    println!("comparable      {}", report.comparable);
    println!(
        "samplesMs       {}",
        report
            .samples_ms
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    println!("wrote           {}", output_path.display());
}

fn read_bench_file(path: &Path) -> Result<StoredBench> {
    let text = fs::read_to_string(path).into_diagnostic()?;
    serde_json::from_str(&text).map_err(|e| miette!("failed to parse {}: {e}", path.display()))
}

pub(crate) fn compare_mismatch_reason(a: &StoredBench, b: &StoredBench) -> Option<&'static str> {
    if a.kind != KIND_WORKLOAD_BENCH || b.kind != KIND_WORKLOAD_BENCH {
        return Some("kind");
    }
    if a.schema_version != SCHEMA_VERSION || b.schema_version != SCHEMA_VERSION {
        return Some("schemaVersion");
    }
    if a.workload != b.workload {
        return Some("workload");
    }
    if a.fixture_id != b.fixture_id {
        return Some("fixtureId");
    }
    None
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkloadBenchReport<'a> {
    schema_version: u32,
    kind: &'a str,
    workload: &'a str,
    build_sha: &'a str,
    git_head: &'a str,
    n: usize,
    samples_ms: &'a [i64],
    p50_ms: i64,
    p95_ms: i64,
    start_state: &'a str,
    fixture_id: &'a str,
    os: &'a str,
    target_triple: &'a str,
    comparable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkloadBenchCompare {
    schema_version: u32,
    kind: &'static str,
    a_path: String,
    b_path: String,
    a_build_sha: String,
    b_build_sha: String,
    delta_p50_ms: i64,
    delta_p95_ms: i64,
    comparable: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredBench {
    schema_version: u32,
    kind: String,
    workload: String,
    build_sha: String,
    p50_ms: i64,
    p95_ms: i64,
    fixture_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::{BenchWorkload, DEFAULT_BENCH_RUNS};

    #[test]
    fn sha_matches_equal_12hex_true() {
        assert!(sha_matches("7ee31ed22b15", "7ee31ed22b15"));
    }

    #[test]
    fn sha_matches_unknown_embed_false() {
        assert!(!sha_matches("unknown", "unknown"));
        assert!(!sha_matches("unknown", "7ee31ed22b15"));
    }

    #[test]
    fn sha_matches_mismatch_false() {
        assert!(!sha_matches("aaaaaaaaaaaa", "bbbbbbbbbbbb"));
    }

    #[test]
    fn percentile_sorted_n5_matches_timings_rank() {
        let sorted = [10_i64, 20, 30, 40, 50];
        assert_eq!(percentile_sorted(&sorted, 50), 30);
        assert_eq!(percentile_sorted(&sorted, 95), 50);
    }

    #[test]
    fn compare_mismatch_reason_kind_or_fixture() {
        let a = StoredBench {
            schema_version: 1,
            kind: KIND_WORKLOAD_BENCH.to_string(),
            workload: "index-cold".into(),
            build_sha: "aaa".into(),
            p50_ms: 10,
            p95_ms: 20,
            fixture_id: "1:abc".into(),
        };
        let mut b = a.clone();
        b.build_sha = "bbb".into();
        b.p50_ms = 15;
        b.p95_ms = 25;
        assert_eq!(compare_mismatch_reason(&a, &b), None);
        assert_eq!(b.p95_ms - a.p95_ms, 5);
        assert_eq!(b.p50_ms - a.p50_ms, 5);
        b.workload = "other".into();
        assert_eq!(compare_mismatch_reason(&a, &b), Some("workload"));
        b.workload = a.workload.clone();
        b.fixture_id = "2:zzz".into();
        assert_eq!(compare_mismatch_reason(&a, &b), Some("fixtureId"));
        b.fixture_id = a.fixture_id.clone();
        b.kind = "timings".into();
        assert_eq!(compare_mismatch_reason(&a, &b), Some("kind"));
        b.kind = KIND_WORKLOAD_BENCH.to_string();
        b.schema_version = 2;
        assert_eq!(compare_mismatch_reason(&a, &b), Some("schemaVersion"));
    }

    #[test]
    fn default_runs_is_named_five() {
        assert_eq!(DEFAULT_BENCH_RUNS, 5);
    }

    #[test]
    fn default_output_path_is_under_engine_output_bench() {
        let engine = Path::new("C:/repo");
        let p = resolve_output_path(None, engine, "abc123def456", "index-cold");
        assert_eq!(
            p,
            engine
                .join("output")
                .join("bench")
                .join("abc123def456-index-cold.json")
        );
    }

    #[test]
    fn fixture_id_propagates_scan_failure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("not-a-dir.txt");
        fs::write(&file, "x").expect("write");
        let err = collect_relative_paths(&file, &file, &mut Vec::new());
        assert!(
            err.is_err(),
            "scanning a file as a directory must fail, got {err:?}"
        );
    }

    #[test]
    fn fixture_id_records_symlink_leaf_without_following() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("real.txt"), "a").expect("write real");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("secret.txt"), "nope").expect("write secret");
        let link = root.join("link-out");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(outside.path(), &link);
        #[cfg(not(windows))]
        let linked = std::os::unix::fs::symlink(outside.path(), &link);
        if linked.is_err() {
            return;
        }
        let id = fixture_id_for(root).expect("fixture id");
        let mut paths = Vec::new();
        collect_relative_paths(root, root, &mut paths).expect("collect");
        paths.sort();
        assert!(
            paths.iter().any(|p| p == "link-out" || p == "link-out/"),
            "symlink must be a leaf, got {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("secret")),
            "must not follow symlink into outside tree: {paths:?}"
        );
        assert!(
            id.starts_with("2:"),
            "two leaves (real.txt + link), got {id}"
        );
    }

    #[test]
    fn workload_child_args_index() {
        assert_eq!(BenchWorkload::IndexCold.as_str(), "index-cold");
        assert_eq!(BenchWorkload::IndexCold.child_args(), &["index"]);
        assert_eq!(BenchWorkload::IndexCold.start_state(), "cold");
    }
}
