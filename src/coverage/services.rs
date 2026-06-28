use crate::impact::packet::{ApiRoute, DataModel, Service};
use crate::index::call_graph::CallGraph;
use crate::index::topology::DirectoryClassification;

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// K4: Service Boundary Detection (Marker-based)
// ---------------------------------------------------------------------------

/// Identifies the kind of manifest that demarcates a service root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryMarker {
    CargoWorkspace,
    NpmPackage,
    GoModule,
    MavenPom,
    Dockerfile,
}

impl BoundaryMarker {
    pub fn as_str(&self) -> &'static str {
        match self {
            BoundaryMarker::CargoWorkspace => "CARGO_WORKSPACE",
            BoundaryMarker::NpmPackage => "NPM_PACKAGE",
            BoundaryMarker::GoModule => "GO_MODULE",
            BoundaryMarker::MavenPom => "MAVEN_POM",
            BoundaryMarker::Dockerfile => "DOCKERFILE",
        }
    }

    /// Confidence weight for this marker kind. Manifest files score higher.
    pub fn confidence(&self) -> f64 {
        match self {
            BoundaryMarker::CargoWorkspace => 0.95,
            BoundaryMarker::NpmPackage => 0.95,
            BoundaryMarker::GoModule => 0.95,
            BoundaryMarker::MavenPom => 0.90,
            BoundaryMarker::Dockerfile => 0.75,
        }
    }
}

/// A detected service boundary from file-system marker scanning.
#[derive(Debug, Clone)]
pub struct DetectedBoundary {
    pub dir_path: PathBuf,
    /// Inferred service name (directory name or package name from manifest).
    pub name: String,
    pub marker: BoundaryMarker,
    pub confidence: f64,
}

/// Walks a repository tree to detect service boundaries via manifest markers.
pub struct BoundaryDetector {
    /// Root of the repository to scan.
    pub root: PathBuf,
}

impl BoundaryDetector {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Recursively walk the tree and return all detected service boundaries.
    /// Skips `node_modules`, `target`, `vendor`, `.git`.
    pub fn detect(&self) -> Vec<DetectedBoundary> {
        let mut results = Vec::new();
        self.walk_dir(&self.root, &mut results, 0);
        // Sort for determinism
        results.sort_by(|a, b| a.dir_path.cmp(&b.dir_path));
        results
    }

    fn walk_dir(&self, dir: &Path, results: &mut Vec<DetectedBoundary>, depth: usize) {
        if depth > 8 {
            return;
        }
        let skip_names: &[&str] = &[
            "node_modules",
            "target",
            "vendor",
            ".git",
            "dist",
            "build",
            ".cache",
        ];

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut child_dirs: Vec<PathBuf> = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if !skip_names.contains(&name.as_str()) {
                    child_dirs.push(path);
                }
            } else if path.is_file() {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let marker_opt = detect_marker_file(file_name, &path);
                if let Some((marker, name)) = marker_opt {
                    let confidence = marker.confidence();
                    results.push(DetectedBoundary {
                        dir_path: dir.to_path_buf(),
                        name,
                        marker,
                        confidence,
                    });
                }
            }
        }

        for child in child_dirs {
            self.walk_dir(&child, results, depth + 1);
        }
    }
}

/// Returns `Some((marker, service_name))` if the file is a boundary marker.
fn detect_marker_file(file_name: &str, full_path: &Path) -> Option<(BoundaryMarker, String)> {
    match file_name {
        "Cargo.toml" => {
            let content = std::fs::read_to_string(full_path).unwrap_or_default();
            let name = parse_cargo_name(&content).unwrap_or_else(|| {
                full_path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("service")
                    .to_string()
            });
            Some((BoundaryMarker::CargoWorkspace, name))
        }
        "package.json" => {
            let content = std::fs::read_to_string(full_path).unwrap_or_default();
            let name = parse_npm_name(&content).unwrap_or_else(|| {
                full_path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("service")
                    .to_string()
            });
            Some((BoundaryMarker::NpmPackage, name))
        }
        "go.mod" => {
            let content = std::fs::read_to_string(full_path).unwrap_or_default();
            let name = parse_go_module_name(&content).unwrap_or_else(|| {
                full_path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("service")
                    .to_string()
            });
            Some((BoundaryMarker::GoModule, name))
        }
        "pom.xml" => {
            let dir_name = full_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("service")
                .to_string();
            Some((BoundaryMarker::MavenPom, dir_name))
        }
        "Dockerfile" | "dockerfile" => {
            let dir_name = full_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("service")
                .to_string();
            Some((BoundaryMarker::Dockerfile, dir_name))
        }
        _ => None,
    }
}

fn parse_cargo_name(content: &str) -> Option<String> {
    content
        .lines()
        .skip_while(|l| !l.trim().starts_with("[package]"))
        .skip(1)
        .find(|l| l.trim().starts_with("name"))
        .and_then(|l| l.split_once('=').map(|x| x.1))
        .map(|v| v.trim().trim_matches('"').to_string())
}

fn parse_npm_name(content: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
}

fn parse_go_module_name(content: &str) -> Option<String> {
    content
        .lines()
        .find(|l| l.trim().starts_with("module "))
        .map(|l| {
            l.trim()
                .trim_start_matches("module ")
                .split('/')
                .next_back()
                .unwrap_or("service")
                .to_string()
        })
}

// ---------------------------------------------------------------------------
// K4: HTTP Client Call Detection (Communication Extraction)
// ---------------------------------------------------------------------------

/// Represents an outbound HTTP call detected in source code.
#[derive(Debug, Clone)]
pub struct HttpClientCall {
    /// The source file containing the call.
    pub source_file: String,
    /// URL pattern or target (may be partial string literal).
    pub target_pattern: String,
    /// Call kind: "RUST_UREQ", "RUST_REQWEST", "TS_FETCH", "TS_AXIOS", "PY_REQUESTS", "PY_HTTPX"
    pub call_kind: String,
    pub confidence: f64,
}

/// Scans source file content for HTTP client call patterns.
/// Returns a list of detected outbound calls.
pub fn detect_http_client_calls(file_path: &str, content: &str) -> Vec<HttpClientCall> {
    let mut calls = Vec::new();
    let lower = content.to_lowercase();

    // Rust: ureq
    for pattern in &[
        "ureq::get(",
        "ureq::post(",
        "ureq::put(",
        "ureq::delete(",
        "ureq::patch(",
    ] {
        if lower.contains(pattern) {
            calls.push(HttpClientCall {
                source_file: file_path.to_string(),
                target_pattern: extract_url_arg(content, pattern).unwrap_or_default(),
                call_kind: "RUST_UREQ".to_string(),
                confidence: 0.85,
            });
        }
    }

    // Rust: reqwest
    for pattern in &["reqwest::get(", "reqwest::post(", "reqwest::Client"] {
        if lower.contains(pattern) {
            calls.push(HttpClientCall {
                source_file: file_path.to_string(),
                target_pattern: extract_url_arg(content, pattern).unwrap_or_default(),
                call_kind: "RUST_REQWEST".to_string(),
                confidence: 0.85,
            });
        }
    }

    // TypeScript/JavaScript: fetch
    if lower.contains("fetch(") {
        calls.push(HttpClientCall {
            source_file: file_path.to_string(),
            target_pattern: extract_url_arg(content, "fetch(").unwrap_or_default(),
            call_kind: "TS_FETCH".to_string(),
            confidence: 0.80,
        });
    }

    // TypeScript/JavaScript: axios
    for pattern in &[
        "axios.get(",
        "axios.post(",
        "axios.put(",
        "axios.delete(",
        "axios.patch(",
        "axios.request(",
    ] {
        if lower.contains(pattern) {
            calls.push(HttpClientCall {
                source_file: file_path.to_string(),
                target_pattern: extract_url_arg(content, pattern).unwrap_or_default(),
                call_kind: "TS_AXIOS".to_string(),
                confidence: 0.85,
            });
        }
    }

    // Python: requests
    for pattern in &[
        "requests.get(",
        "requests.post(",
        "requests.put(",
        "requests.delete(",
        "requests.patch(",
    ] {
        if lower.contains(pattern) {
            calls.push(HttpClientCall {
                source_file: file_path.to_string(),
                target_pattern: extract_url_arg(content, pattern).unwrap_or_default(),
                call_kind: "PY_REQUESTS".to_string(),
                confidence: 0.85,
            });
        }
    }

    // Python: httpx
    for pattern in &[
        "httpx.get(",
        "httpx.post(",
        "httpx.put(",
        "httpx.delete(",
        "httpx.patch(",
        "httpx.client(",
        "httpx.asyncclient(",
    ] {
        if lower.contains(pattern) {
            calls.push(HttpClientCall {
                source_file: file_path.to_string(),
                target_pattern: extract_url_arg(content, pattern).unwrap_or_default(),
                call_kind: "PY_HTTPX".to_string(),
                confidence: 0.85,
            });
        }
    }

    calls
}

/// Attempts to extract the first string literal argument from a call pattern.
fn extract_url_arg(content: &str, pattern: &str) -> Option<String> {
    let lower = content.to_lowercase();
    let pos = lower.find(pattern)?;
    let after = &content[pos + pattern.len()..];
    // Look for a string literal (quoted)
    let after_trimmed = after.trim_start();
    if let Some(stripped) = after_trimmed.strip_prefix('"') {
        let end = stripped.find('"')?;
        return Some(stripped[..end].to_string());
    }
    if let Some(stripped) = after_trimmed.strip_prefix('\'') {
        let end = stripped.find('\'')?;
        return Some(stripped[..end].to_string());
    }
    // Return empty if no string literal found
    None
}

#[derive(Debug, Clone)]
pub struct DataModelSource {
    pub model: DataModel,
    pub source_path: String,
}

pub struct DirectoryTopology {
    pub classifications: Vec<DirectoryClassification>,
}

/// Infer service boundaries from routes, call graph, and directory topology.
pub fn infer_services(
    routes: &[ApiRoute],
    data_models: &[DataModelSource],
    call_graph: &CallGraph,
    topology: &DirectoryTopology,
    declared_services: &[crate::config::model::ServiceDefinition],
) -> Vec<Service> {
    if routes.is_empty()
        && call_graph.edges.is_empty()
        && topology.classifications.is_empty()
        && declared_services.is_empty()
    {
        return Vec::new();
    }

    let mut service_groups: std::collections::HashMap<PathBuf, (String, Vec<String>, Vec<String>)> =
        std::collections::HashMap::new();

    // 0. Pre-populate services from explicit ServiceRoot topology
    for class in &topology.classifications {
        if class.role == crate::index::topology::DirectoryRole::ServiceRoot {
            let path = PathBuf::from(&class.dir_path);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "root".to_string());
            service_groups.insert(path, (name, Vec::new(), Vec::new()));
        }
    }

    // 0.5 Pre-populate from declared services in config
    for ds in declared_services {
        let path = PathBuf::from(&ds.root);
        service_groups.insert(path, (ds.name.clone(), Vec::new(), Vec::new()));
    }

    // 1. Group routes by their service root directory
    for route in routes {
        let (root_dir, name) = find_best_service_root(Path::new(&route.route_source), topology);
        let entry = service_groups
            .entry(root_dir)
            .or_insert_with(|| (name, Vec::new(), Vec::new()));
        if let Some(handler) = &route.handler_symbol_name
            && !entry.1.contains(handler)
        {
            entry.1.push(handler.clone());
        }
    }

    // 1.5 Detect "worker" services using CallGraph (background logic with no routes)
    for edge in &call_graph.edges {
        let (root_dir, name) = find_best_service_root(&edge.caller_file, topology);
        let entry = service_groups
            .entry(root_dir)
            .or_insert_with(|| (name, Vec::new(), Vec::new()));

        // Add caller as a "route" (logical entrypoint) if not already present
        if !entry.1.contains(&edge.caller_name) {
            entry.1.push(edge.caller_name.clone());
        }
    }

    // 2. Associate data models with services
    for dm_source in data_models {
        let (dm_root_dir, _) = find_best_service_root(Path::new(&dm_source.source_path), topology);
        if let Some(entry) = service_groups.get_mut(&dm_root_dir)
            && !entry.2.contains(&dm_source.model.model_name)
        {
            entry.2.push(dm_source.model.model_name.clone());
        }
    }

    // 3. Map groups to Service structs
    let mut services: Vec<Service> = Vec::new();
    let mut unnamed_count = 0;

    let mut sorted_keys: Vec<_> = service_groups.keys().collect();
    sorted_keys.sort();

    for key in sorted_keys {
        let (name, routes, dms) = &service_groups[key];
        let mut final_name = name.clone();

        if final_name.is_empty() {
            if let Some(pkg_name) = find_package_name(key) {
                final_name = pkg_name;
            } else {
                unnamed_count += 1;
                final_name = format!("unnamed-service-{}", unnamed_count);
            }
        }

        // Find declared service for this root
        let declared = declared_services
            .iter()
            .find(|ds| std::path::Path::new(&ds.root) == key);

        services.push(Service {
            name: final_name,
            directory: key.clone(),
            routes: routes.clone(),
            data_models: dms.clone(),
            owners: declared.map(|d| d.owners.clone()).unwrap_or_default(),
            runtime_name: declared.and_then(|d| d.runtime_name.clone()),
            queues: declared.map(|d| d.queues.clone()).unwrap_or_default(),
            topics: declared.map(|d| d.topics.clone()).unwrap_or_default(),
            rpc_endpoints: declared
                .map(|d| d.rpc_endpoints.clone())
                .unwrap_or_default(),
        });
    }

    services.sort_by(|a, b| a.name.cmp(&b.name));
    services
}

fn get_root_dir(path: &Path) -> PathBuf {
    let dir = if path.extension().is_some() {
        path.parent().unwrap_or(Path::new("")).to_path_buf()
    } else {
        path.to_path_buf()
    };

    let components: Vec<_> = dir.components().collect();
    if components.len() > 3 {
        components.iter().take(3).collect()
    } else {
        dir
    }
}

fn find_best_service_root(path: &Path, topology: &DirectoryTopology) -> (PathBuf, String) {
    // 1. Check topology for any prefix that is a ServiceRoot (deepest first)
    let mut current = if path.extension().is_some() {
        path.parent().unwrap_or(Path::new("")).to_path_buf()
    } else {
        path.to_path_buf()
    };

    loop {
        let current_str = current.to_string_lossy().replace('\\', "/");
        if let Some(_class) = topology.classifications.iter().find(|c| {
            (c.dir_path == current_str || (current_str.is_empty() && c.dir_path == "."))
                && c.role == crate::index::topology::DirectoryRole::ServiceRoot
        }) {
            let name = current
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "root".to_string());
            return (current, name);
        }

        if let Some(parent) = current.parent() {
            if parent == current || current == Path::new("") {
                break;
            }
            current = parent.to_path_buf();
        } else {
            break;
        }
    }

    // 2. Fallback to default heuristic
    get_service_root_and_name(&path.to_string_lossy())
}

fn get_service_root_and_name(source_path: &str) -> (PathBuf, String) {
    let path = PathBuf::from(source_path);
    let root_dir = get_root_dir(&path);

    // Rule 3: If directory is just "src", name it "src"
    if root_dir == Path::new("src") {
        return (root_dir, "src".to_string());
    }

    // Rule 1: Last component of the directory path
    if let Some(last) = root_dir.file_name() {
        let name = last.to_string_lossy().to_string();
        if !name.is_empty() && name != "." && name != ".." {
            return (root_dir, name);
        }
    }

    (root_dir, "".to_string())
}

fn find_package_name(dir: &Path) -> Option<String> {
    let mut current = dir;
    loop {
        for filename in &["Cargo.toml", "package.json"] {
            let p = current.join(filename);
            if let Ok(content) = std::fs::read_to_string(&p) {
                if *filename == "Cargo.toml" {
                    if let Ok(value) = toml::from_str::<toml::Value>(&content)
                        && let Some(name) = value
                            .get("package")
                            .and_then(|v| v.get("name"))
                            .and_then(|v| v.as_str())
                    {
                        return Some(name.to_string());
                    }
                } else if *filename == "package.json"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&content)
                    && let Some(name) = value.get("name").and_then(|v| v.as_str())
                {
                    return Some(name.to_string());
                }
            }
        }

        if current.join("__init__.py").exists()
            && let Some(name) = current.file_name().and_then(|n| n.to_str())
        {
            return Some(name.to_string());
        }

        if let Some(parent) = current.parent() {
            if parent == current || current == Path::new("") {
                break;
            }
            current = parent;
        } else {
            break;
        }
    }
    None
}

/// Compute cross-service dependency edges from call graph.
pub fn compute_cross_service_edges(
    services: &[Service],
    call_graph: &CallGraph,
) -> Vec<(String, String, usize)> {
    let mut edges: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for edge in &call_graph.edges {
        if let (Some(caller_svc), Some(callee_svc)) = (
            service_for_symbol(services, &edge.caller_name, Some(&edge.caller_file)),
            service_for_symbol(services, &edge.callee_name, edge.callee_file.as_deref()),
        ) && caller_svc != callee_svc
        {
            *edges.entry((caller_svc, callee_svc)).or_insert(0) += 1;
        }
    }

    let mut result: Vec<_> = edges
        .into_iter()
        .map(|((a, b), count)| (a, b, count))
        .collect();

    result.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    result
}

fn service_for_symbol(
    services: &[Service],
    symbol_name: &str,
    file_path: Option<&Path>,
) -> Option<String> {
    if let Some(file_path) = file_path {
        let mut matches: Vec<&Service> = services
            .iter()
            .filter(|service| path_belongs_to_service(file_path, &service.directory))
            .collect();
        matches.sort_by(|a, b| {
            b.directory
                .components()
                .count()
                .cmp(&a.directory.components().count())
        });
        for service in matches {
            if service.routes.iter().any(|route| route == symbol_name)
                || service.data_models.iter().any(|model| model == symbol_name)
            {
                return Some(service.name.clone());
            }
        }
    }

    let mut matching_services: Vec<&Service> = services
        .iter()
        .filter(|service| {
            service.routes.iter().any(|route| route == symbol_name)
                || service.data_models.iter().any(|model| model == symbol_name)
        })
        .collect();
    matching_services.sort_by(|a, b| a.name.cmp(&b.name));
    if matching_services.len() == 1 {
        Some(matching_services[0].name.clone())
    } else {
        None
    }
}

fn path_belongs_to_service(file_path: &Path, service_dir: &Path) -> bool {
    if service_dir.as_os_str().is_empty() || service_dir == Path::new(".") {
        return file_path
            .parent()
            .is_none_or(|parent| parent.as_os_str().is_empty());
    }
    file_path == service_dir || file_path.starts_with(service_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::call_graph::{CallEdge, CallKind, ResolutionStatus};
    use crate::index::topology::DirectoryRole;

    #[test]
    fn test_infer_services_directory_naming() {
        let routes = vec![ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/users".to_string(),
            handler_symbol_name: Some("get_users".to_string()),
            framework: "actix-web".to_string(),
            route_source: "src/api/users/mod.rs".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: String::new(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        }];
        let topology = DirectoryTopology {
            classifications: vec![DirectoryClassification {
                dir_path: "src/api/users".to_string(),
                role: DirectoryRole::Source,
                confidence: 1.0,
                evidence: "test".to_string(),
            }],
        };
        let call_graph = CallGraph { edges: Vec::new() };

        let services = infer_services(&routes, &[], &call_graph, &topology, &[]);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "users");
    }

    #[test]
    fn test_infer_services_flat_repo() {
        let routes = vec![ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/".to_string(),
            handler_symbol_name: Some("index".to_string()),
            framework: "actix-web".to_string(),
            route_source: "src/handler.rs".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: String::new(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        }];
        let topology = DirectoryTopology {
            classifications: vec![DirectoryClassification {
                dir_path: "src".to_string(),
                role: DirectoryRole::Source,
                confidence: 1.0,
                evidence: "test".to_string(),
            }],
        };
        let call_graph = CallGraph { edges: Vec::new() };

        let services = infer_services(&routes, &[], &call_graph, &topology, &[]);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "src");
    }

    #[test]
    fn test_infer_services_empty() {
        let services = infer_services(
            &[],
            &[],
            &CallGraph { edges: Vec::new() },
            &DirectoryTopology {
                classifications: Vec::new(),
            },
            &[],
        );
        assert!(services.is_empty());
    }

    #[test]
    fn test_infer_services_topology_only_service_root() {
        let topology = DirectoryTopology {
            classifications: vec![DirectoryClassification {
                dir_path: "services/billing".to_string(),
                role: DirectoryRole::ServiceRoot,
                confidence: 1.0,
                evidence: "test".to_string(),
            }],
        };

        let services = infer_services(&[], &[], &CallGraph { edges: Vec::new() }, &topology, &[]);

        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "billing");
        assert_eq!(services[0].directory, PathBuf::from("services/billing"));
    }

    #[test]
    fn test_infer_services_monorepo_depth_cap() {
        let routes = vec![ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/auth".to_string(),
            handler_symbol_name: Some("login".to_string()),
            framework: "actix-web".to_string(),
            route_source: "src/api/users/auth/mod.rs".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: String::new(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        }];
        let topology = DirectoryTopology {
            classifications: vec![],
        };
        let call_graph = CallGraph { edges: Vec::new() };

        let services = infer_services(&routes, &[], &call_graph, &topology, &[]);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "users");
        assert_eq!(services[0].directory, PathBuf::from("src/api/users"));
    }

    #[test]
    fn test_infer_services_with_data_models() {
        let routes = vec![ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/users".to_string(),
            handler_symbol_name: Some("get_users".to_string()),
            framework: "actix-web".to_string(),
            route_source: "src/api/users/handlers.rs".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: String::new(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        }];
        let data_models = vec![DataModelSource {
            model: DataModel {
                model_name: "User".to_string(),
                model_kind: "STRUCT".to_string(),
                confidence: 1.0,
                evidence: None,
            },
            source_path: "src/api/users/models.rs".to_string(),
        }];
        let topology = DirectoryTopology {
            classifications: vec![],
        };
        let call_graph = CallGraph { edges: Vec::new() };

        let services = infer_services(&routes, &data_models, &call_graph, &topology, &[]);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].data_models, vec!["User".to_string()]);
    }

    #[test]
    fn test_infer_services_package_name_fallback() {
        use std::fs;
        let temp = tempfile::tempdir().unwrap();
        let repo_dir = temp.path();
        fs::write(repo_dir.join("package.json"), r#"{"name": "from-json"}"#).unwrap();

        let routes = vec![ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/".to_string(),
            handler_symbol_name: Some("h".to_string()),
            framework: "f".to_string(),
            route_source: "handler.rs".to_string(), // In root, so dir is ""
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: String::new(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        }];

        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(repo_dir).unwrap();

        let topology = DirectoryTopology {
            classifications: vec![],
        };
        let call_graph = CallGraph { edges: Vec::new() };

        let services = infer_services(&routes, &[], &call_graph, &topology, &[]);

        std::env::set_current_dir(old_cwd).unwrap();

        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "from-json");
    }

    #[test]
    fn test_compute_cross_service_edges() {
        let services = vec![
            Service {
                name: "users".to_string(),
                routes: vec!["get_users".to_string()],
                data_models: vec![],
                directory: PathBuf::from("src/api/users"),
                owners: vec![],
                runtime_name: None,
                queues: vec![],
                topics: vec![],
                rpc_endpoints: vec![],
            },
            Service {
                name: "billing".to_string(),
                routes: vec!["charge".to_string()],
                data_models: vec![],
                directory: PathBuf::from("src/api/billing"),
                owners: vec![],
                runtime_name: None,
                queues: vec![],
                topics: vec![],
                rpc_endpoints: vec![],
            },
        ];

        let call_graph = CallGraph {
            edges: vec![CallEdge {
                caller_name: "get_users".to_string(),
                caller_file: PathBuf::from("src/api/users/handlers.rs"),
                callee_name: "charge".to_string(),
                callee_file: Some(PathBuf::from("src/api/billing/handlers.rs")),
                call_kind: CallKind::Direct,
                resolution_status: ResolutionStatus::Resolved,
                confidence: 1.0,
                evidence: "test".to_string(),
            }],
        };

        let edges = compute_cross_service_edges(&services, &call_graph);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0], ("users".to_string(), "billing".to_string(), 1));
    }

    #[test]
    fn test_compute_cross_service_edges_collapsed() {
        let services = vec![
            Service {
                name: "users".to_string(),
                routes: vec!["get_users".to_string(), "create_user".to_string()],
                data_models: vec![],
                directory: PathBuf::from("src/api/users"),
                owners: vec![],
                runtime_name: None,
                queues: vec![],
                topics: vec![],
                rpc_endpoints: vec![],
            },
            Service {
                name: "billing".to_string(),
                routes: vec!["charge".to_string()],
                data_models: vec![],
                directory: PathBuf::from("src/api/billing"),
                owners: vec![],
                runtime_name: None,
                queues: vec![],
                topics: vec![],
                rpc_endpoints: vec![],
            },
        ];

        let call_graph = CallGraph {
            edges: vec![
                CallEdge {
                    caller_name: "get_users".to_string(),
                    caller_file: PathBuf::from("src/api/users/handlers.rs"),
                    callee_name: "charge".to_string(),
                    callee_file: Some(PathBuf::from("src/api/billing/handlers.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "test".to_string(),
                },
                CallEdge {
                    caller_name: "create_user".to_string(),
                    caller_file: PathBuf::from("src/api/users/handlers.rs"),
                    callee_name: "charge".to_string(),
                    callee_file: Some(PathBuf::from("src/api/billing/handlers.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "test".to_string(),
                },
            ],
        };

        let edges = compute_cross_service_edges(&services, &call_graph);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0], ("users".to_string(), "billing".to_string(), 2));
    }

    #[test]
    fn test_compute_cross_service_edges_uses_file_path_for_duplicate_symbols() {
        let services = vec![
            Service {
                name: "frontend".to_string(),
                routes: vec!["index".to_string(), "render".to_string()],
                data_models: vec![],
                directory: PathBuf::from("src/frontend"),
                owners: vec![],
                runtime_name: None,
                queues: vec![],
                topics: vec![],
                rpc_endpoints: vec![],
            },
            Service {
                name: "backend".to_string(),
                routes: vec!["index".to_string(), "serve".to_string()],
                data_models: vec![],
                directory: PathBuf::from("src/backend"),
                owners: vec![],
                runtime_name: None,
                queues: vec![],
                topics: vec![],
                rpc_endpoints: vec![],
            },
        ];

        let call_graph = CallGraph {
            edges: vec![
                CallEdge {
                    caller_name: "render".to_string(),
                    caller_file: PathBuf::from("src/frontend/render.rs"),
                    callee_name: "index".to_string(),
                    callee_file: Some(PathBuf::from("src/backend/index.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "test".to_string(),
                },
                CallEdge {
                    caller_name: "serve".to_string(),
                    caller_file: PathBuf::from("src/backend/serve.rs"),
                    callee_name: "index".to_string(),
                    callee_file: Some(PathBuf::from("src/frontend/index.rs")),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "test".to_string(),
                },
            ],
        };

        let edges = compute_cross_service_edges(&services, &call_graph);

        assert_eq!(edges.len(), 2);
        assert!(edges.contains(&("backend".to_string(), "frontend".to_string(), 1)));
        assert!(edges.contains(&("frontend".to_string(), "backend".to_string(), 1)));
    }
}

#[cfg(test)]
mod k4_tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // BoundaryDetector tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_boundary_detector_cargo_toml() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // Create a Cargo.toml at root (the project itself)
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"my-service\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let detector = BoundaryDetector::new(root);
        let boundaries = detector.detect();

        assert_eq!(boundaries.len(), 1);
        assert_eq!(boundaries[0].name, "my-service");
        assert_eq!(boundaries[0].marker, BoundaryMarker::CargoWorkspace);
        assert!((boundaries[0].confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn test_boundary_detector_multiple_services() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // services/orders has Cargo.toml
        let orders_dir = root.join("services").join("orders");
        std::fs::create_dir_all(&orders_dir).unwrap();
        std::fs::write(
            orders_dir.join("Cargo.toml"),
            "[package]\nname = \"orders-service\"\n",
        )
        .unwrap();

        // services/frontend has package.json
        let frontend_dir = root.join("services").join("frontend");
        std::fs::create_dir_all(&frontend_dir).unwrap();
        std::fs::write(
            frontend_dir.join("package.json"),
            r#"{"name":"frontend-app","version":"1.0.0"}"#,
        )
        .unwrap();

        // services/gateway has Dockerfile
        let gateway_dir = root.join("services").join("gateway");
        std::fs::create_dir_all(&gateway_dir).unwrap();
        std::fs::write(gateway_dir.join("Dockerfile"), "FROM alpine:latest\n").unwrap();

        let detector = BoundaryDetector::new(root);
        let mut boundaries = detector.detect();
        boundaries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(boundaries.len(), 3);

        let names: Vec<&str> = boundaries.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"orders-service"));
        assert!(names.contains(&"frontend-app"));
        assert!(names.contains(&"gateway"));

        let cargo_boundary = boundaries
            .iter()
            .find(|b| b.name == "orders-service")
            .unwrap();
        assert_eq!(cargo_boundary.marker, BoundaryMarker::CargoWorkspace);

        let npm_boundary = boundaries
            .iter()
            .find(|b| b.name == "frontend-app")
            .unwrap();
        assert_eq!(npm_boundary.marker, BoundaryMarker::NpmPackage);

        let docker_boundary = boundaries.iter().find(|b| b.name == "gateway").unwrap();
        assert_eq!(docker_boundary.marker, BoundaryMarker::Dockerfile);
        assert!((docker_boundary.confidence - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_boundary_detector_skips_target() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // Cargo.toml in target/ should be skipped
        let target_dir = root.join("target").join("debug");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(
            target_dir.join("Cargo.toml"),
            "[package]\nname = \"skip-me\"\n",
        )
        .unwrap();

        let detector = BoundaryDetector::new(root);
        let boundaries = detector.detect();

        assert!(
            boundaries.iter().all(|b| b.name != "skip-me"),
            "Should skip Cargo.toml inside target/"
        );
    }

    #[test]
    fn test_parse_cargo_name() {
        let toml = "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n[dependencies]\n";
        assert_eq!(parse_cargo_name(toml), Some("my-crate".to_string()));
    }

    #[test]
    fn test_parse_npm_name() {
        let json = r#"{"name": "my-package", "version": "1.0.0"}"#;
        assert_eq!(parse_npm_name(json), Some("my-package".to_string()));
    }

    #[test]
    fn test_parse_go_module_name() {
        let go_mod = "module github.com/myorg/my-service\n\ngo 1.21\n";
        assert_eq!(parse_go_module_name(go_mod), Some("my-service".to_string()));
    }

    // ---------------------------------------------------------------------------
    // HTTP client detection tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_detect_http_client_calls_ureq() {
        let content = r#"
            let resp = ureq::get("https://api.example.com/users")
                .call()
                .unwrap();
        "#;
        let calls = detect_http_client_calls("src/main.rs", content);
        assert!(!calls.is_empty());
        assert!(calls.iter().any(|c| c.call_kind == "RUST_UREQ"));
    }

    #[test]
    fn test_detect_http_client_calls_fetch() {
        let content = r#"
            const resp = await fetch("https://api.example.com/orders");
        "#;
        let calls = detect_http_client_calls("src/client.ts", content);
        assert!(!calls.is_empty());
        let fetch_calls: Vec<_> = calls.iter().filter(|c| c.call_kind == "TS_FETCH").collect();
        assert!(!fetch_calls.is_empty());
        assert_eq!(
            fetch_calls[0].target_pattern,
            "https://api.example.com/orders"
        );
    }

    #[test]
    fn test_detect_http_client_calls_requests() {
        let content = "import requests\nresp = requests.get('https://payments.internal/charge')\n";
        let calls = detect_http_client_calls("service/client.py", content);
        assert!(!calls.is_empty());
        assert!(calls.iter().any(|c| c.call_kind == "PY_REQUESTS"));
    }

    #[test]
    fn test_detect_http_client_calls_none() {
        let content = "fn no_http() { println!(\"hello\"); }";
        let calls = detect_http_client_calls("src/no_http.rs", content);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_detect_http_client_calls_axios() {
        let content = "const data = await axios.post('/api/orders', payload);";
        let calls = detect_http_client_calls("src/api.js", content);
        let axios_calls: Vec<_> = calls.iter().filter(|c| c.call_kind == "TS_AXIOS").collect();
        assert!(!axios_calls.is_empty());
    }

    #[test]
    fn test_boundary_marker_confidence() {
        assert!((BoundaryMarker::CargoWorkspace.confidence() - 0.95).abs() < 1e-9);
        assert!((BoundaryMarker::NpmPackage.confidence() - 0.95).abs() < 1e-9);
        assert!((BoundaryMarker::GoModule.confidence() - 0.95).abs() < 1e-9);
        assert!((BoundaryMarker::MavenPom.confidence() - 0.90).abs() < 1e-9);
        assert!((BoundaryMarker::Dockerfile.confidence() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_boundary_marker_as_str() {
        assert_eq!(BoundaryMarker::CargoWorkspace.as_str(), "CARGO_WORKSPACE");
        assert_eq!(BoundaryMarker::NpmPackage.as_str(), "NPM_PACKAGE");
        assert_eq!(BoundaryMarker::GoModule.as_str(), "GO_MODULE");
        assert_eq!(BoundaryMarker::MavenPom.as_str(), "MAVEN_POM");
        assert_eq!(BoundaryMarker::Dockerfile.as_str(), "DOCKERFILE");
    }
}
