use camino::Utf8PathBuf;
use ledgerful::config::model::Config;
use ledgerful::index::graph_loader::build_native_graph;
use ledgerful::index::incremental::IncrementalSyncEngine;
use ledgerful::index::orchestrator::ProjectIndexer;
use ledgerful::state::storage::StorageManager;
use ledgerful::watch::batch::{WatchBatch, WatchEvent, WatchEventKind};
use std::fs;

use crate::common::setup_git_repo;

fn setup_repo(root: &Utf8PathBuf) {
    setup_git_repo(root.as_std_path());
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn helper() {}\npub fn main() { helper(); }\n",
    )
    .unwrap();
    fs::write(
        src.join("utils.rs"),
        "pub fn util() {}\npub fn call_util() { util(); }\n",
    )
    .unwrap();
    fs::write(
        src.join("models.rs"),
        "pub struct Model;\nimpl Model { pub fn new() -> Self { Self } }\n",
    )
    .unwrap();
}

#[test]
fn test_incremental_graph_consistency() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    setup_repo(&root);

    let state_a = root.join(".ledgerful_a").join("state");
    fs::create_dir_all(&state_a).unwrap();
    let db_a = state_a.join("ledger.db");

    let state_b = root.join(".ledgerful_b").join("state");
    fs::create_dir_all(&state_b).unwrap();
    let db_b = state_b.join("ledger.db");

    // --- Build graph A via full index ---
    let storage_a = StorageManager::init(db_a.as_std_path()).unwrap();
    let mut indexer_a = ProjectIndexer::new(storage_a, root.clone(), Config::default());
    indexer_a.full_index().unwrap();
    indexer_a.build_call_graph().unwrap();
    {
        let cozo = indexer_a.cozo().expect("CozoDB A should be available");
        build_native_graph(indexer_a.storage(), cozo, "full", &Config::default()).unwrap();
    }

    // --- Apply 5 mutations via IncrementalSyncEngine ---
    let mut engine = IncrementalSyncEngine::new(indexer_a, root.clone());

    // Mutation 1: modify lib.rs (more calls)
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn helper() {}\npub fn main() { helper(); helper(); }\n",
    )
    .unwrap();
    let batch1 = WatchBatch::new(vec![WatchEvent {
        path: root.join("src").join("lib.rs"),
        kind: WatchEventKind::Modify,
    }]);
    let _ = engine.process_batch(&batch1).unwrap();

    // Mutation 2: add a new file
    fs::write(
        root.join("src").join("new.rs"),
        "pub fn fresh() {}\npub fn caller() { fresh(); }\n",
    )
    .unwrap();
    let batch2 = WatchBatch::new(vec![WatchEvent {
        path: root.join("src").join("new.rs"),
        kind: WatchEventKind::Create,
    }]);
    let _ = engine.process_batch(&batch2).unwrap();

    // Mutation 3: modify utils.rs
    fs::write(
        root.join("src").join("utils.rs"),
        "pub fn util() {}\npub fn call_util() { util(); util(); }\n",
    )
    .unwrap();
    let batch3 = WatchBatch::new(vec![WatchEvent {
        path: root.join("src").join("utils.rs"),
        kind: WatchEventKind::Modify,
    }]);
    let _ = engine.process_batch(&batch3).unwrap();

    // Mutation 4: delete models.rs (leaf file, no edges)
    fs::remove_file(root.join("src").join("models.rs")).unwrap();
    let batch4 = WatchBatch::new(vec![WatchEvent {
        path: root.join("src").join("models.rs"),
        kind: WatchEventKind::Delete,
    }]);
    let delta4 = engine.process_batch(&batch4).unwrap();
    eprintln!("DEBUG delta4: {:?}", delta4);

    // Mutation 5: modify lib.rs again
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn helper() { println!(\"hi\"); }\npub fn main() { helper(); }\n",
    )
    .unwrap();
    let batch5 = WatchBatch::new(vec![WatchEvent {
        path: root.join("src").join("lib.rs"),
        kind: WatchEventKind::Modify,
    }]);
    let delta5 = engine.process_batch(&batch5).unwrap();
    eprintln!("DEBUG delta5: {:?}", delta5);

    // --- Build graph B via full index into fresh DB ---
    let storage_b = StorageManager::init(db_b.as_std_path()).unwrap();
    let mut indexer_b = ProjectIndexer::new(storage_b, root.clone(), Config::default());
    indexer_b.full_index().unwrap();
    indexer_b.build_call_graph().unwrap();
    {
        let cozo = indexer_b.cozo().expect("CozoDB B should be available");
        build_native_graph(indexer_b.storage(), cozo, "full", &Config::default()).unwrap();
    }

    // --- Compare A and B ---
    let cozo_a = engine
        .indexer
        .cozo()
        .expect("CozoDB A should still be available");
    let cozo_b = indexer_b.cozo().expect("CozoDB B should be available");

    let all_a = cozo_a.run_script("?[id] := *node{id: id}").unwrap();
    let ids_a: Vec<String> = all_a
        .rows
        .iter()
        .filter_map(|r| match r.first() {
            Some(cozo::DataValue::Str(s)) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    eprintln!("DEBUG nodes in A: {:?}", ids_a);
    let all_b = cozo_b.run_script("?[id] := *node{id: id}").unwrap();
    let ids_b: Vec<String> = all_b
        .rows
        .iter()
        .filter_map(|r| match r.first() {
            Some(cozo::DataValue::Str(s)) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    eprintln!("DEBUG nodes in B: {:?}", ids_b);

    let nodes_a = cozo_a.node_count().unwrap();
    let nodes_b = cozo_b.node_count().unwrap();
    let edges_a = cozo_a.edge_count().unwrap();
    let edges_b = cozo_b.edge_count().unwrap();

    assert_eq!(
        nodes_a, nodes_b,
        "Node count mismatch: A={} B={}",
        nodes_a, nodes_b
    );
    assert_eq!(
        edges_a, edges_b,
        "Edge count mismatch: A={} B={}",
        edges_a, edges_b
    );

    // Reachability query: direct callees of main
    let reach_a = cozo_a
        .run_script("?[target] := *edge{source: 'main', target: target}")
        .unwrap();
    let reach_b = cozo_b
        .run_script("?[target] := *edge{source: 'main', target: target}")
        .unwrap();
    assert_eq!(
        reach_a.rows.len(),
        reach_b.rows.len(),
        "Reachability query result count mismatch"
    );
}
