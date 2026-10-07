//! Pure GraphML and Cypher renderers. No Cozo import.

use super::escape::{cypher_label, cypher_rel_type, cypher_string, xml_text};
use miette::{Result, miette};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Graphml,
    Cypher,
}

impl ExportFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Graphml => "graphml",
            Self::Cypher => "cypher",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportNode {
    pub id: String,
    pub label: String,
    pub category: String,
    pub risk_score: f64,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportEdge {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: f64,
    pub provenance_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportGraph {
    pub nodes: Vec<ExportNode>,
    pub edges: Vec<ExportEdge>,
    pub limit: u64,
    pub truncated: bool,
    pub store_nodes: u64,
    pub store_edges: u64,
}

#[derive(Debug)]
pub struct Rendered {
    pub body: String,
    pub illegal_xml_replacements: u64,
}

pub fn render(graph: &ExportGraph, format: ExportFormat) -> Result<Rendered> {
    match format {
        ExportFormat::Graphml => render_graphml(graph),
        ExportFormat::Cypher => render_cypher(graph),
    }
}

fn yn(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

fn fmt_finite(n: f64, what: &str) -> Result<String> {
    if !n.is_finite() {
        return Err(miette!("graph export: {what} is not a finite number"));
    }
    serde_json::to_string(&n).map_err(|err| miette!("graph export: {what} number format: {err}"))
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut sorted = serde_json::Map::new();
            for (key, child) in entries {
                sorted.insert(key.clone(), sort_json(child));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_json).collect()),
        other => other.clone(),
    }
}

fn metadata_text(value: &Value) -> Result<String> {
    let sorted = sort_json(value);
    serde_json::to_string(&sorted)
        .map_err(|err| miette!("graph export: metadata is not JSON text: {err}"))
}

fn render_graphml(graph: &ExportGraph) -> Result<Rendered> {
    let mut replaced = 0u64;
    let mut body = String::new();
    body.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    body.push_str("<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"http://graphml.graphdrawing.org/xmlns http://graphml.graphdrawing.org/xmlns/1.0/graphml.xsd\">\n");
    for (id, for_attr, name, typ) in GRAPHML_KEYS {
        body.push_str(&format!(
            "  <key id=\"{id}\" for=\"{for_attr}\" attr.name=\"{name}\" attr.type=\"{typ}\"/>\n"
        ));
    }
    body.push_str("  <graph id=\"G\" edgedefault=\"directed\">\n");
    push_xml_data(
        &mut body,
        "truncated",
        yn(graph.truncated),
        4,
        &mut replaced,
    );
    push_xml_data(
        &mut body,
        "limit",
        &graph.limit.to_string(),
        4,
        &mut replaced,
    );
    push_xml_data(
        &mut body,
        "emitted_nodes",
        &graph.nodes.len().to_string(),
        4,
        &mut replaced,
    );
    push_xml_data(
        &mut body,
        "emitted_edges",
        &graph.edges.len().to_string(),
        4,
        &mut replaced,
    );
    push_xml_data(
        &mut body,
        "store_nodes",
        &graph.store_nodes.to_string(),
        4,
        &mut replaced,
    );
    push_xml_data(
        &mut body,
        "store_edges",
        &graph.store_edges.to_string(),
        4,
        &mut replaced,
    );

    let mut synthetic = std::collections::BTreeMap::new();
    for (index, node) in graph.nodes.iter().enumerate() {
        let element_id = format!("n{}", index + 1);
        synthetic.insert(node.id.as_str(), element_id);
    }
    for node in &graph.nodes {
        let element_id = synthetic
            .get(node.id.as_str())
            .ok_or_else(|| miette!("graph export: missing synthetic id for {}", node.id))?;
        body.push_str(&format!("    <node id=\"{element_id}\">\n"));
        push_xml_data(&mut body, "node_id", &node.id, 6, &mut replaced);
        push_xml_data(&mut body, "label", &node.label, 6, &mut replaced);
        push_xml_data(&mut body, "category", &node.category, 6, &mut replaced);
        let risk = fmt_finite(node.risk_score, &format!("risk_score for {}", node.id))?;
        push_xml_data(&mut body, "risk_score", &risk, 6, &mut replaced);
        if let Some(metadata) = metadata_present(&node.metadata) {
            let text = metadata_text(metadata)?;
            push_xml_data(&mut body, "metadata", &text, 6, &mut replaced);
        }
        body.push_str("    </node>\n");
    }
    for (index, edge) in graph.edges.iter().enumerate() {
        let source = synthetic.get(edge.source.as_str()).ok_or_else(|| {
            miette!(
                "graph export: edge source {} is not an emitted node",
                edge.source
            )
        })?;
        let target = synthetic.get(edge.target.as_str()).ok_or_else(|| {
            miette!(
                "graph export: edge target {} is not an emitted node",
                edge.target
            )
        })?;
        let element_id = format!("e{}", index + 1);
        body.push_str(&format!(
            "    <edge id=\"{element_id}\" source=\"{source}\" target=\"{target}\">\n"
        ));
        push_xml_data(&mut body, "relation", &edge.relation, 6, &mut replaced);
        let confidence = fmt_finite(
            edge.confidence,
            &format!(
                "confidence for {} {} {}",
                edge.source, edge.target, edge.relation
            ),
        )?;
        push_xml_data(&mut body, "confidence", &confidence, 6, &mut replaced);
        push_xml_data(
            &mut body,
            "provenance_id",
            &edge.provenance_id,
            6,
            &mut replaced,
        );
        body.push_str("    </edge>\n");
    }
    body.push_str("  </graph>\n");
    body.push_str("</graphml>\n");
    Ok(Rendered {
        body,
        illegal_xml_replacements: replaced,
    })
}

const GRAPHML_KEYS: &[(&str, &str, &str, &str)] = &[
    ("node_id", "node", "id", "string"),
    ("label", "node", "label", "string"),
    ("category", "node", "category", "string"),
    ("risk_score", "node", "risk_score", "double"),
    ("metadata", "node", "metadata", "string"),
    ("relation", "edge", "relation", "string"),
    ("confidence", "edge", "confidence", "double"),
    ("provenance_id", "edge", "provenance_id", "string"),
    ("truncated", "graph", "truncated", "string"),
    ("limit", "graph", "limit", "int"),
    ("emitted_nodes", "graph", "emitted_nodes", "int"),
    ("emitted_edges", "graph", "emitted_edges", "int"),
    ("store_nodes", "graph", "store_nodes", "int"),
    ("store_edges", "graph", "store_edges", "int"),
];

fn push_xml_data(body: &mut String, key: &str, text: &str, indent: usize, replaced: &mut u64) {
    let escaped = xml_text(text, replaced);
    for _ in 0..indent {
        body.push(' ');
    }
    body.push_str(&format!("<data key=\"{key}\">{escaped}</data>\n"));
}

fn metadata_present(metadata: &Option<Value>) -> Option<&Value> {
    match metadata {
        Some(value) if !value.is_null() => Some(value),
        _ => None,
    }
}

fn render_cypher(graph: &ExportGraph) -> Result<Rendered> {
    let mut body = String::new();
    body.push_str(&format!(
        "// ledgerful graph export format=cypher limit={} truncated={} emitted_nodes={} emitted_edges={} store_nodes={} store_edges={}\n",
        graph.limit,
        yn(graph.truncated),
        graph.nodes.len(),
        graph.edges.len(),
        graph.store_nodes,
        graph.store_edges,
    ));
    let mut labels = std::collections::BTreeMap::new();
    for node in &graph.nodes {
        let _ = fmt_finite(node.risk_score, &format!("risk_score for {}", node.id))?;
        labels.insert(node.id.as_str(), cypher_label(&node.category));
    }
    for node in &graph.nodes {
        let label = labels
            .get(node.id.as_str())
            .ok_or_else(|| miette!("graph export: missing label for {}", node.id))?;
        let risk = fmt_finite(node.risk_score, &format!("risk_score for {}", node.id))?;
        body.push_str(&format!(
            "MERGE (n:{label} {{id: {}}})\n",
            cypher_string(&node.id)
        ));
        body.push_str(&format!(
            "SET n.label = {}, n.category = {}, n.risk_score = {risk}",
            cypher_string(&node.label),
            cypher_string(&node.category),
        ));
        if let Some(metadata) = metadata_present(&node.metadata) {
            let text = metadata_text(metadata)?;
            body.push_str(&format!(", n.metadata = {}", cypher_string(&text)));
        }
        body.push_str(";\n");
    }
    for edge in &graph.edges {
        let source_label = labels.get(edge.source.as_str()).ok_or_else(|| {
            miette!(
                "graph export: edge source {} is not an emitted node",
                edge.source
            )
        })?;
        let target_label = labels.get(edge.target.as_str()).ok_or_else(|| {
            miette!(
                "graph export: edge target {} is not an emitted node",
                edge.target
            )
        })?;
        let confidence = fmt_finite(
            edge.confidence,
            &format!(
                "confidence for {} {} {}",
                edge.source, edge.target, edge.relation
            ),
        )?;
        let rel = cypher_rel_type(&edge.relation);
        body.push_str(&format!(
            "MERGE (s:{source_label} {{id: {}}})\n",
            cypher_string(&edge.source)
        ));
        body.push_str(&format!(
            "MERGE (t:{target_label} {{id: {}}})\n",
            cypher_string(&edge.target)
        ));
        body.push_str(&format!("MERGE (s)-[r:{rel}]->(t)\n"));
        body.push_str(&format!(
            "SET r.confidence = {confidence}, r.provenance_id = {}, r.relation = {};\n",
            cypher_string(&edge.provenance_id),
            cypher_string(&edge.relation),
        ));
    }
    Ok(Rendered {
        body,
        illegal_xml_replacements: 0,
    })
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_graph() -> ExportGraph {
        ExportGraph {
            nodes: vec![ExportNode {
                id: "urn:file/a".into(),
                label: "a<b&\"'/\\\n\u{0001}".into(),
                category: "file".into(),
                risk_score: 0.0,
                metadata: Some(json!({"b": 1, "a": {"d": 1, "c": 2}})),
            }],
            edges: vec![ExportEdge {
                source: "urn:file/a".into(),
                target: "urn:file/a".into(),
                relation: "calls".into(),
                confidence: 0.5,
                provenance_id: "prov".into(),
            }],
            limit: 1000,
            truncated: false,
            store_nodes: 1,
            store_edges: 1,
        }
    }

    fn assert_balanced(xml: &str) {
        let mut stack: Vec<String> = Vec::new();
        let mut rest = xml;
        while let Some(start) = rest.find('<') {
            rest = &rest[start..];
            if let Some(stripped) = rest.strip_prefix("<?") {
                let end = stripped.find("?>").expect("xml declaration closes");
                rest = &stripped[end + 2..];
                continue;
            }
            let end = rest.find('>').expect("tag closes");
            let inside = &rest[1..end];
            if let Some(name) = inside.strip_prefix('/') {
                let name = name.split_whitespace().next().expect("close name");
                let open = stack.pop().expect("matching open tag");
                assert_eq!(open, name);
            } else if !inside.ends_with('/') {
                let name = inside.split_whitespace().next().expect("open name");
                stack.push(name.to_string());
            }
            rest = &rest[end + 1..];
        }
        assert!(stack.is_empty(), "unclosed tags: {stack:?}");
    }

    #[test]
    fn graphml_render__awkward_text__escapes_and_keeps_stored_id() {
        let rendered = render(&sample_graph(), ExportFormat::Graphml).unwrap();
        assert!(rendered.body.contains("edgedefault=\"directed\""));
        assert!(
            rendered
                .body
                .contains("xmlns=\"http://graphml.graphdrawing.org/xmlns\"")
        );
        assert!(rendered.body.contains("<node id=\"n1\">"));
        assert!(
            rendered
                .body
                .contains("<edge id=\"e1\" source=\"n1\" target=\"n1\">")
        );
        assert!(!rendered.body.contains("id=\"urn:file/a\""));
        assert!(
            rendered
                .body
                .contains("<data key=\"node_id\">urn:file/a</data>")
        );
        assert!(rendered.body.contains("&lt;"));
        assert!(rendered.body.contains("&amp;"));
        assert!(rendered.body.contains("&quot;"));
        assert!(rendered.body.contains("a&lt;b&amp;&quot;'/\\"));
        assert!(
            rendered
                .body
                .contains("{&quot;a&quot;:{&quot;c&quot;:2,&quot;d&quot;:1},&quot;b&quot;:1}")
        );
        assert!(!rendered.body.contains('\r'));
        assert_balanced(&rendered.body);
        assert_eq!(fmt_finite(0.0, "zero").unwrap(), "0.0");
        assert_eq!(fmt_finite(0.5, "half").unwrap(), "0.5");
        assert_eq!(fmt_finite(1.0, "one").unwrap(), "1.0");
    }

    #[test]
    fn graphml_render__control_char__replaces_and_counts() {
        let mut graph = sample_graph();
        graph.nodes[0].label = "x\u{0001}y\u{FFFE}z\u{FFFF}".into();
        graph.edges.clear();
        let rendered = render(&graph, ExportFormat::Graphml).unwrap();
        assert_eq!(rendered.illegal_xml_replacements, 3);
        assert_eq!(rendered.body.matches('\u{FFFD}').count(), 3);
        assert!(!rendered.body.contains('\u{0001}'));
        assert!(!rendered.body.contains('\u{FFFE}'));
        assert!(!rendered.body.contains('\u{FFFF}'));
        assert_balanced(&rendered.body);
    }

    #[test]
    fn graphml_render__same_rows_twice__byte_identical() {
        let graph = sample_graph();
        let a = render(&graph, ExportFormat::Graphml).unwrap().body;
        let b = render(&graph, ExportFormat::Graphml).unwrap().body;
        assert_eq!(a, b);
    }

    #[test]
    fn graphml_render__over_cap__data_truncated_yes() {
        let mut graph = sample_graph();
        graph.truncated = true;
        graph.limit = 1;
        graph.store_nodes = 4;
        let body = render(&graph, ExportFormat::Graphml).unwrap().body;
        assert!(body.contains("<data key=\"truncated\">yes</data>"));
        assert!(body.contains("<data key=\"limit\">1</data>"));
        assert!(body.contains("<data key=\"store_nodes\">4</data>"));
    }

    #[test]
    fn graphml_render__non_finite__errors() {
        let mut graph = sample_graph();
        graph.nodes[0].risk_score = f64::NAN;
        let err = render(&graph, ExportFormat::Graphml).unwrap_err();
        assert!(err.to_string().contains("not a finite number"));
    }

    #[test]
    fn cypher_render__quote_backslash_newline__escaped() {
        let mut graph = sample_graph();
        graph.nodes[0].label = "a'b\\c\n\u{0001}".into();
        graph.edges.clear();
        graph.nodes[0].metadata = None;
        let body = render(&graph, ExportFormat::Cypher).unwrap().body;
        assert!(body.contains("n.label = 'a\\'b\\\\c\\n\\u0001'"));
        assert!(body.contains("n.risk_score = 0.0"));
        assert!(!body.contains("n.metadata"));
    }

    #[test]
    fn cypher_render__unsafe_category__label_node() {
        let mut graph = sample_graph();
        graph.nodes[0].category = "code-graph".into();
        graph.edges.clear();
        let body = render(&graph, ExportFormat::Cypher).unwrap().body;
        assert!(body.contains("MERGE (n:Node {id:"));
        assert!(body.contains("n.category = 'code-graph'"));
        assert!(!body.contains(":code-graph"));
    }

    #[test]
    fn cypher_render__header_matches_counts() {
        let body = render(&sample_graph(), ExportFormat::Cypher).unwrap().body;
        let first = body.lines().next().unwrap();
        assert_eq!(
            first,
            "// ledgerful graph export format=cypher limit=1000 truncated=no emitted_nodes=1 emitted_edges=1 store_nodes=1 store_edges=1"
        );
    }

    #[test]
    fn cypher_render__edge_statements__lookups_endpoint_labels_and_escapes_relation() {
        let mut graph = sample_graph();
        graph.nodes[0].category = "code-graph".into();
        graph.edges[0].relation = "calls".into();
        let body = render(&graph, ExportFormat::Cypher).unwrap().body;
        assert!(body.contains("MERGE (s:Node {id:"));
        assert!(body.contains("MERGE (t:Node {id:"));
        assert!(body.contains("MERGE (s)-[r:calls]->(t)"));
        assert!(body.contains("r.relation = 'calls'"));
        assert!(!body.contains(":code-graph"));
    }

    #[test]
    fn cypher_render__edge_unsafe_relation__backtick_escaped() {
        let mut graph = sample_graph();
        graph.edges[0].relation = "a`b".into();
        let body = render(&graph, ExportFormat::Cypher).unwrap().body;
        assert!(body.contains("MERGE (s)-[r:`a``b`]->(t)"));
        assert!(body.contains("r.relation = 'a`b'"));
    }

    #[test]
    fn cypher_render__self_edge__emits_valid_statements() {
        let body = render(&sample_graph(), ExportFormat::Cypher).unwrap().body;
        let merges = body.matches("MERGE ").count();
        assert!(
            merges >= 4,
            "node merge plus three edge merges, got {merges}"
        );
        assert!(body.contains("MERGE (s:file {id: 'urn:file/a'})"));
        assert!(body.contains("MERGE (t:file {id: 'urn:file/a'})"));
        assert!(body.contains("MERGE (s)-[r:calls]->(t)"));
        assert!(body.contains("r.confidence = 0.5"));
    }
}
