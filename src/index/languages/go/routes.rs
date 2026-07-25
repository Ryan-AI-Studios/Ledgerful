use super::common::{
    extract_selector_field, extract_selector_operand, node_text, truncate_evidence,
};
use crate::index::routes::ExtractedRoute;
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

const HTTP_VERBS: &[&str] = &[
    "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "TRACE", "CONNECT",
];

const GIN_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "Any"];

pub fn extract_routes(content: &str, _symbols: &[Symbol]) -> Result<Vec<ExtractedRoute>> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Go content"))?;

    let gin_routers = detect_gin_routers(tree.root_node(), content);

    let mut routes = Vec::new();
    collect_go_routes(tree.root_node(), content, &gin_routers, &mut routes);

    // Deterministic order
    routes.sort_by(|a, b| {
        a.framework
            .cmp(&b.framework)
            .then(a.method.cmp(&b.method))
            .then(a.path_pattern.cmp(&b.path_pattern))
            .then(a.handler_name.cmp(&b.handler_name))
    });

    Ok(routes)
}

/// Detect identifiers assigned from gin.Default() / gin.New() plus common defaults.
fn detect_gin_routers(root: Node, content: &str) -> Vec<String> {
    let mut routers = vec![
        "r".to_string(),
        "router".to_string(),
        "engine".to_string(),
        "g".to_string(),
    ];

    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        // short_var_declaration: r := gin.Default()
        // assignment_statement: r = gin.New()
        if matches!(
            node.kind(),
            "short_var_declaration" | "assignment_statement"
        ) {
            let text = node_text(node, content);
            if text.contains("gin.Default(")
                || text.contains("gin.New(")
                || text.contains("gin.Default()")
                || text.contains("gin.New()")
            {
                // left side identifiers
                if let Some(left) = node.child_by_field_name("left") {
                    collect_identifiers(left, content, &mut routers);
                } else {
                    // Fallback: first identifier on the line
                    if let Some(lhs) = text.split('=').next() {
                        let name = lhs.trim().trim_end_matches(':').trim().to_string();
                        if !name.is_empty() && !routers.contains(&name) {
                            routers.push(name);
                        }
                    }
                }
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    routers.sort();
    routers.dedup();
    routers
}

fn collect_identifiers(node: Node, content: &str, out: &mut Vec<String>) {
    if node.kind() == "identifier" {
        let name = node_text(node, content);
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifiers(child, content, out);
    }
}

fn collect_go_routes(
    node: Node,
    content: &str,
    gin_routers: &[String],
    routes: &mut Vec<ExtractedRoute>,
) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
    {
        match function.kind() {
            "selector_expression" => {
                let field = extract_selector_field(function, content);
                let operand = extract_selector_operand(function, content);

                // net/http: http.HandleFunc / mux.HandleFunc / *.Handle
                if (field == "HandleFunc" || field == "Handle")
                    && let Some(route) = extract_nethttp_route(&node, content, &field, &operand)
                {
                    routes.push(route);
                }

                // Gin: r.GET("/path", handler)
                if GIN_METHODS.iter().any(|m| m.eq_ignore_ascii_case(&field))
                    && gin_routers.iter().any(|r| r == &operand)
                    && let Some(route) = extract_gin_route(&node, content, &field, &operand)
                {
                    routes.push(route);
                }
            }
            "identifier" => {
                // Bare HandleFunc rare; skip
            }
            _ => {}
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_go_routes(child, content, gin_routers, routes);
    }
}

fn extract_nethttp_route(
    call: &Node,
    content: &str,
    field: &str,
    operand: &str,
) -> Option<ExtractedRoute> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let arg_nodes: Vec<Node> = args
        .children(&mut cursor)
        .filter(|n| {
            !matches!(
                n.kind(),
                "(" | ")" | "," | "comment"
            )
        })
        .collect();

    if arg_nodes.is_empty() {
        return None;
    }

    let pattern_raw = string_literal_value(arg_nodes[0], content)?;
    let (method, path) = parse_nethttp_pattern(&pattern_raw);

    let handler = arg_nodes
        .get(1)
        .map(|n| handler_name_from_arg(*n, content))
        .unwrap_or_else(|| "<unknown>".to_string());

    let is_dynamic = path.contains('{') || path.contains('*') || path.contains(':');
    let evidence = truncate_evidence(
        &format!("{operand}.{field}(\"{pattern_raw}\", {handler})"),
        200,
    );

    Some(ExtractedRoute {
        method,
        path_pattern: path,
        handler_name: handler,
        framework: "nethttp".to_string(),
        route_source: "METHOD_CALL".to_string(),
        mount_prefix: None,
        is_dynamic,
        route_confidence: 0.95,
        evidence,
        auth_requirements: None,
        schema_refs: None,
        owning_service: None,
        consumers: None,
    })
}

/// Parse Go 1.22 ServeMux patterns: `[METHOD ][HOST]/[PATH]`.
///
/// - If first token is an HTTP verb → method + rest as path (may include host).
/// - Else method defaults to GET and path is the full pattern.
fn parse_nethttp_pattern(pattern: &str) -> (String, String) {
    let pattern = pattern.trim();
    if let Some((first, rest)) = pattern.split_once(' ') {
        let first_upper = first.to_ascii_uppercase();
        if HTTP_VERBS.contains(&first_upper.as_str()) {
            let path = rest.trim();
            // Host-only or host+path after method is fine as path_pattern
            return (first_upper, path.to_string());
        }
    }
    // No method token — default GET; path is the whole pattern (may be host-only).
    ("GET".to_string(), pattern.to_string())
}

fn extract_gin_route(
    call: &Node,
    content: &str,
    method_field: &str,
    operand: &str,
) -> Option<ExtractedRoute> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let arg_nodes: Vec<Node> = args
        .children(&mut cursor)
        .filter(|n| {
            !matches!(
                n.kind(),
                "(" | ")" | "," | "comment"
            )
        })
        .collect();

    if arg_nodes.is_empty() {
        return None;
    }

    let path = string_literal_value(arg_nodes[0], content)?;
    let handler = arg_nodes
        .get(1)
        .map(|n| handler_name_from_arg(*n, content))
        .unwrap_or_else(|| "<unknown>".to_string());

    let method = if method_field.eq_ignore_ascii_case("Any") {
        "ANY".to_string()
    } else {
        method_field.to_ascii_uppercase()
    };

    let is_dynamic = path.contains(':') || path.contains('*') || path.contains('{');
    let evidence = truncate_evidence(
        &format!("{operand}.{method_field}(\"{path}\", {handler})"),
        200,
    );

    Some(ExtractedRoute {
        method,
        path_pattern: path,
        handler_name: handler,
        framework: "gin".to_string(),
        route_source: "METHOD_CALL".to_string(),
        mount_prefix: None,
        is_dynamic,
        route_confidence: 0.95,
        evidence,
        auth_requirements: None,
        schema_refs: None,
        owning_service: None,
        consumers: None,
    })
}

fn string_literal_value(node: Node, content: &str) -> Option<String> {
    match node.kind() {
        "interpreted_string_literal" | "raw_string_literal" => {
            let t = node_text(node, content);
            let trimmed = t
                .trim_matches('"')
                .trim_matches('`')
                .to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        _ => {
            // Sometimes wrapped
            let t = node_text(node, content);
            let trimmed = t.trim().trim_matches('"').trim_matches('`').to_string();
            if trimmed.is_empty() || trimmed.contains('(') {
                None
            } else {
                Some(trimmed)
            }
        }
    }
}

fn handler_name_from_arg(node: Node, content: &str) -> String {
    match node.kind() {
        "identifier" => node_text(node, content),
        "selector_expression" => extract_selector_field(node, content),
        "func_literal" => "<func_literal>".to_string(),
        "unary_expression" | "call_expression" => {
            // Handler may be a call like middleware(handler) — take last identifier-ish
            let text = node_text(node, content);
            text.split(['(', '.', ','])
                .next_back()
                .unwrap_or("<unknown>")
                .trim_matches(')')
                .trim()
                .to_string()
        }
        _ => {
            let text = node_text(node, content);
            if text.is_empty() {
                "<unknown>".to_string()
            } else {
                text
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nethttp_pattern_method_and_path() {
        assert_eq!(
            parse_nethttp_pattern("GET /items/{id}"),
            ("GET".to_string(), "/items/{id}".to_string())
        );
        assert_eq!(
            parse_nethttp_pattern("POST /users"),
            ("POST".to_string(), "/users".to_string())
        );
        assert_eq!(
            parse_nethttp_pattern("/health"),
            ("GET".to_string(), "/health".to_string())
        );
        assert_eq!(
            parse_nethttp_pattern("GET example.com/path"),
            ("GET".to_string(), "example.com/path".to_string())
        );
        assert_eq!(
            parse_nethttp_pattern("example.com/path"),
            ("GET".to_string(), "example.com/path".to_string())
        );
    }

    #[test]
    fn extract_nethttp_handlefunc() {
        let content = r#"
package main

import "net/http"

func getItem(w http.ResponseWriter, r *http.Request) {}

func main() {
    mux := http.NewServeMux()
    mux.HandleFunc("GET /items/{id}", getItem)
    http.HandleFunc("/health", getItem)
}
"#;
        let routes = extract_routes(content, &[]).unwrap();
        let get_item = routes
            .iter()
            .find(|r| r.path_pattern == "/items/{id}" && r.framework == "nethttp")
            .expect("GET /items/{id}");
        assert_eq!(get_item.method, "GET");
        assert_eq!(get_item.handler_name, "getItem");
        assert_eq!(get_item.route_source, "METHOD_CALL");
        assert!(get_item.is_dynamic);

        let health = routes
            .iter()
            .find(|r| r.path_pattern == "/health" && r.framework == "nethttp")
            .expect("/health");
        assert_eq!(health.method, "GET");
    }

    #[test]
    fn extract_gin_routes() {
        let content = r#"
package main

import "github.com/gin-gonic/gin"

func listUsers(c *gin.Context) {}
func createUser(c *gin.Context) {}

func main() {
    r := gin.Default()
    r.GET("/users", listUsers)
    r.POST("/users", createUser)
}
"#;
        let routes = extract_routes(content, &[]).unwrap();
        let get = routes
            .iter()
            .find(|r| r.path_pattern == "/users" && r.method == "GET" && r.framework == "gin")
            .expect("gin GET /users");
        assert_eq!(get.handler_name, "listUsers");
        assert_eq!(get.route_source, "METHOD_CALL");

        let post = routes
            .iter()
            .find(|r| r.path_pattern == "/users" && r.method == "POST" && r.framework == "gin")
            .expect("gin POST /users");
        assert_eq!(post.handler_name, "createUser");
    }
}
