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
        return Ok(());
    }

    println!(
        "{} {}",
        "Graph neighborhood for transaction:".if_supports_color(Stream::Stdout, |s| s.bold()),
        full_id.if_supports_color(Stream::Stdout, |s| s.cyan())
    );

    if args.compact {
        print_compact_bucket("Exact Relations", &filtered.exact);
        print_compact_bucket("Derived Relations", &filtered.derived);
        print_compact_bucket("Heuristic Fallbacks", &filtered.heuristic);
    } else {
        print_full_bucket(
            "Exact Relations",
            Style::new().green().bold(),
            &filtered.exact,
        );
        print_full_bucket(
            "Derived Relations (Transitive / Structural Neighborhood)",
            Style::new().yellow().bold(),
            &filtered.derived,
        );
        print_full_bucket(
            "Heuristic Fallbacks",
            Style::new().red().bold(),
            &filtered.heuristic,
        );
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
    }
}

pub(super) fn compact_bucket_header(name: &str, n: usize) -> String {
    if n <= COMPACT_ROW_CAP {
        format!("{name} ({n})")
    } else {
        format!("{name} (showing {COMPACT_ROW_CAP} of {n})")
    }
}

fn print_compact_bucket(name: &str, rows: &[super::GraphRelation]) {
    println!("\n{}", compact_bucket_header(name, rows.len()));
    if rows.is_empty() {
        println!("  None.");
        return;
    }
    for r in rows.iter().take(COMPACT_ROW_CAP) {
        println!("  {}  {}  {}", r.entity_id, r.label, r.attribution_source);
    }
    if rows.len() > COMPACT_ROW_CAP {
        println!("  … and {} more", rows.len() - COMPACT_ROW_CAP);
    }
}

fn print_full_bucket(title: &str, style: Style, rows: &[super::GraphRelation]) {
    println!(
        "\n{}",
        title.if_supports_color(Stream::Stdout, |s| s.style(style))
    );
    if rows.is_empty() {
        println!("  None.");
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
}
