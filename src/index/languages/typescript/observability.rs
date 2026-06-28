use crate::index::observability::{
    ErrorHandlingPattern, LogLevel, LoggingPattern, TelemetryPattern,
};
use miette::{IntoDiagnostic, Result};
use tree_sitter::Parser;

use super::common::{extract_ts_member_name, extract_ts_object_name};

/// Console method names and their log level mappings.
const CONSOLE_METHODS: &[(&str, LogLevel)] = &[
    ("log", LogLevel::Info),
    ("warn", LogLevel::Warn),
    ("error", LogLevel::Error),
    ("info", LogLevel::Info),
    ("debug", LogLevel::Debug),
];

/// Logger/winston method names and their log level mappings.
const LOGGER_METHODS: &[(&str, LogLevel)] = &[
    ("info", LogLevel::Info),
    ("warn", LogLevel::Warn),
    ("error", LogLevel::Error),
    ("debug", LogLevel::Debug),
];

/// Winston log method mapping (includes "log" -> Info).
const WINSTON_METHODS: &[(&str, LogLevel)] = &[
    ("info", LogLevel::Info),
    ("warn", LogLevel::Warn),
    ("error", LogLevel::Error),
    ("debug", LogLevel::Debug),
    ("log", LogLevel::Info),
];

pub fn extract_logging_patterns(content: &str) -> Result<Vec<LoggingPattern>> {
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content"))?;

    let mut patterns = Vec::new();
    collect_ts_logging_patterns(tree.root_node(), content, &mut patterns);

    // Cap at 1000 patterns per file
    patterns.truncate(1000);
    Ok(patterns)
}

fn collect_ts_logging_patterns(
    node: tree_sitter::Node,
    content: &str,
    patterns: &mut Vec<LoggingPattern>,
) {
    if node.kind() == "call_expression" {
        let callee_node = node.child(0);
        if let Some(callee) = callee_node
            && callee.kind() == "member_expression"
        {
            let obj_name = extract_ts_object_name(callee, content);
            let method_name = extract_ts_member_name(callee, content);

            // Check console.* methods
            if obj_name == "console" {
                for &(method, level) in CONSOLE_METHODS {
                    if method_name == method {
                        let line_start = node.start_position().row as i32 + 1;
                        let in_test = is_in_ts_test(node, content);
                        let evidence = node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
                        let evidence = if evidence.len() > 200 {
                            format!("{}...", &evidence[..197])
                        } else {
                            evidence
                        };

                        patterns.push(LoggingPattern {
                            line_start,
                            level: Some(level),
                            framework: "console".to_string(),
                            in_test,
                            confidence: if in_test { 0.7 } else { 1.0 },
                            evidence,
                        });
                        break;
                    }
                }
            }
            // Check logger.* methods
            else if obj_name == "logger" || obj_name == "Logger" {
                for &(method, level) in LOGGER_METHODS {
                    if method_name == method {
                        let line_start = node.start_position().row as i32 + 1;
                        let in_test = is_in_ts_test(node, content);
                        let evidence = node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
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
            // Check winston.* methods
            else if obj_name == "winston" {
                for &(method, level) in WINSTON_METHODS {
                    if method_name == method {
                        let line_start = node.start_position().row as i32 + 1;
                        let in_test = is_in_ts_test(node, content);
                        let evidence = node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
                        let evidence = if evidence.len() > 200 {
                            format!("{}...", &evidence[..197])
                        } else {
                            evidence
                        };

                        patterns.push(LoggingPattern {
                            line_start,
                            level: Some(level),
                            framework: "winston".to_string(),
                            in_test,
                            confidence: if in_test { 0.7 } else { 1.0 },
                            evidence,
                        });
                        break;
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ts_logging_patterns(child, content, patterns);
    }
}

/// Walk up the tree to check if the node is inside a test block
/// (describe, it, or test call).
fn is_in_ts_test(node: tree_sitter::Node, content: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "call_expression" {
            // Check if the callee is describe, it, or test
            if let Some(callee) = parent.child(0) {
                let callee_text = callee.utf8_text(content.as_bytes()).unwrap_or("");
                if callee_text.starts_with("describe")
                    || callee_text.starts_with("it(")
                    || callee_text.starts_with("test(")
                    || callee_text == "describe"
                    || callee_text == "it"
                    || callee_text == "test"
                {
                    return true;
                }
                // Also handle member expressions like describe.skip, it.only
                if callee.kind() == "member_expression" {
                    let obj = extract_ts_object_name(callee, content);
                    if obj == "describe" || obj == "it" || obj == "test" {
                        return true;
                    }
                }
            }
        }
        current = parent.parent();
    }
    false
}

pub fn extract_error_handling(content: &str) -> Result<Vec<ErrorHandlingPattern>> {
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content"))?;

    let mut patterns = Vec::new();
    collect_ts_error_handling(tree.root_node(), content, &mut patterns);

    // Cap at 1000 patterns per file
    patterns.truncate(1000);
    Ok(patterns)
}

fn collect_ts_error_handling(
    node: tree_sitter::Node,
    content: &str,
    patterns: &mut Vec<ErrorHandlingPattern>,
) {
    let kind = node.kind();

    match kind {
        "try_statement" => {
            // try/catch/finally blocks
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_ts_test(node, content);
            patterns.push(ErrorHandlingPattern {
                line_start,
                level: Some(LogLevel::Info),
                framework: "try_catch".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: "syntactic: try/catch block".to_string(),
            });
        }
        "throw_statement" => {
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_ts_test(node, content);
            patterns.push(ErrorHandlingPattern {
                line_start,
                level: Some(LogLevel::Warn),
                framework: "throw".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: "syntactic: throw statement".to_string(),
            });
        }
        "call_expression" => {
            // Check for .catch() calls and Promise.reject
            let callee_node = node.child(0);
            if let Some(callee) = callee_node
                && callee.kind() == "member_expression"
            {
                let method_name = extract_ts_member_name(callee, content);
                if method_name == "catch" {
                    let line_start = node.start_position().row as i32 + 1;
                    let in_test = is_in_ts_test(node, content);
                    patterns.push(ErrorHandlingPattern {
                        line_start,
                        level: Some(LogLevel::Info),
                        framework: "promise_catch".to_string(),
                        in_test,
                        confidence: if in_test { 0.7 } else { 1.0 },
                        evidence: "syntactic: .catch() call".to_string(),
                    });
                }
                // Check for Promise.reject
                let obj_name = extract_ts_object_name(callee, content);
                if obj_name == "Promise" && method_name == "reject" {
                    let line_start = node.start_position().row as i32 + 1;
                    let in_test = is_in_ts_test(node, content);
                    patterns.push(ErrorHandlingPattern {
                        line_start,
                        level: Some(LogLevel::Warn),
                        framework: "promise_reject".to_string(),
                        in_test,
                        confidence: if in_test { 0.7 } else { 1.0 },
                        evidence: "syntactic: Promise.reject".to_string(),
                    });
                }
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ts_error_handling(child, content, patterns);
    }
}

pub fn extract_telemetry_patterns(content: &str) -> Result<Vec<TelemetryPattern>> {
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content"))?;

    let mut patterns = Vec::new();
    collect_ts_telemetry_patterns(tree.root_node(), content, &mut patterns);

    // Also do line-based heuristic matching for telemetry.* patterns
    for (line_idx, line) in content.lines().enumerate() {
        let line_lower = line.to_ascii_lowercase();
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
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
                    in_test: is_in_ts_test_from_line(line),
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

/// Simple line-level heuristic to detect if a line is inside a test block.
fn is_in_ts_test_from_line(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    lower.contains("describe(")
        || lower.contains("it(")
        || lower.contains("test(")
        || lower.contains("describe.skip(")
        || lower.contains("it.skip(")
}

fn collect_ts_telemetry_patterns(
    node: tree_sitter::Node,
    content: &str,
    patterns: &mut Vec<TelemetryPattern>,
) {
    let kind = node.kind();

    // Check for @Trace() decorator
    if kind == "decorator" {
        let decorator_text = node.utf8_text(content.as_bytes()).unwrap_or("");
        if decorator_text.contains("@Trace") || decorator_text.contains("@trace") {
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_ts_test(node, content);
            patterns.push(TelemetryPattern {
                line_start,
                level: Some(LogLevel::Trace),
                framework: "opentelemetry".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: "decorator: @Trace()".to_string(),
            });
        }
    }

    // Check call expressions for opentelemetry imports and prom-client usage
    if kind == "call_expression" {
        let callee_node = node.child(0);
        if let Some(callee) = callee_node {
            let call_text = callee.utf8_text(content.as_bytes()).unwrap_or("");

            // Check for new Counter/Histogram/Gauge from prom-client
            if call_text.contains("Counter")
                || call_text.contains("Histogram")
                || call_text.contains("Gauge")
                || call_text.contains("Summary")
            {
                // Only match if it looks like prom-client usage (new expression or member access)
                let full_text = node.utf8_text(content.as_bytes()).unwrap_or("");
                if full_text.contains("prom") || full_text.contains("Prom") {
                    let line_start = node.start_position().row as i32 + 1;
                    let in_test = is_in_ts_test(node, content);
                    patterns.push(TelemetryPattern {
                        line_start,
                        level: Some(LogLevel::Trace),
                        framework: "prom-client".to_string(),
                        in_test,
                        confidence: if in_test { 0.7 } else { 1.0 },
                        evidence: format!("call: {}", truncate_str(full_text, 200)),
                    });
                }
            }
        }
    }

    // Check import statements for opentelemetry
    if kind == "import_statement" {
        let import_text = node.utf8_text(content.as_bytes()).unwrap_or("");
        if import_text.contains("opentelemetry") || import_text.contains("@opentelemetry") {
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_ts_test(node, content);
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

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ts_telemetry_patterns(child, content, patterns);
    }
}

/// Truncate a string to a maximum length, adding "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len.saturating_sub(3)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::observability::LogLevel;

    #[test]
    fn test_extract_logging_patterns_console() {
        let content = r#"
            function main() {
                console.log("hello");
                console.warn("warning");
                console.error("error");
                console.info("info");
                console.debug("debug");
            }
        "#;

        let patterns = extract_logging_patterns(content).unwrap();
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "console" && p.level == Some(LogLevel::Info))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "console" && p.level == Some(LogLevel::Warn))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "console" && p.level == Some(LogLevel::Error))
        );
    }

    #[test]
    fn test_extract_logging_patterns_logger() {
        let content = r#"
            function handleRequest() {
                logger.info("request received");
                logger.error("request failed");
            }
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
    fn test_extract_logging_patterns_winston() {
        let content = r#"
            function processItem() {
                winston.info("processing");
                winston.log("general log");
            }
        "#;

        let patterns = extract_logging_patterns(content).unwrap();
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "winston" && p.level == Some(LogLevel::Info))
        );
        assert!(patterns.iter().any(|p| p.framework == "winston"
            && p.level == Some(LogLevel::Info)
            && p.evidence.contains("winston.log")));
    }

    #[test]
    fn test_extract_logging_patterns_in_test() {
        let content = r#"
            describe("my suite", () => {
                it("should work", () => {
                    console.log("test output");
                });
            });
        "#;

        let patterns = extract_logging_patterns(content).unwrap();
        let test_pattern = patterns
            .iter()
            .find(|p| p.framework == "console")
            .expect("should find console pattern");
        assert!(test_pattern.in_test);
    }

    #[test]
    fn test_extract_error_handling_try_catch_and_throw() {
        let content = r#"
            function handleData() {
                try {
                    const result = fetchData();
                    return result;
                } catch (e) {
                    throw new Error("failed");
                }
            }
        "#;

        let patterns = extract_error_handling(content).unwrap();
        let try_catch = patterns
            .iter()
            .find(|p| p.framework == "try_catch")
            .expect("should find try_catch pattern");
        assert_eq!(try_catch.level, Some(LogLevel::Info));
        assert!(!try_catch.in_test);
        assert_eq!(try_catch.evidence, "syntactic: try/catch block");

        let throw_pattern = patterns
            .iter()
            .find(|p| p.framework == "throw")
            .expect("should find throw pattern");
        assert_eq!(throw_pattern.level, Some(LogLevel::Warn));
        assert_eq!(throw_pattern.evidence, "syntactic: throw statement");
    }

    #[test]
    fn test_extract_error_handling_in_test() {
        let content = r#"
            describe("error handling", () => {
                it("should catch errors", () => {
                    try {
                        doSomething();
                    } catch (e) {
                        expect(e).toBeDefined();
                    }
                });
            });

            function normalFn() {
                throw new Error("bad");
            }
        "#;

        let patterns = extract_error_handling(content).unwrap();
        let test_try = patterns
            .iter()
            .find(|p| p.framework == "try_catch" && p.in_test)
            .expect("should find try_catch in test");
        assert!((test_try.confidence - 0.7).abs() < f64::EPSILON);

        let normal_throw = patterns
            .iter()
            .find(|p| p.framework == "throw" && !p.in_test)
            .expect("should find throw not in test");
        assert!((normal_throw.confidence - 1.0).abs() < f64::EPSILON);
    }
}
