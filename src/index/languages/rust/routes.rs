use crate::index::routes::ExtractedRoute;
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Parser};

struct PendingRoute {
    route: ExtractedRoute,
    builder: Option<String>,
}

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
    let mut pending = Vec::new();

    collect_rust_routes(root, content, &mut pending, &handler_info);

    let nest_map = collect_nest_prefixes(root, content);
    let mut routes: Vec<ExtractedRoute> = pending
        .into_iter()
        .map(|mut p| {
            if let Some(ident) = p.builder.as_deref()
                && let Some(prefix) = nest_map.get(ident)
            {
                p.route.mount_prefix = Some(prefix.clone());
            }
            p.route
        })
        .collect();

    routes.sort_by(|a, b| {
        a.path_pattern
            .cmp(&b.path_pattern)
            .then_with(|| a.method.cmp(&b.method))
            .then_with(|| a.framework.cmp(&b.framework))
            .then_with(|| a.handler_name.cmp(&b.handler_name))
    });

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
    routes: &mut Vec<PendingRoute>,
    handler_info: &std::collections::HashMap<String, HandlerInfo>,
) {
    if node.kind() == "call_expression" {
        let function = node
            .child_by_field_name("function")
            .map(|f| f.utf8_text(content.as_bytes()).unwrap_or(""))
            .unwrap_or("");

        // Axum .route() — callee last segment, not substring (avoid false hits).
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
                && let Some(pending) = extract_decorator_route(
                    child.utf8_text(content.as_bytes()).unwrap_or(""),
                    &node,
                    content,
                    handler_info,
                )
            {
                routes.push(pending);
            }
        }

        // Check previous siblings (standard for outer attributes in many rust grammars)
        let mut prev = node.prev_sibling();
        while let Some(p) = prev {
            if p.kind() == "attribute_item"
                && let Some(pending) = extract_decorator_route(
                    p.utf8_text(content.as_bytes()).unwrap_or(""),
                    &node,
                    content,
                    handler_info,
                )
            {
                routes.push(pending);
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

fn call_callee_name(node: Node, content: &str) -> String {
    let Some(func) = node.child_by_field_name("function") else {
        return String::new();
    };
    if func.kind() == "field_expression"
        && let Some(field) = func.child_by_field_name("field")
    {
        return field
            .utf8_text(content.as_bytes())
            .unwrap_or("")
            .to_string();
    }
    func.utf8_text(content.as_bytes()).unwrap_or("").to_string()
}

fn enclosing_let_ident(node: Node, content: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "let_declaration"
            && let Some(pattern) = n.child_by_field_name("pattern")
        {
            return first_identifier(pattern, content);
        }
        current = n.parent();
    }
    None
}

fn first_identifier(node: Node, content: &str) -> Option<String> {
    if node.kind() == "identifier" {
        let text = node.utf8_text(content.as_bytes()).unwrap_or("");
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = first_identifier(child, content) {
            return Some(found);
        }
    }
    None
}

fn enclosing_function_is_test(node: Node, content: &str) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == "function_item" {
            return function_has_test_attrs(n, content);
        }
        current = n.parent();
    }
    false
}

fn function_has_test_attrs(fn_node: Node, content: &str) -> bool {
    let mut cursor = fn_node.walk();
    for child in fn_node.children(&mut cursor) {
        if child.kind() == "attribute_item"
            && is_test_fn_attr(child.utf8_text(content.as_bytes()).unwrap_or(""))
        {
            return true;
        }
    }
    let mut prev = fn_node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "attribute_item" {
            if is_test_fn_attr(p.utf8_text(content.as_bytes()).unwrap_or("")) {
                return true;
            }
        } else if p.kind() != "line_comment" && p.kind() != "block_comment" {
            break;
        }
        prev = p.prev_sibling();
    }
    false
}

fn is_test_fn_attr(text: &str) -> bool {
    let compact = text.replace(' ', "");
    compact.contains("#[test]") || compact.contains("#[test,") || compact.contains("tokio::test")
}

fn collect_nest_prefixes(root: Node, content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression"
            && call_callee_name(node, content) == "nest"
            && let Some((prefix, idents)) = nest_prefix_and_idents(node, content)
        {
            for ident in idents {
                map.insert(ident, prefix.clone());
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    map
}

fn nest_prefix_and_idents(call: Node, content: &str) -> Option<(String, HashSet<String>)> {
    let args = call.child_by_field_name("arguments")?;
    let mut prefix = None;
    let mut router_expr = None;
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.kind() == "(" || child.kind() == ")" || child.kind() == "," {
            continue;
        }
        if prefix.is_none()
            && (child.kind() == "string_literal" || child.kind() == "raw_string_literal")
        {
            let text = child.utf8_text(content.as_bytes()).unwrap_or("");
            prefix = Some(text.trim_matches(|c| c == '"' || c == '#').to_string());
            continue;
        }
        if prefix.is_some() && router_expr.is_none() {
            router_expr = Some(child);
        }
    }
    let prefix = prefix?;
    let expr = router_expr?;
    let mut idents = HashSet::new();
    collect_router_idents(expr, content, &mut idents);
    Some((prefix, idents))
}

fn collect_router_idents(node: Node, content: &str, out: &mut HashSet<String>) {
    const SKIP: &[&str] = &[
        "Router",
        "new",
        "merge",
        "route",
        "layer",
        "route_layer",
        "nest",
        "nest_service",
        "get",
        "post",
        "put",
        "delete",
        "patch",
    ];
    if node.kind() == "identifier" {
        let text = node.utf8_text(content.as_bytes()).unwrap_or("");
        if !text.is_empty() && !SKIP.contains(&text) {
            out.insert(text.to_string());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_router_idents(child, content, out);
    }
}

fn builder_layer_is_secured(route_node: Node, content: &str) -> bool {
    if let Some(value) = enclosing_let_value(route_node) {
        return walk_layer_calls_secured(value, content);
    }
    // Unbound `Router::new().route(...).layer(...)` chains (no `let`).
    let mut current = route_node.parent();
    while let Some(n) = current {
        if n.kind() == "function_item" || n.kind() == "source_file" {
            break;
        }
        if n.kind() == "call_expression" {
            let callee = call_callee_name(n, content);
            if (callee == "layer" || callee == "route_layer") && layer_args_look_secured(n, content)
            {
                return true;
            }
        }
        current = n.parent();
    }
    false
}

fn enclosing_let_value(node: Node) -> Option<Node> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "let_declaration" {
            return n.child_by_field_name("value");
        }
        current = n.parent();
    }
    None
}

fn walk_layer_calls_secured(node: Node, content: &str) -> bool {
    if node.kind() == "call_expression" {
        let callee = call_callee_name(node, content);
        if (callee == "layer" || callee == "route_layer") && layer_args_look_secured(node, content)
        {
            return true;
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if walk_layer_calls_secured(child, content) {
            return true;
        }
    }
    false
}

fn layer_args_look_secured(call: Node, content: &str) -> bool {
    let Some(args) = call.child_by_field_name("arguments") else {
        return false;
    };
    let mut stack = vec![args];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" || node.kind() == "scoped_identifier" {
            let text = node.utf8_text(content.as_bytes()).unwrap_or("");
            let last = text.split("::").last().unwrap_or(text);
            if is_auth_arg_ident(last) {
                return true;
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}

fn is_auth_arg_ident(last_segment: &str) -> bool {
    matches!(
        last_segment,
        "token_layer" | "Auth" | "Claims" | "Session" | "auth"
    )
}

const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "trace",
];

fn extract_axum_route(
    call_node: &Node,
    content: &str,
    routes: &mut Vec<PendingRoute>,
    handler_info: &std::collections::HashMap<String, HandlerInfo>,
) {
    let args_node = match call_node.child_by_field_name("arguments") {
        Some(node) => node,
        None => return,
    };
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

    let builder = enclosing_let_ident(*call_node, content);
    let router_has_auth = builder_layer_is_secured(*call_node, content);
    let route_source = if enclosing_function_is_test(*call_node, content) {
        "TEST"
    } else {
        "BUILDER"
    };

    for arg in args {
        if arg.kind() == "call_expression" {
            let text = arg.utf8_text(content.as_bytes()).unwrap_or("");
            for &method in HTTP_METHODS {
                if text.starts_with(method) || text.contains(&format!("::{}", method)) {
                    let (handler, evidence_suffix) = find_axum_handler_kind(&arg, content);
                    let info = handler_info.get(&handler);

                    let mut auth = Vec::new();
                    if router_has_auth {
                        auth.push("secured".to_string());
                    }
                    if let Some(i) = info
                        && i.is_secured
                        && !auth.contains(&"secured".to_string())
                    {
                        auth.push("secured".to_string());
                    }
                    let auth_reqs = if auth.is_empty() { None } else { Some(auth) };
                    let schemas = info.map(|i| i.schemas.clone());

                    routes.push(PendingRoute {
                        route: ExtractedRoute {
                            method: method.to_uppercase(),
                            path_pattern: path.clone(),
                            handler_name: handler.clone(),
                            framework: "Axum".to_string(),
                            route_source: route_source.to_string(),
                            mount_prefix: None,
                            is_dynamic: path.contains(':') || path.contains('*'),
                            route_confidence: 0.9,
                            evidence: format!("{}({}){}", method, path, evidence_suffix),
                            auth_requirements: auth_reqs,
                            schema_refs: schemas,
                            owning_service: None,
                            consumers: None,
                        },
                        builder: builder.clone(),
                    });
                }
            }
        }
    }
}

fn find_axum_handler_kind(node: &Node, content: &str) -> (String, String) {
    if let Some(args_node) = node.child_by_field_name("arguments") {
        let mut arg_cursor = args_node.walk();
        for arg in args_node.children(&mut arg_cursor) {
            if arg.kind() == "(" || arg.kind() == ")" || arg.kind() == "," {
                continue;
            }
            if arg.kind() == "closure_expression" {
                return ("unknown".to_string(), " -> <closure>".to_string());
            }
            if arg.kind() == "identifier" || arg.kind() == "scoped_identifier" {
                let text = arg.utf8_text(content.as_bytes()).unwrap_or("");
                // Final segment only — collect_handler_info keys by bare name.
                let name = text.split("::").last().unwrap_or(text).to_string();
                return (name.clone(), format!(" -> {name}"));
            }
            if arg.kind() != "line_comment" && arg.kind() != "block_comment" {
                return ("unknown".to_string(), " -> <not_identifier>".to_string());
            }
        }
    }
    ("unknown".to_string(), " -> unknown".to_string())
}

fn extract_decorator_route(
    attr_text: &str,
    fn_node: &Node,
    content: &str,
    handler_info: &std::collections::HashMap<String, HandlerInfo>,
) -> Option<PendingRoute> {
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

            let route_source = if function_has_test_attrs(*fn_node, content) {
                "TEST"
            } else {
                "DECORATOR"
            };

            return Some(PendingRoute {
                route: ExtractedRoute {
                    method: method.to_uppercase(),
                    path_pattern: path.clone(),
                    handler_name: handler,
                    framework: framework.to_string(),
                    route_source: route_source.to_string(),
                    mount_prefix: None,
                    is_dynamic: path.contains('{') || path.contains('<') || path.contains(':'),
                    route_confidence: 0.95,
                    evidence: attr_text.to_string(),
                    auth_requirements: auth,
                    schema_refs: schemas,
                    owning_service: None,
                    consumers: None,
                },
                builder: enclosing_let_ident(*fn_node, content),
            });
        }
    }
    None
}

#[cfg(test)]
mod routes_unwrap_tests {
    use super::*;
    use tree_sitter::Parser;

    #[test]
    fn routes_axum_route_without_arguments_skips_without_panic() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("rust grammar");
        let source = "fn build() { let _ = route(); }";
        let tree = parser.parse(source, None).expect("parse");
        let call = tree
            .root_node()
            .descendant_for_byte_range(source.find("route").unwrap(), source.len())
            .expect("route call");
        let mut routes = Vec::new();
        extract_axum_route(&call, source, &mut routes, &Default::default());
        assert!(routes.is_empty());
    }

    #[test]
    fn axum_nest_sets_mount_prefix() {
        let content = r#"
            pub fn router() -> Router {
                let api_router = Router::new()
                    .route("/session", get(session_handler))
                    .route("/endpoints/changed", get(endpoints_changed_handler));
                let api_public = Router::new().route("/session/exchange", post(exchange));
                let mut app = Router::new()
                    .route("/health", get(health_handler))
                    .nest("/api", api_router.merge(api_public))
                    .nest_service("/_next", ServeDir::new("."));
                app
            }
        "#;
        let routes = extract_routes(content, &[]).expect("extract");
        let session = routes
            .iter()
            .find(|r| r.path_pattern == "/session")
            .expect("session");
        let changed = routes
            .iter()
            .find(|r| r.path_pattern == "/endpoints/changed")
            .expect("changed");
        let exchange = routes
            .iter()
            .find(|r| r.path_pattern == "/session/exchange")
            .expect("exchange");
        let health = routes
            .iter()
            .find(|r| r.path_pattern == "/health")
            .expect("health");
        assert_eq!(session.mount_prefix.as_deref(), Some("/api"));
        assert_eq!(changed.mount_prefix.as_deref(), Some("/api"));
        assert_eq!(exchange.mount_prefix.as_deref(), Some("/api"));
        assert_eq!(health.mount_prefix, None);
        assert!(
            routes
                .iter()
                .all(|r| r.mount_prefix.as_deref() != Some("/_next")),
            "nest_service must not set a route prefix"
        );
    }

    #[test]
    fn axum_test_fn_route_source_is_test() {
        let content = r#"
            #[tokio::test]
            async fn probe_route() {
                let app = Router::new().route("/probe", get(|| async { "ok" }));
            }
        "#;
        let routes = extract_routes(content, &[]).expect("extract");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path_pattern, "/probe");
        assert_eq!(routes[0].route_source, "TEST");
        assert_eq!(routes[0].handler_name, "unknown");
        assert!(
            routes[0].evidence.contains("-> <closure>"),
            "closure evidence: {}",
            routes[0].evidence
        );
    }

    #[test]
    fn axum_health_not_secured_by_comment() {
        let content = r#"
            pub fn router() -> Router {
                // `/events` inherits token_layer. Do not skip Bearer auth.
                let api_router = Router::new()
                    .route("/session", get(session_handler))
                    .route_layer(middleware::from_fn_with_state(state.clone(), token_layer));
                let mut app = Router::new()
                    .route("/health", get(health_handler))
                    .nest("/api", api_router)
                    .layer(local_cors());
                app
            }
        "#;
        let routes = extract_routes(content, &[]).expect("extract");
        let health = routes
            .iter()
            .find(|r| r.path_pattern == "/health")
            .expect("health");
        assert_eq!(
            health.auth_requirements, None,
            "comment + sibling token_layer + local_cors must not secure /health: {:?}",
            health.auth_requirements
        );
    }

    #[test]
    fn axum_route_layer_on_same_builder_is_inferred() {
        let content = r#"
            pub fn router() -> Router {
                let api_router = Router::new()
                    .route("/session", get(session_handler))
                    .route_layer(middleware::from_fn_with_state(state.clone(), token_layer));
                let mut app = Router::new()
                    .route("/health", get(health_handler))
                    .nest("/api", api_router);
                app
            }
        "#;
        let routes = extract_routes(content, &[]).expect("extract");
        let session = routes
            .iter()
            .find(|r| r.path_pattern == "/session")
            .expect("session");
        let health = routes
            .iter()
            .find(|r| r.path_pattern == "/health")
            .expect("health");
        assert_eq!(
            session.auth_requirements.as_deref(),
            Some(["secured".to_string()].as_slice())
        );
        assert_eq!(health.auth_requirements, None);
    }
}
