#[cfg(test)]
use crate::config::model::Config;
use crate::index::call_graph::CallEdge;
use crate::index::orchestrator::{BINARY_EXTENSIONS, ProjectIndexer, SUPPORTED_EXTENSIONS};
use crate::index::rows::{
    delete_file_index_dependents, get_file_id_by_path, insert_symbol_row, upsert_file_row,
};
use crate::index::types::{ProjectFile, ProjectSymbol};
use crate::state::graph_kinds::{EdgeKind, NodeKind};
use crate::state::storage_cozo::{GraphEdge, GraphNode};
use crate::watch::batch::{WatchBatch, WatchEvent, WatchEventKind};
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use serde_json::json;
use std::collections::HashMap;
use tracing::{info, warn};

type SymbolMaps = (
    HashMap<String, Vec<i64>>,
    HashMap<i64, i64>,
    HashMap<i64, String>,
);

pub struct IncrementalSyncEngine {
    pub indexer: ProjectIndexer,
    repo_path: Utf8PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct SyncDelta {
    pub files_processed: usize,
    pub nodes_added: usize,
    pub nodes_removed: usize,
    pub edges_added: usize,
    pub edges_removed: usize,
}

#[derive(Debug, Clone)]
struct AffectedFileRecord {
    file_path: String,
    old_qualified_names: Vec<String>,
    new_nodes: Vec<GraphNode>,
    new_edges: Vec<GraphEdge>,
}

impl IncrementalSyncEngine {
    pub fn new(indexer: ProjectIndexer, repo_path: Utf8PathBuf) -> Self {
        Self { indexer, repo_path }
    }

    pub fn process_batch(&mut self, batch: &WatchBatch) -> Result<SyncDelta> {
        let events = self.filter_and_dedup_events(batch);
        if events.is_empty() {
            return Ok(SyncDelta::default());
        }

        let affected = self.apply_sqlite_delta(&events)?;
        let delta = self.apply_cozo_delta(&affected)?;

        info!(
            "Incremental sync: {} files, +{} nodes, -{} nodes, +{} edges, -{} edges",
            delta.files_processed,
            delta.nodes_added,
            delta.nodes_removed,
            delta.edges_added,
            delta.edges_removed,
        );

        Ok(delta)
    }

    fn filter_and_dedup_events(&self, batch: &WatchBatch) -> Vec<WatchEvent> {
        let mut seen: HashMap<Utf8PathBuf, WatchEvent> = HashMap::new();
        for event in &batch.events {
            let ext = event.path.extension().unwrap_or("");
            if !SUPPORTED_EXTENSIONS.contains(&ext) || BINARY_EXTENSIONS.contains(&ext) {
                continue;
            }
            match seen.get(&event.path) {
                Some(existing)
                    if existing.kind != WatchEventKind::Unknown
                        && event.kind == WatchEventKind::Unknown => {}
                _ => {
                    // Last meaningful event wins for the same path. Some platforms emit
                    // Unknown events after Create/Modify; those must not erase actionable work.
                    seen.insert(event.path.clone(), event.clone());
                }
            }
        }
        seen.into_values().collect()
    }

    fn normalize_relative(raw: &str) -> String {
        raw.replace('\\', "/")
    }

    fn apply_sqlite_delta(&mut self, events: &[WatchEvent]) -> Result<Vec<AffectedFileRecord>> {
        // Collect old qualified names before any mutations (immutable borrow)
        let mut old_names_by_path: HashMap<String, Vec<String>> = HashMap::new();
        {
            let conn = self.indexer.storage().get_connection();
            for event in events {
                let relative = Self::normalize_relative(
                    event
                        .path
                        .strip_prefix(&self.repo_path)
                        .unwrap_or(&event.path)
                        .as_str(),
                );
                let names = Self::get_qualified_names_for_file(conn, &relative)?;
                old_names_by_path.insert(relative.clone(), names);
            }
        }

        // Phase 1: parse all files outside the transaction (file IO, immutable borrow)
        #[derive(Debug, Clone)]
        struct ParsedFile {
            event: WatchEvent,
            project_file: Option<ProjectFile>,
            project_symbols: Vec<ProjectSymbol>,
            calls: Vec<CallEdge>,
        }

        let mut parsed: Vec<ParsedFile> = Vec::new();
        for event in events {
            let relative = Self::normalize_relative(
                event
                    .path
                    .strip_prefix(&self.repo_path)
                    .unwrap_or(&event.path)
                    .as_str(),
            );

            match event.kind {
                WatchEventKind::Create | WatchEventKind::Modify => {
                    let full_path = if event.path.is_absolute() {
                        event.path.clone()
                    } else {
                        self.repo_path.join(&relative)
                    };

                    if !full_path.exists() {
                        warn!("File no longer exists, skipping: {}", full_path);
                        continue;
                    }
                    match self.indexer.index_file_with_edges(&full_path) {
                        Ok((pf, ps, calls)) => {
                            if pf.parse_status == "PARSE_FAILED" {
                                warn!("Skipping parse-failed file: {}", event.path);
                                // Even if parse fails, we process it by leaving it in the affected list
                                // as a deletion of its previous symbols to avoid leaving stale knowledge graph nodes.
                                parsed.push(ParsedFile {
                                    event: WatchEvent {
                                        path: event.path.clone(),
                                        kind: WatchEventKind::Delete,
                                    },
                                    project_file: None,
                                    project_symbols: Vec::new(),
                                    calls: Vec::new(),
                                });
                                continue;
                            }
                            parsed.push(ParsedFile {
                                event: event.clone(),
                                project_file: Some(pf),
                                project_symbols: ps,
                                calls,
                            });
                        }
                        Err(e) => {
                            warn!("Parse failure for {}: {}", event.path, e);
                            // Treat as deletion if we can't parse it at all
                            parsed.push(ParsedFile {
                                event: WatchEvent {
                                    path: event.path.clone(),
                                    kind: WatchEventKind::Delete,
                                },
                                project_file: None,
                                project_symbols: Vec::new(),
                                calls: Vec::new(),
                            });
                            continue;
                        }
                    }
                }
                WatchEventKind::Delete => {
                    parsed.push(ParsedFile {
                        event: event.clone(),
                        project_file: None,
                        project_symbols: Vec::new(),
                        calls: Vec::new(),
                    });
                }
                _ => {}
            }
        }

        // Phase 2: SQLite mutations within the transaction (mutable borrow)
        let conn = self.indexer.storage_mut().get_connection_mut();
        let tx = conn.unchecked_transaction().into_diagnostic()?;

        let mut affected: Vec<AffectedFileRecord> = Vec::new();
        for item in &parsed {
            let relative = Self::normalize_relative(
                item.event
                    .path
                    .strip_prefix(&self.repo_path)
                    .unwrap_or(&item.event.path)
                    .as_str(),
            );
            let old_qualified_names = old_names_by_path
                .get(&relative)
                .cloned()
                .unwrap_or_default();

            match item.event.kind {
                WatchEventKind::Create | WatchEventKind::Modify => {
                    let project_file = item.project_file.as_ref().unwrap();
                    // Delete old dependents and symbols
                    delete_file_index_dependents(&tx, &relative)?;
                    tx.execute(
                        "DELETE FROM project_symbols WHERE file_id IN \
                         (SELECT id FROM project_files WHERE file_path = ?1)",
                        [&relative],
                    )
                    .into_diagnostic()?;

                    // Upsert file row
                    upsert_file_row(&tx, project_file)?;
                    let file_id = get_file_id_by_path(&tx, &relative)?;

                    // Insert new symbols
                    for ps in &item.project_symbols {
                        insert_symbol_row(&tx, ps, file_id)?;
                    }

                    affected.push(AffectedFileRecord {
                        file_path: relative.clone(),
                        old_qualified_names,
                        new_nodes: Self::build_nodes(
                            &relative,
                            project_file,
                            &item.project_symbols,
                        ),
                        new_edges: Vec::new(), // populated later after all symbols are in
                    });
                }
                WatchEventKind::Delete => {
                    delete_file_index_dependents(&tx, &relative)?;
                    tx.execute(
                        "UPDATE project_files SET parse_status = 'DELETED' WHERE file_path = ?1",
                        [&relative],
                    )
                    .into_diagnostic()?;
                    tx.execute(
                        "DELETE FROM project_symbols WHERE file_id IN \
                         (SELECT id FROM project_files WHERE file_path = ?1)",
                        [&relative],
                    )
                    .into_diagnostic()?;

                    affected.push(AffectedFileRecord {
                        file_path: relative.clone(),
                        old_qualified_names,
                        new_nodes: Vec::new(),
                        new_edges: Vec::new(),
                    });
                }
                _ => {}
            }
        }

        // Phase 3: Build symbol resolution maps from the current DB state
        let (symbol_name_to_ids, symbol_id_to_file, name_to_qualified) =
            Self::build_symbol_maps(&tx)?;

        // Phase 4: Resolve calls and insert structural edges, then build GraphEdges
        for item in &parsed {
            let relative = Self::normalize_relative(
                item.event
                    .path
                    .strip_prefix(&self.repo_path)
                    .unwrap_or(&item.event.path)
                    .as_str(),
            );

            if item.event.kind == WatchEventKind::Delete {
                continue;
            }

            let file_id = match get_file_id_by_path(&tx, &relative) {
                Ok(id) => id,
                Err(_) => continue,
            };

            // Get caller symbols in this file
            let mut caller_stmt = tx
                .prepare("SELECT id, symbol_name FROM project_symbols WHERE file_id = ?1")
                .into_diagnostic()?;
            let caller_rows = caller_stmt
                .query_map([file_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .into_diagnostic()?;
            let mut caller_by_name: HashMap<String, i64> = HashMap::new();
            for row in caller_rows {
                let (sym_id, name) = row.into_diagnostic()?;
                caller_by_name.insert(name, sym_id);
            }
            drop(caller_stmt);

            let record = affected
                .iter_mut()
                .find(|r| r.file_path == relative)
                .expect("affected record should exist");

            for call in &item.calls {
                let caller_symbol_id = match caller_by_name.get(&call.caller_name) {
                    Some(&id) => id,
                    None => continue,
                };

                let caller_qualified = name_to_qualified
                    .get(&caller_symbol_id)
                    .cloned()
                    .unwrap_or_else(|| call.caller_name.clone());

                let (callee_symbol_id, callee_file_id, resolution_status, unresolved_callee) =
                    if let Some(callee_ids) = symbol_name_to_ids.get(&call.callee_name) {
                        if callee_ids.len() == 1 {
                            let cid = callee_ids[0];
                            let fid = symbol_id_to_file.get(&cid).copied();
                            (Some(cid), fid, "RESOLVED", None)
                        } else {
                            (None, None, "AMBIGUOUS", Some(call.callee_name.clone()))
                        }
                    } else {
                        (None, None, "UNRESOLVED", Some(call.callee_name.clone()))
                    };

                tx.execute(
                    "INSERT INTO structural_edges \
                     (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, \
                      unresolved_callee, call_kind, resolution_status, confidence, evidence) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        caller_symbol_id,
                        file_id,
                        callee_symbol_id,
                        callee_file_id,
                        unresolved_callee,
                        call.call_kind.as_str(),
                        resolution_status,
                        call.confidence,
                        call.evidence.clone(),
                    ],
                )
                .into_diagnostic()?;

                if resolution_status == "RESOLVED"
                    && let Some(&callee_id) = callee_symbol_id.as_ref()
                    && let Some(target) = name_to_qualified.get(&callee_id)
                {
                    record.new_edges.push(GraphEdge {
                        source: crate::platform::urn::build_urn(
                            NodeKind::Symbol,
                            &caller_qualified,
                        ),
                        target: crate::platform::urn::build_urn(NodeKind::Symbol, target),
                        relation: EdgeKind::Calls,
                        confidence: call.confidence,
                        provenance_id: "incremental_sync".to_string(),
                    });
                }
            }
        }

        tx.commit().into_diagnostic()?;
        Ok(affected)
    }

    fn apply_cozo_delta(&self, affected: &[AffectedFileRecord]) -> Result<SyncDelta> {
        let Some(cozo) = self.indexer.cozo() else {
            let files_processed = affected.len();
            return Ok(SyncDelta {
                files_processed,
                ..SyncDelta::default()
            });
        };

        let mut nodes_to_remove: Vec<String> = Vec::new();
        let mut edges_to_remove_sources: Vec<String> = Vec::new();
        let mut nodes_to_add: Vec<GraphNode> = Vec::new();
        let mut edges_to_add: Vec<GraphEdge> = Vec::new();

        for record in affected {
            // Old nodes: file path + old qualified names (MUST use URNs for deletion)
            nodes_to_remove.push(crate::platform::urn::build_urn(
                NodeKind::File,
                &record.file_path,
            ));
            for qn in &record.old_qualified_names {
                nodes_to_remove.push(crate::platform::urn::build_urn(NodeKind::Symbol, qn));
                edges_to_remove_sources.push(crate::platform::urn::build_urn(NodeKind::Symbol, qn));
            }

            // New nodes and edges
            nodes_to_add.extend(record.new_nodes.clone());
            edges_to_add.extend(record.new_edges.clone());
        }

        let nodes_removed = nodes_to_remove.len();
        let edges_removed = edges_to_remove_sources.len();
        let nodes_added = nodes_to_add.len();
        let edges_added = edges_to_add.len();

        cozo.remove_nodes_by_id(&nodes_to_remove)?;
        cozo.remove_edges_for_source(&edges_to_remove_sources)?;

        // H2: Prune stale snippet embeddings for all affected file paths so the
        // vector store stays consistent after re-indexing.
        let affected_paths: Vec<String> = affected.iter().map(|r| r.file_path.clone()).collect();
        if let Err(e) = cozo.remove_snippets_for_files(&affected_paths) {
            warn!("Failed to prune stale snippet embeddings: {e}");
        }

        cozo.put_node_batch(&nodes_to_add)?;
        cozo.put_edge_batch(&edges_to_add)?;

        Ok(SyncDelta {
            files_processed: affected.len(),
            nodes_added,
            nodes_removed,
            edges_added,
            edges_removed,
        })
    }

    fn get_qualified_names_for_file(conn: &Connection, file_path: &str) -> Result<Vec<String>> {
        let mut stmt = conn
            .prepare(
                "SELECT ps.qualified_name FROM project_symbols ps \
                 JOIN project_files pf ON ps.file_id = pf.id \
                 WHERE pf.file_path = ?1",
            )
            .into_diagnostic()?;
        let rows = stmt
            .query_map([file_path], |row| row.get::<_, String>(0))
            .into_diagnostic()?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row.into_diagnostic()?);
        }
        Ok(names)
    }

    fn build_symbol_maps(conn: &Connection) -> Result<SymbolMaps> {
        let mut stmt = conn
            .prepare("SELECT id, file_id, symbol_name, qualified_name FROM project_symbols")
            .into_diagnostic()?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .into_diagnostic()?;

        let mut symbol_name_to_ids: HashMap<String, Vec<i64>> = HashMap::new();
        let mut symbol_id_to_file: HashMap<i64, i64> = HashMap::new();
        let mut name_to_qualified: HashMap<i64, String> = HashMap::new();

        for row in rows {
            let (sym_id, fid, name, qualified) = row.into_diagnostic()?;
            symbol_name_to_ids.entry(name).or_default().push(sym_id);
            symbol_id_to_file.insert(sym_id, fid);
            name_to_qualified.insert(sym_id, qualified);
        }
        Ok((symbol_name_to_ids, symbol_id_to_file, name_to_qualified))
    }

    fn build_nodes(
        file_path: &str,
        project_file: &ProjectFile,
        project_symbols: &[ProjectSymbol],
    ) -> Vec<GraphNode> {
        let mut nodes = Vec::new();
        nodes.push(GraphNode {
            id: crate::platform::urn::build_urn(NodeKind::File, file_path),
            label: file_path.to_string(),
            category: NodeKind::File,
            risk_score: 0.0,
            metadata: Some(json!({ "language": project_file.language, "schema_version": "v1" })),
        });
        for ps in project_symbols {
            nodes.push(GraphNode {
                id: crate::platform::urn::build_urn(NodeKind::Symbol, &ps.qualified_name),
                label: ps.symbol_name.clone(),
                category: NodeKind::Symbol,
                risk_score: 0.0,
                metadata: Some(json!({ "kind": ps.symbol_kind, "schema_version": "v1" })),
            });
        }
        nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use crate::state::storage_cozo::CozoStorage;
    use camino::Utf8PathBuf;
    use rusqlite::Connection;
    use std::io::Write;

    fn in_memory_storage_with_cozo() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        let mut storage = StorageManager::init_from_conn(conn);
        let cozo = CozoStorage::new(&std::path::PathBuf::from("")).unwrap();
        storage.cozo = Some(cozo);
        storage
    }

    fn in_memory_storage_without_cozo() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn temp_repo_with_file(file_name: &str, content: &str) -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let src_dir = repo_path.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file_path = src_dir.join(file_name);
        let mut file = std::fs::File::create(&file_path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        (dir, repo_path)
    }

    #[test]
    fn test_process_batch_modify_one_file() {
        let storage = in_memory_storage_with_cozo();
        let (_dir, repo_path) = temp_repo_with_file(
            "lib.rs",
            r#"
pub fn helper() {}
pub fn main() { helper(); }
"#,
        );

        let indexer = ProjectIndexer::new(storage, repo_path.clone(), Config::default());
        let mut engine = IncrementalSyncEngine::new(indexer, repo_path.clone());

        // Pre-seed with an old version of the file
        let conn = engine.indexer.storage().get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, parse_status, last_indexed_at) \
             VALUES ('src/lib.rs', 'Rust', 'OK', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
             VALUES (?1, 'old::helper', 'helper', 'Function', '2026-01-01T00:00:00Z')",
            [file_id],
        )
        .unwrap();
        let _ = conn;

        let batch = WatchBatch::new(vec![WatchEvent {
            path: repo_path.join("src/lib.rs"),
            kind: WatchEventKind::Modify,
        }]);

        let delta = engine.process_batch(&batch).unwrap();
        assert_eq!(delta.files_processed, 1);
        assert!(delta.nodes_added >= 2); // file + at least one symbol
        assert_eq!(delta.nodes_removed, 2); // old file node + old helper node
        assert_eq!(delta.edges_added, 1); // main -> helper

        let cozo = engine.indexer.cozo().unwrap();
        let res = cozo
            .run_script("?[id] := *node{id: id}, id = 'urn:ledgerful:file:src/lib.rs'")
            .unwrap();
        assert_eq!(res.rows.len(), 1);
    }

    #[test]
    fn test_process_batch_keeps_create_when_followed_by_unknown() {
        let storage = in_memory_storage_with_cozo();
        let (_dir, repo_path) = temp_repo_with_file("lib.rs", "pub fn foo() {}");

        let indexer = ProjectIndexer::new(storage, repo_path.clone(), Config::default());
        let mut engine = IncrementalSyncEngine::new(indexer, repo_path.clone());

        let batch = WatchBatch::new(vec![
            WatchEvent {
                path: repo_path.join("src/lib.rs"),
                kind: WatchEventKind::Create,
            },
            WatchEvent {
                path: repo_path.join("src/lib.rs"),
                kind: WatchEventKind::Unknown,
            },
        ]);

        let delta = engine.process_batch(&batch).unwrap();
        assert_eq!(delta.files_processed, 1);
        assert!(delta.nodes_added >= 1);
    }
    #[test]
    fn test_process_batch_delete_one_file() {
        let storage = in_memory_storage_with_cozo();
        let (_dir, repo_path) = temp_repo_with_file("lib.rs", "pub fn foo() {}");

        let indexer = ProjectIndexer::new(storage, repo_path.clone(), Config::default());
        let mut engine = IncrementalSyncEngine::new(indexer, repo_path.clone());

        // Pre-seed
        let conn = engine.indexer.storage().get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, parse_status, last_indexed_at) \
             VALUES ('src/lib.rs', 'Rust', 'OK', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
             VALUES (?1, 'crate::foo', 'foo', 'Function', '2026-01-01T00:00:00Z')",
            [file_id],
        )
        .unwrap();
        let _ = conn;

        let batch = WatchBatch::new(vec![WatchEvent {
            path: repo_path.join("src/lib.rs"),
            kind: WatchEventKind::Delete,
        }]);

        let delta = engine.process_batch(&batch).unwrap();
        assert_eq!(delta.files_processed, 1);
        assert_eq!(delta.nodes_added, 0);
        assert_eq!(delta.nodes_removed, 2); // file + foo
        assert_eq!(delta.edges_added, 0);
        assert_eq!(delta.edges_removed, 1); // old qualified name as source

        let cozo = engine.indexer.cozo().unwrap();
        let res = cozo
            .run_script("?[id] := *node{id: id}, id = 'urn:ledgerful:file:src/lib.rs'")
            .unwrap();
        assert_eq!(res.rows.len(), 0);
    }

    #[test]
    fn test_process_batch_parse_failure_skips_file() {
        let storage = in_memory_storage_with_cozo();
        let (_dir, repo_path) = temp_repo_with_file("good.rs", "pub fn good() {}");
        let bad_path = repo_path.join("src/bad.rs");
        // Write invalid UTF-8 to trigger a read failure / PARSE_FAILED
        std::fs::write(&bad_path, vec![0x80, 0x81, 0x82]).unwrap();

        let indexer = ProjectIndexer::new(storage, repo_path.clone(), Config::default());
        let mut engine = IncrementalSyncEngine::new(indexer, repo_path.clone());

        let batch = WatchBatch::new(vec![
            WatchEvent {
                path: repo_path.join("src/good.rs"),
                kind: WatchEventKind::Modify,
            },
            WatchEvent {
                path: bad_path,
                kind: WatchEventKind::Modify,
            },
        ]);

        let delta = engine.process_batch(&batch).unwrap();
        assert_eq!(delta.files_processed, 2);
        assert!(delta.nodes_added >= 1);
    }
    #[test]
    fn test_process_batch_no_cozo_graceful() {
        let storage = in_memory_storage_without_cozo();
        let (_dir, repo_path) = temp_repo_with_file("lib.rs", "pub fn foo() {}");

        let indexer = ProjectIndexer::new(storage, repo_path.clone(), Config::default());
        let mut engine = IncrementalSyncEngine::new(indexer, repo_path.clone());

        let batch = WatchBatch::new(vec![WatchEvent {
            path: repo_path.join("src/lib.rs"),
            kind: WatchEventKind::Modify,
        }]);

        let delta = engine.process_batch(&batch).unwrap();
        assert_eq!(delta.files_processed, 1);
        assert_eq!(delta.nodes_added, 0);
        assert_eq!(delta.nodes_removed, 0);
        assert_eq!(delta.edges_added, 0);
        assert_eq!(delta.edges_removed, 0);

        // SQLite should still have the file
        let conn = engine.indexer.storage().get_connection();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_files WHERE file_path = 'src/lib.rs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
