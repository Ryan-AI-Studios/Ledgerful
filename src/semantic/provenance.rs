//! Semantic-index provenance: current walk vs stored hashes vs vector paths.
//!
//! Classification is independent of lexical `index_metadata.head_hash`.
//! Callers in dry-run, `index --check`, change-context, and search/Ask overlay
//! share this helper so the Cozo scripts cannot diverge.
//! `src/index/surface_freshness.rs` stays a pure classifier over [`EmbeddingsProbe`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cozo::DataValue;
use miette::Result;

use crate::state::storage_cozo::CozoStorage;
use crate::util::path::semantic_path_key;

/// When to walk the tree for candidate files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceWalk {
    /// Always walk (dry-run inspect of candidate files).
    Always,
    /// Walk only when stored hashes or vectors exist (check / overlay cost bound).
    IfStorePopulated,
}

/// Counts comparing the current semantic walk to stored hash and vector keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticProvenance {
    pub candidate_file_count: usize,
    pub stored_hash_file_count: usize,
    pub stored_vector_row_count: usize,
    pub stored_vector_file_count: usize,
    pub orphaned_hash_count: usize,
    pub orphaned_vector_file_count: usize,
    pub missing_hash_count: usize,
    pub stored_dimensions: Option<usize>,
    pub walked: bool,
}

impl SemanticProvenance {
    /// Orphaned stored hash paths or orphaned vector file paths.
    /// Missing hashes and row-vs-file grain do not stale.
    pub fn is_stale(&self) -> bool {
        self.orphaned_hash_count > 0 || self.orphaned_vector_file_count > 0
    }
}

/// Walk the repository for candidate semantic-index files.
///
/// Uses `ignore::WalkBuilder` with `git_ignore(true)` (same class as
/// `RepoWalker`) so gitignored deps/headers do not flood the embed set.
/// Fixed directory name skips remain for non-git noise.
pub fn walk_repo_for_semantic_files(root: &Path) -> Vec<PathBuf> {
    const SKIP_DIRS: &[&str] = &[
        ".git",
        ".ledgerful",
        "target",
        "node_modules",
        ".agents",
        ".claude",
        ".codex",
        ".opencode",
    ];
    const SEMANTIC_EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "py", "go", //
        "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "h++",
    ];

    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                let name = entry.file_name().to_string_lossy();
                return !SKIP_DIRS.iter().any(|s| *s == name);
            }
            true
        })
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Error walking directory for semantic files: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if SEMANTIC_EXTS.contains(&ext) {
            out.push(path.to_path_buf());
        }
    }
    out.sort();
    out
}

/// Compare stored semantic relations against the current walk.
///
/// Missing relations yield empty counts for that side, not a script error.
/// A script error after the relation is known to exist is [`Err`].
pub fn collect_semantic_provenance(
    work_root: &Path,
    cozo: Option<&CozoStorage>,
    walk: ProvenanceWalk,
) -> Result<SemanticProvenance> {
    let mut stored_hash_keys = BTreeSet::new();
    let mut stored_vector_keys = BTreeSet::new();
    let mut stored_vector_row_count = 0usize;
    let mut stored_dimensions = None;

    if let Some(db) = cozo {
        let relations = db.get_relations()?;
        let has_snip = relations.iter().any(|r| r == "snippet_embedding");
        let has_hash = relations.iter().any(|r| r == "semantic_file_hash");
        if has_snip {
            stored_dimensions = db.snippet_embedding_dim()?;
            stored_vector_row_count =
                crate::semantic::vector_store::count_snippet_embedding_rows(db)?;
            stored_vector_keys =
                path_keys_from_relation(db, RelationKind::SnippetEmbedding, work_root)?;
        }
        if has_hash {
            stored_hash_keys =
                path_keys_from_relation(db, RelationKind::SemanticFileHash, work_root)?;
        }
    }

    let store_populated = !stored_hash_keys.is_empty()
        || !stored_vector_keys.is_empty()
        || stored_vector_row_count > 0;
    let should_walk = match walk {
        ProvenanceWalk::Always => true,
        ProvenanceWalk::IfStorePopulated => store_populated,
    };

    let walk_keys = if should_walk {
        walk_repo_for_semantic_files(work_root)
            .into_iter()
            .filter_map(|path| semantic_path_key(work_root, &path).ok())
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };

    let orphaned_hash_count = stored_hash_keys.difference(&walk_keys).count();
    let orphaned_vector_file_count = stored_vector_keys.difference(&walk_keys).count();
    let missing_hash_count = walk_keys.difference(&stored_hash_keys).count();

    Ok(SemanticProvenance {
        candidate_file_count: walk_keys.len(),
        stored_hash_file_count: stored_hash_keys.len(),
        stored_vector_row_count,
        stored_vector_file_count: stored_vector_keys.len(),
        orphaned_hash_count,
        orphaned_vector_file_count,
        missing_hash_count,
        stored_dimensions,
        walked: should_walk,
    })
}

#[derive(Clone, Copy)]
enum RelationKind {
    SemanticFileHash,
    SnippetEmbedding,
}

fn path_keys_from_relation(
    db: &CozoStorage,
    kind: RelationKind,
    work_root: &Path,
) -> Result<BTreeSet<String>> {
    let script = match kind {
        RelationKind::SemanticFileHash => "?[file_path] := *semantic_file_hash{file_path}",
        RelationKind::SnippetEmbedding => "?[file_path] := *snippet_embedding{file_path}",
    };
    let res = db.run_script(script)?;
    let mut keys = BTreeSet::new();
    for row in res.rows {
        let Some(DataValue::Str(raw)) = row.first() else {
            continue;
        };
        keys.insert(normalize_stored_semantic_key(work_root, raw));
    }
    Ok(keys)
}

/// Slash-normalize a stored key. Foreign or unparseable keys stay comparable
/// after `\\` → `/` so they count as orphans against the current walk.
fn normalize_stored_semantic_key(work_root: &Path, raw: &str) -> String {
    match semantic_path_key(work_root, Path::new(raw)) {
        Ok(k) => k,
        Err(_) => raw.replace('\\', "/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::LocalModelConfig;
    use crate::index::symbols::SymbolKind;
    use crate::semantic::SemanticDiscovery;
    use crate::semantic::chunker::AstChunk;
    use crate::state::storage_cozo::CozoStorage;

    fn sample_chunk(path: &str, name: &str, offset: usize) -> AstChunk {
        AstChunk {
            file_path: path.to_string(),
            name: name.to_string(),
            qualified_name: None,
            kind: SymbolKind::Function,
            content: format!("fn {name}() {{}}"),
            docstring: None,
            range: (0, 0),
            lines: (1, 1),
            offset,
        }
    }

    fn seed_engine(storage: &CozoStorage, dim: usize) -> SemanticDiscovery<'_> {
        let config = LocalModelConfig {
            dimensions: dim,
            disable_hnsw: true,
            ..Default::default()
        };
        let semantic = SemanticDiscovery::new(config, storage).expect("semantic");
        semantic.ensure_file_hash_schema().expect("hash schema");
        semantic
    }

    fn put_snippet(
        semantic: &SemanticDiscovery<'_>,
        path: &str,
        name: &str,
        offset: usize,
        dim: usize,
    ) {
        let mut embedding = vec![0.0_f32; dim];
        embedding[0] = 1.0;
        semantic
            .index_chunks_batched(vec![sample_chunk(path, name, offset)], vec![embedding])
            .expect("insert snippet");
    }

    fn write_rs(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, body).expect("write rs");
    }

    #[test]
    fn provenance_deleted_path_is_orphaned_and_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn current() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let semantic = seed_engine(&storage, 3);
        semantic
            .record_file_hash("src/gone.rs", "deadbeef")
            .expect("hash");
        put_snippet(&semantic, "src/gone.rs", "old", 0, 3);

        let prov = collect_semantic_provenance(dir.path(), Some(&storage), ProvenanceWalk::Always)
            .expect("prov");
        assert!(prov.walked);
        assert!(prov.candidate_file_count >= 1);
        assert!(prov.orphaned_hash_count > 0);
        assert!(prov.orphaned_vector_file_count > 0);
        assert!(prov.is_stale());
        assert_eq!(prov.stored_vector_row_count, 1);
        assert_eq!(prov.stored_hash_file_count, 1);
    }

    #[test]
    fn provenance_two_rows_one_current_file_is_not_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn a() {}\nfn b() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let semantic = seed_engine(&storage, 3);
        semantic
            .record_file_hash("src/lib.rs", "hash-lib")
            .expect("hash");
        put_snippet(&semantic, "src/lib.rs", "a", 0, 3);
        put_snippet(&semantic, "src/lib.rs", "b", 1, 3);

        let prov = collect_semantic_provenance(dir.path(), Some(&storage), ProvenanceWalk::Always)
            .expect("prov");
        assert!(!prov.is_stale());
        assert_eq!(prov.orphaned_hash_count, 0);
        assert_eq!(prov.orphaned_vector_file_count, 0);
        assert_eq!(prov.stored_vector_row_count, 2);
        assert_eq!(prov.stored_vector_file_count, 1);
        assert_eq!(prov.stored_hash_file_count, 1);
    }

    #[test]
    fn provenance_backslash_pathbuf_under_work_root_is_not_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn f() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let semantic = seed_engine(&storage, 3);
        semantic
            .record_file_hash("src/lib.rs", "hash-lib")
            .expect("hash");
        put_snippet(&semantic, "src/lib.rs", "f", 0, 3);

        let mut backslash = dir.path().to_path_buf();
        backslash.push("src");
        backslash.push("lib.rs");
        let key = semantic_path_key(dir.path(), &backslash).expect("key");
        assert_eq!(key, "src/lib.rs");

        let mixed = PathBuf::from(format!("{}\\src\\lib.rs", dir.path().display()));
        let mixed_key = semantic_path_key(dir.path(), &mixed);
        if let Ok(k) = mixed_key {
            assert_eq!(k, "src/lib.rs");
        }

        let prov = collect_semantic_provenance(dir.path(), Some(&storage), ProvenanceWalk::Always)
            .expect("prov");
        assert!(
            !prov.is_stale(),
            "backslash walk must match stored slash key"
        );
        assert_eq!(prov.orphaned_hash_count, 0);
        assert_eq!(prov.orphaned_vector_file_count, 0);
    }

    #[test]
    fn provenance_missing_hash_does_not_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn f() {}\n");
        write_rs(dir.path(), "src/new.rs", "fn n() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let semantic = seed_engine(&storage, 3);
        semantic
            .record_file_hash("src/lib.rs", "hash-lib")
            .expect("hash");
        put_snippet(&semantic, "src/lib.rs", "f", 0, 3);

        let prov = collect_semantic_provenance(dir.path(), Some(&storage), ProvenanceWalk::Always)
            .expect("prov");
        assert!(prov.missing_hash_count >= 1);
        assert!(!prov.is_stale());
    }

    #[test]
    fn provenance_foreign_absolute_key_is_orphaned() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn f() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let semantic = seed_engine(&storage, 3);
        semantic
            .record_file_hash("/tmp/other-repo/src/lib.rs", "foreign")
            .expect("hash");
        put_snippet(&semantic, "/tmp/other-repo/src/lib.rs", "f", 0, 3);
        semantic
            .record_file_hash("src/lib.rs", "local")
            .expect("local hash");
        put_snippet(&semantic, "src/lib.rs", "local_fn", 0, 3);

        let prov = collect_semantic_provenance(dir.path(), Some(&storage), ProvenanceWalk::Always)
            .expect("prov");
        assert!(prov.orphaned_hash_count >= 1);
        assert!(prov.orphaned_vector_file_count >= 1);
        assert!(prov.is_stale());
    }

    #[test]
    fn provenance_skips_walk_when_store_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn f() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let prov = collect_semantic_provenance(
            dir.path(),
            Some(&storage),
            ProvenanceWalk::IfStorePopulated,
        )
        .expect("prov");
        assert!(!prov.walked);
        assert_eq!(prov.candidate_file_count, 0);
        assert!(!prov.is_stale());
    }

    #[test]
    fn provenance_missing_relations_are_empty_not_query_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let prov = collect_semantic_provenance(dir.path(), Some(&storage), ProvenanceWalk::Always)
            .expect("missing relations must not fail");
        assert_eq!(prov.stored_vector_row_count, 0);
        assert_eq!(prov.stored_hash_file_count, 0);
        assert!(prov.stored_dimensions.is_none());
    }
}
