use crate::commands::baseline::{
    BaselineIdentity, BaselineScope, EdgeCoverage, FileEdge, FileEdgeSet, InputCompleteness,
};

use super::config::{ArchitectureConfig, parse_architecture_toml};

pub(super) const COMPONENTS: &str = r#"schema_version = 1

[[components]]
name = "a"
paths = ["src/a/**"]

[[components]]
name = "b"
paths = ["src/b/**"]

[[components]]
name = "c"
paths = ["src/c/**"]
"#;

pub(super) const FORBID_B_TO_A: &str = r#"
[[rules]]
id = "b-to-a"
effect = "forbid"
from = "b"
to = "a"
"#;

pub(super) const INCOMPLETE_REASONS: &[&str] = &[
    "INDEX_STALE",
    "CONTENT_DRIFT",
    "INDEX_EMPTY",
    "INDEX_INDETERMINATE",
    "PARSE_FAILED_FILES",
    "CAPPED_EDGES",
    "SCAN_INCOMPLETE",
];

pub(super) fn fixture_config() -> ArchitectureConfig {
    parse_architecture_toml(&format!("{COMPONENTS}{FORBID_B_TO_A}")).unwrap()
}

pub(super) fn edge_set(edges: &[(&str, &str, u32)]) -> FileEdgeSet {
    FileEdgeSet {
        identity: BaselineIdentity {
            baseline_schema: 1,
            payload_kind: "fileDependencyEdges".to_owned(),
            scope: BaselineScope::repo(),
            parser_version: "1".to_owned(),
            edge_sources: vec!["native".to_owned()],
            ledgerful_version: "test-build".to_owned(),
            index_head_hash: Some("fixture-head".to_owned()),
            input_digest: "blake3:fixture".to_owned(),
            input_completeness: InputCompleteness::complete(),
        },
        coverage: EdgeCoverage {
            edges_resolved_cross_file: edges.len() as u64,
            ..EdgeCoverage::default()
        },
        edges: edges
            .iter()
            .map(|&(source, destination, weight)| FileEdge {
                src_path: source.to_owned(),
                dst_path: destination.to_owned(),
                weight,
            })
            .collect(),
    }
}

pub(super) fn permutations<T>(values: &mut [T], visit: &mut impl FnMut(&[T])) {
    fn choose<T>(values: &mut [T], remaining: usize, visit: &mut impl FnMut(&[T])) {
        if remaining <= 1 {
            visit(values);
            return;
        }
        choose(values, remaining - 1, visit);
        for index in 0..remaining - 1 {
            let swapped = if remaining.is_multiple_of(2) {
                index
            } else {
                0
            };
            values.swap(swapped, remaining - 1);
            choose(values, remaining - 1, visit);
        }
    }

    choose(values, values.len(), visit);
}
