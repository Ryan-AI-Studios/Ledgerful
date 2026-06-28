use crate::index::languages;
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

        // 1. Query all project_symbols
        let mut stmt = conn
            .prepare("SELECT id, file_id, symbol_name, symbol_kind, is_public FROM project_symbols")
            .into_diagnostic()?;

        let symbol_rows: Vec<(i64, i64, String, String, bool)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i32>(4)? != 0,
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
        // symbols_by_file: file_id -> list of (symbol_id, symbol_name, symbol_kind, is_public)
        let mut symbols_by_file: HashMap<i64, Vec<(i64, String, String, bool)>> = HashMap::new();
        // symbol_index: (symbol_name, file_id) -> symbol_id
        let mut symbol_index: HashMap<(String, i64), i64> = HashMap::new();
        // symbol_name_to_ids: symbol_name -> Vec<symbol_id>
        let mut symbol_name_to_ids: HashMap<String, Vec<i64>> = HashMap::new();
        // symbol_id_to_file: symbol_id -> file_id (for resolution)
        let mut symbol_id_to_file: HashMap<i64, i64> = HashMap::new();

        for (sym_id, file_id, name, kind, is_public) in &symbol_rows {
            symbols_by_file.entry(*file_id).or_default().push((
                *sym_id,
                name.clone(),
                kind.clone(),
                *is_public,
            ));

            symbol_index.insert((name.clone(), *file_id), *sym_id);
            symbol_name_to_ids
                .entry(name.clone())
                .or_default()
                .push(*sym_id);
            symbol_id_to_file.insert(*sym_id, *file_id);
        }

        // file_paths: file_id -> file_path (available for future use)
        let _file_paths: HashMap<i64, String> = file_rows
            .iter()
            .map(|(id, path, _)| (*id, path.clone()))
            .collect();

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

            // Build Symbol structs for the language extractors
            let sym_vec: Vec<Symbol> = file_symbols
                .iter()
                .map(|(_, name, kind, is_public)| Symbol {
                    name: name.clone(),
                    kind: parse_symbol_kind(kind),
                    is_public: *is_public,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: None,
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
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
                .map(|(sym_id, name, _, is_public)| (name.as_str(), (*sym_id, *is_public)))
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

                let callee_is_public =
                    if let Some(callee_ids) = symbol_name_to_ids.get(&call_edge.callee_name) {
                        callee_ids
                            .iter()
                            .filter_map(|&cid| {
                                file_symbols
                                    .iter()
                                    .find(|(sid, _, _, _)| *sid == cid)
                                    .map(|(_, _, _, pub_flag)| *pub_flag)
                            })
                            .next()
                            .unwrap_or(false)
                    } else {
                        false
                    };

                // Try to resolve the callee
                let (callee_symbol_id, callee_file_id, resolution_status, unresolved_callee) =
                    if let Some(callee_ids) = symbol_name_to_ids.get(&call_edge.callee_name) {
                        if callee_ids.len() == 1 {
                            let cid = callee_ids[0];
                            let fid = symbol_id_to_file[&cid];
                            (Some(cid), Some(fid), ResolutionStatus::Resolved, None)
                        } else {
                            // Multiple matches: ambiguous
                            (
                                None,
                                None,
                                ResolutionStatus::Ambiguous,
                                Some(call_edge.callee_name.clone()),
                            )
                        }
                    } else {
                        (
                            None,
                            None,
                            ResolutionStatus::Unresolved,
                            Some(call_edge.callee_name.clone()),
                        )
                    };

                let confidence = call_edge.call_kind.default_confidence();

                file_edges.push(EdgeRow {
                    caller_symbol_id,
                    caller_file_id: *file_id,
                    callee_symbol_id,
                    callee_file_id,
                    unresolved_callee,
                    call_kind: call_edge.call_kind.as_str().to_string(),
                    resolution_status: resolution_status.as_str().to_string(),
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
}
