use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiEndpoint {
    pub path: String,
    pub method: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub operation_id: Option<String>,
    pub tags: Vec<String>,
    pub spec_file: String,
    pub spec_version: String,
    pub embed_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecParseResult {
    pub spec_file: String,
    pub version: String,
    pub title: String,
    pub endpoints: Vec<ApiEndpoint>,
}

const HTTP_METHODS: &[&str] = &["get", "post", "put", "delete", "patch", "options", "head"];

const REF_DEPTH_LIMIT: usize = 20;

pub fn parse_spec(spec_path: &Path) -> Result<SpecParseResult, String> {
    let spec_file = spec_path.to_string_lossy().to_string();

    let content = std::fs::read_to_string(spec_path)
        .map_err(|e| format!("Failed to read spec file {}: {}", spec_file, e))?;

    let value: Value = serde_json::from_str(&content)
        .or_else(|_| serde_yaml::from_str(&content))
        .map_err(|e| format!("Failed to parse spec {} as JSON or YAML: {}", spec_file, e))?;

    if value.get("openapi").is_some() {
        parse_openapi3(&value, &spec_file)
    } else if value.get("swagger").is_some() {
        parse_swagger2(&value, &spec_file)
    } else {
        Err(format!(
            "Unknown spec format in {}: no 'openapi' or 'swagger' key",
            spec_file
        ))
    }
}

pub fn parse_spec_safe(spec_path: &Path) -> Result<SpecParseResult, String> {
    match parse_spec(spec_path) {
        Ok(result) => Ok(result),
        Err(e) => {
            tracing::warn!("Skipping spec {}: {}", spec_path.display(), e);
            Ok(SpecParseResult {
                spec_file: spec_path.to_string_lossy().to_string(),
                version: "unknown".to_string(),
                title: "unknown".to_string(),
                endpoints: Vec::new(),
            })
        }
    }
}

fn parse_openapi3(doc: &Value, spec_file: &str) -> Result<SpecParseResult, String> {
    let version = doc["openapi"].as_str().unwrap_or("3.0.0").to_string();
    let title = doc["info"]["title"]
        .as_str()
        .unwrap_or("Untitled")
        .to_string();
    let mut endpoints = Vec::new();

    if let Some(paths) = doc["paths"].as_object() {
        for (path, path_item) in paths {
            let resolved = if let Some(ref_str) = path_item.get("$ref").and_then(|v| v.as_str()) {
                resolve_ref(doc, ref_str, 0).unwrap_or_else(|| path_item.clone())
            } else {
                path_item.clone()
            };
            extract_endpoints(&resolved, path, spec_file, &version, &mut endpoints);
        }
    }

    endpoints.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.method.cmp(&b.method)));

    Ok(SpecParseResult {
        spec_file: spec_file.to_string(),
        version,
        title,
        endpoints,
    })
}

fn parse_swagger2(doc: &Value, spec_file: &str) -> Result<SpecParseResult, String> {
    let version = "swagger2".to_string();
    let title = doc["info"]["title"]
        .as_str()
        .unwrap_or("Untitled")
        .to_string();
    let mut endpoints = Vec::new();

    if let Some(paths) = doc["paths"].as_object() {
        for (path, path_item) in paths {
            let resolved = if let Some(ref_str) = path_item.get("$ref").and_then(|v| v.as_str()) {
                resolve_ref(doc, ref_str, 0).unwrap_or_else(|| path_item.clone())
            } else {
                path_item.clone()
            };
            extract_endpoints(&resolved, path, spec_file, &version, &mut endpoints);
        }
    }

    endpoints.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.method.cmp(&b.method)));

    Ok(SpecParseResult {
        spec_file: spec_file.to_string(),
        version,
        title,
        endpoints,
    })
}

fn extract_endpoints(
    path_item: &Value,
    path: &str,
    spec_file: &str,
    spec_version: &str,
    endpoints: &mut Vec<ApiEndpoint>,
) {
    let obj = match path_item.as_object() {
        Some(o) => o,
        None => return,
    };

    for method in HTTP_METHODS {
        if let Some(operation) = obj.get(*method) {
            let summary = operation["summary"].as_str().map(|s| s.to_string());
            let description = operation["description"].as_str().map(|s| s.to_string());
            let operation_id = operation["operationId"].as_str().map(|s| s.to_string());
            let tags: Vec<String> = operation["tags"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let embed_text = build_embed_text(&summary, &description, &tags, method, path);
            if embed_text.len() < 10 {
                continue;
            }

            endpoints.push(ApiEndpoint {
                path: path.to_string(),
                method: method.to_uppercase(),
                summary,
                description,
                operation_id,
                tags,
                spec_file: spec_file.to_string(),
                spec_version: spec_version.to_string(),
                embed_text,
            });
        }
    }
}

fn build_embed_text(
    summary: &Option<String>,
    description: &Option<String>,
    tags: &[String],
    method: &str,
    path: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = summary {
        parts.push(s.clone());
    }
    if let Some(d) = description {
        parts.push(d.clone());
    }
    if !tags.is_empty() {
        parts.push(tags.join(" "));
    }

    let result = parts.join(" ");
    let trimmed = result.trim();
    if trimmed.is_empty() {
        format!("{} {}", method.to_uppercase(), path)
    } else {
        trimmed.to_string()
    }
}

fn resolve_ref(doc: &Value, ref_str: &str, depth: usize) -> Option<Value> {
    if depth > REF_DEPTH_LIMIT {
        return None;
    }

    let path = ref_str.strip_prefix('#')?;
    if path.is_empty() {
        return Some(doc.clone());
    }

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut current = doc;

    for segment_raw in segments {
        let segment = segment_raw.replace("~1", "/").replace("~0", "~");

        if let Some(obj) = current.as_object() {
            current = obj.get(&segment)?;
        } else if let Some(arr) = current.as_array() {
            let idx: usize = segment.parse().ok()?;
            current = arr.get(idx)?;
        } else {
            return None;
        }
    }

    if let Some(nested_ref) = current.get("$ref").and_then(|v| v.as_str()) {
        return resolve_ref(doc, nested_ref, depth + 1);
    }

    Some(current.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oai3_json() -> &'static str {
        r#"{
            "openapi": "3.0.0",
            "info": {
                "title": "Petstore",
                "version": "1.0.0"
            },
            "paths": {
                "/pets": {
                    "get": {
                        "summary": "List all pets",
                        "description": "Returns a list of all pets in the store",
                        "operationId": "listPets",
                        "tags": ["pets"]
                    },
                    "post": {
                        "summary": "Create a pet",
                        "description": "Add a new pet to the store",
                        "operationId": "createPet",
                        "tags": ["pets"]
                    }
                },
                "/pets/{petId}": {
                    "get": {
                        "summary": "Get a pet by ID",
                        "operationId": "getPet",
                        "tags": ["pets"]
                    },
                    "delete": {
                        "summary": "Delete a pet",
                        "operationId": "deletePet",
                        "tags": ["pets"]
                    }
                }
            }
        }"#
    }

    fn oai3_yaml() -> &'static str {
        r#"
openapi: "3.1.0"
info:
  title: Users API
  version: "2.0.0"
paths:
  /users:
    get:
      summary: List users
      description: Returns a paginated list of users
      operationId: listUsers
      tags:
        - users
        - admin
    post:
      summary: Create user
      operationId: createUser
      tags:
        - users
  /users/{userId}:
    get:
      summary: Get user by ID
      operationId: getUser
      tags:
        - users
"#
    }

    fn swagger2_json() -> &'static str {
        r#"{
            "swagger": "2.0",
            "info": {
                "title": "Sample API",
                "version": "1.0.0"
            },
            "paths": {
                "/items": {
                    "get": {
                        "summary": "List items",
                        "operationId": "listItems",
                        "tags": ["items"]
                    }
                },
                "/items/{itemId}": {
                    "get": {
                        "summary": "Get item",
                        "operationId": "getItem",
                        "tags": ["items"]
                    },
                    "put": {
                        "summary": "Update item",
                        "operationId": "updateItem",
                        "tags": ["items"]
                    }
                }
            }
        }"#
    }

    fn swagger2_yaml() -> &'static str {
        r#"
swagger: "2.0"
info:
  title: Inventory API
  version: "1.0.0"
paths:
  /products:
    get:
      summary: List products
      description: Returns all products in the inventory
      operationId: listProducts
      tags:
        - products
  /products/{productId}:
    get:
      summary: Get product details
      operationId: getProduct
      tags:
        - products
    delete:
      summary: Remove product
      operationId: removeProduct
      tags:
        - products
"#
    }

    fn oai3_with_ref() -> &'static str {
        r##"{
            "openapi": "3.0.0",
            "info": { "title": "Ref Test", "version": "1.0" },
            "paths": {
                "/items": {
                    "get": {
                        "summary": "List items",
                        "operationId": "listItems",
                        "tags": ["items"]
                    }
                },
                "/items/{itemId}": {
                    "$ref": "#/paths/~1items"
                }
            }
        }"##
    }

    fn oai3_self_ref_cycle() -> &'static str {
        r##"{
            "openapi": "3.0.0",
            "info": { "title": "Cycle Test", "version": "1.0" },
            "paths": {
                "/self": {
                    "$ref": "#/paths/~1self"
                }
            }
        }"##
    }

    fn short_summary_spec() -> &'static str {
        r#"{
            "openapi": "3.0.0",
            "info": { "title": "Short", "version": "1.0" },
            "paths": {
                "/x": {
                    "get": {
                        "summary": "hi"
                    }
                }
            }
        }"#
    }

    #[test]
    fn parse_openapi3_json() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), oai3_json()).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.title, "Petstore");
        assert_eq!(result.version, "3.0.0");
        assert_eq!(result.endpoints.len(), 4);

        let list_pets = &result.endpoints[0];
        assert_eq!(list_pets.path, "/pets");
        assert_eq!(list_pets.method, "GET");
        assert_eq!(list_pets.summary.as_deref(), Some("List all pets"));
        assert_eq!(list_pets.operation_id.as_deref(), Some("listPets"));
        assert_eq!(list_pets.tags, vec!["pets"]);
        assert!(list_pets.embed_text.contains("List all pets"));
        assert!(list_pets.embed_text.contains("pets"));
    }

    #[test]
    fn parse_openapi3_yaml() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), oai3_yaml()).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.title, "Users API");
        assert_eq!(result.version, "3.1.0");
        assert_eq!(result.endpoints.len(), 3);

        let endpoints: Vec<_> = result
            .endpoints
            .iter()
            .map(|e| (e.path.as_str(), e.method.as_str()))
            .collect();
        assert!(endpoints.contains(&("/users", "GET")));
        assert!(endpoints.contains(&("/users", "POST")));
        assert!(endpoints.contains(&("/users/{userId}", "GET")));

        let user_get = result
            .endpoints
            .iter()
            .find(|e| e.path == "/users" && e.method == "GET")
            .unwrap();
        assert_eq!(user_get.summary.as_deref(), Some("List users"));
        assert!(
            user_get
                .embed_text
                .contains("Returns a paginated list of users")
        );
        assert!(user_get.embed_text.contains("admin"));
    }

    #[test]
    fn parse_swagger2_json() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), swagger2_json()).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.title, "Sample API");
        assert_eq!(result.version, "swagger2");
        assert_eq!(result.endpoints.len(), 3);

        let methods: Vec<&str> = result.endpoints.iter().map(|e| e.method.as_str()).collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"PUT"));
    }

    #[test]
    fn parse_swagger2_yaml() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), swagger2_yaml()).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.title, "Inventory API");
        assert_eq!(result.version, "swagger2");
        assert_eq!(result.endpoints.len(), 3);

        let delete_ep = result
            .endpoints
            .iter()
            .find(|e| e.method == "DELETE")
            .unwrap();
        assert_eq!(delete_ep.path, "/products/{productId}");
        assert_eq!(delete_ep.operation_id.as_deref(), Some("removeProduct"));
    }

    #[test]
    fn parse_malformed_yaml_returns_error() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "{{{{ bad syntax here").unwrap();
        let result = parse_spec(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_with_ref_resolves_correctly() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), oai3_with_ref()).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.endpoints.len(), 2);

        let ref_ep = result
            .endpoints
            .iter()
            .find(|e| e.path == "/items/{itemId}")
            .unwrap();
        assert_eq!(ref_ep.method, "GET");
        assert_eq!(ref_ep.summary.as_deref(), Some("List items"));
    }

    #[test]
    fn parse_ref_cycle_depth_limit() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), oai3_self_ref_cycle()).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.title, "Cycle Test");
        assert_eq!(result.endpoints.len(), 0);
    }

    #[test]
    fn parse_short_embed_skipped() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), short_summary_spec()).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.endpoints.len(), 0);
    }

    #[test]
    fn embed_text_with_no_summary_or_description() {
        let spec = r#"{
            "openapi": "3.0.0",
            "info": { "title": "Test", "version": "1.0" },
            "paths": {
                "/health": {
                    "get": {},
                    "post": { "tags": ["system"] }
                }
            }
        }"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), spec).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.endpoints.len(), 1);

        let get_ep = result.endpoints.iter().find(|e| e.method == "GET").unwrap();
        assert_eq!(get_ep.embed_text, "GET /health");
        assert!(get_ep.summary.is_none());
    }

    #[test]
    fn endpoint_tags_extracted() {
        let spec = r#"{
            "openapi": "3.0.0",
            "info": { "title": "TagTest", "version": "1.0" },
            "paths": {
                "/metrics": {
                    "get": {
                        "summary": "Get metrics for the system",
                        "tags": ["monitoring", "internal"]
                    }
                }
            }
        }"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), spec).unwrap();
        let result = parse_spec(tmp.path()).unwrap();

        assert_eq!(result.endpoints.len(), 1);
        let ep = &result.endpoints[0];
        assert_eq!(ep.tags, vec!["monitoring", "internal"]);
        assert!(ep.embed_text.contains("monitoring"));
        assert!(ep.embed_text.contains("internal"));
    }
}
