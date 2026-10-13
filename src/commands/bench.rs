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
    // Do not migrate: a legacy sibling stays on disk so the next sample is cold.
    let root = Utf8Path::from_path(fixture_canon)
        .ok_or_else(|| miette!("fixture path is not UTF-8: {}", fixture_canon.display()))?;
    Layout::new(root).ensure_state_tree()?;
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

fn target_parent_is_build_root(root: &Path, dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file()
        || dir.join("pom.xml").is_file()
        || dir.join("build.sbt").is_file()
        || sbt_meta_project_dir(root, dir)
}

fn sbt_meta_project_dir(root: &Path, dir: &Path) -> bool {
    let mut cursor = dir;
    loop {
        // Stop before `parent()` so a fixture root named `project` does
        // not consult a `build.sbt` outside the fixture.
        if cursor == root {
            return false;
        }
        match cursor.file_name() {
            Some(name) if name == "project" => {}
            _ => return false,
        }
        let Some(parent) = cursor.parent() else {
            return false;
        };
        if parent.join("build.sbt").is_file() {
            return true;
        }
        cursor = parent;
    }
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
        // paths outside the fixture). Record the link itself as a leaf,
        // including a link named `target` or `node_modules`.
        if is_fixture_leaf(&meta) {
            let rel = path.strip_prefix(root).into_diagnostic()?;
            out.push(rel.to_string_lossy().replace('\\', "/"));
            continue;
        }
        if meta.is_dir() {
            if name == "node_modules" {
                continue;
            }
            // Skip a build `target/` when this directory has `Cargo.toml`,
            // `pom.xml`, or `build.sbt`, or when it is an sbt `project`
            // chain whose parent has `build.sbt`. That chain walk stops
            // at the fixture root. `src/target/` stays source.
            if name == "target" && target_parent_is_build_root(root, dir) {
                continue;
            }
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
    if a.os != b.os {
        return Some("os");
    }
    if a.target_triple != b.target_triple {
        return Some("targetTriple");
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
    os: String,
    target_triple: String,
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
            os: "windows".into(),
            target_triple: "x86_64-pc-windows-msvc".into(),
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
        b.schema_version = a.schema_version;
        b.os = "linux".into();
        assert_eq!(compare_mismatch_reason(&a, &b), Some("os"));
        b.os = a.os.clone();
        b.target_triple = "aarch64-unknown-linux-gnu".into();
        assert_eq!(compare_mismatch_reason(&a, &b), Some("targetTriple"));
        b.target_triple = a.target_triple.clone();
        assert_eq!(compare_mismatch_reason(&a, &b), None);
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
    fn prepare_cold_fixture_keeps_legacy_marker_and_empty_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tmp.path().join("fixture");
        let engine = tmp.path().join("engine");
        fs::create_dir_all(fixture.join(".git")).expect("git dir");
        fs::create_dir_all(&engine).expect("engine dir");
        fs::create_dir_all(fixture.join(".ledgerful")).expect("old state");
        fs::write(fixture.join(".ledgerful").join("warm.txt"), "rows").expect("warm file");
        let legacy = fixture.join(crate::state::layout::LEGACY_STATE_DIR);
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::write(legacy.join("marker.txt"), "keep").expect("marker");

        prepare_cold_fixture(&fixture, &engine).expect("prepare");

        assert!(
            legacy.join("marker.txt").is_file(),
            "legacy marker must stay in the sibling directory"
        );
        assert!(
            !fixture.join(".ledgerful").join("marker.txt").exists(),
            "legacy marker must not be renamed into .ledgerful"
        );
        assert!(
            !fixture.join(".ledgerful").join("warm.txt").exists(),
            "the wiped .ledgerful file must be gone"
        );
        for child in ["logs", "tmp", "reports", "state", "search_index", "docs"] {
            assert!(
                fixture.join(".ledgerful").join(child).is_dir(),
                ".ledgerful/{child} must exist after cold prep"
            );
        }
        let root = Utf8Path::from_path(&fixture).expect("fixture utf-8");
        Layout::new(root)
            .ensure_state_dir()
            .expect("ensure_state_dir");
        assert!(
            legacy.join("marker.txt").is_file(),
            "a later ensure_state_dir must not rename the legacy directory"
        );
        assert!(!fixture.join(".ledgerful").join("marker.txt").exists());
    }

    fn write_rel(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(&path, body).expect(rel);
    }

    fn assert_fixture_id_count_hex(id: &str, count: usize) {
        let (count_text, hex) = id.split_once(':').expect("id splits once on colon");
        assert_eq!(count_text, count.to_string(), "count prefix of {id}");
        assert_eq!(hex.len(), 12, "hex length of {id}");
        assert!(
            hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "hex must be ASCII hex, got {hex}"
        );
    }

    #[test]
    fn fixture_walk_keeps_source_target_and_target_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_rel(root, "Cargo.toml", "");
        write_rel(root, "src/lib.rs", "fn x() {}\n");
        write_rel(root, "src/target/mod.rs", "fn source() {}\n");
        write_rel(root, "crates/bar/Cargo.toml", "");
        write_rel(root, "crates/bar/target", "not a build tree\n");
        write_rel(root, "crates/foo/Cargo.toml", "");
        write_rel(root, "crates/foo/src/lib.rs", "fn y() {}\n");
        write_rel(root, "crates/foo/src/target/mod.rs", "fn nested() {}\n");
        write_rel(root, "extra/target", "file\n");
        write_rel(root, "extra/node_modules", "file\n");
        write_rel(root, "target/debug/foo", "bin");
        write_rel(root, "crates/foo/target/debug/foo", "bin");
        write_rel(root, "pkg/node_modules/a.js", "js");
        write_rel(root, ".git/HEAD", "ref: refs/heads/main\n");
        write_rel(root, ".ledgerful/state.db", "db");

        let mut paths = Vec::new();
        collect_relative_paths(root, root, &mut paths).expect("collect");
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "Cargo.toml".to_string(),
                "crates/bar/Cargo.toml".to_string(),
                "crates/bar/target".to_string(),
                "crates/foo/Cargo.toml".to_string(),
                "crates/foo/src/lib.rs".to_string(),
                "crates/foo/src/target/mod.rs".to_string(),
                "extra/node_modules".to_string(),
                "extra/target".to_string(),
                "src/lib.rs".to_string(),
                "src/target/mod.rs".to_string(),
            ]
        );
    }

    #[test]
    fn fixture_id_changes_when_source_target_path_appears() {
        let first = tempfile::tempdir().expect("tempdir");
        let second = tempfile::tempdir().expect("tempdir");
        write_rel(first.path(), "Cargo.toml", "");
        write_rel(first.path(), "src/lib.rs", "fn x() {}\n");
        write_rel(first.path(), "target/debug/foo", "bin");
        write_rel(second.path(), "Cargo.toml", "");
        write_rel(second.path(), "src/lib.rs", "fn x() {}\n");
        write_rel(second.path(), "target/debug/foo", "bin");
        write_rel(second.path(), "src/target/mod.rs", "fn source() {}\n");

        let mut first_paths = Vec::new();
        collect_relative_paths(first.path(), first.path(), &mut first_paths).expect("collect");
        first_paths.sort();
        let mut second_paths = Vec::new();
        collect_relative_paths(second.path(), second.path(), &mut second_paths).expect("collect");
        second_paths.sort();
        assert_eq!(
            first_paths,
            vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()]
        );
        assert_eq!(
            second_paths,
            vec![
                "Cargo.toml".to_string(),
                "src/lib.rs".to_string(),
                "src/target/mod.rs".to_string(),
            ]
        );

        let first_id = fixture_id_for(first.path()).expect("first id");
        let second_id = fixture_id_for(second.path()).expect("second id");
        assert_ne!(first_id, second_id);
        assert_fixture_id_count_hex(&first_id, 2);
        assert_fixture_id_count_hex(&second_id, 3);
    }

    #[test]
    fn collect_relative_paths_skips_build_trees_and_state_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("Cargo.toml"), "").expect("manifest");
        fs::create_dir_all(root.join("src")).expect("src");
        fs::write(root.join("src").join("lib.rs"), "fn x() {}\n").expect("lib");
        fs::create_dir_all(root.join(".git")).expect("git");
        fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("head");
        fs::create_dir_all(root.join(".ledgerful")).expect("state");
        fs::write(root.join(".ledgerful").join("state.db"), "db").expect("db");
        fs::create_dir_all(root.join("target").join("debug")).expect("target");
        fs::write(root.join("target").join("debug").join("foo"), "bin").expect("foo");
        fs::create_dir_all(root.join("pkg").join("node_modules")).expect("nm");
        fs::write(root.join("pkg").join("node_modules").join("a.js"), "js").expect("js");

        let mut paths = Vec::new();
        collect_relative_paths(root, root, &mut paths).expect("collect");
        paths.sort();
        assert_eq!(
            paths,
            vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()]
        );
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    fn collect_relative_paths__maven_and_sbt_targets__skips_build_trees_and_keeps_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_rel(root, "pom.xml", "<project/>");
        write_rel(root, "src/main/java/App.java", "class App {}");
        write_rel(root, "src/target/mod.rs", "fn source() {}");
        write_rel(root, "target/classes/App.class", "class");
        write_rel(root, "module/pom.xml", "<project/>");
        write_rel(root, "module/src/main/java/A.java", "class A {}");
        write_rel(root, "module/target/classes/A.class", "class");
        write_rel(root, "build.sbt", "");
        write_rel(root, "project/build.properties", "sbt.version=1.10.0");
        write_rel(root, "project/plugins.sbt", "");
        write_rel(root, "project/target/streams/out", "out");
        write_rel(root, "project/project/Plugins.scala", "object Plugins");
        write_rel(root, "project/project/target/cache/x", "x");
        write_rel(root, "sub/build.sbt", "");
        write_rel(root, "sub/src/main/scala/B.scala", "object B");
        write_rel(root, "sub/target/scala/classes/B.class", "class");
        write_rel(root, "plain/readme.txt", "plain");
        write_rel(root, "plain/target/keep.txt", "keep");
        write_rel(root, "notes/project/target/keep.txt", "keep");
        write_rel(root, "cased/Project/target/keep.txt", "keep");
        write_rel(root, "mavenfile/pom.xml", "<project/>");
        write_rel(root, "mavenfile/target", "file");
        write_rel(root, "extra/target", "file");
        write_rel(root, "core/src/main/scala/C.scala", "object C");
        write_rel(root, "core/target/classes/C.class", "class");

        let mut paths = Vec::new();
        collect_relative_paths(root, root, &mut paths).expect("collect");
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "build.sbt".to_string(),
                "cased/Project/target/keep.txt".to_string(),
                "core/src/main/scala/C.scala".to_string(),
                "core/target/classes/C.class".to_string(),
                "extra/target".to_string(),
                "mavenfile/pom.xml".to_string(),
                "mavenfile/target".to_string(),
                "module/pom.xml".to_string(),
                "module/src/main/java/A.java".to_string(),
                "notes/project/target/keep.txt".to_string(),
                "plain/readme.txt".to_string(),
                "plain/target/keep.txt".to_string(),
                "pom.xml".to_string(),
                "project/build.properties".to_string(),
                "project/plugins.sbt".to_string(),
                "project/project/Plugins.scala".to_string(),
                "src/main/java/App.java".to_string(),
                "src/target/mod.rs".to_string(),
                "sub/build.sbt".to_string(),
                "sub/src/main/scala/B.scala".to_string(),
            ]
        );
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    fn fixture_id_for__maven_target_and_bytes__id_follows_paths_only() {
        let clean = tempfile::tempdir().expect("tempdir");
        let built = tempfile::tempdir().expect("tempdir");
        let edited = tempfile::tempdir().expect("tempdir");
        let source_target = tempfile::tempdir().expect("tempdir");

        write_rel(clean.path(), "pom.xml", "<project/>");
        write_rel(clean.path(), "src/main/java/App.java", "class App {}");

        write_rel(built.path(), "pom.xml", "<project/>");
        write_rel(built.path(), "src/main/java/App.java", "class App {}");
        write_rel(built.path(), "target/classes/App.class", "class");

        write_rel(edited.path(), "pom.xml", "<project/>");
        write_rel(
            edited.path(),
            "src/main/java/App.java",
            "class App { int x; }",
        );

        write_rel(source_target.path(), "pom.xml", "<project/>");
        write_rel(
            source_target.path(),
            "src/main/java/App.java",
            "class App {}",
        );
        write_rel(source_target.path(), "src/target/mod.rs", "fn source() {}");

        let mut clean_paths = Vec::new();
        collect_relative_paths(clean.path(), clean.path(), &mut clean_paths).expect("collect");
        clean_paths.sort();
        let mut built_paths = Vec::new();
        collect_relative_paths(built.path(), built.path(), &mut built_paths).expect("collect");
        built_paths.sort();
        let mut edited_paths = Vec::new();
        collect_relative_paths(edited.path(), edited.path(), &mut edited_paths).expect("collect");
        edited_paths.sort();
        let mut source_paths = Vec::new();
        collect_relative_paths(
            source_target.path(),
            source_target.path(),
            &mut source_paths,
        )
        .expect("collect");
        source_paths.sort();

        let two = vec!["pom.xml".to_string(), "src/main/java/App.java".to_string()];
        assert_eq!(clean_paths, two);
        assert_eq!(built_paths, two);
        assert_eq!(edited_paths, two);
        assert_eq!(
            source_paths,
            vec![
                "pom.xml".to_string(),
                "src/main/java/App.java".to_string(),
                "src/target/mod.rs".to_string(),
            ]
        );

        let clean_id = fixture_id_for(clean.path()).expect("clean id");
        let built_id = fixture_id_for(built.path()).expect("built id");
        let edited_id = fixture_id_for(edited.path()).expect("edited id");
        let source_id = fixture_id_for(source_target.path()).expect("source id");
        assert_eq!(clean_id, built_id);
        assert_eq!(clean_id, edited_id);
        assert_ne!(clean_id, source_id);
        assert_fixture_id_count_hex(&clean_id, 2);
        assert_fixture_id_count_hex(&built_id, 2);
        assert_fixture_id_count_hex(&edited_id, 2);
        assert_fixture_id_count_hex(&source_id, 3);
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    fn fixture_id_for__sbt_project_target__unbuilt_equals_built() {
        let unbuilt = tempfile::tempdir().expect("tempdir");
        let built = tempfile::tempdir().expect("tempdir");

        write_rel(unbuilt.path(), "build.sbt", "");
        write_rel(unbuilt.path(), "project/plugins.sbt", "");
        write_rel(unbuilt.path(), "src/main/scala/A.scala", "object A");

        write_rel(built.path(), "build.sbt", "");
        write_rel(built.path(), "project/plugins.sbt", "");
        write_rel(built.path(), "src/main/scala/A.scala", "object A");
        write_rel(built.path(), "target/scala/classes/A.class", "class");
        write_rel(built.path(), "project/target/streams/out", "out");

        let mut unbuilt_paths = Vec::new();
        collect_relative_paths(unbuilt.path(), unbuilt.path(), &mut unbuilt_paths)
            .expect("collect");
        unbuilt_paths.sort();
        let mut built_paths = Vec::new();
        collect_relative_paths(built.path(), built.path(), &mut built_paths).expect("collect");
        built_paths.sort();

        let three = vec![
            "build.sbt".to_string(),
            "project/plugins.sbt".to_string(),
            "src/main/scala/A.scala".to_string(),
        ];
        assert_eq!(unbuilt_paths, three);
        assert_eq!(built_paths, three);

        let unbuilt_id = fixture_id_for(unbuilt.path()).expect("unbuilt id");
        let built_id = fixture_id_for(built.path()).expect("built id");
        assert_eq!(unbuilt_id, built_id);
        assert_fixture_id_count_hex(&unbuilt_id, 3);
        assert_fixture_id_count_hex(&built_id, 3);
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    fn collect_relative_paths__root_named_project_and_outside_build_sbt__records_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tmp.path();
        write_rel(outside, "build.sbt", "");
        let root = outside.join("project");
        write_rel(&root, "target/keep.txt", "keep");
        write_rel(&root, "src/App.scala", "object App");

        let mut paths = Vec::new();
        collect_relative_paths(&root, &root, &mut paths).expect("collect");
        paths.sort();
        assert_eq!(
            paths,
            vec!["src/App.scala".to_string(), "target/keep.txt".to_string(),]
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
