use crate::bridge::model::{
    BridgeDirection, BridgePayload, BridgeRecord, ExportDataset, SnapshotPayload,
};
use crate::git::RepoSnapshot;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::impact::budget::{AnalysisBudget, HistoryWalkStop};
use crate::impact::hotspots::{HotspotCalculation, HotspotQuery};
use crate::impact::orchestrator::{ImpactOrchestrator, map_snapshot_to_packet};
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::db::LedgerDb;
use crate::ledger::types::LedgerEntry;
use crate::state::layout::get_layout_or_cwd_if_not_git;
use crate::state::storage::StorageManager;
use clap::Args;
use miette::{IntoDiagnostic, Result, miette};
use owo_colors::{OwoColorize, Stream};
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const LEDGER_PAGE: usize = 10;

#[derive(Args, Debug, Clone)]
pub struct ExportArgs {
    /// Output path for the exported record (NDJSON)
    #[arg(long, short, alias = "out")]
    pub out_path: Option<String>,

    /// Print to stdout instead of writing to a file
    #[arg(long)]
    pub stdout: bool,

    /// Pretty print JSON output
    #[arg(long)]
    pub pretty: bool,

    /// Include hotspots in the export
    #[arg(long)]
    pub hotspots: bool,

    /// Include ledger entries in the export
    #[arg(long)]
    pub ledger: bool,

    /// Optional path scope to filter hotspots
    #[arg(long)]
    pub scope: Option<Vec<String>>,

    /// Export structured MADR fields from ledger
    #[arg(long)]
    pub madr: bool,

    /// Output as raw JSON instead of BridgeRecord
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExportDest {
    Stdout,
    File(String),
}

/// Slash-normalize scope prefixes (`trim`, `\\` → `/`). Also splits
/// leftover comma tokens so library callers cannot skip clap-layer
/// normalize. Empty tokens drop; empty result is `None`.
pub(crate) fn normalize_scope_prefixes(scope: Option<Vec<String>>) -> Option<Vec<String>> {
    let prefixes: Vec<String> = scope?
        .into_iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(|p| p.trim().replace('\\', "/"))
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
        })
        .collect();
    if prefixes.is_empty() {
        None
    } else {
        Some(prefixes)
    }
}

/// Shared machine predicate for `is_machine_output` and dest banners.
pub(crate) fn export_machine_flags(json: bool, stdout: bool, out: Option<&str>) -> bool {
    json || stdout || out == Some("-")
}

pub fn execute_export(args: ExportArgs) -> Result<()> {
    execute_export_with(
        args,
        |storage, history, query| {
            crate::impact::hotspots::calculate_hotspots_detailed(storage, history, query)
        },
        |storage| {
            let db = LedgerDb::new(storage.get_connection());
            db.get_recent_ledger_entries_paginated(LEDGER_PAGE, 0)
                .map_err(|e| miette!("{e}"))
        },
    )
}

pub(crate) fn execute_export_with<H, L>(
    args: ExportArgs,
    hotspots_fn: H,
    ledger_fn: L,
) -> Result<()>
where
    H: FnOnce(
        &StorageManager,
        &GixHistoryProvider<'_>,
        &HotspotQuery,
    ) -> Result<HotspotCalculation>,
    L: FnOnce(&StorageManager) -> Result<Vec<LedgerEntry>>,
{
    let mut args = args;
    args.scope = normalize_scope_prefixes(args.scope);

    if args.stdout && args.out_path.as_ref().is_some_and(|p| p != "-") {
        return Err(miette!(
            "bridge export: --stdout cannot be combined with --out <path>; \
             use --stdout alone, or -o - for stdout, or --out <file> for a file"
        ));
    }

    let layout = get_layout_or_cwd_if_not_git()?;
    let storage = StorageManager::open_read_only_sqlite_only(&layout)?;

    let project_id = layout.get_project_id();

    let repo = open_repo(layout.root.as_std_path())?;
    let (head_hash, branch_name) = get_head_info(&repo)?;
    let all_changes = get_repo_status(&repo)?;

    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let changes = crate::git::ignore::filter_ignored_changes(
        all_changes,
        &config.watch.ignore_patterns,
        true,
    )?;

    let snapshot = RepoSnapshot {
        head_hash,
        branch_name,
        is_clean: changes.is_empty(),
        changes,
    };

    let mut packet = map_snapshot_to_packet(snapshot, layout.root.as_std_path())?;
    let orchestrator = ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, &storage, &config, layout.root.as_std_path())?;
    packet.finalize();

    let mut datasets = Vec::new();
    let mut hotspots = Vec::new();
    let mut ledger_entries = Vec::new();

    if args.hotspots {
        let discovered = gix::discover(&layout.root).into_diagnostic()?;
        let history_provider = GixHistoryProvider::new(&discovered);
        let cancel = Arc::new(AtomicBool::new(false));
        let filter = hotspot_filter_string(&args.scope);
        let query = HotspotQuery {
            limit: config.hotspots.limit,
            commits: config.hotspots.max_commits,
            decay_half_life: config.hotspots.decay_half_life,
            dir_filters: args.scope.clone().unwrap_or_default(),
            budget: Some(AnalysisBudget::from_secs(
                config.hotspots.history_budget_secs,
                cancel,
            )),
            ..HotspotQuery::default()
        };
        match hotspots_fn(&storage, &history_provider, &query) {
            Ok(calc) => {
                let count = calc.hotspots.len();
                let empty_reason = (count == 0).then(|| "noMatches".to_string());
                datasets.push(ExportDataset {
                    name: "hotspots".to_string(),
                    requested: true,
                    included: true,
                    count,
                    source: Some("live".to_string()),
                    commits_requested: Some(query.commits),
                    commits_walked: Some(calc.commits_walked),
                    stop: walk_stop_token(calc.walk_stop).map(str::to_string),
                    limit: Some(query.limit),
                    filter: Some(filter),
                    empty_reason,
                    next: None,
                });
                hotspots = calc.hotspots;
            }
            Err(_) => {
                datasets.push(ExportDataset {
                    name: "hotspots".to_string(),
                    requested: true,
                    included: false,
                    count: 0,
                    source: Some("live".to_string()),
                    commits_requested: Some(query.commits),
                    commits_walked: None,
                    stop: None,
                    limit: Some(query.limit),
                    filter: Some(filter),
                    empty_reason: Some("historyError".to_string()),
                    next: None,
                });
            }
        }
    }

    if args.ledger {
        match ledger_fn(&storage) {
            Ok(entries) => {
                let count = entries.len();
                let empty_reason = (count == 0).then(|| "noMatches".to_string());
                datasets.push(ExportDataset {
                    name: "ledger".to_string(),
                    requested: true,
                    included: true,
                    count,
                    source: Some("ledgerSqlite".to_string()),
                    commits_requested: None,
                    commits_walked: None,
                    stop: None,
                    limit: Some(LEDGER_PAGE),
                    filter: None,
                    empty_reason,
                    next: None,
                });
                ledger_entries = entries;
            }
            Err(_) => {
                datasets.push(ExportDataset {
                    name: "ledger".to_string(),
                    requested: true,
                    included: false,
                    count: 0,
                    source: Some("ledgerSqlite".to_string()),
                    commits_requested: None,
                    commits_walked: None,
                    stop: None,
                    limit: Some(LEDGER_PAGE),
                    filter: None,
                    empty_reason: Some("ledgerError".to_string()),
                    next: None,
                });
            }
        }
    }

    if args.madr {
        datasets.push(ExportDataset {
            name: "madr".to_string(),
            requested: true,
            included: false,
            count: 0,
            source: None,
            commits_requested: None,
            commits_walked: None,
            stop: None,
            limit: None,
            filter: None,
            empty_reason: Some("notWired".to_string()),
            next: Some("ledgerful ledger adr export".to_string()),
        });
    }

    datasets.push(ExportDataset {
        name: "impact".to_string(),
        requested: false,
        included: true,
        count: 1,
        source: Some("workingTreeImpact".to_string()),
        commits_requested: None,
        commits_walked: None,
        stop: None,
        limit: None,
        filter: None,
        empty_reason: None,
        next: None,
    });
    datasets.sort_by(|a, b| a.name.cmp(&b.name));

    let mut context = HashMap::new();
    context.insert("risk_level".to_string(), format!("{:?}", packet.risk_level));
    context.insert(
        "hotspot_count".to_string(),
        dataset_count(&datasets, "hotspots").to_string(),
    );
    context.insert(
        "ledger_count".to_string(),
        dataset_count(&datasets, "ledger").to_string(),
    );

    let payload = BridgePayload::Snapshot(Box::new(SnapshotPayload {
        project_id: project_id.clone(),
        impact: packet,
        hotspots,
        ledger: ledger_entries,
        metadata: context,
        datasets: datasets.clone(),
    }));

    let record = BridgeRecord::new(BridgeDirection::Outbound, project_id, "snapshot", payload);

    let output = if args.pretty || args.json {
        serde_json::to_string_pretty(&record).into_diagnostic()?
    } else {
        serde_json::to_string(&record).into_diagnostic()?
    };

    let default_path = layout.state_subdir().join("bridge-export.json").to_string();
    let dest = resolve_export_dest(&args, default_path)?;
    let machine = is_export_machine(&args);

    match dest {
        ExportDest::File(path) => {
            let path_buf = std::path::PathBuf::from(&path);
            if let Some(parent) = path_buf.parent() {
                fs::create_dir_all(parent).into_diagnostic()?;
            }
            fs::write(&path, &output).into_diagnostic()?;
            if !machine {
                println!(
                    "Exported bridge snapshot to {}",
                    path.if_supports_color(Stream::Stdout, |s| s.cyan())
                );
                for row in datasets.iter().filter(|d| d.requested) {
                    let detail = row
                        .empty_reason
                        .as_deref()
                        .or(row.source.as_deref())
                        .unwrap_or("-");
                    println!("Dataset: {} {} ({})", row.name, row.count, detail);
                }
            }
        }
        ExportDest::Stdout => {
            println!("{}", output);
        }
    }

    Ok(())
}

pub(crate) fn is_export_machine(args: &ExportArgs) -> bool {
    export_machine_flags(args.json, args.stdout, args.out_path.as_deref())
}

pub(crate) fn resolve_export_dest(args: &ExportArgs, default_path: String) -> Result<ExportDest> {
    let out_is_dash = args.out_path.as_deref() == Some("-");
    if args.stdout && args.out_path.as_ref().is_some_and(|p| p != "-") {
        return Err(miette!(
            "bridge export: --stdout cannot be combined with --out <path>; \
             use --stdout alone, or -o - for stdout, or --out <file> for a file"
        ));
    }
    if args.stdout || out_is_dash || (args.json && args.out_path.is_none()) {
        return Ok(ExportDest::Stdout);
    }
    Ok(ExportDest::File(
        args.out_path.clone().unwrap_or(default_path),
    ))
}

fn hotspot_filter_string(scope: &Option<Vec<String>>) -> String {
    match scope {
        Some(prefixes) if !prefixes.is_empty() => prefixes.join(","),
        _ => "unfiltered".to_string(),
    }
}

fn walk_stop_token(stop: HistoryWalkStop) -> Option<&'static str> {
    match stop {
        HistoryWalkStop::Complete => None,
        HistoryWalkStop::Budget => Some("budget"),
        HistoryWalkStop::Cancelled => Some("cancelled"),
    }
}

fn dataset_count(datasets: &[ExportDataset], name: &str) -> usize {
    datasets
        .iter()
        .find(|d| d.name == name)
        .map(|d| d.count)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use crate::impact::packet::{CIPrediction, Hotspot, ImpactPacket, TemporalCoupling};
    use crate::state::layout::Layout;
    use crate::state::storage::StorageManager;
    use serial_test::serial;
    use std::path::PathBuf;
    use std::process::Command;

    #[allow(dead_code)]
    fn make_test_packet() -> ImpactPacket {
        ImpactPacket {
            hotspots: vec![
                Hotspot {
                    path: PathBuf::from("src/bridge/export.rs"),
                    score: 0.9,
                    display_score: 0.0,
                    complexity: 12,
                    frequency: 5.0,
                    centrality: Some(3),
                },
                Hotspot {
                    path: PathBuf::from("src/bridge/model.rs"),
                    score: 0.7,
                    display_score: 0.0,
                    complexity: 8,
                    frequency: 3.0,
                    centrality: Some(2),
                },
                Hotspot {
                    path: PathBuf::from("src/ledger/db.rs"),
                    score: 0.85,
                    display_score: 0.0,
                    complexity: 15,
                    frequency: 7.0,
                    centrality: Some(5),
                },
            ],
            temporal_couplings: vec![
                TemporalCoupling {
                    file_a: PathBuf::from("src/bridge/export.rs"),
                    file_b: PathBuf::from("src/bridge/model.rs"),
                    score: 0.85,
                },
                TemporalCoupling {
                    file_a: PathBuf::from("src/bridge/export.rs"),
                    file_b: PathBuf::from("src/ledger/db.rs"),
                    score: 0.6,
                },
                TemporalCoupling {
                    file_a: PathBuf::from("src/ledger/db.rs"),
                    file_b: PathBuf::from("src/ledger/types.rs"),
                    score: 0.95,
                },
            ],
            ci_predictions: vec![CIPrediction {
                job_name: "test".to_string(),
                platform: "github".to_string(),
                failure_probability: 0.15,
                explanation: None,
            }],
            ..ImpactPacket::default()
        }
    }

    #[allow(dead_code)]
    fn setup_test_output_dir() -> (tempfile::TempDir, String, Layout) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let root = dir.path().to_string_lossy().to_string();
        let layout = Layout::new(&root);

        let state_dir = layout.state_subdir();
        fs::create_dir_all(&state_dir).expect("failed to create state dir");
        let reports_dir = layout.reports_dir();
        fs::create_dir_all(&reports_dir).expect("failed to create reports dir");

        (dir, root, layout)
    }

    fn args() -> ExportArgs {
        ExportArgs {
            out_path: None,
            stdout: false,
            pretty: false,
            hotspots: false,
            ledger: false,
            scope: None,
            madr: false,
            json: false,
        }
    }

    #[test]
    fn test_export_args_parsing() {
        use clap::Parser;
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            export: ExportArgs,
        }

        let cli = TestCli::parse_from(["test", "--hotspots", "--ledger", "--out", "out.json"]);
        assert!(cli.export.hotspots);
        assert!(cli.export.ledger);
        assert_eq!(cli.export.out_path, Some("out.json".to_string()));
    }

    #[test]
    fn resolve_export_dest__stdout_and_out_path__errors() {
        let mut a = args();
        a.stdout = true;
        a.out_path = Some("out.json".to_string());
        let err = resolve_export_dest(&a, "default.json".to_string()).expect_err("mutex");
        assert!(err.to_string().contains("--stdout cannot be combined"));
    }

    #[test]
    fn resolve_export_dest__dash_or_json_alone__stdout() {
        let mut dash = args();
        dash.out_path = Some("-".to_string());
        assert_eq!(
            resolve_export_dest(&dash, "default.json".to_string()).expect("dash"),
            ExportDest::Stdout
        );

        let mut json = args();
        json.json = true;
        assert_eq!(
            resolve_export_dest(&json, "default.json".to_string()).expect("json"),
            ExportDest::Stdout
        );

        let mut json_out = args();
        json_out.json = true;
        json_out.out_path = Some("out.json".to_string());
        assert_eq!(
            resolve_export_dest(&json_out, "default.json".to_string()).expect("json out"),
            ExportDest::File("out.json".to_string())
        );
    }

    #[test]
    fn is_export_machine__json_stdout_or_dash() {
        let mut a = args();
        assert!(!is_export_machine(&a));
        a.json = true;
        assert!(is_export_machine(&a));
        a.json = false;
        a.stdout = true;
        assert!(is_export_machine(&a));
        a.stdout = false;
        a.out_path = Some("-".to_string());
        assert!(is_export_machine(&a));
    }

    #[test]
    fn export_dataset__camel_case_keys() {
        let row = ExportDataset {
            name: "hotspots".to_string(),
            requested: true,
            included: true,
            count: 0,
            source: Some("live".to_string()),
            commits_requested: Some(500),
            commits_walked: Some(0),
            stop: None,
            limit: Some(10),
            filter: Some("unfiltered".to_string()),
            empty_reason: Some("noMatches".to_string()),
            next: None,
        };
        let json = serde_json::to_string(&row).expect("ser");
        assert!(json.contains("commitsRequested"));
        assert!(json.contains("emptyReason"));
        assert!(!json.contains("commits_requested"));
        assert!(!json.contains("empty_reason"));
    }

    #[test]
    fn dataset_count__absent_is_zero() {
        let impact = ExportDataset {
            name: "impact".to_string(),
            requested: false,
            included: true,
            count: 1,
            source: Some("workingTreeImpact".to_string()),
            commits_requested: None,
            commits_walked: None,
            stop: None,
            limit: None,
            filter: None,
            empty_reason: None,
            next: None,
        };
        assert_eq!(dataset_count(&[impact], "hotspots"), 0);
        assert_eq!(dataset_count(&[], "ledger"), 0);
    }

    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    fn init_git_export_fixture() -> (tempfile::TempDir, CwdGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            Command::new("git")
                .args(["init"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "init",
                ])
                .current_dir(dir.path())
                .status()
                .expect("git commit")
                .success()
        );
        let root = camino::Utf8Path::from_path(dir.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())
            .expect("init sqlite");
        let prev = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("chdir");
        (dir, CwdGuard(prev))
    }

    fn snapshot_datasets(path: &std::path::Path) -> Vec<serde_json::Value> {
        let raw = fs::read_to_string(path).expect("read export");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
        v["payload"]["datasets"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    #[serial(cwd)]
    fn execute_export_with__hotspots_err__history_error_dataset() {
        let (dir, _cwd) = init_git_export_fixture();
        let out = dir.path().join("snap.json");
        let mut a = args();
        a.hotspots = true;
        a.json = true;
        a.out_path = Some(out.to_string_lossy().to_string());
        execute_export_with(
            a,
            |_s, _h, _q| Err(miette!("injected history fail")),
            |_s| Ok(Vec::new()),
        )
        .expect("export still emits");
        let datasets = snapshot_datasets(&out);
        let hotspots = datasets
            .iter()
            .find(|d| d["name"] == "hotspots")
            .expect("hotspots row");
        assert_eq!(hotspots["emptyReason"], "historyError");
        assert_eq!(hotspots["included"], false);
        assert_eq!(hotspots["count"], 0);
        assert_eq!(hotspots["requested"], true);
        let impact = datasets
            .iter()
            .find(|d| d["name"] == "impact")
            .expect("impact row");
        assert_eq!(impact["count"], 1);
        assert!(
            datasets
                .windows(2)
                .all(|w| w[0]["name"].as_str() <= w[1]["name"].as_str()),
            "datasets must be sorted by name: {datasets:?}"
        );
    }

    #[test]
    #[serial(cwd)]
    fn execute_export_with__ledger_err__ledger_error_dataset() {
        let (dir, _cwd) = init_git_export_fixture();
        let out = dir.path().join("snap.json");
        let mut a = args();
        a.ledger = true;
        a.json = true;
        a.out_path = Some(out.to_string_lossy().to_string());
        execute_export_with(
            a,
            |_s, _h, _q| unreachable!("hotspots not requested"),
            |_s| Err(miette!("injected ledger fail")),
        )
        .expect("export still emits");
        let datasets = snapshot_datasets(&out);
        let ledger = datasets
            .iter()
            .find(|d| d["name"] == "ledger")
            .expect("ledger row");
        assert_eq!(ledger["emptyReason"], "ledgerError");
        assert_eq!(ledger["included"], false);
        assert_eq!(ledger["count"], 0);
        assert_eq!(
            datasets.iter().find(|d| d["name"] == "hotspots"),
            None,
            "unrequested hotspots row must be omitted"
        );
    }

    #[test]
    #[serial(cwd)]
    fn execute_export_with__madr__not_wired() {
        let (dir, _cwd) = init_git_export_fixture();
        let out = dir.path().join("snap.json");
        let mut a = args();
        a.madr = true;
        a.json = true;
        a.out_path = Some(out.to_string_lossy().to_string());
        execute_export_with(
            a,
            |_s, _h, _q| unreachable!("hotspots not requested"),
            |_s| Ok(Vec::new()),
        )
        .expect("export");
        let datasets = snapshot_datasets(&out);
        let madr = datasets
            .iter()
            .find(|d| d["name"] == "madr")
            .expect("madr row");
        assert_eq!(madr["emptyReason"], "notWired");
        assert_eq!(madr["next"], "ledgerful ledger adr export");
        assert_eq!(madr["included"], false);
    }

    #[test]
    fn normalize_scope_prefixes__backslash_comma_and_empty() {
        assert_eq!(
            normalize_scope_prefixes(Some(vec![" src\\bridge ".to_string(), "docs".to_string()])),
            Some(vec!["src/bridge".to_string(), "docs".to_string()])
        );
        assert_eq!(
            normalize_scope_prefixes(Some(vec!["src/,docs/".to_string()])),
            Some(vec!["src/".to_string(), "docs/".to_string()])
        );
        assert_eq!(
            normalize_scope_prefixes(Some(vec!["  ".to_string(), ",".to_string()])),
            None
        );
        assert_eq!(normalize_scope_prefixes(None), None);
    }

    fn commit_file(dir: &std::path::Path, rel: &str, body: &str) {
        if let Some(parent) = dir.join(rel).parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(dir.join(rel), body).expect("write");
        assert!(
            Command::new("git")
                .args(["add", "-A"])
                .current_dir(dir)
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-m",
                    rel,
                ])
                .current_dir(dir)
                .status()
                .expect("git commit")
                .success()
        );
    }

    #[test]
    #[serial(cwd)]
    fn execute_export__hotspots_populated__count_and_metadata() {
        let (dir, _cwd) = init_git_export_fixture();
        commit_file(dir.path(), "src/lib.rs", "fn lib() {}\n");
        let out = dir.path().join("snap.json");
        let mut a = args();
        a.hotspots = true;
        a.ledger = true;
        a.json = true;
        a.out_path = Some(out.to_string_lossy().to_string());
        execute_export(a).expect("export");
        let raw = fs::read_to_string(&out).expect("read");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let datasets = v["payload"]["datasets"].as_array().expect("datasets");
        let hotspots = datasets
            .iter()
            .find(|d| d["name"] == "hotspots")
            .expect("hotspots row");
        let count = hotspots["count"].as_u64().expect("count");
        assert!(count > 0, "populated walk must emit count > 0: {hotspots}");
        assert!(
            hotspots["commitsWalked"].as_u64().expect("walked") > 0,
            "{hotspots}"
        );
        assert!(hotspots.get("emptyReason").is_none() || hotspots["emptyReason"].is_null());
        assert_eq!(
            v["payload"]["metadata"]["hotspot_count"],
            count.to_string(),
            "metadata.hotspot_count must equal dataset count"
        );
        let ledger = datasets
            .iter()
            .find(|d| d["name"] == "ledger")
            .expect("ledger row");
        assert_eq!(
            v["payload"]["metadata"]["ledger_count"],
            ledger["count"].as_u64().expect("ledger count").to_string()
        );
        let payload_len = v["payload"]["hotspots"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(payload_len as u64, count);
    }

    #[test]
    #[serial(cwd)]
    fn execute_export__hotspots_empty_history__no_matches() {
        let (dir, _cwd) = init_git_export_fixture();
        let out = dir.path().join("snap.json");
        let mut a = args();
        a.hotspots = true;
        a.json = true;
        a.out_path = Some(out.to_string_lossy().to_string());
        execute_export(a).expect("export");
        let datasets = snapshot_datasets(&out);
        let hotspots = datasets
            .iter()
            .find(|d| d["name"] == "hotspots")
            .expect("hotspots row");
        assert_eq!(hotspots["emptyReason"], "noMatches");
        assert_eq!(hotspots["count"], 0);
        assert_eq!(hotspots["included"], true);
        let raw = fs::read_to_string(&out).expect("read");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(v["payload"]["metadata"]["hotspot_count"], "0");
    }
}
