use crate::bridge::model::{BridgeDirection, BridgePayload, BridgeRecord, SnapshotPayload};
use crate::commands::helpers::get_layout;
use crate::git::RepoSnapshot;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::impact::hotspots::calculate_hotspots;
use crate::impact::orchestrator::{ImpactOrchestrator, map_snapshot_to_packet};
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::db::LedgerDb;
use crate::state::storage::StorageManager;
use clap::Args;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::collections::HashMap;
use std::fs;

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

pub fn execute_export(args: ExportArgs) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;

    let project_id = layout.get_project_id();

    // 1. Core Impact Discovery
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

    let discovered = gix::discover(&layout.root).into_diagnostic()?;
    let history_provider = GixHistoryProvider::new(&discovered);

    // 2. Enrichment: Hotspots
    let hotspots = if args.hotspots {
        let hotspot_query = crate::impact::hotspots::HotspotQuery {
            limit: 25,
            ..Default::default()
        };
        calculate_hotspots(&storage, &history_provider, &hotspot_query).unwrap_or_default()
    } else {
        vec![]
    };

    // 3. Enrichment: Ledger
    let ledger_entries = if args.ledger {
        let db = LedgerDb::new(storage.get_connection());
        db.get_recent_ledger_entries_paginated(10, 0)
            .unwrap_or_default()
    } else {
        vec![]
    };

    // 4. Transform to BridgeRecord
    let mut context = HashMap::new();
    context.insert("risk_level".to_string(), format!("{:?}", packet.risk_level));
    context.insert("hotspot_count".to_string(), hotspots.len().to_string());
    context.insert("ledger_count".to_string(), ledger_entries.len().to_string());

    let payload = BridgePayload::Snapshot(Box::new(SnapshotPayload {
        project_id: project_id.clone(),
        impact: packet,
        hotspots,
        ledger: ledger_entries,
        metadata: context,
    }));

    let record = BridgeRecord::new(BridgeDirection::Outbound, project_id, "snapshot", payload);

    // 5. Output
    let output = if args.pretty || args.json {
        serde_json::to_string_pretty(&record).into_diagnostic()?
    } else {
        serde_json::to_string(&record).into_diagnostic()?
    };

    let dest_path = if args.stdout {
        None
    } else {
        Some(
            args.out_path
                .clone()
                .unwrap_or_else(|| layout.state_subdir().join("bridge-export.json").to_string()),
        )
    };

    if let Some(path) = dest_path {
        let path_buf = std::path::PathBuf::from(&path);
        if let Some(parent) = path_buf.parent() {
            fs::create_dir_all(parent).into_diagnostic()?;
        }
        fs::write(&path, &output).into_diagnostic()?;
        eprintln!("Exported bridge snapshot to {}", path.cyan());
    } else {
        println!("{}", output);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{CIPrediction, Hotspot, ImpactPacket, TemporalCoupling};
    use crate::state::layout::Layout;
    use std::path::PathBuf;

    /// Helper: create a minimal ImpactPacket with hotspots and couplings for testing
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

        // Create necessary subdirectories
        let state_dir = layout.state_subdir();
        fs::create_dir_all(&state_dir).expect("failed to create state dir");
        let reports_dir = layout.reports_dir();
        fs::create_dir_all(&reports_dir).expect("failed to create reports dir");

        (dir, root, layout)
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
}
