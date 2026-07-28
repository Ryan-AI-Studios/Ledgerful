use crate::index::languages;
use crate::index::resolve::{
    ResolveCandidate, ResolveInput, build_resolve_maps, resolve_callee, resolve_candidate_from_row,
};
use crate::index::symbols::{Symbol, SymbolKind};
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CallKind {
    Direct,
    MethodCall,
    TraitDispatch,
    Dynamic,
    External,
}

impl CallKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CallKind::Direct => "DIRECT",
            CallKind::MethodCall => "METHOD_CALL",
            CallKind::TraitDispatch => "TRAIT_DISPATCH",
            CallKind::Dynamic => "DYNAMIC",
            CallKind::External => "EXTERNAL",
        }
    }

    pub fn default_confidence(&self) -> f64 {
        match self {
            CallKind::Direct | CallKind::MethodCall => 1.0,
            CallKind::TraitDispatch => 0.8,
            CallKind::Dynamic => 0.5,
            CallKind::External => 0.3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResolutionStatus {
    Resolved,
    Ambiguous,
    Unresolved,
    Capped,
}

impl ResolutionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolutionStatus::Resolved => "RESOLVED",
            ResolutionStatus::Ambiguous => "AMBIGUOUS",
            ResolutionStatus::Unresolved => "UNRESOLVED",
            ResolutionStatus::Capped => "CAPPED",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallEdge {
    pub caller_name: String,
    pub caller_file: PathBuf,
    pub callee_name: String,
    pub callee_file: Option<PathBuf>,
    pub call_kind: CallKind,
    pub resolution_status: ResolutionStatus,
    pub confidence: f64,
    pub evidence: String,
}

pub struct CallGraph {
    pub edges: Vec<CallEdge>,
}

impl CallGraph {
    /// Enumerate all call chains starting from the given routes up to `max_depth`.
    pub fn enumerate_call_chains(
        &self,
        routes: &[crate::impact::packet::ApiRoute],
        max_depth: usize,
    ) -> Vec<crate::impact::packet::CallChain> {
        use crate::impact::packet::CallChainNode;

        let mut chains = Vec::new();
        let mut edges_by_caller: HashMap<String, Vec<&CallEdge>> = HashMap::new();
        for edge in &self.edges {
            edges_by_caller
                .entry(edge.caller_name.clone())
                .or_default()
                .push(edge);
        }

        for route in routes {
            let start_symbol = route
                .handler_symbol_name
                .as_ref()
                .cloned()
                .unwrap_or_else(|| "".to_string());
            if start_symbol.is_empty() {
                continue;
            }

            // Find starting edge to get the file path for the handler
            let start_file = self
                .edges
                .iter()
                .find(|e| e.caller_name == start_symbol)
                .map(|e| e.caller_file.clone())
                .unwrap_or_else(|| PathBuf::from(&route.route_source));

            let mut current_path = vec![CallChainNode {
                symbol: start_symbol.clone(),
                file_path: start_file,
                is_data_model: false, // Will be populated by coupling detection
                is_external: false,
            }];

            let mut visited = std::collections::HashSet::new();
            visited.insert(start_symbol.clone());

            self.walk_chains(
                &start_symbol,
                &edges_by_caller,
                &mut current_path,
                &mut visited,
                max_depth,
                &mut chains,
            );
        }

        chains
    }

    fn walk_chains(
        &self,
        current_symbol: &str,
        edges_by_caller: &HashMap<String, Vec<&CallEdge>>,
        current_path: &mut Vec<crate::impact::packet::CallChainNode>,
        visited: &mut std::collections::HashSet<String>,
        max_depth: usize,
        chains: &mut Vec<crate::impact::packet::CallChain>,
    ) {
        use crate::impact::packet::{CallChain, CallChainNode};

        if current_path.len() >= max_depth {
            chains.push(CallChain {
                nodes: current_path.clone(),
                has_cycle: false,
            });
            return;
        }

        let next_edges = match edges_by_caller.get(current_symbol) {
            Some(edges) => edges,
            None => {
                if current_path.len() > 1 {
                    chains.push(CallChain {
                        nodes: current_path.clone(),
                        has_cycle: false,
                    });
                }
                return;
            }
        };

        for edge in next_edges {
            if is_stdlib_symbol(&edge.callee_name) {
                continue;
            }

            let node = CallChainNode {
                symbol: edge.callee_name.clone(),
                file_path: edge.callee_file.clone().unwrap_or_default(),
                is_data_model: false,
                is_external: edge.call_kind == CallKind::External,
            };

            if visited.contains(&edge.callee_name) {
                let mut final_path = current_path.clone();
                final_path.push(node);
                chains.push(CallChain {
                    nodes: final_path,
                    has_cycle: true,
                });
                continue;
            }

            current_path.push(node);
            visited.insert(edge.callee_name.clone());

            self.walk_chains(
                &edge.callee_name,
                edges_by_caller,
                current_path,
                visited,
                max_depth,
                chains,
            );

            visited.remove(&edge.callee_name);
            current_path.pop();
        }
    }
}

fn is_stdlib_symbol(name: &str) -> bool {
    name.starts_with("std::")
        || name.starts_with("core::")
        || name.starts_with("alloc::")
        || name.starts_with("tokio::")
        || name.starts_with("actix_web::")
        || name.starts_with("axum::")
}

/// Stats returned after building the call graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallGraphStats {
    pub total_edges: usize,
    pub resolved_edges: usize,
    pub unresolved_edges: usize,
    pub ambiguous_edges: usize,
    pub files_processed: usize,
    pub files_skipped: usize,
}

pub struct CallGraphBuilder<'a> {
    storage: &'a StorageManager,
    repo_path: PathBuf,
}

const EDGE_CAP_PER_FILE: usize = 50_000;
const EDGE_BATCH_SIZE: usize = 500;

impl<'a> CallGraphBuilder<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: PathBuf) -> Self {
        Self { storage, repo_path }
    }

    pub fn build(&self) -> Result<CallGraphStats> {
        let conn = self.storage.get_connection();

        // 1. Query all project_symbols (include qualified_name for Part B reader)
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
        #[derive(Clone)]
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

        // 4. Iterate over source files
        let mut total_edges = 0usize;
        let mut resolved_edges = 0usize;
        let mut unresolved_edges = 0usize;
        let mut ambiguous_edges = 0usize;
        let mut files_processed = 0usize;
        let mut files_skipped = 0usize;
        let mut edge_batch: Vec<EdgeRow> = Vec::new();

        for (file_id, file_path, _language) in &file_rows {
            let full_path = self.repo_path.join(file_path);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => {
                    files_skipped += 1;
                    continue;
                }
            };

            let path = PathBuf::from(file_path);
            let file_symbols = symbols_by_file.get(file_id).cloned().unwrap_or_default();

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

            let calls = match languages::extract_calls(&path, &content, &sym_vec) {
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

            // 6. Batched inserts
            if edge_batch.len() >= EDGE_BATCH_SIZE {
                self.insert_edge_batch(&edge_batch)?;
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
            self.insert_edge_batch(&edge_batch)?;
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

    fn insert_edge_batch(&self, edges: &[EdgeRow]) -> Result<()> {
        let conn = self.storage.get_connection();
        let tx = conn.unchecked_transaction().into_diagnostic()?;

        for edge in edges {
            tx.execute(
                "INSERT INTO structural_edges \
                 (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, \
                  unresolved_callee, call_kind, resolution_status, confidence, evidence) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    edge.caller_symbol_id,
                    edge.caller_file_id,
                    edge.callee_symbol_id,
                    edge.callee_file_id,
                    edge.unresolved_callee,
                    edge.call_kind,
                    edge.resolution_status,
                    edge.confidence,
                    edge.evidence,
                ],
            )
            .into_diagnostic()?;
        }

        tx.commit().into_diagnostic()?;
        Ok(())
    }
}

/// Internal struct for accumulating edge rows before batch insert.
struct EdgeRow {
    caller_symbol_id: i64,
    caller_file_id: i64,
    callee_symbol_id: Option<i64>,
    callee_file_id: Option<i64>,
    unresolved_callee: Option<String>,
    call_kind: String,
    resolution_status: String,
    confidence: f64,
    evidence: String,
    public_priority: bool,
}

/// Helper: parse a symbol_kind string from the DB into a SymbolKind enum.
fn parse_symbol_kind(kind: &str) -> SymbolKind {
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

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    #[test]
    fn test_call_kind_default_confidence() {
        assert!((CallKind::Direct.default_confidence() - 1.0).abs() < f64::EPSILON);
        assert!((CallKind::MethodCall.default_confidence() - 1.0).abs() < f64::EPSILON);
        assert!((CallKind::TraitDispatch.default_confidence() - 0.8).abs() < f64::EPSILON);
        assert!((CallKind::Dynamic.default_confidence() - 0.5).abs() < f64::EPSILON);
        assert!((CallKind::External.default_confidence() - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_resolution_status_as_str() {
        assert_eq!(ResolutionStatus::Resolved.as_str(), "RESOLVED");
        assert_eq!(ResolutionStatus::Ambiguous.as_str(), "AMBIGUOUS");
        assert_eq!(ResolutionStatus::Unresolved.as_str(), "UNRESOLVED");
        assert_eq!(ResolutionStatus::Capped.as_str(), "CAPPED");
    }

    #[test]
    fn test_call_kind_as_str() {
        assert_eq!(CallKind::Direct.as_str(), "DIRECT");
        assert_eq!(CallKind::MethodCall.as_str(), "METHOD_CALL");
        assert_eq!(CallKind::TraitDispatch.as_str(), "TRAIT_DISPATCH");
        assert_eq!(CallKind::Dynamic.as_str(), "DYNAMIC");
        assert_eq!(CallKind::External.as_str(), "EXTERNAL");
    }

    #[test]
    fn test_call_graph_builder_empty_symbols() {
        let storage = in_memory_storage();
        let builder = CallGraphBuilder::new(&storage, PathBuf::from("/tmp/test_repo"));

        let stats = builder.build().unwrap();
        assert_eq!(stats.total_edges, 0);
        assert_eq!(stats.files_processed, 0);
        assert_eq!(stats.files_skipped, 0);
    }

    #[test]
    fn test_call_graph_builder_with_data() {
        let storage = in_memory_storage();

        // Insert a project_file and two symbols
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/lib.rs", "Rust", "hash1", 100, "2026-05-01T00:00:00Z"),
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (file_id, "crate::caller_fn", "caller_fn", "Function", 1, 1.0, "2026-05-01T00:00:00Z"),
        ).unwrap();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (file_id, "crate::callee_fn", "callee_fn", "Function", 1, 1.0, "2026-05-01T00:00:00Z"),
        ).unwrap();

        // Build with a nonexistent repo path so files get skipped
        let builder = CallGraphBuilder::new(&storage, PathBuf::from("/tmp/nonexistent_repo_12345"));
        let stats = builder.build().unwrap();

        // Files should be skipped since the repo path doesn't exist
        assert_eq!(stats.files_skipped, 1);
        assert_eq!(stats.total_edges, 0);
    }

    #[test]
    fn test_parse_symbol_kind() {
        assert_eq!(parse_symbol_kind("Function"), SymbolKind::Function);
        assert_eq!(parse_symbol_kind("Method"), SymbolKind::Method);
        assert_eq!(parse_symbol_kind("Class"), SymbolKind::Class);
        assert_eq!(parse_symbol_kind("Struct"), SymbolKind::Struct);
        assert_eq!(parse_symbol_kind("Enum"), SymbolKind::Enum);
        assert_eq!(parse_symbol_kind("Trait"), SymbolKind::Trait);
        assert_eq!(parse_symbol_kind("Interface"), SymbolKind::Interface);
        assert_eq!(parse_symbol_kind("Type"), SymbolKind::Type);
        assert_eq!(parse_symbol_kind("Variable"), SymbolKind::Variable);
        assert_eq!(parse_symbol_kind("Constant"), SymbolKind::Constant);
        assert_eq!(parse_symbol_kind("Module"), SymbolKind::Module);
        assert_eq!(parse_symbol_kind("Unknown"), SymbolKind::Function);
    }

    #[test]
    fn test_call_graph_stats_serialization() {
        let stats = CallGraphStats {
            total_edges: 100,
            resolved_edges: 80,
            unresolved_edges: 10,
            ambiguous_edges: 10,
            files_processed: 5,
            files_skipped: 1,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("total_edges"));
        assert!(json.contains("resolved_edges"));
    }

    #[test]
    fn test_enumerate_from_route_handler() {
        use crate::impact::packet::ApiRoute;

        let graph = CallGraph {
            edges: vec![
                CallEdge {
                    caller_name: "handler_a".to_string(),
                    caller_file: PathBuf::from("src/routes.rs"),
                    callee_name: "service_b".to_string(),
                    callee_file: Some(PathBuf::from("src/service.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "".to_string(),
                },
                CallEdge {
                    caller_name: "service_b".to_string(),
                    caller_file: PathBuf::from("src/service.rs"),
                    callee_name: "dao_c".to_string(),
                    callee_file: Some(PathBuf::from("src/dao.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "".to_string(),
                },
            ],
        };

        let routes = vec![ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/users".to_string(),
            handler_symbol_name: Some("handler_a".to_string()),
            framework: "Axum".to_string(),
            route_source: "src/routes.rs".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: String::new(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        }];

        let chains = graph.enumerate_call_chains(&routes, 5);
        assert!(!chains.is_empty());
        assert_eq!(chains[0].nodes[0].symbol, "handler_a");
        assert!(
            chains
                .iter()
                .any(|c| c.nodes.len() == 3 && c.nodes[2].symbol == "dao_c"),
            "expected a chain of length 3 ending at dao_c"
        );
    }

    #[test]
    fn test_cycle_terminates_at_max_depth() {
        use crate::impact::packet::ApiRoute;

        // Cyclic graph: A -> B -> A
        let graph = CallGraph {
            edges: vec![
                CallEdge {
                    caller_name: "A".to_string(),
                    caller_file: PathBuf::from("a.rs"),
                    callee_name: "B".to_string(),
                    callee_file: Some(PathBuf::from("b.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "".to_string(),
                },
                CallEdge {
                    caller_name: "B".to_string(),
                    caller_file: PathBuf::from("b.rs"),
                    callee_name: "A".to_string(),
                    callee_file: Some(PathBuf::from("a.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "".to_string(),
                },
            ],
        };

        let routes = vec![ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/".to_string(),
            handler_symbol_name: Some("A".to_string()),
            framework: "Axum".to_string(),
            route_source: "a.rs".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: String::new(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        }];

        let chains = graph.enumerate_call_chains(&routes, 3);
        assert!(
            chains.iter().any(|c| c.has_cycle),
            "expected at least one chain with has_cycle=true"
        );
        // Should terminate and not loop infinitely
    }

    #[test]
    fn test_stdlib_excluded() {
        use crate::impact::packet::ApiRoute;

        let graph = CallGraph {
            edges: vec![
                CallEdge {
                    caller_name: "handler".to_string(),
                    caller_file: PathBuf::from("src/main.rs"),
                    callee_name: "std::fs::read".to_string(),
                    callee_file: Some(PathBuf::from("std")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "".to_string(),
                },
                CallEdge {
                    caller_name: "handler".to_string(),
                    caller_file: PathBuf::from("src/main.rs"),
                    callee_name: "user_fn".to_string(),
                    callee_file: Some(PathBuf::from("src/lib.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "".to_string(),
                },
            ],
        };

        let routes = vec![ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/".to_string(),
            handler_symbol_name: Some("handler".to_string()),
            framework: "Axum".to_string(),
            route_source: "src/main.rs".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: String::new(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        }];

        let chains = graph.enumerate_call_chains(&routes, 5);
        for chain in &chains {
            for node in &chain.nodes {
                assert_ne!(
                    node.symbol, "std::fs::read",
                    "stdlib symbol should be excluded from chains"
                );
            }
        }
    }

    /// E2E Test 1: Full pipeline — Rust call chain
    /// Creates a temp Rust project, seeds the DB with file/symbol rows,
    /// runs CallGraphBuilder::build(), and verifies structural_edges.
    #[test]
    fn test_full_pipeline_rust_call_chain() {
        use std::fs;

        // 1. Create a temporary directory with a Rust project structure
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).expect("failed to create src dir");

        let main_rs_content = r#"fn main() {
    helper();
}
fn helper() {
    internal();
}
fn internal() {}
"#;
        let main_rs_path = src_dir.join("main.rs");
        fs::write(&main_rs_path, main_rs_content).expect("failed to write main.rs");

        // 2. Create an in-memory DB with migrations applied
        let storage = in_memory_storage();

        // 3. Insert project_files and project_symbols entries matching the real file
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/main.rs", "Rust", "hash_e2e", 100, "2026-05-01T00:00:00Z"),
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        // Insert three symbols: main, helper, internal
        for (qualified, name) in [
            ("crate::main", "main"),
            ("crate::helper", "helper"),
            ("crate::internal", "internal"),
        ] {
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (file_id, qualified, name, "Function", 0, 1.0, "2026-05-01T00:00:00Z"),
            ).unwrap();
        }

        // 4. Run CallGraphBuilder::build() pointing at the temp directory
        let builder = CallGraphBuilder::new(&storage, dir.path().to_path_buf());
        let stats = builder.build().expect("call graph build failed");

        // Should have processed 1 file
        assert_eq!(stats.files_processed, 1, "expected 1 file processed");
        assert_eq!(stats.files_skipped, 0, "expected 0 files skipped");

        // 5. Verify structural_edges contains the expected edges
        let mut stmt = conn
            .prepare(
                "SELECT caller_symbol_id, callee_symbol_id, call_kind, resolution_status
                 FROM structural_edges",
            )
            .unwrap();

        let edges: Vec<(i64, Option<i64>, String, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        // Should have at least 2 edges: main->helper and helper->internal
        assert!(
            edges.len() >= 2,
            "expected at least 2 edges, got {}",
            edges.len()
        );

        // 6. Verify edges have call_kind = 'DIRECT' and resolution_status = 'RESOLVED'
        for edge in &edges {
            assert_eq!(
                edge.2, "DIRECT",
                "expected call_kind DIRECT, got {}",
                edge.2
            );
            assert_eq!(
                edge.3, "RESOLVED",
                "expected resolution_status RESOLVED, got {}",
                edge.3
            );
        }

        // Also verify via symbol names that main->helper and helper->internal exist
        // Look up caller symbol names for the edges we found
        let mut caller_names: Vec<String> = Vec::new();
        let mut callee_names: Vec<String> = Vec::new();
        for edge in &edges {
            let caller_name: String = conn
                .query_row(
                    "SELECT symbol_name FROM project_symbols WHERE id = ?1",
                    [edge.0],
                    |row| row.get(0),
                )
                .unwrap();
            let callee_name: Option<String> = edge.1.and_then(|cid| {
                conn.query_row(
                    "SELECT symbol_name FROM project_symbols WHERE id = ?1",
                    [cid],
                    |row| row.get(0),
                )
                .ok()
            });
            caller_names.push(caller_name.clone());
            callee_names.push(callee_name.unwrap_or_default());
        }

        // Verify main->helper edge exists
        assert!(
            caller_names
                .iter()
                .zip(callee_names.iter())
                .any(|(c, e)| c == "main" && e == "helper"),
            "expected main->helper edge, got callers={:?} callees={:?}",
            caller_names,
            callee_names
        );

        // Verify helper->internal edge exists
        assert!(
            caller_names
                .iter()
                .zip(callee_names.iter())
                .any(|(c, e)| c == "helper" && e == "internal"),
            "expected helper->internal edge, got callers={:?} callees={:?}",
            caller_names,
            callee_names
        );
    }

    /// 0092 rolled-in: Py/TS production-path negative — json.loads / axios.get
    /// must persist as UNRESOLVED even with a same-file Function of that name.
    #[test]
    fn production_path_json_loads_and_axios_get_unresolved() {
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();

        // Python: free Function loads + json.loads call
        fs::write(
            src.join("app.py"),
            r#"
def loads(x):
    return x

def caller():
    return json.loads(x)
"#,
        )
        .unwrap();

        // TypeScript: free Function get + axios.get call
        fs::write(
            src.join("client.ts"),
            r#"
export function get() { return 1; }
export function run() { return axios.get("/x"); }
"#,
        )
        .unwrap();

        let storage = in_memory_storage();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/app.py", "Python", "h1", 50, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let py_id = conn.last_insert_rowid();
        for (qn, name) in [("loads", "loads"), ("caller", "caller")] {
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (py_id, qn, name, "Function", 1, 1.0, "2026-05-01T00:00:00Z"),
            )
            .unwrap();
        }

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/client.ts", "TypeScript", "h2", 50, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let ts_id = conn.last_insert_rowid();
        for (qn, name) in [("get", "get"), ("run", "run")] {
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (ts_id, qn, name, "Function", 1, 1.0, "2026-05-01T00:00:00Z"),
            )
            .unwrap();
        }

        let builder = CallGraphBuilder::new(&storage, dir.path().to_path_buf());
        let stats = builder.build().expect("build");
        assert!(stats.total_edges >= 2);

        let mut stmt = conn
            .prepare(
                "SELECT unresolved_callee, resolution_status, callee_symbol_id \
                 FROM structural_edges \
                 WHERE unresolved_callee IN ('json.loads', 'axios.get') \
                    OR (resolution_status = 'RESOLVED' AND callee_symbol_id IS NOT NULL)",
            )
            .unwrap();
        let rows: Vec<(Option<String>, String, Option<i64>)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        let json_edge = rows
            .iter()
            .find(|(u, _, _)| u.as_deref() == Some("json.loads"));
        assert!(
            json_edge.is_some(),
            "expected json.loads edge, got {rows:?}"
        );
        assert_eq!(json_edge.unwrap().1, "UNRESOLVED");
        assert!(json_edge.unwrap().2.is_none());

        let axios_edge = rows
            .iter()
            .find(|(u, _, _)| u.as_deref() == Some("axios.get"));
        assert!(
            axios_edge.is_some(),
            "expected axios.get edge, got {rows:?}"
        );
        assert_eq!(axios_edge.unwrap().1, "UNRESOLVED");
        assert!(axios_edge.unwrap().2.is_none());
    }

    /// DoD-1: bindings round-trip through replace_file_bindings.
    #[test]
    fn file_bindings_round_trip_persist() {
        use crate::index::bindings::FileBinding;
        use crate::index::rows::{load_file_bindings_map, replace_file_bindings};

        let storage = in_memory_storage();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/util/mod.rs", "Rust", "h", 10, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        let bindings = vec![
            FileBinding {
                bound_name: "fs".into(),
                source_path: "fs".into(),
                binding_kind: "mod".into(),
                is_enumerable: true,
                is_local: true,
            },
            FileBinding {
                bound_name: "Map".into(),
                source_path: "std::collections::HashMap".into(),
                binding_kind: "use".into(),
                is_enumerable: true,
                is_local: false,
            },
            FileBinding {
                bound_name: "*".into(),
                source_path: "foo::*".into(),
                binding_kind: "use_wildcard".into(),
                is_enumerable: false,
                is_local: false,
            },
        ];
        replace_file_bindings(conn, file_id, &bindings).unwrap();

        let map = load_file_bindings_map(conn, file_id).unwrap();
        assert!(map.get("fs").is_some_and(|b| b.is_local && b.is_enumerable));
        assert!(map.get("Map").is_some_and(|b| !b.is_local));
        assert!(map.get("*").is_some_and(|b| !b.is_enumerable));

        // Empty replace clears
        replace_file_bindings(conn, file_id, &[]).unwrap();
        let map2 = load_file_bindings_map(conn, file_id).unwrap();
        assert!(map2.is_empty());
    }

    /// DoD-6: full CallGraphBuilder structural_edges identity on tempfile
    /// fixture (two builds over identical DB+files must match).
    #[test]
    fn full_vs_full_structural_edges_identity() {
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("util")).unwrap();
        fs::write(
            src.join("util/mod.rs"),
            "pub mod fs;\nfn caller() { fs::local_write(); }\n",
        )
        .unwrap();
        fs::write(src.join("util/fs.rs"), "pub fn local_write() {}\n").unwrap();
        fs::write(
            src.join("main.rs"),
            "use std::fs;\nfn main() { fs::write(\"x\", b\"\"); }\n",
        )
        .unwrap();

        let storage = in_memory_storage();
        let conn = storage.get_connection();

        // util/mod.rs
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/util/mod.rs", "Rust", "h1", 40, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (mod_id, "caller", "caller", "Function", 0, 1.0, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        crate::index::rows::replace_file_bindings(
            conn,
            mod_id,
            &[crate::index::bindings::FileBinding {
                bound_name: "fs".into(),
                source_path: "fs".into(),
                binding_kind: "mod".into(),
                is_enumerable: true,
                is_local: true,
            }],
        )
        .unwrap();

        // util/fs.rs
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/util/fs.rs", "Rust", "h2", 30, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let fs_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                fs_id,
                "local_write",
                "local_write",
                "Function",
                1,
                1.0,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();

        // main.rs with std::fs
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/main.rs", "Rust", "h3", 50, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let main_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (main_id, "main", "main", "Function", 0, 1.0, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        crate::index::rows::replace_file_bindings(
            conn,
            main_id,
            &[crate::index::bindings::FileBinding {
                bound_name: "fs".into(),
                source_path: "std::fs".into(),
                binding_kind: "use".into(),
                is_enumerable: true,
                is_local: false,
            }],
        )
        .unwrap();

        let builder = CallGraphBuilder::new(&storage, dir.path().to_path_buf());
        builder.build().expect("build 1");

        let edges1: Vec<(i64, Option<i64>, Option<String>, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT caller_symbol_id, callee_symbol_id, unresolved_callee, resolution_status \
                     FROM structural_edges ORDER BY caller_symbol_id, unresolved_callee, resolution_status",
                )
                .unwrap();
            stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };

        // Clear edges and rebuild (identity of resolve path)
        conn.execute("DELETE FROM structural_edges", []).unwrap();
        builder.build().expect("build 2");

        let edges2: Vec<(i64, Option<i64>, Option<String>, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT caller_symbol_id, callee_symbol_id, unresolved_callee, resolution_status \
                     FROM structural_edges ORDER BY caller_symbol_id, unresolved_callee, resolution_status",
                )
                .unwrap();
            stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };

        assert_eq!(edges1, edges2, "repeated full build must be byte-identical");

        // Dual-direction fs check on persisted edges
        let mut stmt = conn
            .prepare(
                "SELECT pf.file_path, se.resolution_status, se.callee_symbol_id, se.unresolved_callee \
                 FROM structural_edges se \
                 JOIN project_files pf ON se.caller_file_id = pf.id \
                 WHERE se.unresolved_callee LIKE 'fs.%' OR se.unresolved_callee LIKE 'fs::%' \
                    OR (se.resolution_status = 'RESOLVED' AND se.unresolved_callee IS NULL)",
            )
            .unwrap();
        let edge_rows: Vec<(String, String, Option<i64>, Option<String>)> = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        let mod_edges: Vec<_> = edge_rows
            .iter()
            .filter(|(p, _, _, _)| p.contains("util/mod"))
            .collect();
        let main_edges: Vec<_> = edge_rows
            .iter()
            .filter(|(p, _, _, u)| {
                p.contains("main.rs") && u.as_deref().is_some_and(|s| s.starts_with("fs."))
            })
            .collect();

        // mod-bound fs::local_write should resolve
        assert!(
            mod_edges
                .iter()
                .any(|(_, st, cid, _)| st == "RESOLVED" && cid.is_some()),
            "mod-bound fs call should resolve, got {mod_edges:?}"
        );
        // std::fs write must stay unresolved
        assert!(
            main_edges
                .iter()
                .any(|(_, st, cid, _)| st == "UNRESOLVED" && cid.is_none()),
            "std::fs call must stay unresolved, got {main_edges:?}"
        );
    }

    /// DoD-6: live full CallGraphBuilder vs IncrementalSyncEngine structural_edges
    /// identity on a hermetic multi-file tempfile (name-keyed edge dump).
    #[test]
    fn full_vs_incremental_structural_edges_identity() {
        use crate::config::model::Config;
        use crate::index::incremental::IncrementalSyncEngine;
        use crate::index::orchestrator::ProjectIndexer;
        use crate::state::storage_cozo::CozoStorage;
        use crate::watch::batch::{WatchBatch, WatchEvent, WatchEventKind};
        use camino::Utf8PathBuf;
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo_path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8 path");
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("util")).unwrap();

        // Multi-file fixture: crate:: paths + local mod binding + external use.
        fs::write(
            src.join("lib.rs"),
            r#"
mod util;

pub fn entry() {
    util::run();
    crate::util::run();
}
"#,
        )
        .unwrap();
        fs::write(
            src.join("util/mod.rs"),
            r#"
pub mod fs;

pub fn run() {
    fs::local_write();
    crate::util::fs::local_write();
}
"#,
        )
        .unwrap();
        fs::write(
            src.join("util/fs.rs"),
            r#"
pub fn local_write() {}
"#,
        )
        .unwrap();
        fs::write(
            src.join("main.rs"),
            r#"
use std::fs;

fn main() {
    fs::write("x", b"");
}
"#,
        )
        .unwrap();

        // Storage with Cozo (required by IncrementalSyncEngine process_batch).
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        let mut storage = StorageManager::init_from_conn(conn);
        let cozo = CozoStorage::new(&std::path::PathBuf::from("")).unwrap();
        storage.cozo = Some(cozo);

        let indexer = ProjectIndexer::new(storage, repo_path.clone(), Config::default());
        let mut engine = IncrementalSyncEngine::new(indexer, repo_path.clone());

        let batch = WatchBatch::new(vec![
            WatchEvent {
                path: repo_path.join("src/lib.rs"),
                kind: WatchEventKind::Create,
            },
            WatchEvent {
                path: repo_path.join("src/util/mod.rs"),
                kind: WatchEventKind::Create,
            },
            WatchEvent {
                path: repo_path.join("src/util/fs.rs"),
                kind: WatchEventKind::Create,
            },
            WatchEvent {
                path: repo_path.join("src/main.rs"),
                kind: WatchEventKind::Create,
            },
        ]);
        let delta = engine.process_batch(&batch).expect("incremental index");
        assert!(
            delta.files_processed >= 4,
            "expected all fixture files processed, got {}",
            delta.files_processed
        );

        fn dump_edge_keys(conn: &Connection) -> Vec<(String, String, String, String, String)> {
            let mut stmt = conn
                .prepare(
                    "SELECT pf.file_path, ps_caller.symbol_name, \
                            COALESCE(ps_callee.symbol_name, se.unresolved_callee, ''), \
                            se.resolution_status, se.call_kind \
                     FROM structural_edges se \
                     JOIN project_files pf ON se.caller_file_id = pf.id \
                     JOIN project_symbols ps_caller ON se.caller_symbol_id = ps_caller.id \
                     LEFT JOIN project_symbols ps_callee ON se.callee_symbol_id = ps_callee.id \
                     ORDER BY pf.file_path, ps_caller.symbol_name, 3, se.resolution_status, se.call_kind",
                )
                .unwrap();
            let mut keys: Vec<(String, String, String, String, String)> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            keys.sort();
            keys
        }

        let conn = engine.indexer.storage().get_connection();
        let incremental_keys = dump_edge_keys(conn);
        assert!(
            !incremental_keys.is_empty(),
            "incremental path must produce structural_edges"
        );

        // Clear edges only — keep files/symbols/bindings for the full builder path.
        conn.execute("DELETE FROM structural_edges", []).unwrap();

        let builder = CallGraphBuilder::new(engine.indexer.storage(), dir.path().to_path_buf());
        builder.build().expect("full CallGraphBuilder");
        let full_keys = dump_edge_keys(conn);

        assert_eq!(
            incremental_keys, full_keys,
            "full vs incremental structural_edges keys must match\nincremental={incremental_keys:?}\nfull={full_keys:?}"
        );

        // Sanity: mod-bound local_write resolves; std::fs write stays unresolved.
        assert!(
            full_keys.iter().any(|(_, caller, callee, status, _)| {
                caller == "run" && callee == "local_write" && status == "RESOLVED"
            }),
            "expected resolved local_write from util::run, got {full_keys:?}"
        );
        assert!(
            full_keys.iter().any(|(_, caller, callee, status, _)| {
                caller == "main" && callee.contains("fs") && status == "UNRESOLVED"
            }),
            "expected unresolved std::fs call from main, got {full_keys:?}"
        );
    }
}
