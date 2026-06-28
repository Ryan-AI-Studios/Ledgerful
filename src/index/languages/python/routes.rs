use crate::index::routes::ExtractedRoute;
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::Parser;

/// HTTP method names used as FastAPI decorator methods.
const PY_HTTP_METHODS: &[&str] = &["get", "post", "put", "delete", "patch", "head", "options"];

pub fn extract_routes(content: &str, _symbols: &[Symbol]) -> Result<Vec<ExtractedRoute>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    // Detect FastAPI router objects and Flask app/blueprint objects
    let fastapi_routers = detect_fastapi_routers(tree.root_node(), content);
    let flask_objects = detect_flask_objects(content);

    let mut routes = Vec::new();
    collect_py_routes(
        tree.root_node(),
        content,
        &fastapi_routers,
        &flask_objects,
        &mut routes,
    );
    Ok(routes)
}

/// Detect variable names assigned from APIRouter() calls (FastAPI).
fn detect_fastapi_routers(root: tree_sitter::Node, content: &str) -> Vec<String> {
    let mut routers = Vec::new();
    // Common defaults: app, router, api_router
    routers.push("app".to_string());
    routers.push("router".to_string());
    routers.push("api_router".to_string());

    // Walk AST looking for assignments like: router = APIRouter()
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "assignment" {
            let mut cursor = node.walk();
            let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
            // assignment: left = right
            // children[0] is the left-hand side (identifier), children[1] is "=",
            // children[2] is the right-hand side (call)
            let lhs = children.first();
            let rhs = children.get(2);
            if let (Some(lhs_node), Some(rhs_node)) = (lhs, rhs)
                && lhs_node.kind() == "identifier"
                && rhs_node.kind() == "call"
            {
                let rhs_text = rhs_node.utf8_text(content.as_bytes()).unwrap_or("");
                if rhs_text.starts_with("APIRouter") {
                    let name = lhs_node
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if !routers.contains(&name) {
                        routers.push(name);
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    routers
}

/// Detect variable names that are Flask app/blueprint objects by scanning content.
fn detect_flask_objects(content: &str) -> Vec<String> {
    let mut objects = Vec::new();
    // Common defaults: app, bp, blueprint
    objects.push("app".to_string());
    objects.push("bp".to_string());
    objects.push("blueprint".to_string());

    // Look for assignments like: app = Flask(__name__) or bp = Blueprint("name", __name__)
    for line in content.lines() {
        let line = line.trim();
        if (line.contains("Flask(") || line.contains("Blueprint("))
            && line.contains('=')
            && let Some(lhs) = line.split('=').next()
        {
            let name = lhs.trim().to_string();
            if !name.is_empty() && !objects.contains(&name) {
                objects.push(name);
            }
        }
    }

    objects
}

fn collect_py_routes(
    node: tree_sitter::Node,
    content: &str,
    fastapi_routers: &[String],
    flask_objects: &[String],
    routes: &mut Vec<ExtractedRoute>,
) {
    let kind = node.kind();

    // In Python tree-sitter, decorators are children of decorated_definition.
    // Each decorator node corresponds to a @... line.
    if kind == "decorator" {
        let decorator_text = node.utf8_text(content.as_bytes()).unwrap_or("");

        // --- FastAPI: @router.get("/path") or @app.post("/path") ---
        // Pattern: @{varname}.{method}("path")
        for router_name in fastapi_routers {
            for &method_str in PY_HTTP_METHODS {
                let pattern = format!("@{router_name}.{method_str}(");
                if decorator_text.contains(&pattern)
                    && let Some(path) = extract_py_decorator_path(decorator_text)
                {
                    // Find the decorated function name from the parent decorated_definition
                    let handler_name = find_py_decorated_function_name(node, content);
                    let evidence =
                        format!("@{router_name}.{method_str}(\"{path}\") on {handler_name}");
                    routes.push(ExtractedRoute {
                        method: method_str.to_ascii_uppercase(),
                        path_pattern: path,
                        handler_name,
                        framework: "fastapi".to_string(),
                        route_source: "DECORATOR".to_string(),
                        mount_prefix: None,
                        is_dynamic: false,
                        route_confidence: 1.0,
                        evidence,
                        auth_requirements: None,
                        schema_refs: None,
                        owning_service: None,
                        consumers: None,
                    });
                }
            }
        }

        // --- Flask: @app.route("/path") or @bp.route("/path", methods=["POST"]) ---
        for obj_name in flask_objects {
            let route_pattern = format!("@{obj_name}.route(");
            if decorator_text.contains(&route_pattern)
                && let Some(path) = extract_py_decorator_path(decorator_text)
            {
                let handler_name = find_py_decorated_function_name(node, content);
                // Determine HTTP method: default GET, check for methods=["POST"] etc.
                let method = extract_flask_method_from_decorator(decorator_text);
                let evidence = format!("@{obj_name}.route(\"{path}\") on {handler_name}");
                routes.push(ExtractedRoute {
                    method,
                    path_pattern: path,
                    handler_name,
                    framework: "flask".to_string(),
                    route_source: "DECORATOR".to_string(),
                    mount_prefix: None,
                    is_dynamic: false,
                    route_confidence: 1.0,
                    evidence,
                    auth_requirements: None,
                    schema_refs: None,
                    owning_service: None,
                    consumers: None,
                });
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_py_routes(child, content, fastapi_routers, flask_objects, routes);
    }
}

/// Extract the path string from a Python decorator text like `@router.get("/items/{item_id}")`.
/// Looks for the content between the first `("` and `")`.
fn extract_py_decorator_path(decorator_text: &str) -> Option<String> {
    let start = decorator_text.find("(\"")? + 2;
    let end = decorator_text[start..]
        .find("\")")
        .or_else(|| decorator_text[start..].find('"'))?
        + start;
    Some(decorator_text[start..end].to_string())
}

/// Given a decorator node, find the decorated function name by looking at the
/// parent (decorated_definition) and then its function_definition child.
fn find_py_decorated_function_name(node: tree_sitter::Node, content: &str) -> String {
    // decorator nodes are children of decorated_definition.
    // The decorated_definition also has a function_definition child.
    if let Some(parent) = node.parent()
        && parent.kind() == "decorated_definition"
    {
        let mut cursor = parent.walk();
        for child in parent.children(&mut cursor) {
            if child.kind() == "function_definition" {
                // The function name is the first identifier child
                let mut fc = child.walk();
                for fchild in child.children(&mut fc) {
                    if fchild.kind() == "identifier" {
                        return fchild
                            .utf8_text(content.as_bytes())
                            .unwrap_or("<unknown>")
                            .to_string();
                    }
                }
            }
        }
    }
    "<unknown>".to_string()
}

/// Extract the HTTP method from a Flask @app.route() decorator.
/// Looks for methods=["POST"] etc. Defaults to GET.
fn extract_flask_method_from_decorator(decorator_text: &str) -> String {
    // Look for methods=["POST"] or methods=['POST']
    if let Some(idx) = decorator_text.find("methods=") {
        let after = &decorator_text[idx + 8..];
        let after = after.trim_start();
        // Look for the method string inside the list
        if let Some(bracket_start) = after.find('[') {
            let rest = &after[bracket_start + 1..];
            // Extract the string content
            for quote in &['"', '\''] {
                if let Some(q_start) = rest.find(*quote) {
                    let inner = &rest[q_start + 1..];
                    if let Some(q_end) = inner.find(*quote) {
                        let method = &inner[..q_end];
                        return method.to_ascii_uppercase();
                    }
                }
            }
        }
    }
    "GET".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_routes_fastapi() {
        let content = r#"
from fastapi import APIRouter

router = APIRouter()

@router.get("/items/{item_id}")
def read_item(item_id: int):
    pass
"#;

        let routes = extract_routes(content, &[]).unwrap();
        let route = routes
            .iter()
            .find(|r| r.path_pattern == "/items/{item_id}" && r.framework == "fastapi")
            .expect("should find fastapi GET /items/{item_id} route");
        assert_eq!(route.method, "GET");
        assert_eq!(route.handler_name, "read_item");
        assert_eq!(route.route_source, "DECORATOR");
        assert!(!route.is_dynamic);
        assert_eq!(route.route_confidence, 1.0);
    }

    #[test]
    fn test_extract_routes_flask_get() {
        let content = r#"
from flask import Flask

app = Flask(__name__)

@app.route("/users")
def get_users():
    return "users"
"#;

        let routes = extract_routes(content, &[]).unwrap();
        let route = routes
            .iter()
            .find(|r| r.path_pattern == "/users" && r.framework == "flask")
            .expect("should find flask GET /users route");
        assert_eq!(route.method, "GET");
        assert_eq!(route.handler_name, "get_users");
        assert_eq!(route.route_source, "DECORATOR");
        assert!(!route.is_dynamic);
        assert_eq!(route.route_confidence, 1.0);
    }

    #[test]
    fn test_extract_routes_flask_post() {
        let content = r#"
from flask import Flask

app = Flask(__name__)

@app.route("/items", methods=["POST"])
def create_item():
    return "created"
"#;

        let routes = extract_routes(content, &[]).unwrap();
        let route = routes
            .iter()
            .find(|r| r.path_pattern == "/items" && r.framework == "flask")
            .expect("should find flask POST /items route");
        assert_eq!(route.method, "POST");
        assert_eq!(route.handler_name, "create_item");
        assert_eq!(route.route_source, "DECORATOR");
        assert!(!route.is_dynamic);
        assert_eq!(route.route_confidence, 1.0);
    }
}
