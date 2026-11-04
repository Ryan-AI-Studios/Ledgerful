//! Live CLI workload harness (`ledgerful bench`).
//!
//! Two directories: the parent stays on the engine checkout (sha gate);
//! the child is `current_exe()` plus `-C <fixture>`. Not `timings`, not
//! `criterion`. Child rows never enter `command_timings`.

use crate::cli::args::{BenchCliArgs, BenchWorkload};
use crate::commands::perf_budget;
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
use std::sync::mpsc;
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// Child wall budget per sample (spec §5.3).
const CHILD_TIMEOUT: Duration = Duration::from_secs(120);
const CHILD_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const KIND_WORKLOAD_BENCH: &str = "workloadBench";
const KIND_COMPARE: &str = "workloadBenchCompare";
const SCHEMA_VERSION: u32 = 1;
/// Process-tree memory sampling interval for the peak-RSS diagnostic.
const SAMPLER_TICK: Duration = Duration::from_millis(100);
/// Throwaway child config home, relative to the engine root.
const CHILD_HOME_REL: &str = "output/bench/child-home";
/// The seven live ambient budget overrides a measured child must not inherit.
const CHILD_ENV_BUDGET_OVERRIDES: [&str; 7] = [
    "LEDGERFUL_HISTORY_BUDGET_SECS",
    "LEDGERFUL_PROSPECTIVE_BUDGET_SECS",
    "LEDGERFUL_REVIEW_BUDGET_SECS",
    "LEDGERFUL_HOTSPOTS_OVERALL_BUDGET_SECS",
    "LEDGERFUL_AUDIT_OVERALL_BUDGET_SECS",
    "LEDGERFUL_DEPLOY_OVERALL_BUDGET_SECS",
    "LEDGERFUL_BRIDGE_EXPORT_OVERALL_BUDGET_SECS",
];

pub fn execute_bench(args: BenchCliArgs) -> Result<()> {
    if !args.compare.is_empty() {
        return execute_compare(&args);
    }
    if !args.check.is_empty() {
        return execute_check(&args);
    }
    let fixture = args
        .fixture
        .as_ref()
        .ok_or_else(|| miette!("--fixture PATH is required unless --compare or --check"))?;
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
    let (fixture_id, fixture_files) = fixture_id_and_files(&fixture_canon)?;
    let provenance = fixture_provenance(&fixture_canon)?;
    let config_home = prepare_child_config_home(&engine_canon)?;

    // Warm runs separate setup, first process and steady state.
    let mut setup_ms: Option<i64> = None;
    let mut warmup_ms: Option<i64> = None;
    if args.workload.expects_setup() {
        ensure_warm_state_tree(&fixture_canon)?;
        setup_ms = Some(run_untimed_child(
            &exe,
            &fixture_canon,
            &config_home,
            BenchWorkload::IndexCold,
        )?);
        warmup_ms = Some(run_untimed_child(
            &exe,
            &fixture_canon,
            &config_home,
            args.workload,
        )?);
    }

    let mut samples_ms: Vec<i64> = Vec::with_capacity(args.runs as usize);
    let mut observed_ms: Vec<i64> = Vec::with_capacity(args.runs as usize);
    let mut peak_rss_bytes: Option<u64> = None;
    let mut rss_ticks: u64 = 0;
    let mut rss_measured_samples: u64 = 0;
    let mut sampler_wall_ms: u64 = 0;

    for _ in 0..args.runs {
        if !args.workload.expects_setup() {
            prepare_cold_fixture(&fixture_canon, &engine_canon)?;
        }
        let cmd = child_command(&exe, &fixture_canon, &config_home, args.workload);
        // Sampled only for timed samples: an untimed setup index would become a
        // fast command's peak.
        let sampler = MemorySampler::start(std::process::id());
        let started = Instant::now();
        let outcome = spawn_wait_grouped_captured(cmd, CHILD_TIMEOUT, CHILD_MAX_OUTPUT_BYTES);
        // Capture the sample clock the instant the child wait returns, before
        // the sampler is stopped/joined, so a join sleep cannot inflate it.
        let ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let sample = sampler.stop_and_join();
        match outcome {
            Ok(captured) => {
                if captured.truncated {
                    return Err(miette!(
                        "bench child output exceeded the {CHILD_MAX_OUTPUT_BYTES}-byte capture cap; \
                         no workloadBench JSON written"
                    ));
                }
                if !captured.status.success() {
                    eprint!("{}", captured.stderr);
                    let _ = io::stderr().flush();
                    return Err(miette!(
                        "bench child exited non-zero; no workloadBench JSON written"
                    ));
                }
                if let Some(marker) =
                    partial_marker(args.workload, &captured.stdout, &captured.stderr)?
                {
                    eprint!("{}", captured.stderr);
                    let _ = io::stderr().flush();
                    return Err(miette!(
                        "bench child emitted a bounded partial ({marker}); no workloadBench JSON written"
                    ));
                }
                samples_ms.push(ms);
                observed_ms.push(ms);
                rss_ticks += sample.ticks;
                sampler_wall_ms += sample.wall_ms;
                if let Some(peak) = sample.peak_bytes {
                    rss_measured_samples += 1;
                    peak_rss_bytes = Some(peak_rss_bytes.map_or(peak, |seen: u64| seen.max(peak)));
                }
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
        observed_ms: &observed_ms,
        p50_ms: p50,
        p95_ms: p95,
        start_state: args.workload.start_state(),
        fixture_id: fixture_id.as_str(),
        fixture_files,
        fixture_commits: provenance.commits,
        fixture_root_commit: provenance.root_commit.as_deref(),
        child_args: args.workload.child_args(),
        host_class: args.host_class.as_str(),
        peak_rss_bytes,
        rss_ticks,
        rss_measured_samples,
        sampler_wall_ms,
        setup_ms,
        warmup_ms,
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

/// Build the isolated child command: no ambient state dir, no operator config,
/// offline, and none of the seven inherited budget overrides.
fn child_command(
    exe: &Path,
    fixture_canon: &Path,
    config_home: &Path,
    workload: BenchWorkload,
) -> Command {
    let mut cmd = Command::new(exe);
    cmd.arg("-C")
        .arg(fixture_canon)
        .args(workload.child_args())
        .stdin(Stdio::null())
        .env_remove(LEDGERFUL_STATE_DIR_ENV)
        .env("LEDGERFUL_NO_NETWORK", "1")
        .env("LEDGERFUL_CONFIG_HOME", config_home);
    for name in CHILD_ENV_BUDGET_OVERRIDES {
        cmd.env_remove(name);
    }
    cmd
}

/// Throwaway config home shared by every child of this engine.
///
/// The path shape is load-bearing: `is_self_timing_enabled()` reads
/// `<CONFIG_HOME>/config.toml` while `load_user_config()` reads
/// `<CONFIG_HOME>.parent()/.ledgerful/config.toml`; only a `.../.ledgerful`
/// value makes both readers resolve the same file.
fn prepare_child_config_home(engine_root: &Path) -> Result<PathBuf> {
    let config_home = engine_root.join(CHILD_HOME_REL).join(".ledgerful");
    fs::create_dir_all(&config_home).into_diagnostic()?;
    fs::write(config_home.join("config.toml"), "self_timing = false\n").into_diagnostic()?;
    Ok(config_home)
}

/// Create only the empty state tree on a fresh clone (never wipe warm state).
fn ensure_warm_state_tree(fixture_canon: &Path) -> Result<()> {
    if fixture_canon.join(".ledgerful").join("state").is_dir() {
        return Ok(());
    }
    let root = Utf8Path::from_path(fixture_canon)
        .ok_or_else(|| miette!("fixture path is not UTF-8: {}", fixture_canon.display()))?;
    Layout::new(root).ensure_state_tree()
}

/// Run one untimed child (setup `index` or the workload warmup); returns ms.
fn run_untimed_child(
    exe: &Path,
    fixture_canon: &Path,
    config_home: &Path,
    workload: BenchWorkload,
) -> Result<i64> {
    let cmd = child_command(exe, fixture_canon, config_home, workload);
    let started = Instant::now();
    let captured = spawn_wait_grouped_captured(cmd, CHILD_TIMEOUT, CHILD_MAX_OUTPUT_BYTES)
        .map_err(|err| {
            miette!(
                "bench setup/warmup child failed for {}: {err}",
                workload.as_str()
            )
        })?;
    let ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    if captured.truncated {
        return Err(miette!(
            "bench setup/warmup child output exceeded the {CHILD_MAX_OUTPUT_BYTES}-byte capture cap"
        ));
    }
    if !captured.status.success() {
        eprint!("{}", captured.stderr);
        let _ = io::stderr().flush();
        return Err(miette!(
            "bench setup/warmup child exited non-zero for {}; no workloadBench JSON written",
            workload.as_str()
        ));
    }
    Ok(ms)
}

/// A bounded partial must never be timed as complete work.
fn partial_marker(
    workload: BenchWorkload,
    stdout: &str,
    stderr: &str,
) -> Result<Option<&'static str>> {
    if stderr.contains("stopped: overall budget") {
        return Ok(Some("stderr: stopped: overall budget"));
    }
    if stderr.contains("history walk stopped (budget)") {
        return Ok(Some("stderr: history walk stopped (budget)"));
    }
    if stderr.contains("history walk stopped (cancelled)") {
        return Ok(Some("stderr: history walk stopped (cancelled)"));
    }
    if stderr.contains("history walk truncated") {
        return Ok(Some("stderr: history walk truncated"));
    }
    if !workload.emits_json() {
        return Ok(None);
    }
    let trimmed = stdout.trim();
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|err| {
        miette!(
            "bench child stdout is not a JSON packet for --json workload {}: {err}",
            workload.as_str()
        )
    })?;
    // Top-level `status` is inspected only for `change-context`, which renders a
    // bounded partial as `not_ready`. `review` nests `status` under `blast`/`tests`
    // and those must never be read as a packet status.
    if workload == BenchWorkload::ContextWarm
        && value.get("status").and_then(serde_json::Value::as_str) == Some("not_ready")
    {
        return Ok(Some("status: not_ready"));
    }
    if let Some(stop) = completeness_stop(value.get("completeness")) {
        return Ok(Some(stop));
    }
    for key in ["blast", "tests"] {
        if let Some(stop) = value
            .get(key)
            .and_then(|nested| nested.get("completeness"))
            .and_then(|c| completeness_stop(Some(c)))
        {
            return Ok(Some(stop));
        }
    }
    Ok(None)
}

fn completeness_stop(completeness: Option<&serde_json::Value>) -> Option<&'static str> {
    match completeness?.get("stop")?.as_str()? {
        "budget" => Some("completeness.stop: budget"),
        "cancelled" => Some("completeness.stop: cancelled"),
        "error" => Some("completeness.stop: error"),
        _ => None,
    }
}

/// Per-timed-sample process-tree working-set sampler (100 ms ticks, mem only).
struct MemorySampler {
    stop_tx: mpsc::Sender<()>,
    handle: Option<std::thread::JoinHandle<SamplerSample>>,
}

#[derive(Debug, Default, Clone, Copy)]
struct SamplerSample {
    peak_bytes: Option<u64>,
    ticks: u64,
    wall_ms: u64,
}

impl MemorySampler {
    fn start(root_pid: u32) -> Self {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let mut system = System::new();
            let mut sample = SamplerSample::default();
            loop {
                match stop_rx.recv_timeout(SAMPLER_TICK) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let refresh_started = Instant::now();
                system.refresh_processes_specifics(
                    ProcessesToUpdate::All,
                    true,
                    ProcessRefreshKind::nothing().with_memory(),
                );
                sample.wall_ms +=
                    u64::try_from(refresh_started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let rows = system
                    .processes()
                    .iter()
                    .map(|(pid, process)| {
                        (
                            pid.as_u32(),
                            process.parent().map(|parent| parent.as_u32()),
                            process.memory(),
                        )
                    })
                    .collect::<Vec<_>>();
                let bytes = tree_rss(&rows, root_pid);
                // A tick that observed no descendant is not coverage: counting it
                // would break the receipt's `rssTicks == 0 iff rssMeasuredSamples == 0`
                // invariant when a child exits between spawn and the first tick.
                if bytes > 0 {
                    sample.ticks += 1;
                    sample.peak_bytes =
                        Some(sample.peak_bytes.map_or(bytes, |seen| seen.max(bytes)));
                }
            }
            sample
        });
        Self {
            stop_tx,
            handle: Some(handle),
        }
    }

    fn stop_and_join(mut self) -> SamplerSample {
        let _ = self.stop_tx.send(());
        match self.handle.take() {
            Some(handle) => handle.join().unwrap_or_default(),
            None => SamplerSample::default(),
        }
    }
}

/// Sum working set for strict descendants of `root`; the runner itself is excluded.
fn tree_rss(rows: &[(u32, Option<u32>, u64)], root: u32) -> u64 {
    let mut children: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
    for (pid, parent, _) in rows {
        if let Some(parent) = parent {
            children.entry(*parent).or_default().push(*pid);
        }
    }
    let mut seen: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    seen.insert(root);
    let mut stack = vec![root];
    let mut total = 0_u64;
    while let Some(pid) = stack.pop() {
        if let Some(kids) = children.get(&pid) {
            for kid in kids {
                // A cycle (impossible for real pids, possible for bad data) must
                // not loop forever.
                if seen.insert(*kid) {
                    if let Some((_, _, memory)) = rows.iter().find(|(id, _, _)| id == kid) {
                        total = total.saturating_add(*memory);
                    }
                    stack.push(*kid);
                }
            }
        }
    }
    total
}

/// Fixture identity: file count is part of the id (`"<count>:<hash>"`).
fn fixture_id_and_files(fixture: &Path) -> Result<(String, u64)> {
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
    let files = u64::try_from(paths.len()).unwrap_or(u64::MAX);
    Ok((format!("{}:{}", paths.len(), short), files))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixtureProvenance {
    commits: u64,
    root_commit: Option<String>,
}

/// Commits + single root commit; an unborn or multi-root fixture keeps working
/// but records `None` (identity unavailable, so the receipt cannot qualify).
fn fixture_provenance(fixture: &Path) -> Result<FixtureProvenance> {
    let head = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(fixture)
        .output()
        .into_diagnostic()?;
    if !head.status.success() {
        return Ok(FixtureProvenance {
            commits: 0,
            root_commit: None,
        });
    }
    let commits_text = git_stdout(fixture, &["rev-list", "--count", "--all"])?;
    let commits: u64 = commits_text
        .trim()
        .parse()
        .map_err(|e| miette!("fixture commit count is not a number ({e})"))?;
    let roots_text = git_stdout(fixture, &["rev-list", "--max-parents=0", "--all"])?;
    let roots = roots_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let root_commit = if roots.len() == 1 {
        Some(roots[0].to_string())
    } else {
        None
    };
    Ok(FixtureProvenance {
        commits,
        root_commit,
    })
}

fn git_stdout(fixture: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(fixture)
        .output()
        .into_diagnostic()?;
    if !output.status.success() {
        return Err(miette!(
            "git {} failed in {}: {}",
            args.join(" "),
            fixture.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn execute_check(args: &BenchCliArgs) -> Result<()> {
    let embed = env!("LEDGERFUL_GIT_SHA");
    if embed == "unknown" {
        return Err(miette!(
            "build sha unknown; no perfBudgetCheck verdict can be issued"
        ));
    }
    let mut receipts = Vec::with_capacity(args.check.len());
    for path in &args.check {
        let text = fs::read_to_string(path).into_diagnostic()?;
        let mut stored: StoredBench = serde_json::from_str(&text)
            .map_err(|e| miette!("failed to parse {}: {e}", path.display()))?;
        if stored.kind != KIND_WORKLOAD_BENCH {
            return Err(miette!(
                "{} is not a workloadBench receipt (kind: {})",
                path.display(),
                stored.kind
            ));
        }
        if stored.schema_version != SCHEMA_VERSION {
            return Err(miette!(
                "{} has unsupported schemaVersion {} (expected {SCHEMA_VERSION})",
                path.display(),
                stored.schema_version
            ));
        }
        stored.receipt_path = path.display().to_string();
        receipts.push(stored);
    }

    let packet = perf_budget::check(&receipts, embed);
    let json = serde_json::to_string_pretty(&packet).into_diagnostic()?;
    if args.json {
        println!("{json}");
    } else {
        print_human_check(&packet);
    }
    if packet.status == perf_budget::STATUS_WITHIN_BUDGET {
        return Ok(());
    }
    for result in &packet.results {
        if result.status != perf_budget::STATUS_WITHIN_BUDGET {
            eprintln!(
                "{:>22}  {}  {}",
                result.status,
                result.workload,
                result.reason.clone().unwrap_or_default()
            );
        }
    }
    Err(miette!(
        "perf budget check failed: {} over budget, {} unqualified",
        packet.violations,
        packet.unqualified
    ))
}

fn print_human_check(packet: &perf_budget::PerfBudgetCheck) {
    println!("kind            {}", packet.kind);
    println!("status          {}", packet.status);
    println!("violations      {}", packet.violations);
    println!("unqualified     {}", packet.unqualified);
    for result in &packet.results {
        println!(
            "result          {} {} p50={} p95={} target={} status={}",
            result.workload,
            result.host_class,
            result.p50_ms,
            result.p95_ms,
            result.target_status,
            result.status
        );
    }
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

#[cfg(test)]
fn fixture_id_for(fixture: &Path) -> Result<String> {
    fixture_id_and_files(fixture).map(|(id, _)| id)
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

pub(crate) fn os_name() -> &'static str {
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
    println!("fixtureFiles    {}", report.fixture_files);
    println!("fixtureCommits  {}", report.fixture_commits);
    println!(
        "fixtureRoot     {}",
        report.fixture_root_commit.unwrap_or("null")
    );
    println!("hostClass       {}", report.host_class);
    println!("childArgs       {}", report.child_args.join(" "));
    println!(
        "setupMs         {}",
        report
            .setup_ms
            .map_or_else(|| "null".to_string(), |v| v.to_string())
    );
    println!(
        "warmupMs        {}",
        report
            .warmup_ms
            .map_or_else(|| "null".to_string(), |v| v.to_string())
    );
    println!(
        "peakRssBytes    {}",
        report
            .peak_rss_bytes
            .map_or_else(|| "null".to_string(), |v| v.to_string())
    );
    println!("rssTicks        {}", report.rss_ticks);
    println!("rssMeasured     {}", report.rss_measured_samples);
    println!("samplerWallMs   {}", report.sampler_wall_ms);
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
    println!(
        "observedMs      {}",
        report
            .observed_ms
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
    observed_ms: &'a [i64],
    p50_ms: i64,
    p95_ms: i64,
    start_state: &'a str,
    fixture_id: &'a str,
    fixture_files: u64,
    fixture_commits: u64,
    fixture_root_commit: Option<&'a str>,
    child_args: &'a [&'a str],
    host_class: &'a str,
    peak_rss_bytes: Option<u64>,
    rss_ticks: u64,
    rss_measured_samples: u64,
    sampler_wall_ms: u64,
    setup_ms: Option<i64>,
    warmup_ms: Option<i64>,
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

/// Parsed `workloadBench` receipt. The nine original fields stay required (an
/// older receipt missing `os`/`targetTriple` must fail to parse); the fifteen
/// additive fields are field-level `#[serde(default)]` so pre-0564 receipts
/// still parse and then report `invalid-receipt` / `insufficient-samples`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredBench {
    pub(crate) schema_version: u32,
    pub(crate) kind: String,
    pub(crate) workload: String,
    pub(crate) build_sha: String,
    pub(crate) p50_ms: i64,
    pub(crate) p95_ms: i64,
    pub(crate) fixture_id: String,
    pub(crate) os: String,
    pub(crate) target_triple: String,
    #[serde(default)]
    pub(crate) n: usize,
    #[serde(default)]
    pub(crate) samples_ms: Vec<i64>,
    #[serde(default)]
    pub(crate) observed_ms: Vec<i64>,
    #[serde(default)]
    #[allow(dead_code)] // recorded on every receipt for the measurement/docs leg
    pub(crate) sampler_wall_ms: u64,
    #[serde(default)]
    pub(crate) fixture_root_commit: Option<String>,
    #[serde(default)]
    pub(crate) child_args: Vec<String>,
    #[serde(default)]
    pub(crate) start_state: String,
    #[serde(default)]
    pub(crate) setup_ms: Option<i64>,
    #[serde(default)]
    pub(crate) warmup_ms: Option<i64>,
    #[serde(default)]
    pub(crate) host_class: String,
    #[serde(default)]
    pub(crate) fixture_files: u64,
    #[serde(default)]
    pub(crate) fixture_commits: u64,
    #[serde(default)]
    pub(crate) peak_rss_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) rss_ticks: u64,
    #[serde(default)]
    pub(crate) rss_measured_samples: u64,
    #[serde(skip)]
    pub(crate) receipt_path: String,
}

#[cfg(test)]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
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
            ..Default::default()
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

    #[test]
    fn workload_table_matches_locked_argv_and_start_state() {
        let expected: [(&str, &[&str], &str); 8] = [
            ("index-cold", &["index"], "cold"),
            ("status-warm", &["ledger", "status", "--compact"], "warm"),
            (
                "search-warm",
                &["search", "change-context", "--limit", "3", "--json"],
                "warm",
            ),
            (
                "context-warm",
                &[
                    "change-context",
                    "--json",
                    "--paths",
                    "src/main.rs",
                    "--timeout",
                    "120",
                ],
                "warm",
            ),
            ("doctor-warm", &["doctor", "--json"], "warm"),
            (
                "hotspots-warm",
                &[
                    "hotspots",
                    "--timeout",
                    "120",
                    "explain",
                    "src/main.rs",
                    "--json",
                ],
                "warm",
            ),
            (
                "impact-warm",
                &[
                    "impact",
                    "--paths",
                    "src/main.rs",
                    "--json",
                    "--timeout",
                    "120",
                ],
                "warm",
            ),
            (
                "review-warm",
                &["review", "HEAD~5..HEAD", "--json", "--timeout", "120"],
                "warm",
            ),
        ];
        assert_eq!(BenchWorkload::ALL.len(), expected.len());
        for (workload, (slug, argv, state)) in BenchWorkload::ALL.iter().zip(expected) {
            assert_eq!(workload.as_str(), slug);
            assert_eq!(workload.child_args(), argv);
            assert_eq!(workload.start_state(), state);
            assert_eq!(workload.expects_setup(), state == "warm");
        }
        assert!(BenchWorkload::StatusWarm.expects_setup());
        assert!(!BenchWorkload::IndexCold.expects_setup());
        assert!(BenchWorkload::ImpactWarm.emits_json());
        assert!(!BenchWorkload::StatusWarm.emits_json());
    }

    #[test]
    fn partial_marker__top_level_completeness_stop__rejected() {
        let stdout = r#"{"status":"ready","completeness":{"stop":"budget"}}"#;
        let marker = partial_marker(BenchWorkload::ImpactWarm, stdout, "").expect("parse");
        assert_eq!(marker, Some("completeness.stop: budget"));
    }

    #[test]
    fn partial_marker__nested_blast_completeness_stop__rejected() {
        let stdout = r#"{"status":"ready","blast":{"completeness":{"stop":"cancelled"}}}"#;
        let marker = partial_marker(BenchWorkload::ReviewWarm, stdout, "").expect("parse");
        assert_eq!(marker, Some("completeness.stop: cancelled"));
    }

    #[test]
    fn partial_marker__nested_blast_status__not_read_as_packet_status() {
        // A nested `blast.status` is not the packet status and must never reject.
        let stdout = r#"{"status":"ready","blast":{"status":"not_ready"},"tests":{"status":"ok"}}"#;
        let marker = partial_marker(BenchWorkload::ReviewWarm, stdout, "").expect("parse");
        assert_eq!(marker, None);
    }

    #[rstest::rstest]
    #[case(r#"{"status":"not_ready"}"#, Some("status: not_ready"))]
    #[case(r#"{ "status" : "ready" }"#, None)]
    #[case(r#"{"status":"empty"}"#, None)]
    fn partial_marker__context_status__only_not_ready_rejects(
        #[case] stdout: &str,
        #[case] expected: Option<&'static str>,
    ) {
        let marker = partial_marker(BenchWorkload::ContextWarm, stdout, "").expect("parse");
        assert_eq!(marker, expected);
    }

    #[rstest::rstest]
    #[case("prospective analysis stopped: overall budget")]
    #[case("review stopped: overall budget")]
    #[case("hotspots stopped: overall budget")]
    #[case("history walk stopped (budget)")]
    #[case("history walk stopped (cancelled)")]
    #[case("history walk truncated")]
    fn partial_marker__stderr_tokens__rejected(#[case] stderr: &str) {
        let marker = partial_marker(BenchWorkload::StatusWarm, "", stderr).expect("parse");
        assert!(marker.is_some(), "stderr token must reject: {stderr}");
    }

    #[test]
    fn partial_marker__json_workload_with_plain_stdout__hard_error_rejects() {
        let err = partial_marker(BenchWorkload::DoctorWarm, "not json at all", "");
        assert!(
            err.is_err(),
            "a --json workload with unparseable stdout must fail closed"
        );
    }

    #[test]
    fn partial_marker__text_workload_ignores_stdout() {
        let marker = partial_marker(BenchWorkload::StatusWarm, "not json at all", "")
            .expect("text workload does not parse stdout");
        assert_eq!(marker, None);
    }

    #[test]
    fn tree_rss__excludes_root_and_survives_cycle__sums_descendants() {
        // root 10 -> 11 -> 12; 20 is unrelated; 12 -> 10 closes a cycle.
        let rows = [
            (10_u32, None, 1_000_u64),
            (11, Some(10), 2_000),
            (12, Some(11), 4_000),
            (20, None, 8_000),
        ];
        assert_eq!(tree_rss(&rows, 10), 6_000);
        let cyclic = [
            (10_u32, Some(12), 1_000_u64),
            (11, Some(10), 2_000),
            (12, Some(11), 4_000),
        ];
        assert_eq!(
            tree_rss(&cyclic, 10),
            6_000,
            "a cycle must not double-count or hang"
        );
    }

    #[test]
    fn fixture_provenance__unborn_fixture__zero_commits_and_null_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(tmp.path())
            .status()
            .expect("git init");
        assert!(status.success());
        let provenance = fixture_provenance(tmp.path()).expect("provenance");
        assert_eq!(provenance.commits, 0);
        assert_eq!(provenance.root_commit, None);
    }

    #[test]
    fn fixture_provenance__two_roots__null_root_but_commit_count() {
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
        fs::write(dir.join("a.txt"), "a\n").expect("write a");
        for args in [vec!["add", "a.txt"], vec!["commit", "-m", "root a"]] {
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
        fs::write(dir.join("b.txt"), "b\n").expect("write b");
        for args in [vec!["add", "b.txt"], vec!["commit", "-m", "root b"]] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(dir)
                .status()
                .expect("git");
            assert!(status.success());
        }
        let provenance = fixture_provenance(dir).expect("provenance");
        assert_eq!(provenance.commits, 2);
        assert_eq!(provenance.root_commit, None);
    }

    #[test]
    fn memory_sampler_stop_is_interruptible() {
        let sampler = MemorySampler::start(std::process::id());
        let started = Instant::now();
        let sample = sampler.stop_and_join();
        assert!(
            started.elapsed() < SAMPLER_TICK,
            "stop_and_join must not block out the 100 ms tick"
        );
        assert_eq!(sample.ticks, 0, "an immediate stop observes no tick");
        assert_eq!(sample.peak_bytes, None);
    }

    #[test]
    fn memory_sampler__childless_runner__no_coverage() {
        let sampler = MemorySampler::start(std::process::id());
        std::thread::sleep(SAMPLER_TICK * 3);
        let sample = sampler.stop_and_join();
        assert_eq!(
            sample.ticks, 0,
            "a tick with no live descendant is not RSS coverage"
        );
        assert_eq!(sample.peak_bytes, None);
        assert!(sample.wall_ms <= SAMPLER_TICK.as_millis() as u64 * 30);
    }

    #[test]
    fn memory_sampler__live_child__observes_its_working_set() {
        let mut child = if cfg!(windows) {
            Command::new("cmd")
                .args(["/C", "ping", "-n", "3", "127.0.0.1"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn child")
        } else {
            Command::new("sleep")
                .arg("1")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn child")
        };
        let sampler = MemorySampler::start(std::process::id());
        std::thread::sleep(SAMPLER_TICK * 4);
        let sample = sampler.stop_and_join();
        // Let the child exit on its own so the test leaves nothing running.
        let _ = child.wait();
        assert!(
            sample.ticks >= 1,
            "a live child must be observed at least once"
        );
        assert!(
            sample.peak_bytes.is_some_and(|bytes| bytes > 0),
            "a live child must contribute a positive working set"
        );
    }
}
