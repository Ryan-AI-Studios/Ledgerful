use crate::index::routes::ExtractedRoute;
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

pub fn extract_routes(content: &str, _symbols: &[Symbol]) -> Result<Vec<ExtractedRoute>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Rust content"))?;
    let root = tree.root_node();

    let handler_info = collect_handler_info(root, content);
    let mut routes = Vec::new();

    collect_rust_routes(root, content, &mut routes, &handler_info);

    Ok(routes)
}

#[derive(Default, Debug)]
struct HandlerInfo {
    schemas: Vec<String>,
    is_secured: bool,
}

fn collect_handler_info(
    root: Node,
    content: &str,
) -> std::collections::HashMap<String, HandlerInfo> {
    let mut info_map = std::collections::HashMap::new();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        if node.kind() == "function_item"
            && let Some(name_node) = node.child_by_field_name("name")
        {
            let name = name_node
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .to_string();
            let mut info = HandlerInfo::default();

            if let Some(params_node) = node.child_by_field_name("parameters") {
                let mut pcursor = params_node.walk();
                for param in params_node.children(&mut pcursor) {
                    let param_text = param.utf8_text(content.as_bytes()).unwrap_or("");
                    // Detect Json<T>, Form<T>, Query<T>
                    if let Some(schema) = extract_schema_from_param(param_text) {
                        info.schemas.push(schema);
                    }
                    // Detect Auth extractors (heuristic: contains "Auth" or "Claims")
                    if param_text.contains("Auth")
                        || param_text.contains("Claims")
                        || param_text.contains("Session")
                    {
                        info.is_secured = true;
                    }
                }
            }
            info_map.insert(name, info);
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    info_map
}

fn extract_schema_from_param(text: &str) -> Option<String> {
    if (text.contains("Json<") || text.contains("Form<") || text.contains("Query<"))
        && let Some(start) = text.find('<')
        && let Some(end) = text.find('>')
    {
        return Some(text[start + 1..end].to_string());
    }
    None
}

fn collect_rust_routes(
    node: Node,
    content: &str,
    routes: &mut Vec<ExtractedRoute>,
    handler_info: &std::collections::HashMap<String, HandlerInfo>,
) {
    if node.kind() == "call_expression" {
        let function = node
            .child_by_field_name("function")
            .map(|f| f.utf8_text(content.as_bytes()).unwrap_or(""))
            .unwrap_or("");

        // Axum .route()
        if function == "route" || function.ends_with(".route") {
            extract_axum_route(&node, content, routes, handler_info);
        }
    }

    // Decorator-based routes (Actix/Rocket)
    if node.kind() == "function_item" {
        // Check children (for inner attributes or some parser versions)
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "attribute_item"
                && let Some(route) = extract_decorator_route(
                    child.utf8_text(content.as_bytes()).unwrap_or(""),
                    &node,
                    content,
                    handler_info,
                )
            {
                routes.push(route);
            }
        }

        // Check previous siblings (standard for outer attributes in many rust grammars)
        let mut prev = node.prev_sibling();
        while let Some(p) = prev {
            if p.kind() == "attribute_item"
                && let Some(route) = extract_decorator_route(
                    p.utf8_text(content.as_bytes()).unwrap_or(""),
                    &node,
                    content,
                    handler_info,
                )
            {
                routes.push(route);
            } else if p.kind() == "line_comment" || p.kind() == "block_comment" {
                // skip
            } else {
                break;
            }
            prev = p.prev_sibling();
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_routes(child, content, routes, handler_info);
    }
}

const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "trace",
];

fn extract_axum_route(
    call_node: &Node,
    content: &str,
    routes: &mut Vec<ExtractedRoute>,
    handler_info: &std::collections::HashMap<String, HandlerInfo>,
) {
    let args_node = call_node
        .child_by_field_name("arguments")
        .expect("route() should have arguments");
    let mut arg_cursor = args_node.walk();
    let args: Vec<Node> = args_node.children(&mut arg_cursor).collect();

    if args.len() < 2 {
        return;
    }

    let path = match args.get(1) {
        Some(t) => {
            let t = t.utf8_text(content.as_bytes()).unwrap_or("");
            t.trim_matches('"').to_string()
        }
        None => return,
    };

    // Heuristic for middleware-based auth: look up the chain
    let mut middleware_auth = Vec::new();
    let mut current = call_node.parent();
    while let Some(parent) = current {
        let parent_text = parent.utf8_text(content.as_bytes()).unwrap_or("");
        if parent_text.contains(".layer")
            && (parent_text.contains("auth") || parent_text.contains("Auth"))
        {
            middleware_auth.push("secured".to_string());
            break;
        }
        current = parent.parent();
        if parent.kind() == "function_item" {
            break;
        }
    }

    for arg in args {
        if arg.kind() == "call_expression" {
            let text = arg.utf8_text(content.as_bytes()).unwrap_or("");
            for &method in HTTP_METHODS {
                if text.starts_with(method) || text.contains(&format!("::{}", method)) {
                    let handler = find_axum_handler(&arg, content);
                    let info = handler_info.get(&handler);

                    let mut auth = middleware_auth.clone();
                    if let Some(i) = info
                        && i.is_secured
                        && !auth.contains(&"secured".to_string())
                    {
                        auth.push("secured".to_string());
                    }
                    let auth_reqs = if auth.is_empty() { None } else { Some(auth) };
                    let schemas = info.map(|i| i.schemas.clone());

                    routes.push(ExtractedRoute {
                        method: method.to_uppercase(),
                        path_pattern: path.clone(),
                        handler_name: handler.clone(),
                        framework: "Axum".to_string(),
                        route_source: "BUILDER".to_string(),
                        mount_prefix: None,
                        is_dynamic: path.contains(':') || path.contains('*'),
                        route_confidence: 0.9,
                        evidence: format!("{}({}) -> {}", method, path, handler),
                        auth_requirements: auth_reqs,
                        schema_refs: schemas,
                        owning_service: None,
                        consumers: None,
                    });
                }
            }
        }
    }
}

fn find_axum_handler(node: &Node, content: &str) -> String {
    if let Some(args_node) = node.child_by_field_name("arguments") {
        let mut arg_cursor = args_node.walk();
        for arg in args_node.children(&mut arg_cursor) {
            if arg.kind() == "identifier" || arg.kind() == "scoped_identifier" {
                let text = arg.utf8_text(content.as_bytes()).unwrap_or("");
                // We extract the full path from the syntax node, but return just the
                // final segment to maintain consistency with `collect_handler_info`
                // which keys its map by the bare function name.
                return text.split("::").last().unwrap_or(text).to_string();
            }
        }
    }
    "unknown".to_string()
}

fn extract_decorator_route(
    attr_text: &str,
    fn_node: &Node,
    content: &str,
    handler_info: &std::collections::HashMap<String, HandlerInfo>,
) -> Option<ExtractedRoute> {
    let text = attr_text.to_lowercase();
    for &method in HTTP_METHODS {
        // Match #[get(...)] or #[actix_web::get(...)] or @get(...) etc.
        if text.contains(&format!("[{}(", method))
            || text.contains(&format!("::{}", method))
            || text.contains(&format!("[{}", method))
        // Some might not have ( if no path
        {
            let path = if let Some(start) = attr_text.find('(') {
                if let Some(end) = attr_text.rfind(')') {
                    attr_text[start + 1..end].trim_matches('"').to_string()
                } else {
                    "/unknown".to_string()
                }
            } else {
                "/".to_string()
            };

            let name_node = fn_node.child_by_field_name("name")?;
            let handler = name_node
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .to_string();
            let info = handler_info.get(&handler);

            let auth = if let Some(i) = info
                && i.is_secured
            {
                Some(vec!["secured".to_string()])
            } else {
                None
            };
            let schemas = info.map(|i| i.schemas.clone());

            let framework = if text.contains("actix") {
                "Actix"
            } else if text.contains("rocket") {
                "Rocket"
            } else {
                "Actix" // Default for decorators for now to satisfy existing tests
            };

            return Some(ExtractedRoute {
                method: method.to_uppercase(),
                path_pattern: path.clone(),
                handler_name: handler,
                framework: framework.to_string(),
                route_source: "DECORATOR".to_string(),
                mount_prefix: None,
                is_dynamic: path.contains('{') || path.contains('<') || path.contains(':'),
                route_confidence: 0.95,
                evidence: attr_text.to_string(),
                auth_requirements: auth,
                schema_refs: schemas,
                owning_service: None,
                consumers: None,
            });
        }
    }
    None
}
