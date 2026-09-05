use crate::index::content_cache::load_source_content;
use crate::index::languages;
use crate::index::resolve::{
    ResolveCandidate, ResolveInput, build_resolve_maps, resolve_callee, resolve_candidate_from_row,
};
use crate::index::symbols::{Symbol, SymbolKind};
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use super::persist::{EdgeRow, clear_native_structural_edges, insert_edge_batch};
use super::types::{CallGraphStats, ResolutionStatus};

pub struct CallGraphBuilder<'a> {
    storage: &'a StorageManager,
    repo_path: PathBuf,
    content_cache: Option<&'a HashMap<String, Arc<str>>>,
}

const EDGE_CAP_PER_FILE: usize = 50_000;
const EDGE_BATCH_SIZE: usize = 500;

impl<'a> CallGraphBuilder<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: PathBuf) -> Self {
        Self {
            storage,
            repo_path,
            content_cache: None,
        }
    }

    pub fn with_content_cache(mut self, cache: &'a HashMap<String, Arc<str>>) -> Self {
        self.content_cache = Some(cache);
        self
    }

    pub fn build(&self) -> Result<CallGraphStats> {
        let conn = self.storage.get_connection();

        // 1. Query all project_symbols first. Empty skip must not DELETE leftover
        // natives — there is nothing to rebuild from.
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, symbol_name, symbol_kind, is_public, qualified_name \
                 FROM project_symbols",
            )
            .into_diagnostic()?;

        let symbol_rows: Vec<(i64, i64, String, String, bool, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i32>(4)? != 0,
                    row.get::<_, String>(5)?,
                ))
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(stmt);

        if symbol_rows.is_empty() {
            info!("No project symbols indexed; skipping call graph.");
            return Ok(CallGraphStats {
                total_edges: 0,
                resolved_edges: 0,
                unresolved_edges: 0,
                ambiguous_edges: 0,
                files_processed: 0,
                files_skipped: 0,
            });
        }

        // 2. Query all project_files
        let mut file_stmt = conn
            .prepare("SELECT id, file_path, language FROM project_files")
            .into_diagnostic()?;

        let file_rows: Vec<(i64, String, Option<String>)> = file_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(file_stmt);

        // 3. Build lookup maps
        // symbols_by_file: file_id -> per-file symbol rows for extractors / callers
        struct FileSymbolRow {
            id: i64,
            name: String,
            kind: String,
            is_public: bool,
            qualified_name: String,
        }
        let mut symbols_by_file: HashMap<i64, Vec<FileSymbolRow>> = HashMap::new();
        let mut resolve_candidates: Vec<ResolveCandidate> = Vec::new();
        // symbol_id -> is_public (edge-cap priority)
        let mut symbol_id_to_public: HashMap<i64, bool> = HashMap::new();

        for (sym_id, file_id, name, kind, is_public, qn) in &symbol_rows {
            symbols_by_file
                .entry(*file_id)
                .or_default()
                .push(FileSymbolRow {
                    id: *sym_id,
                    name: name.clone(),
                    kind: kind.clone(),
                    is_public: *is_public,
                    qualified_name: qn.clone(),
                });
            symbol_id_to_public.insert(*sym_id, *is_public);
            resolve_candidates.push(resolve_candidate_from_row(
                *sym_id,
                *file_id,
                name.clone(),
                qn.clone(),
                kind.clone(),
            ));
        }

        let (candidates_by_bare, candidates_by_qn) = build_resolve_maps(resolve_candidates);

        // file_id -> path and module path (0092)
        let file_paths: HashMap<i64, String> = file_rows
            .iter()
            .map(|(id, path, _)| (*id, path.clone()))
            .collect();
        let mut module_path_by_file: HashMap<i64, String> = HashMap::new();
        for (id, path) in &file_paths {
            if let Some(mp) = crate::index::module_path::derive_module_path(path, None) {
                module_path_by_file.insert(*id, mp);
            }
        }
        let bindings_by_file = crate::index::rows::load_all_file_bindings(conn)?;

        // Native DELETE + all INSERTs share this transaction. Failure before
        // commit rolls back the wipe so leftover natives remain.
        let tx = conn.unchecked_transaction().into_diagnostic()?;
        clear_native_structural_edges(&tx)?;

        // 4. Iterate over source files
        let mut total_edges = 0usize;
        let mut resolved_edges = 0usize;
        let mut unresolved_edges = 0usize;
        let mut ambiguous_edges = 0usize;
        let mut files_processed = 0usize;
        let mut files_skipped = 0usize;
        let mut edge_batch: Vec<EdgeRow> = Vec::new();

        for (file_id, file_path, _language) in &file_rows {
            let content = match load_source_content(self.content_cache, file_path, &self.repo_path)
            {
                Ok(c) => c,
                Err(_) => {
                    files_skipped += 1;
                    continue;
                }
            };

            let path = PathBuf::from(file_path);
            let file_symbols = symbols_by_file
                .get(file_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);

            // Build Symbol structs for the language extractors (pass through real QN)
            let sym_vec: Vec<Symbol> = file_symbols
                .iter()
                .map(|row| {
                    let qualified_name =
                        if row.qualified_name.is_empty() || row.qualified_name == row.name {
                            None
                        } else {
                            Some(row.qualified_name.clone())
                        };
                    Symbol {
                        name: row.name.clone(),
                        kind: parse_symbol_kind(&row.kind),
                        is_public: row.is_public,
                        cognitive_complexity: None,
                        cyclomatic_complexity: None,
                        line_start: None,
                        line_end: None,
                        qualified_name,
                        byte_start: None,
                        byte_end: None,
                        entrypoint_kind: None,
                        metadata: std::collections::BTreeMap::new(),
                    }
                })
                .collect();

            let calls = match languages::extract_calls(&path, content.as_ref(), &sym_vec) {
                Ok(c) => c,
                Err(_) => {
                    files_skipped += 1;
                    continue;
                }
            };

            files_processed += 1;

            // Build a name -> (symbol_id, is_public) lookup for callers in this file
            let caller_by_name: HashMap<&str, (i64, bool)> = file_symbols
                .iter()
                .map(|row| (row.name.as_str(), (row.id, row.is_public)))
                .collect();

            // Collect edges with their public-ness for sorting/capping
            let mut file_edges: Vec<EdgeRow> = Vec::new();

            for call_edge in &calls {
                // Find caller symbol in this file
                let (caller_symbol_id, caller_is_public) =
                    match caller_by_name.get(call_edge.caller_name.as_str()) {
                        Some(&(id, pub_flag)) => (id, pub_flag),
                        None => continue, // skip if caller not found in this file's symbols
                    };

                let empty_bindings = std::collections::HashMap::new();
                let caller_bindings = bindings_by_file.get(file_id).unwrap_or(&empty_bindings);
                let caller_module = module_path_by_file.get(file_id);
                let resolved = resolve_callee(ResolveInput {
                    callee_name: &call_edge.callee_name,
                    caller_file_id: *file_id,
                    candidates_by_bare_name: &candidates_by_bare,
                    candidates_by_qualified: &candidates_by_qn,
                    caller_module_path: caller_module.map(String::as_str),
                    caller_bindings,
                    module_path_by_file: &module_path_by_file,
                });

                let callee_is_public = resolved
                    .callee_symbol_id
                    .and_then(|cid| symbol_id_to_public.get(&cid).copied())
                    .unwrap_or(false);

                let confidence = call_edge.call_kind.default_confidence();

                file_edges.push(EdgeRow {
                    caller_symbol_id,
                    caller_file_id: *file_id,
                    callee_symbol_id: resolved.callee_symbol_id,
                    callee_file_id: resolved.callee_file_id,
                    unresolved_callee: resolved.unresolved_callee,
                    call_kind: call_edge.call_kind.as_str().to_string(),
                    resolution_status: resolved.status.as_str().to_string(),
                    confidence,
                    evidence: call_edge.evidence.clone(),
                    // Used for sorting/cap prioritization:
                    public_priority: caller_is_public || callee_is_public,
                });
            }

            // 5. Edge cap: if > 50,000 edges per file, sort by public priority first
            if file_edges.len() > EDGE_CAP_PER_FILE {
                // Sort: public-caller or public-callee first (true > false)
                file_edges.sort_by_key(|b| std::cmp::Reverse(b.public_priority));
                let capped_count = file_edges.len() - EDGE_CAP_PER_FILE;
                for edge in file_edges.iter_mut().skip(EDGE_CAP_PER_FILE) {
                    edge.resolution_status = ResolutionStatus::Capped.as_str().to_string();
                }
                eprintln!(
                    "WARNING: File {} produced {} edges, capping at {} ({} capped)",
                    file_path,
                    file_edges.len(),
                    EDGE_CAP_PER_FILE,
                    capped_count
                );
                // Keep all edges but mark overflow as CAPPED
            }

            edge_batch.extend(file_edges);

            // 6. Batched inserts on the rebuild transaction
            if edge_batch.len() >= EDGE_BATCH_SIZE {
                insert_edge_batch(&tx, &edge_batch)?;
                total_edges += edge_batch.len();
                for edge in &edge_batch {
                    match edge.resolution_status.as_str() {
                        "RESOLVED" => resolved_edges += 1,
                        "UNRESOLVED" | "CAPPED" => unresolved_edges += 1,
                        "AMBIGUOUS" => ambiguous_edges += 1,
                        _ => {}
                    }
                }
                edge_batch.clear();
            }
        }

        // Flush remaining edges
        if !edge_batch.is_empty() {
            insert_edge_batch(&tx, &edge_batch)?;
            total_edges += edge_batch.len();
            for edge in &edge_batch {
                match edge.resolution_status.as_str() {
                    "RESOLVED" => resolved_edges += 1,
                    "UNRESOLVED" | "CAPPED" => unresolved_edges += 1,
                    "AMBIGUOUS" => ambiguous_edges += 1,
                    _ => {}
                }
            }
        }

        tx.commit().into_diagnostic()?;

        info!(
            "Call graph build complete: {} edges ({} resolved, {} ambiguous, {} unresolved) from {} files",
            total_edges, resolved_edges, ambiguous_edges, unresolved_edges, files_processed
        );

        Ok(CallGraphStats {
            total_edges,
            resolved_edges,
            unresolved_edges,
            ambiguous_edges,
            files_processed,
            files_skipped,
        })
    }
}

/// Helper: parse a symbol_kind string from the DB into a SymbolKind enum.
pub(crate) fn parse_symbol_kind(kind: &str) -> SymbolKind {
    match kind {
        "Function" => SymbolKind::Function,
        "Method" => SymbolKind::Method,
        "Class" => SymbolKind::Class,
        "Struct" => SymbolKind::Struct,
        "Enum" => SymbolKind::Enum,
        "Trait" => SymbolKind::Trait,
        "Interface" => SymbolKind::Interface,
        "Type" => SymbolKind::Type,
        "Variable" => SymbolKind::Variable,
        "Constant" => SymbolKind::Constant,
        "Module" => SymbolKind::Module,
        _ => SymbolKind::Function,
    }
}
