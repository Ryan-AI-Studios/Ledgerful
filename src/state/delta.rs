use crate::state::storage_cozo::CozoStorage;
use cozo::{DataValue, Num};
use miette::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VizNode {
    pub id: String,
    pub label: String,
    pub category: String,
    pub risk_score: f64,
    pub community: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VizEdge {
    pub from: String,
    pub to: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub nodes: HashMap<String, VizNode>,
    pub edges: HashSet<VizEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphDelta {
    pub added_nodes: Vec<VizNode>,
    pub removed_nodes: Vec<String>,
    pub updated_nodes: Vec<VizNode>,
    pub added_edges: Vec<VizEdge>,
    pub removed_edges: Vec<VizEdge>,
}

impl GraphSnapshot {
    pub fn from_cozo(cozo: &CozoStorage) -> Result<Self> {
        let mut communities = HashMap::new();
        if let Ok(comms) = cozo.run_community_louvain() {
            for (node, comm) in comms {
                communities.insert(node, comm);
            }
        }

        let nodes_res = cozo.run_script(
            "?[id, label, category, risk_score] := *node{id, label, category, risk_score}",
        )?;
        let mut nodes = HashMap::new();
        for row in nodes_res.rows {
            let id = match row.first() {
                Some(DataValue::Str(s)) => s.to_string(),
                _ => continue,
            };
            let label = match row.get(1) {
                Some(DataValue::Str(s)) => s.to_string(),
                _ => continue,
            };
            let category = match row.get(2) {
                Some(DataValue::Str(s)) => s.to_string(),
                _ => continue,
            };
            let risk_score = match row.get(3) {
                Some(DataValue::Num(Num::Float(f))) => *f,
                Some(DataValue::Num(Num::Int(i))) => *i as f64,
                _ => continue,
            };
            let community = communities.get(&id).copied().unwrap_or(0);
            nodes.insert(
                id.clone(),
                VizNode {
                    id,
                    label,
                    category,
                    risk_score,
                    community,
                },
            );
        }

        let edges_res =
            cozo.run_script("?[source, target, relation] := *edge{source, target, relation}")?;
        let mut edges = HashSet::new();
        for row in edges_res.rows {
            let from = match row.first() {
                Some(DataValue::Str(s)) => s.to_string(),
                _ => continue,
            };
            let to = match row.get(1) {
                Some(DataValue::Str(s)) => s.to_string(),
                _ => continue,
            };
            let label = match row.get(2) {
                Some(DataValue::Str(s)) => s.to_string(),
                _ => continue,
            };
            edges.insert(VizEdge { from, to, label });
        }

        Ok(GraphSnapshot { nodes, edges })
    }

    pub fn diff(&self, other: &Self) -> GraphDelta {
        let mut added_nodes = Vec::new();
        let mut updated_nodes = Vec::new();

        for (id, node) in &other.nodes {
            match self.nodes.get(id) {
                None => added_nodes.push(node.clone()),
                Some(old) if old != node => updated_nodes.push(node.clone()),
                _ => {}
            }
        }

        let mut removed_nodes = Vec::new();
        for id in self.nodes.keys() {
            if !other.nodes.contains_key(id) {
                removed_nodes.push(id.clone());
            }
        }

        let added_edges: Vec<VizEdge> = other.edges.difference(&self.edges).cloned().collect();
        let removed_edges: Vec<VizEdge> = self.edges.difference(&other.edges).cloned().collect();

        // Sort for determinism
        added_nodes.sort_by(|a, b| a.id.cmp(&b.id));
        removed_nodes.sort();
        updated_nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let mut added_edges = added_edges;
        let mut removed_edges = removed_edges;
        added_edges.sort_by(|a, b| (&a.from, &a.to, &a.label).cmp(&(&b.from, &b.to, &b.label)));
        removed_edges.sort_by(|a, b| (&a.from, &a.to, &a.label).cmp(&(&b.from, &b.to, &b.label)));

        GraphDelta {
            added_nodes,
            removed_nodes,
            updated_nodes,
            added_edges,
            removed_edges,
        }
    }
}

impl GraphDelta {
    pub fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.updated_nodes.is_empty()
            && self.added_edges.is_empty()
            && self.removed_edges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, risk: f64, community: i64) -> VizNode {
        VizNode {
            id: id.to_string(),
            label: id.to_string(),
            category: "test".to_string(),
            risk_score: risk,
            community,
        }
    }

    fn make_edge(from: &str, to: &str, label: &str) -> VizEdge {
        VizEdge {
            from: from.to_string(),
            to: to.to_string(),
            label: label.to_string(),
        }
    }

    #[test]
    fn test_empty_diff() {
        let s1 = GraphSnapshot {
            nodes: HashMap::new(),
            edges: HashSet::new(),
        };
        let s2 = GraphSnapshot {
            nodes: HashMap::new(),
            edges: HashSet::new(),
        };
        let delta = s1.diff(&s2);
        assert!(delta.is_empty());
    }

    #[test]
    fn test_node_addition_and_removal() {
        let mut nodes1 = HashMap::new();
        nodes1.insert("a".to_string(), make_node("a", 0.1, 0));
        let s1 = GraphSnapshot {
            nodes: nodes1,
            edges: HashSet::new(),
        };

        let mut nodes2 = HashMap::new();
        nodes2.insert("b".to_string(), make_node("b", 0.2, 1));
        let s2 = GraphSnapshot {
            nodes: nodes2,
            edges: HashSet::new(),
        };

        let delta = s1.diff(&s2);
        assert_eq!(delta.added_nodes.len(), 1);
        assert_eq!(delta.added_nodes[0].id, "b");
        assert_eq!(delta.removed_nodes.len(), 1);
        assert_eq!(delta.removed_nodes[0], "a");
        assert!(delta.updated_nodes.is_empty());
    }

    #[test]
    fn test_node_update() {
        let mut nodes1 = HashMap::new();
        nodes1.insert("a".to_string(), make_node("a", 0.1, 0));
        let s1 = GraphSnapshot {
            nodes: nodes1,
            edges: HashSet::new(),
        };

        let mut nodes2 = HashMap::new();
        nodes2.insert("a".to_string(), make_node("a", 0.9, 0));
        let s2 = GraphSnapshot {
            nodes: nodes2,
            edges: HashSet::new(),
        };

        let delta = s1.diff(&s2);
        assert!(delta.added_nodes.is_empty());
        assert!(delta.removed_nodes.is_empty());
        assert_eq!(delta.updated_nodes.len(), 1);
        assert_eq!(delta.updated_nodes[0].risk_score, 0.9);
    }

    #[test]
    fn test_edge_addition_and_removal() {
        let s1 = GraphSnapshot {
            nodes: HashMap::new(),
            edges: [make_edge("a", "b", "calls")].into_iter().collect(),
        };
        let s2 = GraphSnapshot {
            nodes: HashMap::new(),
            edges: [make_edge("b", "c", "calls")].into_iter().collect(),
        };

        let delta = s1.diff(&s2);
        assert_eq!(delta.added_edges.len(), 1);
        assert_eq!(delta.added_edges[0].from, "b");
        assert_eq!(delta.removed_edges.len(), 1);
        assert_eq!(delta.removed_edges[0].from, "a");
    }
}
