use crate::index::observability::{
    ErrorHandlingPattern, LogLevel, LoggingPattern, TelemetryPattern,
};
use miette::{IntoDiagnostic, Result};
use tree_sitter::Parser;

/// Python logging method names and their level mappings.
const PY_LOGGING_METHODS: &[(&str, LogLevel)] = &[
    ("info", LogLevel::Info),
    ("warning", LogLevel::Warn),
    ("error", LogLevel::Error),
    ("debug", LogLevel::Debug),
    ("critical", LogLevel::Error),
];

pub fn extract_logging_patterns(content: &str) -> Result<Vec<LoggingPattern>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let mut patterns = Vec::new();
    collect_py_logging_patterns(tree.root_node(), content, &mut patterns);

    // Cap at 1000 patterns per file
    patterns.truncate(1000);
    Ok(patterns)
}

fn collect_py_logging_patterns(
    node: tree_sitter::Node,
    content: &str,
    patterns: &mut Vec<LoggingPattern>,
) {
    if node.kind() == "call" {
        let callee_node = node.child(0);
        if let Some(callee) = callee_node {
            match callee.kind() {
                "attribute" => {
                    // Handle logging.info(...), logger.warning(...), etc.
                    let obj_name = extract_py_attribute_object(callee, content);
                    let method_name = super::common::extract_py_attribute_name(callee, content);

                    // logging.* and logger.* methods
                    if obj_name == "logging" {
                        for &(method, level) in PY_LOGGING_METHODS {
                            if method_name == method {
                                let line_start = node.start_position().row as i32 + 1;
                                let in_test = is_in_py_test(node, content);
                                let evidence =
                                    node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
                                let evidence = if evidence.len() > 200 {
                                    format!("{}...", &evidence[..197])
                                } else {
                                    evidence
                                };

                                patterns.push(LoggingPattern {
                                    line_start,
                                    level: Some(level),
                                    framework: "logging".to_string(),
                                    in_test,
                                    confidence: if in_test { 0.7 } else { 1.0 },
                                    evidence,
                                });
                                break;
                            }
                        }
                    } else if obj_name == "logger" {
                        for &(method, level) in PY_LOGGING_METHODS {
                            if method_name == method {
                                let line_start = node.start_position().row as i32 + 1;
                                let in_test = is_in_py_test(node, content);
                                let evidence =
                                    node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
                                let evidence = if evidence.len() > 200 {
                                    format!("{}...", &evidence[..197])
                                } else {
                                    evidence
                                };

                                patterns.push(LoggingPattern {
                                    line_start,
                                    level: Some(level),
                                    framework: "logger".to_string(),
                                    in_test,
                                    confidence: if in_test { 0.7 } else { 1.0 },
                                    evidence,
                                });
                                break;
                            }
                        }
                    }
                }
                "identifier" => {
                    // Handle print() calls
                    let name = callee
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if name == "print" {
                        let line_start = node.start_position().row as i32 + 1;
                        let in_test = is_in_py_test(node, content);
                        let evidence = node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
                        let evidence = if evidence.len() > 200 {
                            format!("{}...", &evidence[..197])
                        } else {
                            evidence
                        };

                        patterns.push(LoggingPattern {
                            line_start,
                            level: Some(LogLevel::Info),
                            framework: "print".to_string(),
                            in_test,
                            confidence: if in_test { 0.5 } else { 0.8 },
                            evidence,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_py_logging_patterns(child, content, patterns);
    }
}

/// Extract the object name from a Python attribute node (e.g. logging.info -> "logging").
fn extract_py_attribute_object(node: tree_sitter::Node, content: &str) -> String {
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
    // The first child is the object (identifier or nested attribute)
    if let Some(first) = children.first() {
        if first.kind() == "identifier" {
            return first
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .to_string();
        }
        if first.kind() == "attribute" {
            // Nested attribute like self.logger - take the last identifier
            return extract_py_attribute_object(*first, content);
        }
    }
    String::new()
}

/// Walk up the tree to check if the node is inside a function starting with test_.
fn is_in_py_test(node: tree_sitter::Node, content: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_definition" {
            // Check if the function name starts with "test_"
            if let Some(name_node) = parent.child_by_field_name("name") {
                let name = name_node.utf8_text(content.as_bytes()).unwrap_or("");
                if name.starts_with("test_") {
                    return true;
                }
            }
        }
        current = parent.parent();
    }
    false
}

pub fn extract_error_handling(content: &str) -> Result<Vec<ErrorHandlingPattern>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let mut patterns = Vec::new();
    collect_py_error_handling(tree.root_node(), content, &mut patterns);

    // Cap at 1000 patterns per file
    patterns.truncate(1000);
    Ok(patterns)
}

fn collect_py_error_handling(
    node: tree_sitter::Node,
    content: &str,
    patterns: &mut Vec<ErrorHandlingPattern>,
) {
    let kind = node.kind();

    match kind {
        "try_statement" => {
            // try/except/finally blocks
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_py_test(node, content);
            patterns.push(ErrorHandlingPattern {
                line_start,
                level: Some(LogLevel::Info),
                framework: "try_except".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: "syntactic: try/except block".to_string(),
            });
        }
        "raise_statement" => {
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_py_test(node, content);
            patterns.push(ErrorHandlingPattern {
                line_start,
                level: Some(LogLevel::Warn),
                framework: "raise".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: "syntactic: raise statement".to_string(),
            });
        }
        "assert_statement" => {
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_py_test(node, content);
            patterns.push(ErrorHandlingPattern {
                line_start,
                level: Some(LogLevel::Info),
                framework: "assert".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: "syntactic: assert statement".to_string(),
            });
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_py_error_handling(child, content, patterns);
    }
}

pub fn extract_telemetry_patterns(content: &str) -> Result<Vec<TelemetryPattern>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let mut patterns = Vec::new();
    collect_py_telemetry_patterns(tree.root_node(), content, &mut patterns);

    // Also do line-based heuristic matching for telemetry.* patterns
    for (line_idx, line) in content.lines().enumerate() {
        let line_lower = line.to_ascii_lowercase();
        let trimmed = line.trim_start();
        if trimmed.starts_with("#") {
            continue;
        }
        if line_lower.contains("telemetry") || line_lower.contains("monitoring") {
            let line_start = (line_idx + 1) as i32;
            let already_matched = patterns.iter().any(|p| p.line_start == line_start);
            if !already_matched {
                patterns.push(TelemetryPattern {
                    line_start,
                    level: Some(LogLevel::Trace),
                    framework: "custom".to_string(),
                    in_test: line.trim().starts_with("def test_"),
                    confidence: 0.7,
                    evidence: "heuristic: telemetry.* pattern match".to_string(),
                });
            }
        }
    }

    // Cap at 1000 patterns per file
    patterns.truncate(1000);
    Ok(patterns)
}

fn collect_py_telemetry_patterns(
    node: tree_sitter::Node,
    content: &str,
    patterns: &mut Vec<TelemetryPattern>,
) {
    let kind = node.kind();

    // Check for @tracer.start_as_current_span / @tracer.start_span decorators
    if kind == "decorator" {
        let decorator_text = node.utf8_text(content.as_bytes()).unwrap_or("");
        if decorator_text.contains("start_as_current_span") || decorator_text.contains("start_span")
        {
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_py_test(node, content);
            patterns.push(TelemetryPattern {
                line_start,
                level: Some(LogLevel::Trace),
                framework: "opentelemetry".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: "decorator: tracer span".to_string(),
            });
        }
    }

    // Check for import statements with opentelemetry
    if kind == "import_statement" || kind == "import_from_statement" {
        let import_text = node.utf8_text(content.as_bytes()).unwrap_or("");
        if import_text.contains("opentelemetry") {
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_py_test(node, content);
            patterns.push(TelemetryPattern {
                line_start,
                level: Some(LogLevel::Trace),
                framework: "opentelemetry".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: "import: opentelemetry".to_string(),
            });
        }
    }

    // Check for prometheus_client.Counter/Gauge/Histogram/Summary usage
    if kind == "call" {
        let callee_node = node.child(0);
        if let Some(callee) = callee_node
            && callee.kind() == "attribute"
        {
            let obj_name = extract_py_attribute_object(callee, content);
            let method_name = super::common::extract_py_attribute_name(callee, content);

            // prometheus_client.Counter/Gauge/Histogram/Summary
            if obj_name == "Counter"
                || obj_name == "Gauge"
                || obj_name == "Histogram"
                || obj_name == "Summary"
            {
                let line_start = node.start_position().row as i32 + 1;
                let in_test = is_in_py_test(node, content);
                patterns.push(TelemetryPattern {
                    line_start,
                    level: Some(LogLevel::Trace),
                    framework: "prometheus_client".to_string(),
                    in_test,
                    confidence: if in_test { 0.7 } else { 1.0 },
                    evidence: format!("call: {}()", obj_name),
                });
            }

            // tracer.start_as_current_span / tracer.start_span calls
            if (obj_name == "tracer" || obj_name.starts_with("tracer."))
                && (method_name == "start_as_current_span" || method_name == "start_span")
            {
                let line_start = node.start_position().row as i32 + 1;
                let in_test = is_in_py_test(node, content);
                patterns.push(TelemetryPattern {
                    line_start,
                    level: Some(LogLevel::Trace),
                    framework: "opentelemetry".to_string(),
                    in_test,
                    confidence: if in_test { 0.7 } else { 1.0 },
                    evidence: format!("call: {}.{}()", obj_name, method_name),
                });
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_py_telemetry_patterns(child, content, patterns);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::observability::LogLevel;

    #[test]
    fn test_extract_logging_patterns_logging() {
        let content = r#"
import logging

def handle_request():
    logging.info("request received")
    logging.warning("slow request")
    logging.error("request failed")
    logging.debug("debug details")
    logging.critical("critical failure")
"#;

        let patterns = extract_logging_patterns(content).unwrap();
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "logging" && p.level == Some(LogLevel::Info))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "logging" && p.level == Some(LogLevel::Warn))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "logging" && p.level == Some(LogLevel::Error))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "logging" && p.level == Some(LogLevel::Debug))
        );
        // critical maps to Error
        assert!(patterns.iter().any(|p| p.framework == "logging"
            && p.level == Some(LogLevel::Error)
            && p.evidence.contains("critical")));
    }

    #[test]
    fn test_extract_logging_patterns_logger() {
        let content = r#"
import logging

logger = logging.getLogger(__name__)

def process():
    logger.info("processing")
    logger.error("processing failed")
"#;

        let patterns = extract_logging_patterns(content).unwrap();
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "logger" && p.level == Some(LogLevel::Info))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "logger" && p.level == Some(LogLevel::Error))
        );
    }

    #[test]
    fn test_extract_logging_patterns_print() {
        let content = r#"
def main():
    print("hello world")
"#;

        let patterns = extract_logging_patterns(content).unwrap();
        let print_pat = patterns
            .iter()
            .find(|p| p.framework == "print")
            .expect("should find print pattern");
        assert_eq!(print_pat.level, Some(LogLevel::Info));
        assert!(!print_pat.in_test);
    }

    #[test]
    fn test_extract_logging_patterns_in_test() {
        let content = r#"
import logging

logger = logging.getLogger(__name__)

def test_something():
    logging.info("test log")
    logger.warning("test warning")
"#;

        let patterns = extract_logging_patterns(content).unwrap();
        for p in &patterns {
            assert!(
                p.in_test,
                "patterns inside test_ functions should be in_test"
            );
        }
    }

    #[test]
    fn test_extract_error_handling_try_except_and_raise() {
        let content = r#"
def handle_data():
    try:
        result = fetch_data()
        return result
    except ValueError as e:
        raise RuntimeError("bad data")
"#;

        let patterns = extract_error_handling(content).unwrap();
        let try_except = patterns
            .iter()
            .find(|p| p.framework == "try_except")
            .expect("should find try_except pattern");
        assert_eq!(try_except.level, Some(LogLevel::Info));
        assert!(!try_except.in_test);
        assert_eq!(try_except.evidence, "syntactic: try/except block");

        let raise_pattern = patterns
            .iter()
            .find(|p| p.framework == "raise")
            .expect("should find raise pattern");
        assert_eq!(raise_pattern.level, Some(LogLevel::Warn));
        assert_eq!(raise_pattern.evidence, "syntactic: raise statement");
    }

    #[test]
    fn test_extract_error_handling_in_test() {
        let content = r#"
def test_error_handling():
    try:
        do_something()
    except Exception:
        pass
    assert result == 42

def normal_fn():
    try:
        do_work()
    except ValueError:
        raise
"#;

        let patterns = extract_error_handling(content).unwrap();
        let test_try = patterns
            .iter()
            .find(|p| p.framework == "try_except" && p.in_test)
            .expect("should find try_except in test");
        assert!((test_try.confidence - 0.7).abs() < f64::EPSILON);

        let test_assert = patterns
            .iter()
            .find(|p| p.framework == "assert" && p.in_test)
            .expect("should find assert in test");
        assert!((test_assert.confidence - 0.7).abs() < f64::EPSILON);

        let normal_try = patterns
            .iter()
            .find(|p| p.framework == "try_except" && !p.in_test)
            .expect("should find try_except not in test");
        assert!((normal_try.confidence - 1.0).abs() < f64::EPSILON);
    }
}
