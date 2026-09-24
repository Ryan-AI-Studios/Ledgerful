use super::assemble::LedgerGraphData;
use super::{GraphLayer, LedgerGraphArgs};
use crate::output::table::Table;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use std::collections::HashSet;

const COMPACT_ROW_CAP: usize = 5;

pub(super) fn print_ledger_graph(
    args: &LedgerGraphArgs,
    full_id: &str,
    data: &LedgerGraphData,
) -> Result<()> {
    if args.compact && args.json {
        miette::bail!("--compact cannot be combined with --json");
    }

    let filtered = apply_layer_filter(data, &args.layer);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&filtered).into_diagnostic()?
        );
    } else {
        println!(
            "{} {}",
            "Graph neighborhood for transaction:".if_supports_color(Stream::Stdout, |s| s.bold()),
            full_id.if_supports_color(Stream::Stdout, |s| s.cyan())
        );

        let selected = selected_layers(&args.layer);
        let exact_assembled_empty = data.exact.is_empty();
        if args.compact {
            print_compact_bucket(
                "Exact Relations",
                GraphLayer::Exact,
                selected.contains(&GraphLayer::Exact),
                exact_assembled_empty,
                &data.entity,
                &filtered.exact,
            );
            print_compact_bucket(
                "Derived Relations",
                GraphLayer::Derived,
                selected.contains(&GraphLayer::Derived),
                exact_assembled_empty,
                &data.entity,
                &filtered.derived,
            );
            print_compact_bucket(
                "Heuristic Fallbacks",
                GraphLayer::Heuristic,
                selected.contains(&GraphLayer::Heuristic),
                exact_assembled_empty,
                &data.entity,
                &filtered.heuristic,
            );
        } else {
            print_full_bucket(
                "Exact Relations",
                Style::new().green().bold(),
                GraphLayer::Exact,
                selected.contains(&GraphLayer::Exact),
                exact_assembled_empty,
                &data.entity,
                &filtered.exact,
            );
            print_full_bucket(
                "Derived Relations (Transitive / Structural Neighborhood)",
                Style::new().yellow().bold(),
                GraphLayer::Derived,
                selected.contains(&GraphLayer::Derived),
                exact_assembled_empty,
                &data.entity,
                &filtered.derived,
            );
            print_full_bucket(
                "Heuristic Fallbacks",
                Style::new().red().bold(),
                GraphLayer::Heuristic,
                selected.contains(&GraphLayer::Heuristic),
                exact_assembled_empty,
                &data.entity,
                &filtered.heuristic,
            );
        }
    }

    if data.completeness.is_some() {
        eprintln!("truncated: neighborhood capped at maxDepth 2 / maxNodes 150");
    }

    Ok(())
}

fn selected_layers(layers: &[GraphLayer]) -> HashSet<GraphLayer> {
    if layers.is_empty() {
        return HashSet::from([
            GraphLayer::Exact,
            GraphLayer::Derived,
            GraphLayer::Heuristic,
        ]);
    }
    let mut seen = HashSet::new();
    let mut out = HashSet::new();
    for layer in layers {
        if seen.insert(*layer) {
            out.insert(*layer);
        }
    }
    out
}

fn apply_layer_filter(data: &LedgerGraphData, layers: &[GraphLayer]) -> LedgerGraphData {
    let selected = selected_layers(layers);
    LedgerGraphData {
        exact: if selected.contains(&GraphLayer::Exact) {
            data.exact.clone()
        } else {
            Vec::new()
        },
        derived: if selected.contains(&GraphLayer::Derived) {
            data.derived.clone()
        } else {
            Vec::new()
        },
        heuristic: if selected.contains(&GraphLayer::Heuristic) {
            data.heuristic.clone()
        } else {
            Vec::new()
        },
        completeness: data.completeness.clone(),
        entity: data.entity.clone(),
    }
}

pub(super) fn format_empty_bucket_human(
    layer: GraphLayer,
    exact_assembled_empty: bool,
    entity: &str,
) -> String {
    let reason = match layer {
        GraphLayer::Exact => {
            "  no exact token_provenance, changed_files, ledger_link, or knowledge_graph edges."
        }
        GraphLayer::Derived => "  no further Cozo hops from the exact/heuristic seeds.",
        GraphLayer::Heuristic if exact_assembled_empty => "  no legacy fallback edges.",
        GraphLayer::Heuristic => "  heuristic not computed (fallback only when exact is empty).",
    };
    format!("{reason}\n  Next: ledgerful ledger audit {entity}")
}

fn empty_bucket_text(
    layer: GraphLayer,
    selected: bool,
    exact_assembled_empty: bool,
    entity: &str,
) -> String {
    if selected {
        format_empty_bucket_human(layer, exact_assembled_empty, entity)
    } else {
        "  None.".to_string()
    }
}

fn print_empty_bucket_or_none(
    layer: GraphLayer,
    selected: bool,
    exact_assembled_empty: bool,
    entity: &str,
) {
    println!(
        "{}",
        empty_bucket_text(layer, selected, exact_assembled_empty, entity)
    );
}

pub(super) fn compact_bucket_header(name: &str, n: usize) -> String {
    if n <= COMPACT_ROW_CAP {
        format!("{name} ({n})")
    } else {
        format!("{name} (showing {COMPACT_ROW_CAP} of {n})")
    }
}

fn print_compact_bucket(
    name: &str,
    layer: GraphLayer,
    selected: bool,
    exact_assembled_empty: bool,
    entity: &str,
    rows: &[super::GraphRelation],
) {
    println!("\n{}", compact_bucket_header(name, rows.len()));
    if rows.is_empty() {
        print_empty_bucket_or_none(layer, selected, exact_assembled_empty, entity);
        return;
    }
    for r in rows.iter().take(COMPACT_ROW_CAP) {
        println!("  {}  {}  {}", r.entity_id, r.label, r.attribution_source);
    }
    if rows.len() > COMPACT_ROW_CAP {
        println!("  … and {} more", rows.len() - COMPACT_ROW_CAP);
    }
}

fn print_full_bucket(
    title: &str,
    style: Style,
    layer: GraphLayer,
    selected: bool,
    exact_assembled_empty: bool,
    entity: &str,
    rows: &[super::GraphRelation],
) {
    println!(
        "\n{}",
        title.if_supports_color(Stream::Stdout, |s| s.style(style))
    );
    if rows.is_empty() {
        print_empty_bucket_or_none(layer, selected, exact_assembled_empty, entity);
        return;
    }
    let mut table = Table::new();
    table.set_header(vec![
        "Entity ID",
        "Label",
        "Category",
        "Relation",
        "Attribution Source",
    ]);
    for r in rows {
        table.add_row(vec![
            r.entity_id.clone(),
            r.label.clone(),
            r.category.clone(),
            r.relation.clone(),
            r.attribution_source.clone(),
        ]);
    }
    println!("{}", table);
}

#[cfg(test)]
mod tests {
    use super::super::assemble::GraphCompleteness;
    use super::*;
    use crate::commands::ledger_graph::GraphRelation;

    fn rel(label: &str) -> GraphRelation {
        GraphRelation {
            entity_id: format!("urn:{label}"),
            label: label.to_string(),
            category: "file".to_string(),
            relation: "modified".to_string(),
            exactness: "exact".to_string(),
            attribution_source: "changed_files".to_string(),
        }
    }

    #[test]
    fn graph_compact_prints_bucket_counts() {
        assert_eq!(
            compact_bucket_header("Exact Relations", 3),
            "Exact Relations (3)"
        );
        assert_eq!(
            compact_bucket_header("Derived Relations", 12),
            "Derived Relations (showing 5 of 12)"
        );
        assert_eq!(
            compact_bucket_header("Heuristic Fallbacks", 0),
            "Heuristic Fallbacks (0)"
        );
    }

    #[test]
    fn graph_layer_exact_empties_other_buckets() {
        let data = LedgerGraphData {
            exact: vec![rel("a")],
            derived: vec![rel("b")],
            heuristic: vec![rel("c")],
            completeness: None,
            entity: "slug".to_string(),
        };
        let filtered = apply_layer_filter(&data, &[GraphLayer::Exact]);
        assert_eq!(filtered.exact.len(), 1);
        assert!(filtered.derived.is_empty());
        assert!(filtered.heuristic.is_empty());
        assert!(filtered.completeness.is_none());
    }

    #[test]
    fn graph_layer_repeat_dedupes() {
        let data = LedgerGraphData {
            exact: vec![rel("a")],
            derived: vec![rel("b")],
            heuristic: vec![rel("c")],
            completeness: Some(GraphCompleteness {
                stop: "cap",
                max_depth: 2,
                max_nodes: 150,
            }),
            entity: "slug".to_string(),
        };
        let filtered = apply_layer_filter(
            &data,
            &[GraphLayer::Exact, GraphLayer::Exact, GraphLayer::Derived],
        );
        assert_eq!(filtered.exact.len(), 1);
        assert_eq!(filtered.derived.len(), 1);
        assert!(filtered.heuristic.is_empty());
        assert!(filtered.completeness.is_some());
    }

    #[test]
    fn print_ledger_graph_wires_cap_stderr_without_early_return() {
        let src = include_str!("print.rs");
        let start = src
            .find("fn print_ledger_graph(")
            .expect("print_ledger_graph must exist");
        let after = &src[start..];
        let next_fn = after
            .find("\nfn selected_layers")
            .expect("selected_layers must follow print_ledger_graph");
        let body = &after[..next_fn];
        assert!(
            body.contains(
                "eprintln!(\"truncated: neighborhood capped at maxDepth 2 / maxNodes 150\")"
            ),
            "print_ledger_graph must write the cap stderr line: {body}"
        );
        assert!(
            !body.contains("return Ok"),
            "print_ledger_graph must not return before the cap stderr: {body}"
        );
    }

    #[test]
    fn graph_empty_bucket_exact_names_four_sources() {
        let out = format_empty_bucket_human(GraphLayer::Exact, true, "slug");
        assert!(out.contains("token_provenance"), "{out}");
        assert!(out.contains("changed_files"), "{out}");
        assert!(out.contains("ledger_link"), "{out}");
        assert!(out.contains("knowledge_graph"), "{out}");
        assert!(out.contains("Next: ledgerful ledger audit slug"), "{out}");
    }

    #[test]
    fn graph_empty_bucket_derived_names_hops() {
        let out = format_empty_bucket_human(GraphLayer::Derived, false, "slug");
        assert!(
            out.contains("no further Cozo hops from the exact/heuristic seeds"),
            "{out}"
        );
        assert!(out.contains("Next: ledgerful ledger audit slug"), "{out}");
    }

    #[test]
    fn graph_empty_bucket_heuristic_branches_on_exact() {
        let empty = format_empty_bucket_human(GraphLayer::Heuristic, true, "slug");
        assert!(empty.contains("no legacy fallback edges"), "{empty}");
        let populated = format_empty_bucket_human(GraphLayer::Heuristic, false, "slug");
        assert!(
            populated.contains("heuristic not computed (fallback only when exact is empty)"),
            "{populated}"
        );
        assert!(
            !populated.contains("no legacy fallback edges"),
            "{populated}"
        );
    }

    #[test]
    fn graph_layer_exact_hidden_buckets_stay_none() {
        let derived = empty_bucket_text(GraphLayer::Derived, false, true, "slug");
        let heuristic = empty_bucket_text(GraphLayer::Heuristic, false, false, "slug");
        assert_eq!(derived, "  None.");
        assert_eq!(heuristic, "  None.");
        assert!(!derived.contains("Cozo hops"), "{derived}");
        assert!(!heuristic.contains("heuristic not computed"), "{heuristic}");
        let shown = empty_bucket_text(GraphLayer::Exact, true, true, "slug");
        assert!(shown.contains("token_provenance"), "{shown}");
    }

    #[test]
    fn graph_json_omits_entity_and_keeps_three_arrays() {
        let data = LedgerGraphData {
            exact: vec![],
            derived: vec![],
            heuristic: vec![],
            completeness: None,
            entity: "slug".to_string(),
        };
        let value = serde_json::to_value(&data).expect("serialize");
        let obj = value.as_object().expect("object");
        let keys: Vec<&String> = obj.keys().collect();
        assert_eq!(keys, ["exact", "derived", "heuristic"]);
        assert!(!obj.contains_key("entity"));
        assert!(!obj.contains_key("completeness"));
        assert!(!obj.contains_key("schemaVersion"));
    }
}
