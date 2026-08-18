use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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

pub(crate) fn is_stdlib_symbol(name: &str) -> bool {
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
