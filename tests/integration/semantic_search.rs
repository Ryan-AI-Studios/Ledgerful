//! Integration tests for VectorStore::query covering all reachable query tiers.
//!
//! - Tier 1: HNSW approximate nearest-neighbour index (L2 over normalized vectors)
//! - Tier 2: Cozo-native cos_dist fallback
//! - Tier 3: Rust-side cosine_sim last-resort safety net (not reachable with cozo-redux;
//!   preserved as a guard against a future fork that drops `cos_dist`.)
//!
//! See `conductor/track56-1/`.

use ledgerful::index::symbols::SymbolKind;
use ledgerful::semantic::chunker::AstChunk;
use ledgerful::semantic::vector_store::VectorStore;
use ledgerful::state::storage_cozo::CozoStorage;
use tempfile::TempDir;

fn sled_cozo(_tmp: &TempDir) -> CozoStorage {
    CozoStorage::new_in_memory().unwrap()
}

fn make_chunk(file_path: &str, name: &str, offset: usize, content: &str) -> AstChunk {
    AstChunk {
        file_path: file_path.to_string(),
        name: name.to_string(),
        offset,
        content: content.to_string(),
        kind: SymbolKind::Function,
        docstring: None,
        range: (0, content.len()),
        lines: (offset, offset + 1),
    }
}

fn consistency_chunks() -> Vec<AstChunk> {
    (0..5)
        .map(|i| make_chunk(&format!("f{}.rs", i), &format!("fn_{}", i), i, "fn"))
        .collect()
}

fn consistency_embeddings() -> Vec<Vec<f32>> {
    vec![
        vec![1.0, 2.0, 3.0],
        vec![2.0, 0.5, 0.0],
        vec![0.0, 0.0, 5.0],
        vec![3.0, 3.0, 3.0],
        vec![0.1, 10.0, 0.1],
    ]
}

/// Tier 1: HNSW happy path using cosine-equivalent normalized L2 distance.
#[test]
fn hnsw_query_returns_ordered_results() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = sled_cozo(&tmp);
    let store = VectorStore::new(&storage, 3, false).unwrap();

    let chunks = vec![
        make_chunk("a.rs", "fn_a", 0, "fn a"),
        make_chunk("b.rs", "fn_b", 1, "fn b"),
        make_chunk("c.rs", "fn_c", 2, "fn c"),
    ];
    // Non-unit embeddings where chunk[0] is closest to [1.0, 0.0, 0.0].
    let embeddings = vec![
        vec![2.0, 0.0, 0.0],
        vec![0.0, 3.0, 0.0],
        vec![0.0, 0.0, 4.0],
    ];

    store.index_chunks(chunks, embeddings).unwrap();

    let results = store.query(vec![0.9, 0.1, 0.0], 2).unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "a.rs");
}

/// Tier 2: cos_dist fallback, with no HNSW index present.
#[test]
fn cos_dist_fallback_when_hnsw_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = sled_cozo(&tmp);
    let store = VectorStore::new_without_hnsw(&storage, 3).unwrap();

    let chunks = vec![
        make_chunk("x.rs", "fn_x", 0, "fn x"),
        make_chunk("y.rs", "fn_y", 1, "fn y"),
    ];
    let embeddings = vec![vec![2.0, 0.0, 0.0], vec![0.0, 3.0, 0.0]];

    store.index_chunks(chunks, embeddings).unwrap();

    let results = store.query(vec![0.9, 0.1, 0.0], 2).unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "x.rs");
}

/// HNSW normalized L2 and cos_dist fallback return the same ordering for the same data.
#[test]
fn hnsw_and_cos_dist_produce_same_ordering() {
    let hnsw_tmp = tempfile::tempdir().unwrap();
    let hnsw_storage = sled_cozo(&hnsw_tmp);
    let store_hnsw = VectorStore::new(&hnsw_storage, 3, false).unwrap();
    store_hnsw
        .index_chunks(consistency_chunks(), consistency_embeddings())
        .unwrap();
    let hnsw_results = store_hnsw.query(vec![1.0, 1.0, 1.0], 5).unwrap();

    let cos_tmp = tempfile::tempdir().unwrap();
    let cos_storage = sled_cozo(&cos_tmp);
    let store_no_hnsw = VectorStore::new_without_hnsw(&cos_storage, 3).unwrap();
    store_no_hnsw
        .index_chunks(consistency_chunks(), consistency_embeddings())
        .unwrap();
    let cos_dist_results = store_no_hnsw.query(vec![1.0, 1.0, 1.0], 5).unwrap();

    assert_eq!(hnsw_results.len(), cos_dist_results.len());
    let hnsw_names: Vec<&str> = hnsw_results.iter().map(|(_, n, _, _)| n.as_str()).collect();
    let cos_names: Vec<&str> = cos_dist_results
        .iter()
        .map(|(_, n, _, _)| n.as_str())
        .collect();
    assert_eq!(&hnsw_names[..2], &cos_names[..2]);
}

/// A dimension mismatch between query and stored vectors must produce an error.
#[test]
fn dimension_mismatch_produces_error() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = sled_cozo(&tmp);
    let store = VectorStore::new_without_hnsw(&storage, 3).unwrap();

    let chunks = vec![make_chunk("p.rs", "fn_p", 0, "fn p")];
    let embeddings = vec![vec![1.0, 0.0, 0.0]];

    store.index_chunks(chunks, embeddings).unwrap();

    let result = store.query(vec![0.9, 0.1, 0.0, 0.0], 2);
    assert!(result.is_err(), "Dimension mismatch must return an error");
}

/// Verify that remove_file_snippets deletes only the specified file's snippets.
#[test]
fn test_remove_file_snippets() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = sled_cozo(&tmp);
    let store = VectorStore::new_without_hnsw(&storage, 3).unwrap();

    let chunks = vec![
        make_chunk("file_a.rs", "fn_a", 0, "fn a"),
        make_chunk("file_b.rs", "fn_b", 1, "fn b"),
    ];
    let embeddings = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];

    store.index_chunks(chunks, embeddings).unwrap();
    assert_eq!(store.get_vector_count().unwrap(), 2);

    store.remove_file_snippets("file_a.rs").unwrap();

    assert_eq!(store.get_vector_count().unwrap(), 1);

    // Verify file_b.rs is still present
    let results = store.query(vec![0.0, 1.0, 0.0], 2).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "file_b.rs");
}

/// Verify that file hash tracking (ensure_file_hash_schema, record_file_hash, is_file_hash_current) works correctly.
#[test]
fn test_file_hash_tracking() {
    use ledgerful::config::model::LocalModelConfig;
    use ledgerful::semantic::SemanticDiscovery;
    use std::path::Path;

    let tmp = tempfile::tempdir().unwrap();
    let storage = sled_cozo(&tmp);

    let config = LocalModelConfig {
        dimensions: 3,
        disable_hnsw: true,
        ..Default::default()
    };

    let semantic = SemanticDiscovery::new(config, &storage).unwrap();
    semantic.ensure_file_hash_schema().unwrap();

    let path_a = Path::new("src/foo.rs");
    let hash_a = "abc123hash";

    // Initially hash should not be current
    assert!(!semantic.is_file_hash_current(path_a, hash_a));

    // Record the hash
    semantic.record_file_hash(path_a, hash_a).unwrap();

    // Now it should be current
    assert!(semantic.is_file_hash_current(path_a, hash_a));

    // A different hash for the same file should not be current
    assert!(!semantic.is_file_hash_current(path_a, "different_hash"));

    // A different file with the same hash should not be current
    let path_b = Path::new("src/bar.rs");
    assert!(!semantic.is_file_hash_current(path_b, hash_a));
}

/// Verify get_tracked_files and remove_file_hash helpers.
#[test]
fn test_get_tracked_files_and_remove_file_hash() {
    use ledgerful::config::model::LocalModelConfig;
    use ledgerful::semantic::SemanticDiscovery;
    use std::path::Path;

    let tmp = tempfile::tempdir().unwrap();
    let storage = sled_cozo(&tmp);

    let config = LocalModelConfig {
        dimensions: 3,
        disable_hnsw: true,
        ..Default::default()
    };

    let semantic = SemanticDiscovery::new(config, &storage).unwrap();
    semantic.ensure_file_hash_schema().unwrap();

    let path_a = Path::new("src/foo.rs");
    let path_b = Path::new("src/bar.rs");

    semantic.record_file_hash(path_a, "hasha").unwrap();
    semantic.record_file_hash(path_b, "hashb").unwrap();

    let mut tracked = semantic.get_tracked_files().unwrap();
    tracked.sort();
    assert_eq!(tracked.len(), 2);
    assert_eq!(tracked[0], "src/bar.rs");
    assert_eq!(tracked[1], "src/foo.rs");

    // Remove one hash
    semantic.remove_file_hash("src/foo.rs").unwrap();

    let tracked = semantic.get_tracked_files().unwrap();
    assert_eq!(tracked.len(), 1);
    assert_eq!(tracked[0], "src/bar.rs");
}

/// Verify that on incremental indexing, files missing from filesystem are pruned from both snippets and hashes.
#[test]
fn test_incremental_deletions_pruning() {
    use ledgerful::config::model::LocalModelConfig;
    use ledgerful::semantic::SemanticDiscovery;

    let tmp = tempfile::tempdir().unwrap();
    let storage = sled_cozo(&tmp);

    let config = LocalModelConfig {
        dimensions: 3,
        disable_hnsw: true,
        ..Default::default()
    };

    let semantic = SemanticDiscovery::new(config, &storage).unwrap();
    semantic.ensure_file_hash_schema().unwrap();

    // Create a temporary workspace
    let ws_dir = tempfile::tempdir().unwrap();
    let file_a = ws_dir.path().join("a.rs");
    let file_b = ws_dir.path().join("b.rs");

    std::fs::write(&file_a, "fn a() {}").unwrap();
    std::fs::write(&file_b, "fn b() {}").unwrap();

    // Mock record hashes and snippets
    let path_a_str = file_a.to_string_lossy().to_string();
    let path_b_str = file_b.to_string_lossy().to_string();

    semantic.record_file_hash(&file_a, "hasha").unwrap();
    semantic.record_file_hash(&file_b, "hashb").unwrap();

    let chunks = vec![
        make_chunk(&path_a_str, "fn_a", 0, "fn a() {}"),
        make_chunk(&path_b_str, "fn_b", 0, "fn b() {}"),
    ];
    let embeddings = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];
    semantic.index_chunks_batched(chunks, embeddings).unwrap();

    // Verify initial state
    assert_eq!(semantic.get_tracked_files().unwrap().len(), 2);
    assert_eq!(semantic.get_vector_count().unwrap(), 2);

    // Now remove file_a from the filesystem
    std::fs::remove_file(&file_a).unwrap();

    // Simulating the incremental deletions phase in execute_semantic_index:
    let tracked_files = semantic.get_tracked_files().unwrap();
    for tracked in tracked_files {
        let path = std::path::Path::new(&tracked);
        if !path.exists() {
            semantic.remove_file_snippets(&tracked).unwrap();
            semantic.remove_file_hash(&tracked).unwrap();
        }
    }

    // Verify file_a's snippets and hashes are completely pruned
    let remaining_hashes = semantic.get_tracked_files().unwrap();
    assert_eq!(remaining_hashes.len(), 1);
    assert_eq!(remaining_hashes[0], path_b_str.replace('\\', "/"));

    assert_eq!(semantic.get_vector_count().unwrap(), 1);
    let remaining_query = semantic.query_raw(vec![0.0, 1.0, 0.0], 5).unwrap();
    assert_eq!(remaining_query.len(), 1);
    assert_eq!(remaining_query[0].0, path_b_str.replace('\\', "/"));
}
