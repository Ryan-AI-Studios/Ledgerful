use super::common::{
    collect_imports, extract_selector_field, extract_selector_operand, is_in_go_test, node_text,
    truncate_evidence,
};
use crate::index::observability::{
    ErrorHandlingPattern, LogLevel, LoggingPattern, TelemetryPattern,
};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

const SLOG_METHODS: &[(&str, LogLevel)] = &[
    ("Info", LogLevel::Info),
    ("Error", LogLevel::Error),
    ("Warn", LogLevel::Warn),
    ("Debug", LogLevel::Debug),
    // slog also has InfoContext etc.; match prefix-free exact names first
    ("Default", LogLevel::Info),
];

pub fn extract_logging_patterns(content: &str) -> Result<Vec<LoggingPattern>> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Go content"))?;

    let imports = collect_imports(tree.root_node(), content);
    let has_zap = imports
        .iter()
        .any(|(_, path)| path.contains("go.uber.org/zap") || path.ends_with("/zap"));
    let has_zerolog = imports.iter().any(|(_, path)| path.contains("zerolog"));

    let mut patterns = Vec::new();
    collect_go_logging(
        tree.root_node(),
        content,
        has_zap,
        has_zerolog,
        &mut patterns,
    );
    patterns.truncate(1000);
    // Deterministic order for stable index output
    patterns.sort_by(|a, b| {
        a.line_start
            .cmp(&b.line_start)
            .then(a.framework.cmp(&b.framework))
            .then(a.evidence.cmp(&b.evidence))
    });
    Ok(patterns)
}

fn collect_go_logging(
    node: Node,
    content: &str,
    has_zap: bool,
    has_zerolog: bool,
    patterns: &mut Vec<LoggingPattern>,
) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && function.kind() == "selector_expression"
    {
        let field = extract_selector_field(function, content);
        let operand = extract_selector_operand(function, content);

        // slog.Info / slog.Error / ... or logger.Info when operand is slog.Default() chain
        // Primary: package slog methods
        let is_slog_pkg = operand == "slog" || operand.ends_with(".slog") || operand == "log/slog";

        // Also slog.Default().Info style: operand may be call_expression text — handled if
        // nested selector like slog.Default().Info is two nested selectors; field=Info,
        // operand may be "slog.Default()" as full text.
        let operand_is_slog_default =
            operand.contains("slog.Default") || operand.starts_with("slog.");

        if is_slog_pkg || operand_is_slog_default {
            for &(method, level) in SLOG_METHODS {
                if field == method && method != "Default" {
                    let line_start = node.start_position().row as i32 + 1;
                    let in_test = is_in_go_test(node, content);
                    let evidence = truncate_evidence(&node_text(node, content), 200);
                    patterns.push(LoggingPattern {
                        line_start,
                        level: Some(level),
                        framework: "slog".to_string(),
                        in_test,
                        confidence: if in_test { 0.7 } else { 1.0 },
                        evidence,
                    });
                    break;
                }
            }
        }

        // Secondary: zap/zerolog only when import path confirms the framework (avoids noise).
        if field == "Info" || field == "Error" || field == "Warn" || field == "Debug" {
            let level = match field.as_str() {
                "Info" => LogLevel::Info,
                "Error" => LogLevel::Error,
                "Warn" => LogLevel::Warn,
                "Debug" => LogLevel::Debug,
                _ => LogLevel::Info,
            };
            if has_zap
                && (operand == "zap"
                    || operand.starts_with("zap.")
                    || operand.contains("zap.L()")
                    || operand.contains("zap.S()"))
            {
                let line_start = node.start_position().row as i32 + 1;
                let in_test = is_in_go_test(node, content);
                patterns.push(LoggingPattern {
                    line_start,
                    level: Some(level),
                    framework: "zap".to_string(),
                    in_test,
                    confidence: if in_test { 0.7 } else { 0.9 },
                    evidence: truncate_evidence(&node_text(node, content), 200),
                });
            }
            // zerolog package is often imported as "log" — require zerolog import path.
            if has_zerolog && (operand == "log" || operand == "zerolog" || operand.starts_with("zerolog."))
            {
                let line_start = node.start_position().row as i32 + 1;
                let in_test = is_in_go_test(node, content);
                patterns.push(LoggingPattern {
                    line_start,
                    level: Some(level),
                    framework: "zerolog".to_string(),
                    in_test,
                    confidence: if in_test { 0.6 } else { 0.8 },
                    evidence: truncate_evidence(&node_text(node, content), 200),
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_go_logging(child, content, has_zap, has_zerolog, patterns);
    }
}

pub fn extract_error_handling(content: &str) -> Result<Vec<ErrorHandlingPattern>> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Go content"))?;

    let mut patterns = Vec::new();
    collect_go_error_handling(tree.root_node(), content, &mut patterns);
    patterns.truncate(1000);
    // Deterministic order for stable index output
    patterns.sort_by(|a, b| {
        a.line_start
            .cmp(&b.line_start)
            .then(a.framework.cmp(&b.framework))
            .then(a.evidence.cmp(&b.evidence))
    });
    Ok(patterns)
}

fn collect_go_error_handling(
    node: Node,
    content: &str,
    patterns: &mut Vec<ErrorHandlingPattern>,
) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
    {
        let (pkg, method) = match function.kind() {
            "selector_expression" => (
                extract_selector_operand(function, content),
                extract_selector_field(function, content),
            ),
            "identifier" => (String::new(), node_text(function, content)),
            _ => (String::new(), String::new()),
        };

        // errors.Is / errors.As
        if pkg == "errors" && (method == "Is" || method == "As") {
            let line_start = node.start_position().row as i32 + 1;
            let in_test = is_in_go_test(node, content);
            patterns.push(ErrorHandlingPattern {
                line_start,
                level: Some(LogLevel::Info),
                framework: "errors".to_string(),
                in_test,
                confidence: if in_test { 0.7 } else { 1.0 },
                evidence: format!("errors.{method}"),
            });
        }

        // fmt.Errorf with %w wrapping
        if pkg == "fmt" && method == "Errorf" {
            let call_text = node_text(node, content);
            if call_text.contains("%w") {
                let line_start = node.start_position().row as i32 + 1;
                let in_test = is_in_go_test(node, content);
                patterns.push(ErrorHandlingPattern {
                    line_start,
                    level: Some(LogLevel::Warn),
                    framework: "fmt".to_string(),
                    in_test,
                    confidence: if in_test { 0.7 } else { 1.0 },
                    evidence: truncate_evidence(&call_text, 200),
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_go_error_handling(child, content, patterns);
    }
}

pub fn extract_telemetry_patterns(content: &str) -> Result<Vec<TelemetryPattern>> {
    // Minimal: no dedicated Go OTel extraction in this track; empty is acceptable.
    // Keep signature parity with other languages and scan for open-telemetry imports lightly.
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Go content"))?;

    let mut patterns = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "import_spec" {
            let text = node_text(node, content);
            if text.contains("go.opentelemetry.io") || text.contains("otel") {
                let line_start = node.start_position().row as i32 + 1;
                patterns.push(TelemetryPattern {
                    line_start,
                    level: Some(LogLevel::Trace),
                    framework: "opentelemetry".to_string(),
                    in_test: is_in_go_test(node, content),
                    confidence: 0.8,
                    evidence: "import: opentelemetry".to_string(),
                });
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    patterns.truncate(1000);
    // Deterministic order for stable index output
    patterns.sort_by(|a, b| {
        a.line_start
            .cmp(&b.line_start)
            .then(a.framework.cmp(&b.framework))
            .then(a.evidence.cmp(&b.evidence))
    });
    Ok(patterns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::observability::LogLevel;

    #[test]
    fn extract_slog_logging() {
        let content = r#"
package demo

import "log/slog"

func handle() {
    slog.Info("request received")
    slog.Error("request failed")
    slog.Warn("slow")
    slog.Debug("detail")
}
"#;
        let patterns = extract_logging_patterns(content).unwrap();
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "slog" && p.level == Some(LogLevel::Info))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "slog" && p.level == Some(LogLevel::Error))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "slog" && p.level == Some(LogLevel::Warn))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "slog" && p.level == Some(LogLevel::Debug))
        );
    }

    #[test]
    fn extract_errors_is_as_and_fmt_wrap() {
        let content = r#"
package demo

import (
    "errors"
    "fmt"
)

func handle(err error) error {
    if errors.Is(err, ErrNotFound) {
        return err
    }
    if errors.As(err, &target) {
        return err
    }
    return fmt.Errorf("wrap: %w", err)
}
"#;
        let patterns = extract_error_handling(content).unwrap();
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "errors" && p.evidence.contains("errors.Is"))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "errors" && p.evidence.contains("errors.As"))
        );
        assert!(
            patterns
                .iter()
                .any(|p| p.framework == "fmt" && p.evidence.contains("%w"))
        );
    }

    #[test]
    fn slog_in_test_function_marked_in_test() {
        let content = r#"
package demo

import "log/slog"

func TestHandle(t *testing.T) {
    slog.Info("in test")
}
"#;
        let patterns = extract_logging_patterns(content).unwrap();
        assert!(
            patterns.iter().any(|p| p.framework == "slog" && p.in_test),
            "slog inside Test* should be in_test"
        );
    }

    #[test]
    fn zap_requires_import_path() {
        // Operand looks like zap but no matching import — must not emit.
        let no_import = r#"
package demo

func handle(zap *Logger) {
    zap.Info("noise")
}
"#;
        let patterns = extract_logging_patterns(no_import).unwrap();
        assert!(
            patterns.iter().all(|p| p.framework != "zap"),
            "zap without import path must not match"
        );

        let with_import = r#"
package demo

import "go.uber.org/zap"

func handle() {
    zap.L().Info("ok")
}
"#;
        let patterns = extract_logging_patterns(with_import).unwrap();
        assert!(
            patterns.iter().any(|p| p.framework == "zap"),
            "zap with go.uber.org/zap import should match; got {patterns:?}"
        );
    }
}
