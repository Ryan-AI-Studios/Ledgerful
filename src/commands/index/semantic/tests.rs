use super::progress::*;
use super::stages::*;
use super::*;
use crate::config::model::Config;
use crate::index::symbols::SymbolKind;
use crate::semantic::chunker::AstChunk;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;

fn chunk(name: &str) -> AstChunk {
    AstChunk {
        file_path: "src/lib.rs".to_string(),
        name: name.to_string(),
        kind: SymbolKind::Function,
        content: format!("fn {name}() {{}}"),
        docstring: None,
        range: (0, 0),
        lines: (1, 1),
        offset: 0,
    }
}

#[test]
fn semantic_embedding_batches_preserve_order() {
    let chunks: Vec<AstChunk> = (0..10).map(|i| chunk(&format!("chunk_{i}"))).collect();

    let batches = semantic_embedding_batches(&chunks, 4);
    let flattened_names: Vec<&str> = batches
        .iter()
        .flat_map(|batch| batch.iter().map(|chunk| chunk.name.as_str()))
        .collect();

    assert_eq!(batches.len(), 3);
    assert_eq!(batches[0].len(), 4);
    assert_eq!(batches[1].len(), 4);
    assert_eq!(batches[2].len(), 2);
    assert_eq!(
        flattened_names,
        chunks
            .iter()
            .map(|chunk| chunk.name.as_str())
            .collect::<Vec<_>>()
    );
}

// ── 0167 non-TTY progress step (0161-A) ─────────────────────────────────
// Poller only starts when total > 1 (caller gate in NonTtyPhaseProgress::start).

#[test]
fn non_tty_progress_step_matrix() {
    assert_eq!(non_tty_progress_step(0), 1);
    assert_eq!(non_tty_progress_step(1), 1);
    assert_eq!(non_tty_progress_step(2), 1);
    assert_eq!(non_tty_progress_step(20), 1);
    assert_eq!(non_tty_progress_step(25), 1);
    assert_eq!(non_tty_progress_step(100), 5);
    assert_eq!(non_tty_progress_step(500), 25);
    // Large totals must not clamp to 25 (0161 flood: ~522 mid-lines).
    assert_eq!(non_tty_progress_step(13_065), 653);
    assert_ne!(non_tty_progress_step(13_065), 25);
}

/// Soft E (0167 D8): bars/spinners hidden under `--json` even when TTY is interactive.
#[test]
fn hide_semantic_progress_bars_matrix() {
    // json | interactive | hide
    assert!(hide_semantic_progress_bars(true, true));
    assert!(hide_semantic_progress_bars(true, false));
    assert!(!hide_semantic_progress_bars(false, true));
    assert!(hide_semantic_progress_bars(false, false));
}

/// Soft C (0167 D6): "embedding done" line only when embed collect succeeded.
#[test]
fn embedding_done_progress_line_only_on_success() {
    let ok = embedding_done_progress_line(42, true).expect("Ok path must emit a line");
    assert!(
        ok.contains("embedding done") && ok.contains("42"),
        "success line: {ok}"
    );
    assert!(
        embedding_done_progress_line(42, false).is_none(),
        "failure path must not produce an embedding-done line"
    );
    assert!(embedding_done_progress_line(0, false).is_none());
}

// ── 0161 mode resolution matrix ────────────────────────────────────────

#[test]
fn resolve_mode_cold_neither_is_full_cold_store() {
    let (incr, reason) = resolve_semantic_index_mode(false, false, 0);
    assert!(!incr);
    assert_eq!(reason, "cold-store");
}

#[test]
fn resolve_mode_warm_neither_is_auto_incremental() {
    let (incr, reason) = resolve_semantic_index_mode(false, false, 42);
    assert!(incr);
    assert_eq!(reason, "auto-incremental");
}

/// Pure cold-store alias of `resolve_mode_cold_neither_is_full_cold_store`.
///
/// Documents the wipe-edge product rule: warm SoT is `vector_count > 0` only.
/// Orphan file hashes with zero vectors still resolve to full / `cold-store`
/// (not a separate wipe-edge mode string).
#[test]
fn resolve_mode_wipe_edge_orphan_hashes_is_pure_cold_store_alias() {
    let (incr, reason) = resolve_semantic_index_mode(false, false, 0);
    assert!(!incr);
    assert_eq!(reason, "cold-store");
}

#[test]
fn resolve_mode_full_wins_over_incremental_and_warm() {
    let (incr, reason) = resolve_semantic_index_mode(true, true, 999);
    assert!(!incr);
    assert_eq!(reason, "--full");
}

#[test]
fn resolve_mode_explicit_incremental() {
    let (incr, reason) = resolve_semantic_index_mode(false, true, 0);
    assert!(incr);
    assert_eq!(reason, "explicit-incremental");
    let (incr_warm, reason_warm) = resolve_semantic_index_mode(false, true, 10);
    assert!(incr_warm);
    assert_eq!(reason_warm, "explicit-incremental");
}

#[test]
fn resolve_mode_vector_count_zero_never_panics() {
    // unwrap_or(0) path: callers pass 0 on error; pure fn must not panic.
    let _ = resolve_semantic_index_mode(false, false, 0);
    let _ = resolve_semantic_index_mode(false, false, usize::MAX);
}

/// 0161 Codex R1 / B7: production `get_vector_count().unwrap_or(0)` must resolve
/// cold-store (full) without panic — count Err and missing relation both map to 0.
#[test]
fn count_error_or_zero_treated_as_cold_store_no_panic() {
    // Production call site (execute_semantic_index):
    //   let vector_count = semantic.get_vector_count().unwrap_or(0);
    //   resolve_semantic_index_mode(..., vector_count)
    // Same soft-fail contract as production unwrap_or(0).
    fn soft_vector_count(count: Result<usize, miette::Report>) -> usize {
        count.unwrap_or(0)
    }
    let vector_count = soft_vector_count(Err(miette::miette!("cozo count failed")));
    assert_eq!(vector_count, 0);
    let (incremental, reason) = resolve_semantic_index_mode(false, false, vector_count);
    assert!(
        !incremental,
        "count error must not warm-skip into incremental"
    );
    assert_eq!(reason, "cold-store");
    // Ok(0) / missing-relation path also resolves cold.
    assert_eq!(soft_vector_count(Ok(0)), 0);
    let (incr0, reason0) = resolve_semantic_index_mode(false, false, soft_vector_count(Ok(0)));
    assert!(!incr0);
    assert_eq!(reason0, "cold-store");
    // Bounds: huge counts still pure / non-panicking.
    let _ = resolve_semantic_index_mode(false, false, usize::MAX);
}

/// 0161 Codex R1 / B7: orphan file hashes + zero vectors ⇒ full/`cold-store` on the
/// real post-purge execute path, and C1 filter re-processes (not silent up-to-date).
///
/// Mirrors `execute_semantic_index` order: purge → get_vector_count → resolve → filter.
#[test]
fn wipe_edge_orphan_hashes_execute_path_cold_store_and_reprocesses() {
    use crate::config::model::LocalModelConfig;
    use crate::semantic::SemanticDiscovery;
    use crate::state::storage_cozo::CozoStorage;

    let storage = CozoStorage::new_in_memory().expect("cozo");
    let config = LocalModelConfig {
        dimensions: 3,
        disable_hnsw: true,
        ..Default::default()
    };
    let semantic = SemanticDiscovery::new(config, &storage).expect("semantic");
    semantic.ensure_file_hash_schema().expect("hash schema");

    // Plant orphan hash rows with zero snippet_embedding vectors (dim-wipe edge).
    let path_key = "src/orphan.rs";
    let content_hash = "deadbeef_hash_only_no_vectors";
    semantic
        .record_file_hash(path_key, content_hash)
        .expect("record orphan hash");
    assert!(
        semantic.is_file_hash_current(path_key, content_hash),
        "precondition: hash must be current"
    );
    assert!(
        !semantic.file_has_snippets(path_key),
        "precondition: no snippet rows for orphan hash"
    );

    // Execute-path order: dual foreign purge BEFORE warm detection.
    let work_root = tempfile::tempdir().expect("work root");
    let purged = semantic
        .purge_foreign_semantic_keys(work_root.path())
        .expect("purge");
    assert_eq!(
        purged, 0,
        "relative orphan hash is not foreign; purge must not remove it"
    );

    // Warm SoT = vector_count > 0 after purge (safe 0 on error — never panic).
    let vector_count = semantic.get_vector_count().unwrap_or(0);
    assert_eq!(
        vector_count, 0,
        "hash-only store must report zero vectors (not warm)"
    );

    let (incremental, reason) = resolve_semantic_index_mode(false, false, vector_count);
    assert!(
        !incremental,
        "orphan hashes + empty vectors must full-bootstrap, not auto-incremental"
    );
    assert_eq!(reason, "cold-store");

    // C1 filter (incremental branch): skip only when hash-current AND has snippets.
    // Hash-only must re-process even under forced incremental.
    let would_skip = semantic.is_file_hash_current(path_key, content_hash)
        && semantic.file_has_snippets(path_key);
    assert!(
        !would_skip,
        "hash-current without snippets must re-process (not silent up-to-date)"
    );
}

#[test]
fn semantic_json_result_omits_zero_purge_and_false_hnsw() {
    let result = SemanticIndexJsonResult {
        schema_version: 1,
        mode: "incremental",
        reason: "auto-incremental",
        files_processed: 0,
        files_candidates: 3,
        chunks_embedded: 0,
        purged_foreign: None,
        up_to_date: true,
        hnsw_rebuilt: None,
    };
    let s = serde_json::to_string(&result).expect("serialize");
    assert!(s.contains("\"schemaVersion\":1"));
    assert!(s.contains("\"upToDate\":true"));
    assert!(!s.contains("purgedForeign"));
    assert!(!s.contains("hnswRebuilt"));
    // Pure single object — no human prose.
    assert!(s.starts_with('{'));
    assert!(s.ends_with('}'));
}

#[test]
fn semantic_json_result_includes_purge_and_hnsw_when_set() {
    let result = SemanticIndexJsonResult {
        schema_version: 1,
        mode: "full",
        reason: "--full",
        files_processed: 2,
        files_candidates: 2,
        chunks_embedded: 10,
        purged_foreign: Some(3),
        up_to_date: false,
        hnsw_rebuilt: Some(true),
    };
    let s = serde_json::to_string(&result).expect("serialize");
    assert!(s.contains("\"purgedForeign\":3"));
    assert!(s.contains("\"hnswRebuilt\":true"));
    assert!(s.contains("\"mode\":\"full\""));
}

/// DoD-1: unconfigured backend refuses before any semantic write.
#[test]
fn execute_semantic_index_refuses_when_unconfigured() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 path");
    let layout = Layout::new(root);
    layout.ensure_state_dir().expect("state dir");
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).expect("storage init");

    // Default Config has empty embedding URL — not configured.
    let config = Config::default();
    assert!(
        !crate::embed::client::is_embedding_backend_configured(&config.local_model),
        "precondition: default config must be unconfigured"
    );

    let err = execute_semantic_index(&layout, storage, &config, false, false, None, false)
        .expect_err("must refuse when unconfigured");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("embedding backend") || msg.contains("base_url"),
        "error must name backend/config requirement: {msg}"
    );
    assert!(
        msg.contains("semantic-dry-run"),
        "error must point at dry-run inspect: {msg}"
    );
}
