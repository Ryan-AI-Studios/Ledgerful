use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "cycles_tests.rs"]
mod tests;

use serde::Serialize;

const MAX_WITNESSES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CrossEdge {
    pub(super) src_component: String,
    pub(super) dst_component: String,
    pub(super) src_path: String,
    pub(super) dst_path: String,
    pub(super) weight: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Witness {
    pub(super) src_path: String,
    pub(super) dst_path: String,
    pub(super) weight: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ComponentEdge {
    pub(super) src: String,
    pub(super) dst: String,
    pub(super) weight: u64,
    pub(super) witness_count: u64,
    pub(super) witness_truncated: bool,
    pub(super) witnesses: Vec<Witness>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CycleGroup {
    pub(super) members: Vec<String>,
    pub(super) component_edges: Vec<ComponentEdge>,
}

pub(super) fn build_component_edges(edges: &[CrossEdge]) -> Vec<ComponentEdge> {
    let mut sorted: Vec<_> = edges
        .iter()
        .filter(|edge| edge.src_component != edge.dst_component)
        .collect();
    sorted.sort_unstable_by(|left, right| {
        (&left.src_path, &left.dst_path, left.weight).cmp(&(
            &right.src_path,
            &right.dst_path,
            right.weight,
        ))
    });

    let mut components = BTreeMap::new();
    for edge in sorted {
        let key = (edge.src_component.as_str(), edge.dst_component.as_str());
        let aggregate = components.entry(key).or_insert_with(|| ComponentEdge {
            src: key.0.to_owned(),
            dst: key.1.to_owned(),
            weight: 0,
            witness_count: 0,
            witness_truncated: false,
            witnesses: Vec::new(),
        });
        aggregate.weight += u64::from(edge.weight);
        aggregate.witness_count += 1;
        if aggregate.witnesses.len() < MAX_WITNESSES {
            aggregate.witnesses.push(Witness {
                src_path: edge.src_path.clone(),
                dst_path: edge.dst_path.clone(),
                weight: edge.weight,
            });
        } else {
            aggregate.witness_truncated = true;
        }
    }
    components.into_values().collect()
}

pub(super) fn find_cycle_groups(edges: &[ComponentEdge]) -> Vec<CycleGroup> {
    let names: Vec<_> = edges
        .iter()
        .flat_map(|edge| [edge.src.as_str(), edge.dst.as_str()])
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let ids: BTreeMap<_, _> = names
        .iter()
        .enumerate()
        .map(|(id, &name)| (name, id))
        .collect();
    let mut adjacency = vec![Vec::new(); names.len()];
    for edge in edges {
        adjacency[ids[edge.src.as_str()]].push(ids[edge.dst.as_str()]);
    }
    for neighbors in &mut adjacency {
        neighbors.sort_unstable();
        neighbors.dedup();
    }

    let mut group_for_node = vec![None; names.len()];
    let mut groups = Vec::new();
    for mut component in Tarjan::new(&adjacency).run() {
        if component.len() < 2 {
            continue;
        }
        component.sort_unstable();
        let group_id = groups.len();
        for &member in &component {
            group_for_node[member] = Some(group_id);
        }
        groups.push(CycleGroup {
            members: component
                .into_iter()
                .map(|member| names[member].to_owned())
                .collect(),
            component_edges: Vec::new(),
        });
    }

    for edge in edges {
        let target_group = group_for_node[ids[edge.dst.as_str()]];
        if let Some(group_id) = group_for_node[ids[edge.src.as_str()]]
            && target_group == Some(group_id)
        {
            groups[group_id].component_edges.push(edge.clone());
        }
    }
    for group in &mut groups {
        group
            .component_edges
            .sort_by(|left, right| (&left.src, &left.dst).cmp(&(&right.src, &right.dst)));
    }
    groups.sort_by(|left, right| left.members.cmp(&right.members));
    groups
}

struct Frame {
    node: usize,
    next_neighbor: usize,
}

struct Tarjan<'a> {
    adjacency: &'a [Vec<usize>],
    indices: Vec<Option<usize>>,
    lowlinks: Vec<usize>,
    on_stack: Vec<bool>,
    active_nodes: Vec<usize>,
    traversal: Vec<Frame>,
    next_index: usize,
    components: Vec<Vec<usize>>,
}

impl<'a> Tarjan<'a> {
    fn new(adjacency: &'a [Vec<usize>]) -> Self {
        let len = adjacency.len();
        Self {
            adjacency,
            indices: vec![None; len],
            lowlinks: vec![0; len],
            on_stack: vec![false; len],
            active_nodes: Vec::new(),
            traversal: Vec::new(),
            next_index: 0,
            components: Vec::new(),
        }
    }

    fn enter(&mut self, node: usize) {
        self.indices[node] = Some(self.next_index);
        self.lowlinks[node] = self.next_index;
        self.next_index += 1;
        self.active_nodes.push(node);
        self.on_stack[node] = true;
        self.traversal.push(Frame {
            node,
            next_neighbor: 0,
        });
    }

    fn run(mut self) -> Vec<Vec<usize>> {
        for root in 0..self.adjacency.len() {
            if self.indices[root].is_some() {
                continue;
            }
            self.enter(root);
            while let Some(frame) = self.traversal.last_mut() {
                let node = frame.node;
                if let Some(&neighbor) = self.adjacency[node].get(frame.next_neighbor) {
                    frame.next_neighbor += 1;
                    match self.indices[neighbor] {
                        None => self.enter(neighbor),
                        Some(index) if self.on_stack[neighbor] => {
                            self.lowlinks[node] = self.lowlinks[node].min(index);
                        }
                        Some(_) => {}
                    }
                    continue;
                }

                self.traversal.pop();
                if let Some(parent) = self.traversal.last() {
                    self.lowlinks[parent.node] =
                        self.lowlinks[parent.node].min(self.lowlinks[node]);
                }
                if self.indices[node] == Some(self.lowlinks[node]) {
                    self.finish_component(node);
                }
            }
        }
        self.components
    }

    fn finish_component(&mut self, root: usize) {
        let mut members = Vec::new();
        while let Some(node) = self.active_nodes.pop() {
            self.on_stack[node] = false;
            members.push(node);
            if node == root {
                break;
            }
        }
        self.components.push(members);
    }
}
