//! CozoDB knowledge-graph queries for the `/api/*` endpoints.

use crate::commands::web::api::handlers::collect_recent_commits;
use crate::commands::web::types::{
    KgEdge, KgNode, KnowledgeGraphResponse, SecurityBoundariesResponse,
};
use crate::git::repo::open_repo;
use crate::git::status::get_repo_status;
use crate::impact::hotspots::query_file_complexities;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::Result;
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Maximum number of nodes returned by `/api/knowledge-graph`.
pub(crate) const KG_MAX_LIMIT: usize = 1000;

// ---------------------------------------------------------------------------
// Security boundaries endpoint
// ---------------------------------------------------------------------------

pub(crate) fn fetch_security_boundaries(layout: &Layout) -> Result<SecurityBoundariesResponse> {
    let storage = match StorageManager::open_read_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Storage not available for /api/security/boundaries: {e}");
            return Ok(empty_boundaries_response());
        }
    };

    let cozo = match storage.cozo {
        Some(c) => c,
        None => {
            tracing::warn!("CozoDB not available for /api/security/boundaries");
            return Ok(empty_boundaries_response());
        }
    };

    // Authorisation nodes: policy, principal, action, resource.
    let auth_res = cozo.run_script(
        "?[id, label, category] := *node{id, label, category}, \
         category in ['policy', 'principal', 'action', 'resource']",
    )?;

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut auth_nodes = Vec::new();
    for row in &auth_res.rows {
        if let (
            Some(cozo::DataValue::Str(id)),
            Some(cozo::DataValue::Str(label)),
            Some(cozo::DataValue::Str(cat)),
        ) = (row.first(), row.get(1), row.get(2))
        {
            *counts.entry(cat.to_string()).or_insert(0) += 1;
            auth_nodes.push(json!({
                "id": id.to_string(),
                "label": label.to_string(),
                "category": cat.to_string(),
            }));
        }
    }

    // Cross-surface boundary edges: policy -> protected entity.
    let boundary_res = cozo.run_script(
        "?[policy_id, policy_label, relation, target_id, target_label, target_cat] := \
         *node{id: policy_id, label: policy_label, category: 'policy'}, \
         *edge{source: policy_id, target: target_id, relation: rel}, \
         *node{id: target_id, label: target_label, category: target_cat}, \
         target_cat in ['service', 'endpoint', 'config_key', 'deploy_surface', 'adr'], \
         relation = rel",
    )?;

    let mut boundary_edges = Vec::new();
    for row in &boundary_res.rows {
        if let (
            Some(cozo::DataValue::Str(pid)),
            Some(cozo::DataValue::Str(plabel)),
            Some(cozo::DataValue::Str(rel)),
            Some(cozo::DataValue::Str(tid)),
            Some(cozo::DataValue::Str(tlabel)),
            Some(cozo::DataValue::Str(tcat)),
        ) = (
            row.first(),
            row.get(1),
            row.get(2),
            row.get(3),
            row.get(4),
            row.get(5),
        ) {
            boundary_edges.push(json!({
                "policy_id": pid.to_string(),
                "policy_label": plabel.to_string(),
                "relation": rel.to_string(),
                "target_id": tid.to_string(),
                "target_label": tlabel.to_string(),
                "target_category": tcat.to_string(),
            }));
        }
    }

    Ok(SecurityBoundariesResponse {
        meta: json!({ "counts": counts }),
        boundaries: json!({
            "auth_nodes": auth_nodes,
            "boundary_edges": boundary_edges,
        }),
    })
}

fn empty_boundaries_response() -> SecurityBoundariesResponse {
    SecurityBoundariesResponse {
        meta: json!({ "counts": {} }),
        boundaries: json!({
            "auth_nodes": [],
            "boundary_edges": [],
        }),
    }
}

// ---------------------------------------------------------------------------
// Knowledge-graph subgraph endpoint
// ---------------------------------------------------------------------------

pub(crate) fn fetch_knowledge_graph(
    layout: &Layout,
    limit: usize,
    focus_changed: bool,
) -> Result<KnowledgeGraphResponse> {
    let storage = match StorageManager::open_read_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Storage not available for /api/knowledge-graph: {e}");
            return Ok(empty_kg_response());
        }
    };

    let cozo = match storage.cozo {
        Some(c) => c,
        None => {
            tracing::warn!("CozoDB not available for /api/knowledge-graph");
            return Ok(empty_kg_response());
        }
    };

    let mut node_ids: Vec<String> = Vec::new();
    let mut truncated = false;

    if focus_changed {
        let changed_files = collect_changed_file_paths(layout);
        if !changed_files.is_empty() {
            let params = id_list_param("files", &changed_files);
            let changed_res = cozo.run_script_with_params(
                "?[id] := *node{id, metadata: meta}, \
                 source_file = get(meta, 'source_file'), \
                 source_file in $files",
                params,
                cozo::ScriptMutability::Immutable,
            );

            let mut seed_ids = HashSet::new();
            if let Ok(res) = changed_res {
                for row in &res.rows {
                    if let Some(cozo::DataValue::Str(id)) = row.first() {
                        seed_ids.insert(id.to_string());
                    }
                }
            }

            if !seed_ids.is_empty() {
                let mut ids_within_two_hops =
                    expand_two_hops(&cozo, &seed_ids, limit.saturating_mul(2))?;
                if ids_within_two_hops.len() > limit {
                    truncated = true;
                    ids_within_two_hops.truncate(limit);
                }
                node_ids = ids_within_two_hops.into_iter().collect();
            }
        }
    }

    if node_ids.is_empty() {
        // Fallback: highest-risk nodes when there are no recent changes or focus is off.
        node_ids = fetch_top_risk_nodes(&cozo, limit)?;
    }

    if node_ids.is_empty() {
        return Ok(empty_kg_response());
    }

    let mut nodes = fetch_node_details(&cozo, &node_ids)?;
    let edges = fetch_edges_among(&cozo, &node_ids)?;

    enrich_kg_nodes(layout, &mut nodes);

    Ok(KnowledgeGraphResponse {
        nodes,
        edges,
        truncated,
    })
}

/// Enrich knowledge-graph nodes with SQLite-derived file paths and complexities.
/// This closes the gap between the backend node shape and the frontend graph
/// table, which expects top-level `file_path` and `complexity` fields.
fn enrich_kg_nodes(layout: &Layout, nodes: &mut [KgNode]) {
    let storage = match StorageManager::open_read_only_sqlite_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Storage not available for knowledge-graph enrichment: {e}");
            return;
        }
    };

    let file_paths: Vec<String> = nodes
        .iter()
        .filter(|n| n.category == "file")
        .filter_map(|n| {
            n.id.strip_prefix("urn:ledgerful:file:")
                .map(|s| s.to_string())
        })
        .collect();
    let symbol_qns: Vec<String> = nodes
        .iter()
        .filter(|n| n.category == "symbol")
        .filter_map(|n| {
            n.id.strip_prefix("urn:ledgerful:symbol:")
                .map(|s| s.to_string())
        })
        .collect();

    // File nodes: complexity from the hotspots table, file path from the URN.
    if let Ok(complexities) = query_file_complexities(&storage, &file_paths) {
        for n in nodes.iter_mut().filter(|n| n.category == "file") {
            if let Some(p) = n.id.strip_prefix("urn:ledgerful:file:") {
                n.complexity = complexities.get(p).copied().unwrap_or(0);
            }
        }
    }

    // Symbol nodes: file path and cognitive complexity from project_symbols.
    if !symbol_qns.is_empty() {
        let placeholders = symbol_qns.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT ps.qualified_name, pf.file_path, ps.cognitive_complexity \
             FROM project_symbols ps \
             JOIN project_files pf ON ps.file_id = pf.id \
             WHERE ps.qualified_name IN ({})",
            placeholders
        );
        let conn = storage.get_connection();
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let rows = stmt.query_map(rusqlite::params_from_iter(&symbol_qns), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            });
            if let Ok(rows) = rows {
                let mut lookup: HashMap<String, (String, i32)> = HashMap::new();
                for (qn, file_path, complexity) in rows.flatten() {
                    lookup.insert(qn, (file_path, complexity));
                }
                for n in nodes.iter_mut() {
                    if n.category == "symbol"
                        && let Some(qn) = n.id.strip_prefix("urn:ledgerful:symbol:")
                        && let Some((file_path, complexity)) = lookup.get(qn)
                    {
                        if n.file_path.is_empty() {
                            n.file_path.clone_from(file_path);
                        }
                        n.complexity = *complexity;
                    }
                }
            }
        }
    }
}

fn empty_kg_response() -> KnowledgeGraphResponse {
    KnowledgeGraphResponse {
        nodes: Vec::new(),
        edges: Vec::new(),
        truncated: false,
    }
}

fn collect_changed_file_paths(layout: &Layout) -> Vec<String> {
    let mut paths = HashSet::new();

    // Current working-tree changes.
    if let Ok(repo) = open_repo(layout.root.as_std_path())
        && let Ok(changes) = get_repo_status(&repo)
    {
        for change in changes {
            paths.insert(change.path.to_string_lossy().replace('\\', "/"));
        }
    }

    // Recent commits (last 7 days) for broader context.
    if let Ok(repo) = open_repo(layout.root.as_std_path())
        && let Ok(commits) = collect_recent_commits(&repo, 7, 1000)
    {
        for (_, files) in commits {
            for file in files {
                paths.insert(file);
            }
        }
    }

    paths.into_iter().collect()
}

fn id_list_param(key: &str, ids: &[String]) -> BTreeMap<String, cozo::DataValue> {
    let mut params = BTreeMap::new();
    let list: Vec<cozo::DataValue> = ids
        .iter()
        .map(|id| cozo::DataValue::Str(id.clone().into()))
        .collect();
    params.insert(key.to_string(), cozo::DataValue::List(Box::new(list)));
    params
}

/// Return all node IDs within two undirected hops of `seed_ids`, capped at `cap`.
fn expand_two_hops(
    cozo: &crate::state::storage_cozo::CozoStorage,
    seed_ids: &HashSet<String>,
    cap: usize,
) -> Result<Vec<String>> {
    let mut current: HashSet<String> = seed_ids.clone();
    let seed_vec: Vec<String> = seed_ids.iter().cloned().collect();

    // First hop.
    let params = id_list_param("ids", &seed_vec);
    let res = cozo.run_script_with_params(
        "?[nid] := *edge{source: s, target: nid}, s in $ids \n\
         ?[nid] := *edge{source: nid, target: s}, s in $ids",
        params,
        cozo::ScriptMutability::Immutable,
    )?;
    for row in &res.rows {
        if let Some(cozo::DataValue::Str(id)) = row.first() {
            current.insert(id.to_string());
            if current.len() >= cap {
                break;
            }
        }
    }

    // Second hop, using the expanded first-hop set.
    let first_hop: Vec<String> = current.iter().cloned().collect();
    let params = id_list_param("ids", &first_hop);
    let res = cozo.run_script_with_params(
        "?[nid] := *edge{source: s, target: nid}, s in $ids \n\
         ?[nid] := *edge{source: nid, target: s}, s in $ids",
        params,
        cozo::ScriptMutability::Immutable,
    )?;
    for row in &res.rows {
        if let Some(cozo::DataValue::Str(id)) = row.first() {
            current.insert(id.to_string());
            if current.len() >= cap {
                break;
            }
        }
    }

    let mut ids: Vec<String> = current.into_iter().collect();
    ids.sort_unstable();
    Ok(ids)
}

fn fetch_top_risk_nodes(
    cozo: &crate::state::storage_cozo::CozoStorage,
    limit: usize,
) -> Result<Vec<String>> {
    let query = format!(
        "?[id, risk_score] := *node{{id, risk_score}} \n         :order -risk_score \n         :limit {}",
        limit
    );
    let res = cozo.run_script(&query)?;
    let mut ids = Vec::new();
    for row in &res.rows {
        if let Some(cozo::DataValue::Str(id)) = row.first() {
            ids.push(id.to_string());
            if ids.len() >= limit {
                break;
            }
        }
    }
    Ok(ids)
}

fn fetch_node_details(
    cozo: &crate::state::storage_cozo::CozoStorage,
    ids: &[String],
) -> Result<Vec<KgNode>> {
    let params = id_list_param("ids", ids);
    let res = cozo.run_script_with_params(
        "?[id, label, category, risk_score, metadata] := \
         *node{id, label, category, risk_score, metadata}, id in $ids",
        params,
        cozo::ScriptMutability::Immutable,
    )?;

    let mut nodes = Vec::with_capacity(res.rows.len());
    for row in &res.rows {
        if let (
            Some(cozo::DataValue::Str(id)),
            Some(cozo::DataValue::Str(label)),
            Some(cozo::DataValue::Str(category)),
            Some(cozo::DataValue::Num(cozo::Num::Float(risk_score))),
            maybe_meta,
        ) = (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
        {
            let metadata = maybe_meta.and_then(|v| match v {
                cozo::DataValue::Json(val) => serde_json::to_value(val).ok(),
                _ => None,
            });
            let file_path = node_file_path(category, id, label, &metadata);
            let complexity = node_complexity(&metadata);
            nodes.push(KgNode {
                id: id.to_string(),
                label: label.to_string(),
                category: category.to_string(),
                risk_score: *risk_score,
                file_path,
                complexity,
                metadata,
            });
        }
    }
    Ok(nodes)
}

/// Derive a displayable file path for a knowledge-graph node from its metadata
/// or URN. File nodes use their identifier as the path; everything else falls
/// back to any `source_file` recorded in metadata.
fn node_file_path(
    category: &str,
    id: &str,
    _label: &str,
    metadata: &Option<serde_json::Value>,
) -> String {
    if let Some(m) = metadata
        && let Some(s) = m.get("source_file").and_then(|v| v.as_str())
    {
        return s.to_string();
    }
    if category == "file"
        && let Some(suffix) = id.strip_prefix("urn:ledgerful:file:")
    {
        return suffix.to_string();
    }
    String::new()
}

fn node_complexity(metadata: &Option<serde_json::Value>) -> i32 {
    metadata
        .as_ref()
        .and_then(|m| m.get("complexity").and_then(|v| v.as_i64()))
        .unwrap_or(0) as i32
}

fn fetch_edges_among(
    cozo: &crate::state::storage_cozo::CozoStorage,
    ids: &[String],
) -> Result<Vec<KgEdge>> {
    let params = id_list_param("ids", ids);
    let res = cozo.run_script_with_params(
        "?[source, target, relation, confidence, provenance_id] := \
         *edge{source, target, relation, confidence, provenance_id}, \
         source in $ids, target in $ids",
        params,
        cozo::ScriptMutability::Immutable,
    )?;

    let mut edges = Vec::with_capacity(res.rows.len());
    for row in &res.rows {
        if let (
            Some(cozo::DataValue::Str(source)),
            Some(cozo::DataValue::Str(target)),
            Some(cozo::DataValue::Str(relation)),
            confidence,
            provenance,
        ) = (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
        {
            let confidence = confidence.and_then(|v| match v {
                cozo::DataValue::Num(cozo::Num::Float(f)) => Some(*f),
                _ => None,
            });
            let provenance_id = provenance.and_then(|v| match v {
                cozo::DataValue::Str(s) => Some(s.to_string()),
                _ => None,
            });
            edges.push(KgEdge {
                source: source.to_string(),
                target: target.to_string(),
                relation: relation.to_string(),
                confidence,
                provenance_id,
            });
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_file_path_prefers_metadata_source_file() {
        let meta = Some(json!({ "source_file": "policies/auth.cedar" }));
        assert_eq!(
            node_file_path("policy", "urn:ledgerful:policy:x", "auth", &meta),
            "policies/auth.cedar"
        );
    }

    #[test]
    fn node_file_path_derives_from_file_urn() {
        assert_eq!(
            node_file_path("file", "urn:ledgerful:file:src/lib.rs", "src/lib.rs", &None),
            "src/lib.rs"
        );
    }

    #[test]
    fn node_file_path_returns_empty_when_unavailable() {
        assert_eq!(
            node_file_path("service", "urn:ledgerful:service:svc", "svc", &None),
            ""
        );
    }

    #[test]
    fn node_complexity_reads_metadata_integer() {
        let meta = Some(json!({ "complexity": 42 }));
        assert_eq!(node_complexity(&meta), 42);
    }

    #[test]
    fn node_complexity_defaults_to_zero() {
        assert_eq!(node_complexity(&None), 0);
    }
}
