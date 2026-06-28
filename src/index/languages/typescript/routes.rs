use crate::index::routes::ExtractedRoute;
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::Parser;

use super::common::{extract_ts_member_name, extract_ts_object_name};

/// HTTP method names used as Express/Fastify method calls.
const TS_HTTP_METHODS: &[&str] = &["get", "post", "put", "delete", "patch"];

/// Common variable names for Express app/router objects.
const EXPRESS_APP_NAMES: &[&str] = &["app", "router", "Router"];
/// Common variable names for Fastify instances.
const FASTIFY_APP_NAMES: &[&str] = &["fastify", "app"];

pub fn extract_routes(content: &str, _symbols: &[Symbol]) -> Result<Vec<ExtractedRoute>> {
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content"))?;

    let is_fastify = detect_fastify(content);
    let mut routes = Vec::new();
    collect_ts_routes(tree.root_node(), content, is_fastify, &mut routes);
    Ok(routes)
}

/// Detect whether the file is likely Fastify (vs Express) by scanning for
/// import statements or variable assignments referencing fastify.
fn detect_fastify(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    // Check for fastify imports or factory calls
    lower.contains("fastify")
        || lower.contains("fastify(")
        || lower.contains("require('fastify')")
        || lower.contains("from 'fastify'")
}

fn collect_ts_routes(
    node: tree_sitter::Node,
    content: &str,
    is_fastify: bool,
    routes: &mut Vec<ExtractedRoute>,
) {
    let kind = node.kind();

    // Look for call_expression where the callee is a member_expression like
    // app.get("/path", handler) or router.post("/path", handler)
    if kind == "call_expression" {
        let callee_node = node.child(0);
        if let Some(callee) = callee_node
            && callee.kind() == "member_expression"
        {
            let method_name = extract_ts_member_name(callee, content);
            let obj_name = extract_ts_object_name(callee, content);

            if TS_HTTP_METHODS.contains(&method_name.as_str()) {
                // Check if the object name is a known app/router variable
                let is_express_obj = EXPRESS_APP_NAMES.contains(&obj_name.as_str());
                let is_fastify_obj = FASTIFY_APP_NAMES.contains(&obj_name.as_str());

                if is_express_obj || is_fastify_obj {
                    // Find the arguments child node
                    let mut cursor = node.walk();
                    let args_node = node.children(&mut cursor).find(|c| c.kind() == "arguments");

                    if let Some(args_node) = args_node {
                        // Collect actual argument expressions (skip punctuation)
                        let mut cursor2 = args_node.walk();
                        let args: Vec<tree_sitter::Node> = args_node
                            .children(&mut cursor2)
                            .filter(|c| c.kind() != "," && c.kind() != "(" && c.kind() != ")")
                            .collect();

                        if args.len() >= 2 {
                            // First argument should be the path (string)
                            let path_node = args[0];
                            let path_text = path_node.utf8_text(content.as_bytes()).unwrap_or("");
                            let path = extract_ts_string_literal(path_text);

                            // Second argument is the handler
                            let handler_node = args[1];
                            let handler_text =
                                handler_node.utf8_text(content.as_bytes()).unwrap_or("");

                            let (handler_name, is_dynamic, route_confidence) =
                                if handler_node.kind() == "identifier" {
                                    (handler_text.to_string(), false, 1.0)
                                } else {
                                    // Inline function / arrow function / callback
                                    ("anonymous".to_string(), true, 0.5)
                                };

                            let framework = if is_fastify && is_fastify_obj {
                                "fastify"
                            } else {
                                "express"
                            };

                            let evidence =
                                format!("{obj_name}.{method_name}(\"{path}\", {handler_text})");
                            routes.push(ExtractedRoute {
                                method: method_name.to_ascii_uppercase(),
                                path_pattern: path,
                                handler_name,
                                framework: framework.to_string(),
                                route_source: "APP_METHOD".to_string(),
                                mount_prefix: None,
                                is_dynamic,
                                route_confidence,
                                evidence,
                                auth_requirements: None,
                                schema_refs: None,
                                owning_service: None,
                                consumers: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ts_routes(child, content, is_fastify, routes);
    }
}

/// Strip surrounding quotes from a string literal, returning the inner content.
fn extract_ts_string_literal(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"'))
        || (s.starts_with('\'') && s.ends_with('\''))
        || (s.starts_with('`') && s.ends_with('`'))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_routes_express_get() {
        let content = r#"
            import express from 'express';
            const app = express();

            app.get("/api/users", getUsers);
        "#;

        let routes = extract_routes(content, &[]).unwrap();
        let route = routes
            .iter()
            .find(|r| r.path_pattern == "/api/users" && r.framework == "express")
            .expect("should find express GET /api/users route");
        assert_eq!(route.method, "GET");
        assert_eq!(route.handler_name, "getUsers");
        assert_eq!(route.route_source, "APP_METHOD");
        assert!(!route.is_dynamic);
        assert_eq!(route.route_confidence, 1.0);
    }

    #[test]
    fn test_extract_routes_express_router_post() {
        let content = r#"
            import { Router } from 'express';
            const router = Router();

            router.post("/items", createItem);
        "#;

        let routes = extract_routes(content, &[]).unwrap();
        let route = routes
            .iter()
            .find(|r| r.path_pattern == "/items" && r.framework == "express")
            .expect("should find express POST /items route");
        assert_eq!(route.method, "POST");
        assert_eq!(route.handler_name, "createItem");
        assert_eq!(route.route_source, "APP_METHOD");
    }

    #[test]
    fn test_extract_routes_fastify_get() {
        let content = r#"
            import Fastify from 'fastify';
            const fastify = Fastify();

            fastify.get("/health", healthCheck);
        "#;

        let routes = extract_routes(content, &[]).unwrap();
        let route = routes
            .iter()
            .find(|r| r.path_pattern == "/health" && r.framework == "fastify")
            .expect("should find fastify GET /health route");
        assert_eq!(route.method, "GET");
        assert_eq!(route.handler_name, "healthCheck");
        assert_eq!(route.route_source, "APP_METHOD");
    }
}
