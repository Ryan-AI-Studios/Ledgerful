//! Semantic-index provenance: current walk vs stored hashes vs vector paths.
//!
//! Classification is independent of lexical `index_metadata.head_hash`.
//! Callers in dry-run, `index --check`, and change-context share this helper so
//! the Cozo scripts cannot diverge. The search/Ask overlay uses the cheaper
//! [`collect_stored_path_orphans`] (no tree walk).
//! `src/index/surface_freshness.rs` stays a pure classifier over [`EmbeddingsProbe`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cozo::DataValue;
use miette::Result;

use crate::state::storage_cozo::CozoStorage;
use crate::util::path::{path_is_under_work_root, semantic_path_key};

/// When to walk the tree for candidate files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceWalk {
    /// Always walk (dry-run inspect of candidate files).
    Always,
    /// Walk only when stored hashes or vectors exist (check cost bound), and
    /// additionally compare each stored `semantic_file_hash` against the current
    /// file content (0536). Used by `index --check` and change-context, which claim
    /// `contentHash`. The search/Ask overlay does not walk
    /// ([`collect_stored_path_orphans`], 0537).
    IfStorePopulatedHashed,
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
    /// Walk files whose stored hash differs from the current content. Always `0`
    /// unless the walk mode compares hashes ([`ProvenanceWalk::IfStorePopulatedHashed`]).
    pub drifted_hash_count: usize,
    pub stored_dimensions: Option<usize>,
    pub walked: bool,
}

impl SemanticProvenance {
    /// Orphaned stored hash paths or orphaned vector file paths.
    /// Missing hashes and row-vs-file grain do not stale.
    pub fn is_stale(&self) -> bool {
        self.orphaned_hash_count > 0 || self.orphaned_vector_file_count > 0
    }

    /// [`Self::is_stale`] or a stored hash that no longer matches the file content.
    pub fn is_content_stale(&self) -> bool {
        self.is_stale() || self.drifted_hash_count > 0
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
    let mut stored_hashes: BTreeMap<String, String> = BTreeMap::new();
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
            if walk == ProvenanceWalk::IfStorePopulatedHashed {
                stored_hashes = stored_hashes_from_relation(db, work_root)?;
            }
        }
    }

    let store_populated = !stored_hash_keys.is_empty()
        || !stored_vector_keys.is_empty()
        || stored_vector_row_count > 0;
    let should_walk = match walk {
        ProvenanceWalk::Always => true,
        ProvenanceWalk::IfStorePopulatedHashed => store_populated,
    };

    let walk_files: Vec<(String, PathBuf)> = if should_walk {
        walk_repo_for_semantic_files(work_root)
            .into_iter()
            .filter_map(|path| semantic_path_key(work_root, &path).ok().map(|k| (k, path)))
            .collect()
    } else {
        Vec::new()
    };
    let walk_keys = walk_files
        .iter()
        .map(|(k, _)| k.clone())
        .collect::<BTreeSet<_>>();

    let mut drifted_hash_count = 0usize;
    if should_walk && walk == ProvenanceWalk::IfStorePopulatedHashed {
        // Same recipe as the indexer: encoding-aware read, then grain-mixed blake3.
        // A file that cannot be read is skipped (not stale), but logged.
        for (key, path) in &walk_files {
            let Some(stored) = stored_hashes.get(key) else {
                continue;
            };
            match crate::util::fs::read_to_string_with_encoding(path) {
                Ok(content) => {
                    if crate::semantic::chunker::semantic_file_content_hash(&content) != *stored {
                        drifted_hash_count += 1;
                    }
                }
                Err(e) => tracing::warn!(
                    "semantic hash compare skipped unreadable file {}: {e}",
                    path.display()
                ),
            }
        }
    }

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
        drifted_hash_count,
        stored_dimensions,
        walked: should_walk,
    })
}

/// Orphan counts for stored semantic paths whose file is gone from disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoredPathOrphans {
    pub orphaned_hash_count: usize,
    pub orphaned_vector_file_count: usize,
}

impl StoredPathOrphans {
    pub fn is_stale(&self) -> bool {
        self.orphaned_hash_count > 0 || self.orphaned_vector_file_count > 0
    }
}

/// Count stored semantic paths that no longer exist on disk (0537).
///
/// Unlike [`collect_semantic_provenance`] this never walks the repository: it
/// stats each distinct stored path once, so cost is `O(stored files)`, not
/// `O(repo files)`. A stored file that still exists but fell out of the walk set
/// (newly gitignored, under a skip dir) is **not** an orphan here; `index --check`
/// and change-context keep the exact walk-membership answer.
///
/// A key outside the work root (absolute escape, `..`, unparseable) is an orphan
/// without any filesystem call, so a host file cannot make it look present.
/// Missing relations yield zero counts. Metadata errors other than not-found
/// count as present and are logged.
pub fn collect_stored_path_orphans(
    work_root: &Path,
    cozo: Option<&CozoStorage>,
) -> Result<StoredPathOrphans> {
    let Some(db) = cozo else {
        return Ok(StoredPathOrphans::default());
    };
    let relations = db.get_relations()?;
    let hash_keys = if relations.iter().any(|r| r == "semantic_file_hash") {
        path_keys_from_relation(db, RelationKind::SemanticFileHash, work_root)?
    } else {
        BTreeSet::new()
    };
    let vector_keys = if relations.iter().any(|r| r == "snippet_embedding") {
        path_keys_from_relation(db, RelationKind::SnippetEmbedding, work_root)?
    } else {
        BTreeSet::new()
    };

    let missing: BTreeSet<&String> = hash_keys
        .union(&vector_keys)
        .filter(|key| !stored_path_is_present(work_root, key))
        .collect();
    Ok(StoredPathOrphans {
        orphaned_hash_count: hash_keys.iter().filter(|k| missing.contains(k)).count(),
        orphaned_vector_file_count: vector_keys.iter().filter(|k| missing.contains(k)).count(),
    })
}

fn stored_path_is_present(work_root: &Path, key: &str) -> bool {
    if !path_is_under_work_root(work_root, key) {
        return false;
    }
    let path = work_root.join(key);
    match std::fs::metadata(&path) {
        Ok(meta) => meta.is_file(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            tracing::warn!(
                "semantic overlay could not stat {}: {e}; treating as present",
                path.display()
            );
            true
        }
    }
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

/// Stored `(normalized path key => content_hash)` pairs from `semantic_file_hash`.
fn stored_hashes_from_relation(
    db: &CozoStorage,
    work_root: &Path,
) -> Result<BTreeMap<String, String>> {
    let res = db
        .run_script("?[file_path, content_hash] := *semantic_file_hash{file_path, content_hash}")?;
    let mut out = BTreeMap::new();
    for row in res.rows {
        if let (Some(DataValue::Str(path)), Some(DataValue::Str(hash))) = (row.first(), row.get(1))
        {
            out.insert(
                normalize_stored_semantic_key(work_root, path),
                hash.to_string(),
            );
        }
    }
    Ok(out)
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
            ProvenanceWalk::IfStorePopulatedHashed,
        )
        .expect("prov");
        assert!(!prov.walked);
        assert_eq!(prov.candidate_file_count, 0);
        assert!(!prov.is_stale());
    }

    fn seed_hashed(dir: &Path, storage: &CozoStorage, rel: &str, body: &str) {
        write_rs(dir, rel, body);
        let semantic = seed_engine(storage, 3);
        semantic
            .record_file_hash(
                rel,
                &crate::semantic::chunker::semantic_file_content_hash(body),
            )
            .expect("hash");
        put_snippet(&semantic, rel, "f", 0, 3);
    }

    #[test]
    fn hashed_edited_file_without_reindex_drifts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_hashed(dir.path(), &storage, "src/lib.rs", "fn f() {}\n");
        write_rs(dir.path(), "src/lib.rs", "fn f() { 1; }\n");

        let prov = collect_semantic_provenance(
            dir.path(),
            Some(&storage),
            ProvenanceWalk::IfStorePopulatedHashed,
        )
        .expect("prov");
        assert_eq!(prov.drifted_hash_count, 1);
        assert_eq!(prov.orphaned_hash_count, 0);
        assert!(prov.is_content_stale());
        assert!(!prov.is_stale(), "is_stale stays orphan-only");
    }

    #[test]
    fn hashed_unchanged_file_has_no_drift() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_hashed(dir.path(), &storage, "src/lib.rs", "fn f() {}\n");

        let prov = collect_semantic_provenance(
            dir.path(),
            Some(&storage),
            ProvenanceWalk::IfStorePopulatedHashed,
        )
        .expect("prov");
        assert_eq!(prov.drifted_hash_count, 0);
        assert!(!prov.is_content_stale());
    }

    #[test]
    fn hashed_missing_hash_is_not_drift_and_orphan_is_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_hashed(dir.path(), &storage, "src/lib.rs", "fn f() {}\n");
        write_rs(dir.path(), "src/new.rs", "fn n() {}\n");
        let semantic = seed_engine(&storage, 3);
        semantic.record_file_hash("src/gone.rs", "x").expect("hash");

        let prov = collect_semantic_provenance(
            dir.path(),
            Some(&storage),
            ProvenanceWalk::IfStorePopulatedHashed,
        )
        .expect("prov");
        assert_eq!(prov.drifted_hash_count, 0);
        assert_eq!(prov.missing_hash_count, 1);
        assert_eq!(prov.orphaned_hash_count, 1);
        assert!(prov.is_content_stale());
    }

    #[test]
    fn hashed_backslash_stored_key_matches_walk_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let body = "fn f() {}\n";
        write_rs(dir.path(), "src/nested/lib.rs", body);
        let semantic = seed_engine(&storage, 3);
        semantic
            .record_file_hash(
                "src\\nested\\lib.rs",
                &crate::semantic::chunker::semantic_file_content_hash(body),
            )
            .expect("hash");

        let prov = collect_semantic_provenance(
            dir.path(),
            Some(&storage),
            ProvenanceWalk::IfStorePopulatedHashed,
        )
        .expect("prov");
        assert_eq!(prov.drifted_hash_count, 0);
        assert_eq!(prov.orphaned_hash_count, 0);
    }

    #[test]
    fn non_hashed_modes_do_not_compare_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_hashed(dir.path(), &storage, "src/lib.rs", "fn f() {}\n");
        write_rs(dir.path(), "src/lib.rs", "fn f() { 1; }\n");

        let prov = collect_semantic_provenance(dir.path(), Some(&storage), ProvenanceWalk::Always)
            .expect("prov");
        assert_eq!(prov.drifted_hash_count, 0);
        assert!(!prov.is_content_stale());
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

    fn seed_stored_paths(storage: &CozoStorage, hashes: &[&str], vectors: &[&str]) {
        let semantic = seed_engine(storage, 3);
        for (i, p) in hashes.iter().enumerate() {
            semantic
                .record_file_hash(p, &format!("hash{i}"))
                .expect("hash");
        }
        for p in vectors {
            put_snippet(&semantic, p, "f", 0, 3);
        }
    }

    #[test]
    fn stored_orphans_deleted_file_counts_and_present_file_does_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn f() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_stored_paths(
            &storage,
            &["src/lib.rs", "src/gone.rs"],
            &["src/lib.rs", "src/gone.rs"],
        );
        let o = collect_stored_path_orphans(dir.path(), Some(&storage)).expect("orphans");
        assert_eq!(o.orphaned_hash_count, 1);
        assert_eq!(o.orphaned_vector_file_count, 1);
        assert!(o.is_stale());
    }

    #[test]
    fn stored_orphans_all_present_is_not_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn f() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_stored_paths(&storage, &["src/lib.rs"], &["src/lib.rs"]);
        let o = collect_stored_path_orphans(dir.path(), Some(&storage)).expect("orphans");
        assert_eq!(o, StoredPathOrphans::default());
        assert!(!o.is_stale());
    }

    #[test]
    fn stored_orphans_empty_store_and_missing_relations_are_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn f() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let o = collect_stored_path_orphans(dir.path(), Some(&storage)).expect("missing relations");
        assert_eq!(o, StoredPathOrphans::default());
        let none = collect_stored_path_orphans(dir.path(), None).expect("no store");
        assert_eq!(none, StoredPathOrphans::default());
    }

    #[test]
    fn stored_orphans_backslash_key_matches_present_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/nested/lib.rs", "fn f() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_stored_paths(&storage, &[r"src\nested\lib.rs"], &[]);
        let o = collect_stored_path_orphans(dir.path(), Some(&storage)).expect("orphans");
        assert_eq!(o.orphaned_hash_count, 0);
    }

    #[test]
    fn stored_orphans_absolute_outside_root_is_orphan_even_if_host_file_exists() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        write_rs(outside.path(), "foreign.rs", "fn f() {}\n");
        let foreign = outside.path().join("foreign.rs");
        assert!(foreign.is_file());
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_stored_paths(
            &storage,
            &[&foreign.to_string_lossy()],
            &[&foreign.to_string_lossy()],
        );
        let o = collect_stored_path_orphans(root.path(), Some(&storage)).expect("orphans");
        assert_eq!(o.orphaned_hash_count, 1);
        assert_eq!(o.orphaned_vector_file_count, 1);
    }

    #[test]
    fn stored_orphans_dotdot_escape_is_orphan_even_if_host_file_exists() {
        let parent = tempfile::tempdir().expect("parent");
        write_rs(parent.path(), "sibling.rs", "fn f() {}\n");
        let root = parent.path().join("repo");
        std::fs::create_dir_all(&root).expect("mkdir");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_stored_paths(&storage, &["../sibling.rs"], &[]);
        let o = collect_stored_path_orphans(&root, Some(&storage)).expect("orphans");
        assert_eq!(o.orphaned_hash_count, 1);
    }

    #[test]
    fn stored_orphans_directory_at_stored_path_is_orphan() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src/dir.rs")).expect("mkdir");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_stored_paths(&storage, &["src/dir.rs"], &[]);
        let o = collect_stored_path_orphans(dir.path(), Some(&storage)).expect("orphans");
        assert_eq!(o.orphaned_hash_count, 1);
    }

    #[cfg(unix)]
    #[test]
    fn stored_orphans_symlink_to_file_present_dangling_orphan() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/real.rs", "fn f() {}\n");
        std::os::unix::fs::symlink(
            dir.path().join("src/real.rs"),
            dir.path().join("src/link.rs"),
        )
        .expect("symlink");
        std::os::unix::fs::symlink(
            dir.path().join("src/none.rs"),
            dir.path().join("src/dangling.rs"),
        )
        .expect("dangling");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_stored_paths(&storage, &["src/link.rs", "src/dangling.rs"], &[]);
        let o = collect_stored_path_orphans(dir.path(), Some(&storage)).expect("orphans");
        assert_eq!(o.orphaned_hash_count, 1);
    }

    /// 0537 discriminator: a stored file that still exists but is outside the walk
    /// set (skip dir) is an orphan to the walk and present to the stat overlay.
    #[test]
    fn stored_orphans_skip_dir_file_is_present_while_walk_reports_orphan() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rs(dir.path(), "src/lib.rs", "fn f() {}\n");
        write_rs(dir.path(), ".agents/skill.rs", "fn g() {}\n");
        let storage = CozoStorage::new_in_memory().expect("cozo");
        seed_stored_paths(&storage, &["src/lib.rs", ".agents/skill.rs"], &[]);

        let o = collect_stored_path_orphans(dir.path(), Some(&storage)).expect("orphans");
        assert_eq!(o, StoredPathOrphans::default());

        let walked =
            collect_semantic_provenance(dir.path(), Some(&storage), ProvenanceWalk::Always)
                .expect("walk");
        assert_eq!(walked.orphaned_hash_count, 1);
    }

    /// Manual timing harness (0537): `LEDGERFUL_BENCH_ROOT=<repo> cargo test -- --ignored bench_overlay`.
    #[test]
    #[ignore = "manual timing harness for 0537; needs LEDGERFUL_BENCH_ROOT (owner: semantic)"]
    fn bench_overlay_walk_cost() {
        let Ok(root) = std::env::var("LEDGERFUL_BENCH_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let storage =
            CozoStorage::new_read_only(&root.join(".ledgerful/state/ledger.cozo")).expect("open");
        let mut times = Vec::new();
        for _ in 0..7 {
            let mut readiness = crate::semantic::SemanticReadiness {
                backend_status: crate::semantic::BackendStatus::Ready,
                model_name: "bench".into(),
                dimensions: 3,
                vector_count: 1,
                zero_vector_count: 0,
                is_stale: false,
                dimension_mismatch: false,
            };
            let t = std::time::Instant::now();
            crate::semantic::overlay_semantic_walk_stale(&mut readiness, &root, &storage);
            times.push(t.elapsed().as_millis());
        }
        times.sort_unstable();
        let prov =
            collect_semantic_provenance(&root, Some(&storage), ProvenanceWalk::Always).expect("p");
        eprintln!(
            "BENCH overlay ms={times:?} median={} stored_hash_files={} candidates={}",
            times[3], prov.stored_hash_file_count, prov.candidate_file_count
        );
    }
}
