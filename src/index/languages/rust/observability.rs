use crate::index::observability::{ErrorHandlingPattern, LogLevel, TelemetryPattern};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

pub fn extract_observability(
    content: &str,
    _symbols: &[Symbol],
) -> Result<(Vec<TelemetryPattern>, Vec<ErrorHandlingPattern>)> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Rust content"))?;

    let mut telemetry = Vec::new();
    let mut errors = Vec::new();
    collect_rust_observability(tree.root_node(), content, &mut telemetry, &mut errors);
    Ok((telemetry, errors))
}

fn collect_rust_observability(
    node: Node,
    content: &str,
    telemetry: &mut Vec<TelemetryPattern>,
    errors: &mut Vec<ErrorHandlingPattern>,
) {
    let kind = node.kind();
    let text = node.utf8_text(content.as_bytes()).unwrap_or("");

    if kind == "call_expression"
        && (text.contains("info!")
            || text.contains("warn!")
            || text.contains("error!")
            || text.contains("debug!")
            || text.contains("trace!"))
    {
        telemetry.push(TelemetryPattern {
            line_start: node.start_position().row as i32 + 1,
            level: Some(LogLevel::Info),
            framework: "tracing".to_string(),
            in_test: false,
            confidence: 1.0,
            evidence: text.chars().take(100).collect(),
        });
    }

    if kind == "macro_invocation" && text.contains("error!") {
        errors.push(ErrorHandlingPattern {
            line_start: node.start_position().row as i32 + 1,
            level: Some(LogLevel::Error),
            framework: "error!".to_string(),
            in_test: false,
            confidence: 1.0,
            evidence: text.chars().take(100).collect(),
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_observability(child, content, telemetry, errors);
    }
}
