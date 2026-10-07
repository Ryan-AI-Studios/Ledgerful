//! Read a capped node/edge selection from Cozo. The renderer stays pure.

use super::render::{ExportEdge, ExportGraph, ExportNode};
use crate::state::cozo::queries;
use crate::state::storage_cozo::CozoStorage;
use cozo::{DataValue, Num};
use miette::{Result, miette};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub enum Selection {
    IdPrefix,
    Walk { entity: String, depth: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct IncidentEdge {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: f64,
    pub provenance_id: String,
}

pub struct WalkSelection {
    pub ids: Vec<String>,
    pub edges: Vec<IncidentEdge>,
    pub truncated: bool,
}

pub fn load_graph(cozo: &CozoStorage, limit: u64, selection: Selection) -> Result<ExportGraph> {
    if !(1..=super::MAX_LIMIT).contains(&limit) {
        return Err(miette!(
            "graph export: limit {limit} is outside 1..={}",
            super::MAX_LIMIT
        ));
    }
    let store_nodes = read_count(cozo, queries::node_count_query(), "store_nodes")?;
    let store_edges = read_count(cozo, queries::edge_count_query(), "store_edges")?;
    let (mut nodes, edges, truncated) = match selection {
        Selection::IdPrefix => load_prefix(cozo, limit)?,
        Selection::Walk { entity, depth } => load_walk(cozo, limit, &entity, depth)?,
    };
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges = edges;
    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.target.cmp(&b.target))
            .then(a.relation.cmp(&b.relation))
    });
    Ok(ExportGraph {
        nodes,
        edges,
        limit,
        truncated,
        store_nodes,
        store_edges,
    })
}

fn load_prefix(cozo: &CozoStorage, limit: u64) -> Result<(Vec<ExportNode>, Vec<ExportEdge>, bool)> {
    let fetch = limit.saturating_add(1);
    let script = format!(
        "?[id, label, category, risk_score, metadata] := *node{{id, label, category, risk_score, metadata}} :order +id :limit {fetch}\n"
    );
    let rows = cozo.run_script(&script)?;
    let mut nodes = Vec::with_capacity(rows.rows.len());
    for row in rows.rows {
        nodes.push(parse_node(&row)?);
    }
    let (nodes, truncated) = select_id_prefix(nodes, limit);
    let edges = if nodes.is_empty() {
        Vec::new()
    } else {
        let ids: Vec<String> = nodes.iter().map(|node| node.id.clone()).collect();
        let script = both_ends_script(&ids)
            .ok_or_else(|| miette!("graph export: kept node set was empty after the prefix cap"))?;
        parse_edges(&cozo.run_script(&script)?.rows)?
    };
    Ok((nodes, edges, truncated))
}

fn load_walk(
    cozo: &CozoStorage,
    limit: u64,
    entity: &str,
    depth: u32,
) -> Result<(Vec<ExportNode>, Vec<ExportEdge>, bool)> {
    let root = load_root(cozo, entity)?;
    let root_id = root.id.clone();
    let mut by_id = BTreeMap::from([(root_id.clone(), root)]);
    let walk = select_walk(&root_id, depth, limit, |frontier| {
        let script = incident_script(frontier)
            .ok_or_else(|| miette!("graph export: walk frontier was empty"))?;
        parse_incident(&cozo.run_script(&script)?.rows)
    })?;
    let missing: Vec<String> = walk
        .ids
        .iter()
        .filter(|id| !by_id.contains_key(*id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        let script = nodes_by_id_script(&missing)
            .ok_or_else(|| miette!("graph export: attribute id set was empty"))?;
        for node in parse_nodes(&cozo.run_script(&script)?.rows)? {
            by_id.insert(node.id.clone(), node);
        }
    }
    let mut nodes = Vec::with_capacity(walk.ids.len());
    for id in &walk.ids {
        let node = by_id.remove(id).ok_or_else(|| {
            miette!("graph export: node {id} is referenced by an edge and is not in the node table")
        })?;
        nodes.push(node);
    }
    let edges = walk
        .edges
        .into_iter()
        .map(|edge| ExportEdge {
            source: edge.source,
            target: edge.target,
            relation: edge.relation,
            confidence: edge.confidence,
            provenance_id: edge.provenance_id,
        })
        .collect();
    Ok((nodes, edges, walk.truncated))
}

fn load_root(cozo: &CozoStorage, entity: &str) -> Result<ExportNode> {
    let escaped = escape_script_literal(entity);
    let script = format!(
        "?[id, label, category, risk_score, metadata] := *node{{id, label, category, risk_score, metadata}}, id == '{escaped}'\n"
    );
    let rows = cozo.run_script(&script)?;
    let row = rows
        .rows
        .first()
        .ok_or_else(|| miette!("entity not in graph: {entity}"))?;
    parse_node(row)
}

pub fn select_id_prefix(mut nodes: Vec<ExportNode>, limit: u64) -> (Vec<ExportNode>, bool) {
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let lim = usize::try_from(limit).unwrap_or(usize::MAX);
    if nodes.len() > lim {
        nodes.truncate(lim);
        (nodes, true)
    } else {
        (nodes, false)
    }
}

pub fn select_walk<F>(
    root: &str,
    depth: u32,
    limit: u64,
    mut incident_of: F,
) -> Result<WalkSelection>
where
    F: FnMut(&[String]) -> Result<Vec<IncidentEdge>>,
{
    let mut kept = vec![root.to_string()];
    let mut kept_set = HashSet::from([root.to_string()]);
    if depth == 0 {
        return Ok(WalkSelection {
            ids: kept,
            edges: Vec::new(),
            truncated: false,
        });
    }
    let mut frontier = vec![root.to_string()];
    let mut edge_map: BTreeMap<(String, String, String), IncidentEdge> = BTreeMap::new();
    let mut truncated = false;
    for _hop in 0..depth {
        if frontier.is_empty() {
            break;
        }
        let incident = incident_of(&frontier)?;
        let hop = admit_hop(&kept_set, &incident, limit);
        for edge in incident {
            let key = (
                edge.source.clone(),
                edge.target.clone(),
                edge.relation.clone(),
            );
            edge_map.entry(key).or_insert(edge);
        }
        for id in &hop.admitted {
            kept_set.insert(id.clone());
            kept.push(id.clone());
        }
        truncated = hop.truncated;
        if truncated {
            break;
        }
        frontier = hop.admitted;
    }
    let edges = edge_map
        .into_values()
        .filter(|edge| kept_set.contains(&edge.source) && kept_set.contains(&edge.target))
        .collect();
    Ok(WalkSelection {
        ids: kept,
        edges,
        truncated,
    })
}

struct HopAdmit {
    admitted: Vec<String>,
    truncated: bool,
}

fn admit_hop(kept: &HashSet<String>, incident: &[IncidentEdge], limit: u64) -> HopAdmit {
    let mut candidates: Vec<(&IncidentEdge, &str)> = Vec::new();
    for edge in incident {
        if let Some(other) = other_new(edge, kept) {
            candidates.push((edge, other));
        }
    }
    candidates.sort_by(|a, b| {
        a.0.relation
            .cmp(&b.0.relation)
            .then(a.1.cmp(b.1))
            .then(a.0.source.cmp(&b.0.source))
            .then(a.0.target.cmp(&b.0.target))
    });
    let mut admitted = Vec::new();
    let mut seen = HashSet::new();
    let mut truncated = false;
    for (_edge, other) in candidates {
        if kept.contains(other) || seen.contains(other) {
            continue;
        }
        if (kept.len() + admitted.len()) as u64 >= limit {
            truncated = true;
            break;
        }
        seen.insert(other.to_string());
        admitted.push(other.to_string());
    }
    HopAdmit {
        admitted,
        truncated,
    }
}

fn other_new<'a>(edge: &'a IncidentEdge, kept: &HashSet<String>) -> Option<&'a str> {
    let source_in = kept.contains(&edge.source);
    let target_in = kept.contains(&edge.target);
    match (source_in, target_in) {
        (true, false) => Some(edge.target.as_str()),
        (false, true) => Some(edge.source.as_str()),
        _ => None,
    }
}

pub(crate) fn both_ends_script(ids: &[String]) -> Option<String> {
    let allowed = allowed_rule(ids)?;
    Some(format!(
        "{allowed}\n?[source, target, relation, confidence, provenance_id] := *edge{{source, target, relation, confidence, provenance_id}}, allowed[source], allowed[target]\n"
    ))
}

fn incident_script(ids: &[String]) -> Option<String> {
    let allowed = allowed_rule(ids)?;
    Some(format!(
        "{allowed}\nincident[source, target, relation, confidence, provenance_id] := *edge{{source, target, relation, confidence, provenance_id}}, allowed[source]\nincident[source, target, relation, confidence, provenance_id] := *edge{{source, target, relation, confidence, provenance_id}}, allowed[target]\n?[source, target, relation, confidence, provenance_id] := incident[source, target, relation, confidence, provenance_id]\n"
    ))
}

fn nodes_by_id_script(ids: &[String]) -> Option<String> {
    let allowed = allowed_rule(ids)?;
    Some(format!(
        "{allowed}\n?[id, label, category, risk_score, metadata] := *node{{id, label, category, risk_score, metadata}}, allowed[id]\n"
    ))
}

/// Cozo single-quoted literals escape `\` and `'` with a backslash
/// (`cozoscript.pest` `s_char`, unescape in `parse/expr.rs`). A doubled
/// quote does not parse on the pinned cozo rev.
pub(crate) fn escape_script_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

fn allowed_rule(ids: &[String]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let mut ordered = ids.to_vec();
    ordered.sort();
    let rows = ordered
        .iter()
        .map(|id| format!("['{}']", escape_script_literal(id)))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("allowed[id] <- [{rows}]"))
}

fn read_count(cozo: &CozoStorage, script: &str, what: &str) -> Result<u64> {
    let rows = cozo.run_script(script)?;
    let cell = rows
        .rows
        .first()
        .and_then(|row| row.first())
        .ok_or_else(|| miette!("graph export: {what} returned no count"))?;
    match cell {
        DataValue::Num(Num::Int(count)) if *count >= 0 => u64::try_from(*count)
            .map_err(|_| miette!("graph export: {what} count {count} does not fit")),
        _ => Err(miette!(
            "graph export: {what} count is not a non-negative integer"
        )),
    }
}

fn parse_nodes<R: AsRef<[DataValue]>>(rows: &[R]) -> Result<Vec<ExportNode>> {
    rows.iter().map(|row| parse_node(row.as_ref())).collect()
}

fn parse_node(row: &[DataValue]) -> Result<ExportNode> {
    if row.len() < 5 {
        return Err(miette!(
            "graph export: node row has {} cells, expected 5",
            row.len()
        ));
    }
    let id = require_str(&row[0], "node id")?;
    Ok(ExportNode {
        label: require_str(&row[1], &format!("label for {id}"))?,
        category: require_str(&row[2], &format!("category for {id}"))?,
        risk_score: require_finite(&row[3], &format!("risk_score for {id}"))?,
        metadata: require_metadata(&row[4], &id)?,
        id,
    })
}

fn parse_edges<R: AsRef<[DataValue]>>(rows: &[R]) -> Result<Vec<ExportEdge>> {
    rows.iter()
        .map(|row| {
            let incident = parse_incident_row(row.as_ref())?;
            Ok(ExportEdge {
                source: incident.source,
                target: incident.target,
                relation: incident.relation,
                confidence: incident.confidence,
                provenance_id: incident.provenance_id,
            })
        })
        .collect()
}

fn parse_incident<R: AsRef<[DataValue]>>(rows: &[R]) -> Result<Vec<IncidentEdge>> {
    rows.iter()
        .map(|row| parse_incident_row(row.as_ref()))
        .collect()
}

fn parse_incident_row(row: &[DataValue]) -> Result<IncidentEdge> {
    if row.len() < 5 {
        return Err(miette!(
            "graph export: edge row has {} cells, expected 5",
            row.len()
        ));
    }
    let source = require_str(&row[0], "edge source")?;
    let target = require_str(&row[1], "edge target")?;
    let relation = require_str(&row[2], &format!("relation for {source} -> {target}"))?;
    let what = format!("confidence for {source} {target} {relation}");
    Ok(IncidentEdge {
        confidence: require_finite(&row[3], &what)?,
        provenance_id: require_str(
            &row[4],
            &format!("provenance_id for {source} {target} {relation}"),
        )?,
        source,
        target,
        relation,
    })
}

fn require_str(cell: &DataValue, what: &str) -> Result<String> {
    match cell {
        DataValue::Str(text) => Ok(text.to_string()),
        _ => Err(miette!("graph export: {what} is not a string")),
    }
}

fn require_finite(cell: &DataValue, what: &str) -> Result<f64> {
    let number = match cell {
        DataValue::Num(Num::Float(value)) => *value,
        DataValue::Num(Num::Int(value)) => *value as f64,
        _ => return Err(miette!("graph export: {what} is not a number")),
    };
    if !number.is_finite() {
        return Err(miette!("graph export: {what} is not a finite number"));
    }
    Ok(number)
}

fn require_metadata(cell: &DataValue, id: &str) -> Result<Option<Value>> {
    match cell {
        DataValue::Null => Ok(None),
        DataValue::Json(value) => {
            let parsed = serde_json::to_value(value)
                .map_err(|err| miette!("graph export: metadata for {id} is not JSON: {err}"))?;
            if parsed.is_null() {
                Ok(None)
            } else {
                Ok(Some(parsed))
            }
        }
        _ => Err(miette!(
            "graph export: metadata for {id} is not JSON or absent"
        )),
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str) -> ExportNode {
        ExportNode {
            id: id.into(),
            label: id.into(),
            category: "file".into(),
            risk_score: 0.0,
            metadata: None,
        }
    }

    fn edge(source: &str, target: &str, relation: &str) -> IncidentEdge {
        IncidentEdge {
            source: source.into(),
            target: target.into(),
            relation: relation.into(),
            confidence: 1.0,
            provenance_id: "p".into(),
        }
    }

    #[test]
    fn select_id_prefix__orders_then_caps() {
        let (kept, truncated) = select_id_prefix(vec![node("c"), node("a"), node("b")], 2);
        assert!(truncated);
        assert_eq!(
            kept.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        let (kept, truncated) = select_id_prefix(vec![node("b"), node("a")], 2);
        assert!(!truncated);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn select_walk__depth_zero_is_root() {
        let mut called = false;
        let walk = select_walk("root", 0, 10, |_| {
            called = true;
            Ok(vec![edge("root", "other", "calls")])
        })
        .unwrap();
        assert!(!called);
        assert_eq!(walk.ids, vec!["root".to_string()]);
        assert!(walk.edges.is_empty());
        assert!(!walk.truncated);
    }

    #[test]
    fn select_walk__neighbor_sort_is_stable() {
        let incident = vec![
            edge("root", "m", "b_rel"),
            edge("root", "z", "a_rel"),
            edge("root", "a", "a_rel"),
        ];
        let walk = select_walk("root", 1, 10, |_| Ok(incident.clone())).unwrap();
        assert_eq!(walk.ids, vec!["root", "a", "z", "m"]);
        assert!(!walk.truncated);
        let capped = select_walk("root", 1, 2, |_| Ok(incident.clone())).unwrap();
        assert_eq!(capped.ids, vec!["root", "a"]);
        assert!(capped.truncated);
        assert!(capped.edges.iter().all(|item| item.target == "a"));
    }

    #[test]
    fn prefix_edges__empty_ids__skips_script() {
        assert!(both_ends_script(&[]).is_none());
        let script = both_ends_script(&["o'hare\\x".into()]).unwrap();
        assert!(script.contains("['o\\'hare\\\\x']"));
        assert!(!script.contains("allowed[id] <- []"));
    }

    #[test]
    fn require_metadata__json_object__round_trips() {
        let cell = DataValue::from(json!({"z": 1, "a": 2}));
        let value = require_metadata(&cell, "n").unwrap().unwrap();
        assert_eq!(value["a"], 2);
        assert_eq!(value["z"], 1);
        assert!(require_metadata(&DataValue::Null, "n").unwrap().is_none());
        assert!(
            require_metadata(&DataValue::from(json!(null)), "n")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parse_incident_row__int_confidence__accepted() {
        let row = vec![
            DataValue::Str("a".into()),
            DataValue::Str("b".into()),
            DataValue::Str("calls".into()),
            DataValue::Num(Num::Int(1)),
            DataValue::Str("p".into()),
        ];
        let edge = parse_incident_row(&row).unwrap();
        assert_eq!(edge.confidence, 1.0);
        assert_eq!(edge.provenance_id, "p");
    }

    #[test]
    fn parse_incident_row__null_provenance__errors() {
        let row = vec![
            DataValue::Str("a".into()),
            DataValue::Str("b".into()),
            DataValue::Str("calls".into()),
            DataValue::Num(Num::Float(0.5)),
            DataValue::Null,
        ];
        let err = parse_incident_row(&row).unwrap_err();
        assert!(err.to_string().contains("provenance_id"));
    }

    #[test]
    fn select_walk__inbound_depth_and_cap__matches_contract() {
        let incident = [
            edge("in", "a", "links"),
            edge("a", "b", "calls"),
            edge("b", "c", "next"),
        ];
        let mut filter = |frontier: &[String]| {
            Ok(incident
                .iter()
                .filter(|item| {
                    frontier
                        .iter()
                        .any(|id| id == &item.source || id == &item.target)
                })
                .cloned()
                .collect())
        };
        let depth_one = select_walk("a", 1, 10, &mut filter).unwrap();
        assert_eq!(depth_one.ids, vec!["a", "b", "in"]);
        assert!(!depth_one.truncated);
        assert!(depth_one.edges.iter().any(|item| item.relation == "links"));
        assert!(depth_one.edges.iter().any(|item| item.relation == "calls"));
        assert!(depth_one.edges.iter().all(|item| item.relation != "next"));

        let capped = select_walk("a", 2, 2, &mut filter).unwrap();
        assert_eq!(capped.ids, vec!["a", "b"]);
        assert!(capped.truncated);
        assert_eq!(capped.edges.len(), 1);
        assert_eq!(capped.edges[0].relation, "calls");
    }
}
