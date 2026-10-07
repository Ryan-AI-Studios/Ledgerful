//! `ledgerful graph path` — directed fewest-hop query on the Cozo edge store.

use cozo::{DataValue, NamedRows, Num};
use miette::{IntoDiagnostic, Result, miette};

use crate::commands::graph_export::escape_script_literal;
use crate::state::storage::StorageManager;
use crate::state::storage_cozo::CozoStorage;

/// Escape `\`, tab, CR, and LF so one hop stays one TSV line.
/// This is not [`escape_script_literal`]: that helper leaves tab raw.
pub(crate) fn escape_tsv_field(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

pub fn execute_graph_path(from: String, to: String, relation: Option<String>) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let cozo = storage
        .cozo()
        .ok_or_else(|| miette!("CozoDB not initialized. Run 'index' first."))?;

    if !node_exists(cozo, &from)? {
        miette::bail!("entity not in graph: {from}");
    }
    if from == to {
        write_stderr(&header(&from, &to, true, Some(0)))?;
        return Ok(());
    }
    if !node_exists(cozo, &to)? {
        miette::bail!("entity not in graph: {to}");
    }

    let script = bfs_script(&from, &to, relation.as_deref());
    let rows = cozo.run_script(&script)?;
    let path = parse_path(&rows)?;
    let Some(nodes) = path else {
        write_stderr(&header(&from, &to, false, None))?;
        return Ok(());
    };
    if nodes.len() < 2 {
        miette::bail!("graph path: shortest path was shorter than one hop");
    }

    let mut lines = Vec::new();
    for (index, pair) in nodes.windows(2).enumerate() {
        let source = &pair[0];
        let target = &pair[1];
        let mut edges = load_hop(cozo, source, target, relation.as_deref())?;
        if edges.is_empty() {
            miette::bail!("graph path: hop from {source} to {target} has no stored edge");
        }
        edges.sort_by(cmp_hop_edge);
        let hop = index + 1;
        for edge in edges {
            lines.push(format_line(hop, source, target, &edge));
        }
    }

    write_stderr(&header(&from, &to, true, Some(nodes.len() - 1)))?;
    write_stdout(&lines.concat())?;
    Ok(())
}

struct HopEdge {
    relation: String,
    confidence_text: String,
    provenance_id: String,
}

/// The `edge` key is `(source, target, relation)`, so one pair cannot store two
/// rows with the same relation. The later keys still define the total order.
fn cmp_hop_edge(left: &HopEdge, right: &HopEdge) -> std::cmp::Ordering {
    left.relation
        .cmp(&right.relation)
        .then(left.provenance_id.cmp(&right.provenance_id))
        .then(left.confidence_text.cmp(&right.confidence_text))
}

fn header(from: &str, to: &str, found: bool, hops: Option<usize>) -> String {
    let path = if found { "yes" } else { "none" };
    let mut out = format!(
        "source: Cozo nodes/edges\nfrom: {from}\nto: {to}\ndirection: source-to-target\npath: {path}\n"
    );
    if let Some(hops) = hops {
        out.push_str(&format!("hops: {hops}\n"));
    }
    out
}

fn format_line(hop: usize, source: &str, target: &str, edge: &HopEdge) -> String {
    format!(
        "{hop}\t{}\t{}\t{}\t{}\t{}\n",
        escape_tsv_field(source),
        escape_tsv_field(&edge.relation),
        escape_tsv_field(target),
        edge.confidence_text,
        escape_tsv_field(&edge.provenance_id),
    )
}

fn node_exists(cozo: &CozoStorage, id: &str) -> Result<bool> {
    let escaped = escape_script_literal(id);
    // `{id: 'literal'}` binds the column to a constant, so a head named `id` is unbound.
    // The stored-key form used by graph export keeps the head variable bound.
    let script = format!("?[id] := *node{{id}}, id == '{escaped}'\n");
    let rows = cozo.run_script(&script)?;
    Ok(!rows.rows.is_empty())
}

fn bfs_script(from: &str, to: &str, relation: Option<&str>) -> String {
    let from_lit = escape_script_literal(from);
    let to_lit = escape_script_literal(to);
    let edges = match relation {
        Some(relation) => {
            let relation_lit = escape_script_literal(relation);
            format!(
                "edges[src, dst] := *edge{{source: src, target: dst, relation: '{relation_lit}'}}"
            )
        }
        None => "edges[src, dst] := *edge{source: src, target: dst}".to_string(),
    };
    format!(
        "{edges}\nstart[] <- [['{from_lit}']]\ngoal[] <- [['{to_lit}']]\n?[fr, to, path] <~ ShortestPathBFS(edges[], start[], goal[])\n"
    )
}

fn hop_script(source: &str, target: &str, relation: Option<&str>) -> String {
    let source_lit = escape_script_literal(source);
    let target_lit = escape_script_literal(target);
    match relation {
        Some(relation) => {
            let relation_lit = escape_script_literal(relation);
            format!(
                "?[relation, confidence, provenance_id] := *edge{{source, target, relation, confidence, provenance_id}}, source == '{source_lit}', target == '{target_lit}', relation == '{relation_lit}'\n"
            )
        }
        None => format!(
            "?[relation, confidence, provenance_id] := *edge{{source, target, relation, confidence, provenance_id}}, source == '{source_lit}', target == '{target_lit}'\n"
        ),
    }
}

fn load_hop(
    cozo: &CozoStorage,
    source: &str,
    target: &str,
    relation: Option<&str>,
) -> Result<Vec<HopEdge>> {
    let rows = cozo.run_script(&hop_script(source, target, relation))?;
    let mut edges = Vec::with_capacity(rows.rows.len());
    for row in rows.rows {
        if row.len() < 3 {
            miette::bail!("graph path: hop from {source} to {target} returned a short row");
        }
        let relation = require_str(&row[0], "relation")?;
        let confidence_text = confidence_text(&row[1], source, target)?;
        let provenance_id = require_str(&row[2], "provenance_id")?;
        edges.push(HopEdge {
            relation,
            confidence_text,
            provenance_id,
        });
    }
    Ok(edges)
}

fn parse_path(rows: &NamedRows) -> Result<Option<Vec<String>>> {
    if rows.rows.len() != 1 {
        let count = rows.rows.len();
        miette::bail!("graph path: shortest path returned {count} rows");
    }
    let row = &rows.rows[0];
    if row.len() < 3 {
        miette::bail!("graph path: shortest path row is not arity 3");
    }
    match &row[2] {
        DataValue::Null => Ok(None),
        DataValue::List(items) => {
            let mut nodes = Vec::with_capacity(items.len());
            for item in items.iter() {
                nodes.push(require_str(item, "path node")?);
            }
            Ok(Some(nodes))
        }
        _ => Err(miette!("graph path: shortest path is not a list")),
    }
}

fn require_str(cell: &DataValue, what: &str) -> Result<String> {
    match cell {
        DataValue::Str(text) => Ok(text.to_string()),
        _ => Err(miette!("graph path: {what} is not a string")),
    }
}

fn confidence_text(cell: &DataValue, source: &str, target: &str) -> Result<String> {
    let number = match cell {
        DataValue::Num(Num::Float(value)) => *value,
        DataValue::Num(Num::Int(value)) => *value as f64,
        _ => {
            return Err(miette!(
                "graph path: confidence for {source} -> {target} is not a number"
            ));
        }
    };
    if !number.is_finite() {
        return Err(miette!(
            "graph path: confidence for {source} -> {target} is not a finite number"
        ));
    }
    serde_json::to_string(&number).map_err(|err| {
        miette!("graph path: confidence for {source} -> {target} number format: {err}")
    })
}

fn write_stdout(text: &str) -> Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    stdout.write_all(text.as_bytes()).into_diagnostic()?;
    stdout.flush().into_diagnostic()?;
    Ok(())
}

fn write_stderr(text: &str) -> Result<()> {
    use std::io::Write;
    let mut stderr = std::io::stderr();
    stderr.write_all(text.as_bytes()).into_diagnostic()?;
    stderr.flush().into_diagnostic()?;
    Ok(())
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    #[test]
    fn escape_tsv_field__controls__single_line() {
        let escaped = escape_tsv_field("a\\b\tc\rd\ne");
        assert_eq!(escaped, "a\\\\b\\tc\\rd\\ne");
        assert!(!escaped.contains('\t'));
        assert!(!escaped.contains('\n'));
        assert!(!escaped.contains('\r'));
    }

    #[test]
    fn bfs_script__quote_and_backslash__cozo_literal() {
        let script = bfs_script("a'b\\c", "z", None);
        assert!(script.contains("a\\'b\\\\c"), "{script}");
        assert!(!script.contains("''"), "{script}");
        assert!(script.contains("edges[src, dst] := *edge{source: src, target: dst}\n"));
        assert!(script.contains("?[fr, to, path] <~ ShortestPathBFS(edges[], start[], goal[])"));
    }

    #[test]
    fn bfs_script__relation__named_literal() {
        let script = bfs_script("a", "b", Some("calls"));
        assert!(
            script
                .contains("edges[src, dst] := *edge{source: src, target: dst, relation: 'calls'}")
        );
    }

    #[test]
    fn cmp_hop_edge__relation_then_provenance_then_confidence() {
        let calls = HopEdge {
            relation: "calls".to_string(),
            confidence_text: "0.5".to_string(),
            provenance_id: "z".to_string(),
        };
        let depends = HopEdge {
            relation: "depends_on".to_string(),
            confidence_text: "0.0".to_string(),
            provenance_id: "a".to_string(),
        };
        assert_eq!(cmp_hop_edge(&calls, &depends), std::cmp::Ordering::Less);

        let provenance_a = HopEdge {
            relation: "calls".to_string(),
            confidence_text: "0.9".to_string(),
            provenance_id: "a".to_string(),
        };
        let provenance_b = HopEdge {
            relation: "calls".to_string(),
            confidence_text: "0.1".to_string(),
            provenance_id: "b".to_string(),
        };
        assert_eq!(
            cmp_hop_edge(&provenance_a, &provenance_b),
            std::cmp::Ordering::Less
        );

        let confidence_low = HopEdge {
            relation: "calls".to_string(),
            confidence_text: "0.25".to_string(),
            provenance_id: "p".to_string(),
        };
        let confidence_high = HopEdge {
            relation: "calls".to_string(),
            confidence_text: "0.5".to_string(),
            provenance_id: "p".to_string(),
        };
        assert_eq!(
            cmp_hop_edge(&confidence_low, &confidence_high),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            cmp_hop_edge(&confidence_low, &confidence_low),
            std::cmp::Ordering::Equal
        );
    }
}
