use super::GraphRelation;
use super::query::{
    get_changed_files_from_db, get_legacy_fallback_edges, is_real_file_path, query_incoming_edges,
    query_outgoing_edges, resolve_qualified_name,
};
use crate::state::graph_kinds::NodeKind;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::state::storage_cozo::CozoStorage;
use miette::Result;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};

/// Three-bucket graph payload. JSON keys are `{exact, derived, heuristic}`
/// with no `schemaVersion`. `completeness` is omit-empty when the assemble
/// cap did not hide further hops or nodes.
#[derive(Clone, Serialize)]
pub(super) struct LedgerGraphData {
    pub exact: Vec<GraphRelation>,
    pub derived: Vec<GraphRelation>,
    pub heuristic: Vec<GraphRelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completeness: Option<GraphCompleteness>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(super) struct GraphCompleteness {
    pub stop: &'static str,
    #[serde(rename = "maxDepth")]
    pub max_depth: u32,
    #[serde(rename = "maxNodes")]
    pub max_nodes: u32,
}

fn category_is_filtered(category: &str) -> bool {
    let c = category.to_lowercase();
    c == "ledger_transaction" || c == "transaction" || c == "adr"
}

fn relation_count(
    exact: &[GraphRelation],
    derived: &[GraphRelation],
    heuristic: &[GraphRelation],
) -> usize {
    exact.len() + derived.len() + heuristic.len()
}

fn has_further_unvisited_edge(
    cozo: &CozoStorage,
    root: &camino::Utf8Path,
    urn: &str,
    visited: &HashSet<String>,
) -> Result<bool> {
    let outgoing = query_outgoing_edges(cozo, urn, root)?;
    let incoming = query_incoming_edges(cozo, urn, root)?;
    for (neighbor, _, category, _) in outgoing.into_iter().chain(incoming) {
        if category_is_filtered(&category) {
            continue;
        }
        if !visited.contains(&neighbor) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn assemble_ledger_graph(
    layout: &Layout,
    storage: &StorageManager,
    cozo: &CozoStorage,
    db: &crate::ledger::db::LedgerDb,
    full_id: &str,
) -> Result<LedgerGraphData> {
    let tx_opt = db
        .get_transaction(full_id)
        .map_err(|e| miette::miette!("{}", e))?;

    let mut exact_relations = Vec::new();
    let mut derived_relations = Vec::new();
    let mut heuristic_relations = Vec::new();

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    // 1. Gather Token Provenance (Exact Symbols & Files)
    let token_prov = db
        .get_token_provenance_for_tx(full_id)
        .map_err(|e| miette::miette!("{}", e))?;
    for prov in &token_prov {
        let file_urn = crate::platform::urn::build_urn(NodeKind::File, &prov.entity_normalized);
        if visited.insert(file_urn.clone()) {
            let exists = layout.root.join(&prov.entity_normalized).exists();
            let label = if exists {
                prov.entity_normalized.clone()
            } else {
                format!("[HISTORICAL] {}", prov.entity_normalized)
            };
            exact_relations.push(GraphRelation {
                entity_id: file_urn.clone(),
                label,
                category: "file".to_string(),
                relation: format!("affects ({})", prov.action.to_string().to_lowercase()),
                exactness: "exact".to_string(),
                attribution_source: "token_provenance".to_string(),
            });
            queue.push_back((file_urn, 0));
        }

        // Try to resolve qualified name from project_symbol table
        let symbol_urn = if let Some(qn) = resolve_qualified_name(
            cozo,
            &prov.entity_normalized,
            &prov.symbol_name,
            &prov.symbol_type,
        )? {
            crate::platform::urn::build_urn(NodeKind::Symbol, &qn)
        } else {
            format!(
                "urn:ledgerful:symbol:historical:{}::{}",
                prov.entity_normalized, prov.symbol_name
            )
        };

        if visited.insert(symbol_urn.clone()) {
            let is_historical = symbol_urn.contains(":historical:");
            let label = if is_historical {
                format!("[HISTORICAL] {}", prov.symbol_name)
            } else {
                prov.symbol_name.clone()
            };
            exact_relations.push(GraphRelation {
                entity_id: symbol_urn.clone(),
                label,
                category: "symbol".to_string(),
                relation: format!(
                    "{} ({})",
                    prov.action.to_string().to_lowercase(),
                    prov.symbol_type.to_lowercase()
                ),
                exactness: "exact".to_string(),
                attribution_source: "token_provenance".to_string(),
            });
            queue.push_back((symbol_urn, 0));
        }
    }

    // 2. Staged/Committed changed files (Exact Files)
    let changed_files = get_changed_files_from_db(storage.get_connection(), full_id)?;
    for file_path in &changed_files {
        let file_urn = crate::platform::urn::build_urn(NodeKind::File, file_path);
        if visited.insert(file_urn.clone()) {
            let exists = layout.root.join(file_path).exists();
            let label = if exists {
                file_path.clone()
            } else {
                format!("[HISTORICAL] {}", file_path)
            };
            exact_relations.push(GraphRelation {
                entity_id: file_urn.clone(),
                label,
                category: "file".to_string(),
                relation: "modified".to_string(),
                exactness: "exact".to_string(),
                attribution_source: "changed_files".to_string(),
            });
            queue.push_back((file_urn, 0));
        }
    }

    // 3. Exact target entity ledger links
    if let Some(ref tx) = tx_opt
        && is_real_file_path(&tx.entity_normalized)
    {
        let is_synthetic = !tx.entity_normalized.contains('.')
            && !tx.entity_normalized.contains('/')
            && !tx.entity_normalized.contains('\\')
            && !layout.root.join(&tx.entity_normalized).exists();

        if !is_synthetic {
            let file_urn = crate::platform::urn::build_urn(NodeKind::File, &tx.entity_normalized);
            if visited.insert(file_urn.clone()) {
                let exists = layout.root.join(&tx.entity_normalized).exists();
                let label = if exists {
                    tx.entity_normalized.clone()
                } else {
                    format!("[HISTORICAL] {}", tx.entity_normalized)
                };
                exact_relations.push(GraphRelation {
                    entity_id: file_urn.clone(),
                    label,
                    category: "file".to_string(),
                    relation: "affects".to_string(),
                    exactness: "exact".to_string(),
                    attribution_source: "ledger_link".to_string(),
                });
                queue.push_back((file_urn, 0));
            }
        }
    }

    // 3.2. Link from transaction_links
    let stmt = storage.get_connection().prepare(
        "SELECT entity_normalized FROM transaction_links WHERE tx_id = ?1 AND entity_type = 'FILE'",
    );
    if let Ok(mut stmt) = stmt
        && let Ok(mut rows) = stmt.query([full_id])
    {
        while let Ok(Some(row)) = rows.next() {
            if let Ok(file_path) = row.get::<_, String>(0)
                && is_real_file_path(&file_path)
            {
                let file_urn = crate::platform::urn::build_urn(NodeKind::File, &file_path);
                if visited.insert(file_urn.clone()) {
                    let exists = layout.root.join(&file_path).exists();
                    let label = if exists {
                        file_path.clone()
                    } else {
                        format!("[HISTORICAL] {}", file_path)
                    };
                    exact_relations.push(GraphRelation {
                        entity_id: file_urn.clone(),
                        label,
                        category: "file".to_string(),
                        relation: "linked".to_string(),
                        exactness: "exact".to_string(),
                        attribution_source: "ledger_link".to_string(),
                    });
                    queue.push_back((file_urn, 0));
                }
            }
        }
    }

    // 3.5. Knowledge Graph committed transaction edges
    let tx_urn = crate::platform::urn::build_urn(NodeKind::LedgerTransaction, full_id);
    if let Ok(edges) = query_outgoing_edges(cozo, &tx_urn, &layout.root) {
        for (target_urn, label, category, relation) in edges {
            if visited.insert(target_urn.clone()) {
                exact_relations.push(GraphRelation {
                    entity_id: target_urn.clone(),
                    label,
                    category,
                    relation,
                    exactness: "exact".to_string(),
                    attribution_source: "knowledge_graph".to_string(),
                });
                queue.push_back((target_urn, 0));
            }
        }
    }

    // 4. Degraded legacy fallbacks (if set is empty)
    if exact_relations.is_empty()
        && let Some(ref tx) = tx_opt
    {
        let derived = get_legacy_fallback_edges(tx);
        for (id, label, cat, rel) in derived {
            if visited.insert(id.clone()) {
                heuristic_relations.push(GraphRelation {
                    entity_id: id.clone(),
                    label,
                    category: cat,
                    relation: rel,
                    exactness: "heuristic".to_string(),
                    attribution_source: "heuristic_fallback".to_string(),
                });
                queue.push_back((id, 0));
            }
        }
    }

    // BFS Neighborhood Traversal (Derived Relationships)
    let max_depth = 2;
    let max_nodes = 150;
    let mut truncated = false;

    while let Some((curr_urn, depth)) = queue.pop_front() {
        if relation_count(&exact_relations, &derived_relations, &heuristic_relations) >= max_nodes {
            truncated = true;
            continue;
        }
        if depth >= max_depth {
            // Unreachable while enqueue is gated by `depth + 1 < max_depth`.
            continue;
        }

        let outgoing = query_outgoing_edges(cozo, &curr_urn, &layout.root)?;
        let incoming = query_incoming_edges(cozo, &curr_urn, &layout.root)?;

        for (target_urn, target_label, target_category, relation) in outgoing {
            if relation_count(&exact_relations, &derived_relations, &heuristic_relations)
                >= max_nodes
            {
                truncated = true;
                break;
            }
            if category_is_filtered(&target_category) {
                continue;
            }
            if visited.insert(target_urn.clone()) {
                derived_relations.push(GraphRelation {
                    entity_id: target_urn.clone(),
                    label: target_label,
                    category: target_category,
                    relation: format!("{} (derived)", relation),
                    exactness: "derived".to_string(),
                    attribution_source: "knowledge_graph".to_string(),
                });
                if depth + 1 < max_depth {
                    queue.push_back((target_urn, depth + 1));
                } else if has_further_unvisited_edge(cozo, &layout.root, &target_urn, &visited)? {
                    truncated = true;
                }
            }
        }

        for (source_urn, source_label, source_category, relation) in incoming {
            if relation_count(&exact_relations, &derived_relations, &heuristic_relations)
                >= max_nodes
            {
                truncated = true;
                break;
            }
            if category_is_filtered(&source_category) {
                continue;
            }
            if visited.insert(source_urn.clone()) {
                derived_relations.push(GraphRelation {
                    entity_id: source_urn.clone(),
                    label: source_label,
                    category: source_category,
                    relation: format!("inv_{} (derived)", relation),
                    exactness: "derived".to_string(),
                    attribution_source: "knowledge_graph".to_string(),
                });
                if depth + 1 < max_depth {
                    queue.push_back((source_urn, depth + 1));
                } else if has_further_unvisited_edge(cozo, &layout.root, &source_urn, &visited)? {
                    truncated = true;
                }
            }
        }
    }

    exact_relations.sort();
    derived_relations.sort();
    heuristic_relations.sort();

    let completeness = if truncated {
        Some(GraphCompleteness {
            stop: "cap",
            max_depth: 2,
            max_nodes: 150,
        })
    } else {
        None
    };

    Ok(LedgerGraphData {
        exact: exact_relations,
        derived: derived_relations,
        heuristic: heuristic_relations,
        completeness,
    })
}

pub(super) fn parse_urn(urn: &str, root_path: &camino::Utf8Path) -> (String, String) {
    if urn.starts_with("urn:ledgerful:") {
        let parts: Vec<&str> = urn.split(':').collect();
        if parts.len() >= 4 && parts[2] == "symbol" && parts[3] == "historical" {
            let identifier = parts[4..].join(":");
            if let Some(pos) = identifier.find("::") {
                let symbol_name = &identifier[pos + 2..];
                return (
                    format!("[HISTORICAL] {}", symbol_name),
                    "symbol".to_string(),
                );
            }
            return (format!("[HISTORICAL] {}", identifier), "symbol".to_string());
        }
        if parts.len() >= 3 {
            let kind = parts[2];
            let identifier = parts[3..].join(":");
            let label = if kind == "file" {
                let file_exists = root_path.join(&identifier).exists();
                if file_exists {
                    identifier
                } else {
                    format!("[HISTORICAL] {}", identifier)
                }
            } else {
                identifier
            };
            return (label, kind.to_string());
        }
    }
    (urn.to_string(), "unknown".to_string())
}
