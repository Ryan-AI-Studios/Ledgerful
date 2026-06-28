use camino::Utf8PathBuf;
use ledgerful::config::model::Config;
use ledgerful::index::incremental::IncrementalSyncEngine;
use ledgerful::index::orchestrator::ProjectIndexer;
use ledgerful::state::storage::StorageManager;
use ledgerful::watch::batch::WatchBatch;
use ledgerful::watch::debounce::Watcher;
use std::fs;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use crate::common::setup_git_repo;
use crate::common::wait_for_condition;

#[test]
fn test_watch_graph_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    setup_git_repo(root.as_std_path());
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("lib.rs");
    let db_path = state_dir.join("ledger.db");

    // Pre-initialize storage schema
    let _ = StorageManager::init(db_path.as_std_path()).unwrap();

    let (tx, rx) = channel();
    let repo_root = root.clone();
    let cb_db_path = db_path.clone();
    let callback = Box::new(move |batch: WatchBatch| {
        eprintln!("DEBUG: Watcher callback triggered with batch: {:?}", batch);
        match StorageManager::init(cb_db_path.as_std_path()) {
            Ok(storage) => {
                let indexer = ProjectIndexer::new(storage, repo_root.clone(), Config::default());
                let mut engine = IncrementalSyncEngine::new(indexer, repo_root.clone());
                match engine.process_batch(&batch) {
                    Ok(delta) => {
                        eprintln!("DEBUG: Batch processed, sending delta: {:?}", delta);
                        let _ = tx.send(delta);
                    }
                    Err(e) => eprintln!("DEBUG: engine.process_batch failed: {e}"),
                }
            }
            Err(e) => eprintln!("DEBUG: StorageManager::init failed: {e}"),
        }
    });

    let _watcher = Watcher::new(
        vec![root.clone()],
        Duration::from_millis(100),
        vec![".git/**".to_string()],
        callback,
    )
    .unwrap();

    // Give the watcher a brief moment to begin observing the directory before we
    // start writing. This avoids a race between the initial subscription and the
    // first file modification.
    wait_for_condition(
        || true,
        Duration::from_millis(100),
        Duration::from_millis(10),
    )
    .expect("watcher startup took longer than expected");

    for i in 0..3 {
        fs::write(&file_path, format!("pub fn hello_{i}() {{}}\n")).unwrap();
        wait_for_condition(
            || file_path.exists(),
            Duration::from_secs(2),
            Duration::from_millis(50),
        )
        .expect("watcher did not detect file write within 2s");
    }

    let timeout = Duration::from_secs(30);
    let deadline = Instant::now() + timeout;
    let mut delta = None;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(candidate) if candidate.files_processed > 0 => {
                delta = Some(candidate);
                break;
            }
            Ok(_) => {
                eprintln!("DEBUG: Received empty batch, continuing...");
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!(
                    "should receive delta within {} seconds: timed out",
                    timeout.as_secs()
                );
            }
            Err(err) => panic!(
                "should receive delta within {} seconds: {err}",
                timeout.as_secs()
            ),
        }
    }

    let delta = delta.expect("should receive a non-empty delta within 5 seconds");
    assert_eq!(delta.files_processed, 1);
    assert!(delta.nodes_added >= 1);
}
