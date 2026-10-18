use super::SemanticIndexJsonResult;
use super::chunk_failures::*;
use super::skip_warm_incremental_file;
use super::stages::SemanticFileError;
use crate::config::model::LocalModelConfig;
use crate::index::symbols::SymbolKind;
use crate::semantic::SemanticDiscovery;
use crate::semantic::chunker::{AstChunk, semantic_file_content_hash};
use crate::semantic::provenance::{ProvenanceWalk, collect_semantic_provenance};
use crate::state::storage_cozo::CozoStorage;
use rstest::rstest;
use std::path::PathBuf;

const OLD: &str = "fn old() {}\n";
const NEW: &str = "fn new_and_broken() {}\n";

fn semantic_for(storage: &CozoStorage) -> SemanticDiscovery<'_> {
    let config = LocalModelConfig {
        dimensions: 3,
        disable_hnsw: true,
        ..Default::default()
    };
    let semantic = SemanticDiscovery::new(config, storage).expect("semantic");
    semantic.ensure_file_hash_schema().expect("hash schema");
    semantic
}

fn chunk_for(key: &str) -> AstChunk {
    AstChunk {
        file_path: key.to_string(),
        name: "f".to_string(),
        qualified_name: None,
        kind: SymbolKind::Function,
        content: "fn f() {}".to_string(),
        docstring: None,
        range: (0, 0),
        lines: (1, 1),
        offset: 0,
    }
}

/// Index `key` with one snippet and a recorded hash of `content`.
fn seed(semantic: &SemanticDiscovery<'_>, key: &str, content: &str) {
    semantic
        .index_chunks_batched(vec![chunk_for(key)], vec![vec![0.1, 0.2, 0.3]])
        .expect("index chunk");
    semantic
        .record_file_hash(key, &semantic_file_content_hash(content))
        .expect("record hash");
}

fn chunk_error(root: &std::path::Path, rel: &str, content: &str) -> SemanticFileError {
    SemanticFileError::Chunk {
        file: root.join(rel),
        content_hash: semantic_file_content_hash(content),
        message: "boom".to_string(),
    }
}

fn write(root: &std::path::Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(p, content).expect("write");
}

#[test]
fn failed_chunk_files_keeps_only_in_root_chunk_errors_with_slash_keys() {
    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    let errors = vec![
        chunk_error(root.path(), "src/a.rs", NEW),
        SemanticFileError::Read {
            path: root.path().join("src/b.rs").display().to_string(),
            message: "denied".to_string(),
        },
        SemanticFileError::Chunk {
            file: outside.path().join("c.rs"),
            content_hash: "h".to_string(),
            message: "boom".to_string(),
        },
    ];
    let got = failed_chunk_files(&errors, root.path());
    assert_eq!(
        got,
        vec![ChunkFailure {
            key: "src/a.rs".to_string(),
            content_hash: semantic_file_content_hash(NEW),
        }]
    );
}

#[test]
fn chunk_error_display_is_unchanged() {
    let e = SemanticFileError::Chunk {
        file: PathBuf::from("src/a.rs"),
        content_hash: "h".to_string(),
        message: "boom".to_string(),
    };
    assert_eq!(e.to_string(), "src/a.rs: boom");
}

/// DoD-1: stored hash differs, and the no-stored-hash variant.
#[rstest]
#[case::stale_hash(true)]
#[case::no_stored_hash(false)]
fn changed_chunk_error_clears_hash_and_snippets(#[case] had_hash: bool) {
    let storage = CozoStorage::new_in_memory().expect("cozo");
    let semantic = semantic_for(&storage);
    seed(&semantic, "src/bad.rs", OLD);
    if !had_hash {
        semantic.remove_file_hash("src/bad.rs").expect("rm hash");
    }
    seed(&semantic, "src/ok.rs", OLD);
    let before = semantic.get_vector_count().expect("count");
    assert_eq!(before, 2);

    let failures = vec![ChunkFailure {
        key: "src/bad.rs".to_string(),
        content_hash: semantic_file_content_hash(NEW),
    }];
    let out = drop_failed_chunk_rows(&semantic, &failures);

    assert_eq!(
        out,
        ChunkDropOutcome {
            cleared: 1,
            kept_current: 0,
            failed: 0
        }
    );
    assert!(!semantic.is_file_hash_current("src/bad.rs", &semantic_file_content_hash(OLD)));
    assert!(!semantic.file_has_snippets("src/bad.rs"));
    assert_eq!(semantic.get_vector_count().expect("count"), before - 1);
    assert!(
        !semantic
            .get_tracked_files()
            .expect("tracked")
            .contains(&"src/bad.rs".to_string())
    );
    // DoD-7: the healthy sibling is untouched.
    assert!(semantic.file_has_snippets("src/ok.rs"));
    assert!(semantic.is_file_hash_current("src/ok.rs", &semantic_file_content_hash(OLD)));
}

/// DoD-2: the user-visible drift goes 1 -> 0 with a populated, walked store.
#[test]
fn drop_clears_index_check_drift_for_the_file() {
    let storage = CozoStorage::new_in_memory().expect("cozo");
    let semantic = semantic_for(&storage);
    let root = tempfile::tempdir().expect("root");
    write(root.path(), "src/bad.rs", NEW);
    write(root.path(), "src/ok.rs", OLD);
    seed(&semantic, "src/bad.rs", OLD); // stored hash is for OLD, file now holds NEW
    seed(&semantic, "src/ok.rs", OLD);

    let before = collect_semantic_provenance(
        root.path(),
        Some(&storage),
        ProvenanceWalk::IfStorePopulatedHashed,
    )
    .expect("provenance before");
    assert!(before.walked);
    assert_eq!(before.drifted_hash_count, 1, "drift before the drop");
    assert!(before.is_content_stale());

    let errors = vec![chunk_error(root.path(), "src/bad.rs", NEW)];
    let failures = failed_chunk_files(&errors, root.path());
    drop_failed_chunk_rows(&semantic, &failures);

    let after = collect_semantic_provenance(
        root.path(),
        Some(&storage),
        ProvenanceWalk::IfStorePopulatedHashed,
    )
    .expect("provenance after");
    assert!(after.walked);
    assert_eq!(after.drifted_hash_count, 0, "drift after the drop");
    assert!(!after.is_content_stale());
}

/// DoD-3: unchanged content keeps a valid index.
#[test]
fn chunk_error_on_current_hash_keeps_rows() {
    let storage = CozoStorage::new_in_memory().expect("cozo");
    let semantic = semantic_for(&storage);
    seed(&semantic, "src/bad.rs", OLD);
    let failures = vec![ChunkFailure {
        key: "src/bad.rs".to_string(),
        content_hash: semantic_file_content_hash(OLD),
    }];
    let out = drop_failed_chunk_rows(&semantic, &failures);
    assert_eq!(
        out,
        ChunkDropOutcome {
            cleared: 0,
            kept_current: 1,
            failed: 0
        }
    );
    assert!(semantic.file_has_snippets("src/bad.rs"));
    assert!(semantic.is_file_hash_current("src/bad.rs", &semantic_file_content_hash(OLD)));
}

/// DoD-4: the hash is never removed alone.
#[rstest]
#[case::snippets_fail(true, false, false)]
#[case::both_ok(false, true, true)]
fn clear_rows_removes_hash_only_after_snippets(
    #[case] snippets_fail: bool,
    #[case] expect_hash_call: bool,
    #[case] expect_cleared: bool,
) {
    use std::cell::Cell;
    let hash_called = Cell::new(false);
    let cleared = clear_rows(
        "src/a.rs",
        || {
            if snippets_fail {
                Err("snippets")
            } else {
                Ok(())
            }
        },
        || {
            hash_called.set(true);
            Ok(())
        },
    );
    assert_eq!(hash_called.get(), expect_hash_call);
    assert_eq!(cleared, expect_cleared);
}

/// DoD-5: second drop is a no-op and the file stays in `files_to_process`.
#[test]
fn second_drop_is_idempotent_and_file_is_reprocessed() {
    let storage = CozoStorage::new_in_memory().expect("cozo");
    let semantic = semantic_for(&storage);
    seed(&semantic, "src/bad.rs", OLD);
    seed(&semantic, "src/ok.rs", OLD);
    let failures = vec![ChunkFailure {
        key: "src/bad.rs".to_string(),
        content_hash: semantic_file_content_hash(NEW),
    }];
    let first = drop_failed_chunk_rows(&semantic, &failures);
    assert_eq!(first.cleared, 1);
    let count = semantic.get_vector_count().expect("count");
    let again = drop_failed_chunk_rows(&semantic, &failures);
    assert_eq!(again.kept_current, 0);
    assert_eq!(semantic.get_vector_count().expect("count"), count);
    let hash = semantic_file_content_hash(NEW);
    assert!(
        !skip_warm_incremental_file(
            &semantic,
            std::path::Path::new("src/bad.rs"),
            NEW,
            "src/bad.rs",
            &hash
        ),
        "no hash row => stays in files_to_process"
    );
}

/// DoD-6: progress line.
#[rstest]
#[case::zero(0, None)]
#[case::some(
    2,
    Some(
        "Semantic index: 2 file(s) failed to chunk and will be retried; stored hash and snippets cleared where the content changed."
    )
)]
fn chunk_failure_progress_line_matrix(#[case] n: usize, #[case] want: Option<&str>) {
    assert_eq!(chunk_failure_progress_line(n).as_deref(), want);
}

/// DoD-6: JSON key set.
#[rstest]
#[case::zero(None, false)]
#[case::some(Some(2), true)]
fn json_chunk_failed_files_only_when_nonzero(#[case] n: Option<usize>, #[case] present: bool) {
    let result = SemanticIndexJsonResult {
        schema_version: 1,
        mode: "incremental",
        reason: "auto-incremental",
        files_processed: 1,
        files_candidates: 3,
        chunks_embedded: 4,
        purged_foreign: None,
        up_to_date: false,
        hnsw_rebuilt: None,
        chunk_failed_files: n,
    };
    let v: serde_json::Value = serde_json::to_value(&result).expect("json");
    let mut keys: Vec<&str> = v
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut want = vec![
        "chunksEmbedded",
        "filesCandidates",
        "filesProcessed",
        "mode",
        "reason",
        "schemaVersion",
        "upToDate",
    ];
    if present {
        want.push("chunkFailedFiles");
    }
    want.sort_unstable();
    assert_eq!(keys, want);
    if present {
        assert_eq!(v["chunkFailedFiles"], 2);
    }
}
