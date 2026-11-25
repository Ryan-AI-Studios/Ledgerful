use super::*;
use crate::commands::architecture::test_support::permutations;

fn edge(from: &str, to: &str, index: usize) -> CrossEdge {
    CrossEdge {
        src_component: from.to_owned(),
        dst_component: to.to_owned(),
        src_path: format!("{from}/source{index:02}.rs"),
        dst_path: format!("{to}/target.rs"),
        weight: 1,
    }
}

fn cycles(pairs: &[(&str, &str)]) -> Vec<CycleGroup> {
    let edges: Vec<_> = pairs
        .iter()
        .enumerate()
        .map(|(i, (from, to))| edge(from, to, i))
        .collect();
    find_cycle_groups(&build_component_edges(&edges))
}

fn members(groups: Vec<CycleGroup>) -> Vec<Vec<String>> {
    groups.into_iter().map(|group| group.members).collect()
}

#[test]
fn architecture_cycles_acyclic_has_no_groups() {
    assert!(cycles(&[("a", "b"), ("b", "c")]).is_empty());
}

#[test]
fn architecture_cycles_cancellation_discards_partial_scc_partitions() {
    let adjacency = [vec![1], vec![0], vec![3], vec![2]];
    let mut checks = 0;
    let result = Tarjan::new(&adjacency).run(&mut || {
        checks += 1;
        // The first SCC is complete and the second SCC is being traversed.
        if checks == 9 { Err("expired") } else { Ok(()) }
    });
    assert_eq!(result, Err("expired"));
    assert_eq!(checks, 9);
}

#[test]
fn architecture_cycles_two_cycle_one_group_with_witness_each_direction() {
    let result = cycles(&[("a", "b"), ("b", "a")]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].members, ["a", "b"]);
    let edges = &result[0].component_edges;
    assert_eq!(edges.len(), 2);
    assert_eq!((&*edges[0].src, &*edges[0].dst), ("a", "b"));
    assert_eq!((&*edges[1].src, &*edges[1].dst), ("b", "a"));
    assert_eq!(edges[0].witnesses[0].src_path, "a/source00.rs");
    assert_eq!(edges[1].witnesses[0].src_path, "b/source01.rs");
}

#[test]
fn architecture_cycles_three_cycle() {
    assert_eq!(
        members(cycles(&[("a", "b"), ("b", "c"), ("c", "a")])),
        [vec!["a", "b", "c"]]
    );
}

#[test]
fn architecture_cycles_disjoint_groups_sorted_by_members() {
    assert_eq!(
        members(cycles(&[("e", "d"), ("b", "a"), ("d", "e"), ("a", "b")])),
        [vec!["a", "b"], vec!["d", "e"]]
    );
}

#[test]
fn architecture_cycles_self_loop_input_is_not_a_group() {
    assert!(build_component_edges(&[edge("a", "a", 0)]).is_empty());
    let self_loop = ComponentEdge {
        src: "a".to_owned(),
        dst: "a".to_owned(),
        weight: 1,
        witness_count: 0,
        witness_truncated: false,
        witnesses: Vec::new(),
    };
    assert!(find_cycle_groups(&[self_loop]).is_empty());
}

#[test]
fn architecture_cycles_permutations_serialize_identically() {
    let mut edges = [
        edge("a", "b", 1),
        edge("b", "c", 2),
        edge("c", "a", 3),
        edge("b", "a", 4),
        edge("a", "c", 5),
        edge("a", "b", 6),
    ];
    let expected = serde_json::to_vec(&find_cycle_groups(&build_component_edges(&edges))).unwrap();
    let mut count = 0;
    permutations(&mut edges, &mut |order| {
        assert_eq!(
            serde_json::to_vec(&find_cycle_groups(&build_component_edges(order))).unwrap(),
            expected
        );
        count += 1;
    });
    assert_eq!(count, 720);
}

#[test]
fn architecture_cycles_witness_cap_five_with_true_count() {
    let mut edges: Vec<_> = (0..7)
        .rev()
        .map(|index| {
            let mut input = edge("a", "b", index);
            input.weight = index as u32 + 1;
            input
        })
        .collect();
    let result = build_component_edges(&edges);
    assert_eq!(result[0].weight, 28);
    assert_eq!(result[0].witness_count, 7);
    assert!(result[0].witness_truncated);
    assert_eq!(
        result[0]
            .witnesses
            .iter()
            .map(|row| row.weight)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    edges.truncate(5);
    let exact = build_component_edges(&edges);
    assert_eq!(exact[0].witness_count, 5);
    assert!(!exact[0].witness_truncated);
}

#[test]
fn architecture_cycles_witnesses_sorted_bytewise() {
    let mut edges = [edge("a", "b", 0), edge("a", "b", 1), edge("a", "b", 2)];
    edges[0].src_path = "a/Z.rs".to_owned();
    edges[1].src_path = "a/A.rs".to_owned();
    edges[2].src_path = "a/a.rs".to_owned();
    assert_eq!(
        build_component_edges(&edges)[0]
            .witnesses
            .iter()
            .map(|entry| entry.src_path.as_str())
            .collect::<Vec<_>>(),
        ["a/A.rs", "a/Z.rs", "a/a.rs"],
    );
}

#[test]
fn architecture_cycles_internal_edges_only() {
    let result = cycles(&[("a", "b"), ("b", "a"), ("b", "c")]);
    assert_eq!(result[0].members, ["a", "b"]);
    assert_eq!(result[0].component_edges.len(), 2);
}

fn verify_oracle(n: usize, pairs: &[(usize, usize)]) {
    let mut reachable = vec![vec![false; n]; n];
    for &(from, to) in pairs {
        reachable[from][to] = true;
    }
    for through in 0..n {
        let destinations = reachable[through].clone();
        for reachable_from in &mut reachable {
            if reachable_from[through] {
                for (destination, reachable_through) in reachable_from.iter_mut().zip(&destinations)
                {
                    *destination |= *reachable_through;
                }
            }
        }
    }

    let names: Vec<_> = (0..n).map(|node| format!("n{node}")).collect();
    let mut unseen = vec![true; n];
    let mut expected = Vec::new();
    for node in 0..n {
        if !unseen[node] {
            continue;
        }
        let mut group = vec![names[node].clone()];
        for peer in node + 1..n {
            if reachable[node][peer] && reachable[peer][node] {
                unseen[peer] = false;
                group.push(names[peer].clone());
            }
        }
        if group.len() > 1 {
            expected.push(group);
        }
    }

    let graph: Vec<_> = pairs
        .iter()
        .map(|&(from, to)| (names[from].as_str(), names[to].as_str()))
        .collect();
    assert_eq!(members(cycles(&graph)), expected, "{n} nodes and {pairs:?}");
}

#[test]
fn architecture_cycles_matches_closure_oracle_exhaustive_to_4_nodes() {
    for n in 1..=4 {
        for mask in 0..1u32 << (n * n) {
            let pairs: Vec<_> = (0..n * n)
                .filter(|bit| mask & (1 << bit) != 0)
                .map(|bit| (bit / n, bit % n))
                .collect();
            verify_oracle(n, &pairs);
        }
    }
}

#[test]
fn architecture_cycles_matches_closure_oracle_fixed_5_node_set() {
    let ring = [(0, 1), (1, 2), (2, 3), (3, 4), (4, 0)];
    let chain = &ring[..4];
    let disjoint = [(0, 1), (1, 0), (2, 3), (3, 4), (4, 2)];
    let bowtie = [(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 2)];
    let star: Vec<_> = (1..5).flat_map(|node| [(0, node), (node, 0)]).collect();
    let complete: Vec<_> = (0..5)
        .flat_map(|from| {
            (0..5)
                .filter(move |&to| from != to)
                .map(move |to| (from, to))
        })
        .collect();
    for graph in [
        &ring[..],
        chain,
        &disjoint[..],
        &bowtie[..],
        &star,
        &complete,
    ] {
        verify_oracle(5, graph);
    }
}

#[test]
#[allow(non_snake_case)]
fn architecture_cycles_long_ring_does_not_overflow_stack__slow() {
    let names: Vec<_> = (0..50_000).map(|node| format!("n{node:05}")).collect();
    let graph: Vec<_> = (0..names.len())
        .map(|node| {
            (
                names[node].as_str(),
                names[(node + 1) % names.len()].as_str(),
            )
        })
        .collect();
    let groups = cycles(&graph);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].members, names);
}
