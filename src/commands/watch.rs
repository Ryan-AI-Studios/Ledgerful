use camino::Utf8PathBuf;
use miette::Result;
use owo_colors::OwoColorize;
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::bridge::notify::push_risk_alert;
use crate::config::load::load_config;
use crate::impact::temporal::{GixHistoryProvider, TemporalEngine};
use crate::index::incremental::IncrementalSyncEngine;
use crate::index::orchestrator::ProjectIndexer;
use crate::ledger::drift::DriftManager;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::watch::batch::WatchBatch;
use crate::watch::debounce::Watcher;

/// Throttle temporal coupling checks to every Nth batch.
const TEMPORAL_CHECK_INTERVAL: usize = 10;

pub fn execute_watch(interval_ms: u64, json_output: bool, no_graph_sync: bool) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let path = Utf8PathBuf::from_path_buf(current_dir)
        .map_err(|e| miette::miette!("Invalid UTF-8 path: {:?}", e))?;
    let layout = Layout::new(path.as_str());
    let config = load_config(&layout)?;
    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();

    ctrlc::set_handler(move || {
        signal.store(false, Ordering::SeqCst);
    })
    .map_err(|e| miette::miette!("Failed to install Ctrl+C handler: {}", e))?;

    if !json_output {
        println!(
            "{} {}  {}",
            "Watching:".bold().green(),
            path,
            "(press Ctrl+C to stop)".dimmed()
        );
    }

    // Use config debounce_ms when no CLI override is provided (interval_ms == 0)
    let effective_interval = if interval_ms > 0 {
        interval_ms
    } else {
        config.watch.debounce_ms
    };

    let batch_path = layout.state_subdir().join("current-batch.json");
    let db_path = layout.state_subdir().join("ledger.db");
    let repo_root = path.clone();
    let config_for_callback = config.clone();
    let callback = Box::new(move |batch: WatchBatch| {
        if !json_output {
            println!(
                "\n{} - Received batch of {} events",
                batch
                    .timestamp
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
                    .dimmed(),
                batch.events.len().bold()
            );

            for event in &batch.events {
                let kind_str = format!("{:?}", event.kind);
                let kind_colored = match event.kind {
                    crate::watch::batch::WatchEventKind::Create => kind_str.green().to_string(),
                    crate::watch::batch::WatchEventKind::Modify => kind_str.yellow().to_string(),
                    crate::watch::batch::WatchEventKind::Delete => kind_str.red().to_string(),
                    _ => kind_str,
                };
                println!("  [{}] {}", kind_colored, event.path);
            }
        } else {
            // In JSON mode, we just emit the batch as a single line
            if let Ok(json) = serde_json::to_string(&batch) {
                println!("{}", json);
            }
        }

        if let Err(err) = batch.save(&batch_path) {
            tracing::warn!("Failed to save watch batch JSON: {err}");
        }

        if let Ok(mut storage) = StorageManager::init(db_path.as_std_path()) {
            if let Ok(batch_json) = serde_json::to_string(&batch) {
                let _ = storage.save_batch(
                    &batch.timestamp.to_rfc3339(),
                    batch.events.len() as u32,
                    &batch_json,
                );
            }

            // Drift detection
            let drift_config = load_config(&layout).unwrap_or_default();
            let mut drift_mgr = DriftManager::new(
                storage.get_connection_mut(),
                repo_root.as_std_path().to_path_buf(),
                drift_config,
            );
            for event in &batch.events {
                if let Err(e) = drift_mgr.process_event(event.path.as_str()) {
                    tracing::warn!("Failed to process drift for {}: {:?}", event.path, e);
                }
            }

            // Incremental graph sync
            if !no_graph_sync {
                let indexer =
                    ProjectIndexer::new(storage, repo_root.clone(), config_for_callback.clone());
                let mut engine = IncrementalSyncEngine::new(indexer, repo_root.clone());
                match engine.process_batch(&batch) {
                    Ok(delta) => {
                        tracing::info!(
                            "Incremental sync: {} files, +{} nodes, -{} nodes, +{} edges, -{} edges",
                            delta.files_processed,
                            delta.nodes_added,
                            delta.nodes_removed,
                            delta.edges_added,
                            delta.edges_removed,
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Incremental graph sync failed: {}", e);
                    }
                }
            }

            // Temporal coupling risk alerts (throttled)
            check_temporal_coupling_alerts(&batch, &layout, &repo_root);
        }
    });

    let _watcher = Watcher::new(
        vec![path],
        Duration::from_millis(effective_interval),
        config.watch.ignore_patterns,
        callback,
    )
    .map_err(|e| miette::miette!("Failed to start watcher: {}", e))?;

    while running.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(1));
    }

    if !json_output {
        println!("{}", "Watch stopped.".yellow());
    }
    Ok(())
}

/// Throttled temporal coupling check for risk alerts.
///
/// Runs every `TEMPORAL_CHECK_INTERVAL` batches to avoid excessive git history analysis.
/// For each coupling pair above the risk threshold, extracts approximate affected symbols
/// from the changed file paths and emits a `RiskAlert` via the IPC bridge (fire-and-forget).
fn check_temporal_coupling_alerts(batch: &WatchBatch, layout: &Layout, repo_root: &Utf8PathBuf) {
    static BATCH_COUNT: AtomicUsize = AtomicUsize::new(0);
    let count = BATCH_COUNT.fetch_add(1, Ordering::Relaxed);
    if !count.is_multiple_of(TEMPORAL_CHECK_INTERVAL) {
        return;
    }

    let threshold = match load_config(layout) {
        Ok(cfg) => cfg.temporal.coupling_threshold as f64,
        Err(_) => return,
    };

    let repo = match gix::open(repo_root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Cannot open git repo for temporal coupling check: {:?}", e);
            return;
        }
    };

    let provider = GixHistoryProvider::new(&repo);
    let config = match load_config(layout) {
        Ok(cfg) => cfg.temporal,
        Err(_) => return,
    };

    let engine = TemporalEngine::new(provider, config);
    let couplings = match engine.calculate_couplings() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("Temporal coupling calculation failed: {:?}", e);
            return;
        }
    };

    // Collect changed file paths from this batch for symbol extraction
    let changed_paths: std::collections::HashSet<String> = batch
        .events
        .iter()
        .map(|ev| ev.path.as_str().to_string())
        .collect();

    for coupling in &couplings {
        let score = coupling.score as f64;
        if score < threshold {
            continue;
        }

        let file_a_str = coupling.file_a.to_string_lossy().to_string();
        let file_b_str = coupling.file_b.to_string_lossy().to_string();

        // Only alert if at least one of the coupled files was changed in this batch
        if !changed_paths.contains(&file_a_str) && !changed_paths.contains(&file_b_str) {
            continue;
        }

        // Derive affected symbols from changed file paths (approximate).
        // Uses file stems as a proxy for symbol names when real symbol analysis
        // is not available during watch.
        let mut affected_symbols: Vec<String> = Vec::new();
        for path in &changed_paths {
            if let Some(stem) = std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
            {
                affected_symbols.push(stem.to_string());
            }
        }
        affected_symbols.sort();
        affected_symbols.dedup();

        let risk_level = if score > 0.95 { "High" } else { "Medium" };

        let suggested_remediation = format!(
            "High temporal coupling ({:.0}%) between {} and {}. Consider testing both files together and reviewing shared dependencies.",
            score * 100.0,
            file_a_str,
            file_b_str,
        );

        push_risk_alert(
            &file_a_str,
            &file_b_str,
            score,
            &affected_symbols,
            &suggested_remediation,
            risk_level,
            threshold,
        );
    }
}
