use crate::impact::packet::{ChangedFile, DeployManifestChange, ManifestType};
use globset::{Glob, GlobSetBuilder};
use std::collections::HashSet;
use std::path::Path;
use tracing::warn;

const HIGH_BLAST_RESOURCES: &[&str] = &[
    "aws_rds_cluster",
    "kubernetes_deployment",
    "google_compute_instance",
    "azurerm_kubernetes_cluster",
];

pub fn classify_deploy_manifest(file_path: &Path) -> Option<ManifestType> {
    let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let path_str = file_path
        .to_string_lossy()
        .to_lowercase()
        .replace('\\', "/");

    let is_yaml = file_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("yml") || e.eq_ignore_ascii_case("yaml"))
        .unwrap_or(false);

    // Non-YAML files can be classified by name/path alone.
    if !is_yaml {
        if file_name.eq_ignore_ascii_case("dockerfile")
            || file_name.to_lowercase().starts_with("dockerfile.")
        {
            return Some(ManifestType::Dockerfile);
        }
        if path_str.ends_with(".tf") || path_str.ends_with(".tfvars") {
            return Some(ManifestType::Terraform);
        }
        return None;
    }

    // YAML files must be parseable before classification.
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return None,
    };
    if serde_yaml::from_str::<serde_yaml::Value>(&content).is_err() {
        return None;
    }

    if file_name.to_lowercase().starts_with("docker-compose")
        && (file_name.ends_with(".yml") || file_name.ends_with(".yaml"))
    {
        return Some(ManifestType::DockerCompose);
    }
    if (path_str.contains("/k8s/") || path_str.contains("/kubernetes/")) && is_yaml {
        return Some(ManifestType::Kubernetes);
    }
    if (path_str.contains("/helm/") || file_name.eq_ignore_ascii_case("chart.yaml")) && is_yaml {
        return Some(ManifestType::Helm);
    }
    if path_str.contains(".github/workflows/") && is_yaml {
        return Some(ManifestType::CiWorkflow);
    }

    None
}

pub fn scan_dockerfile_directives(content: &str, changed_source_files: &[&str]) -> Vec<String> {
    let mut matched = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let directive = parts[0].to_uppercase();
        if (directive == "COPY" || directive == "ADD") && parts.len() >= 2 {
            let sources = extract_copy_sources(&parts[1..]);
            for src in &sources {
                for changed in changed_source_files {
                    if src_matches_changed(src, changed) {
                        matched.push(src.clone());
                    }
                }
            }
        }
    }
    matched.sort_unstable();
    matched.dedup();
    matched
}

fn extract_copy_sources(args: &[&str]) -> Vec<String> {
    let joined = args.join(" ");
    if joined.starts_with('[') && joined.ends_with(']') {
        let inner = &joined[1..joined.len() - 1];
        let parts: Vec<String> = inner
            .split(',')
            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
            .collect();
        if parts.len() >= 2 {
            parts[..parts.len() - 1].to_vec()
        } else {
            Vec::new()
        }
    } else if args.len() >= 2 {
        args[..args.len() - 1]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    }
}

fn src_matches_changed(src: &str, changed: &str) -> bool {
    let src_norm = src.trim_start_matches('.').trim_start_matches('/');
    let changed_norm = changed.trim_start_matches('.').trim_start_matches('/');
    changed_norm.starts_with(src_norm) || src_norm == changed_norm
}

pub fn scan_terraform_resources(content: &str) -> Vec<(String, bool)> {
    let mut results = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("resource") {
            let parts: Vec<&str> = trimmed.split('"').collect();
            if parts.len() >= 2 {
                let resource_type = parts[1];
                let is_high = HIGH_BLAST_RESOURCES.contains(&resource_type);
                results.push((resource_type.to_string(), is_high));
            }
        }
    }
    results
}

pub fn scan_docker_compose_build_contexts(content: &str) -> Vec<String> {
    let mut contexts = Vec::new();
    if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(content)
        && let Some(services) = value.get("services").and_then(|v| v.as_mapping())
    {
        for (_, service) in services {
            if let Some(build) = service.get("build") {
                if let Some(ctx) = build.as_str() {
                    contexts.push(ctx.to_string());
                } else if let Some(mapping) = build.as_mapping()
                    && let Some(ctx) = mapping.get("context").and_then(|v| v.as_str())
                {
                    contexts.push(ctx.to_string());
                }
            }
        }
    }
    contexts
}

pub fn detect_deploy_manifest_changes(
    changed_files: &[ChangedFile],
    patterns: &[String],
    project_root: &Path,
) -> Vec<DeployManifestChange> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        match Glob::new(pat) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => {
                warn!("Invalid deploy manifest glob pattern '{}': {}", pat, e);
            }
        }
    }

    let glob_set = match builder.build() {
        Ok(set) => set,
        Err(e) => {
            warn!("Failed to build deploy manifest glob set: {}", e);
            return Vec::new();
        }
    };

    let changed_paths: Vec<String> = changed_files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    let mut changes = Vec::new();
    let mut manifest_types_seen = HashSet::new();

    for file in changed_files {
        if !glob_set.is_match(&file.path) {
            continue;
        }
        let full_path = project_root.join(&file.path);
        let manifest_type = match classify_deploy_manifest(&full_path) {
            Some(mt) => mt,
            None => continue,
        };
        manifest_types_seen.insert(manifest_type.clone());

        let content = std::fs::read_to_string(&full_path).unwrap_or_default();

        let mut risk_tier: u8 = match manifest_type {
            ManifestType::Dockerfile => 1,
            ManifestType::DockerCompose => 2,
            ManifestType::Kubernetes => 2,
            ManifestType::Terraform => 2,
            ManifestType::Helm => 2,
            ManifestType::CiWorkflow => 2,
            ManifestType::Unknown => 1,
        };

        let mut coupled_files = Vec::new();
        let mut high_blast_resources = Vec::new();

        match manifest_type {
            ManifestType::Dockerfile => {
                let changed_refs: Vec<&str> = changed_paths.iter().map(|s| s.as_str()).collect();
                let matches = scan_dockerfile_directives(&content, &changed_refs);
                if !matches.is_empty() {
                    risk_tier = risk_tier.saturating_add(1);
                    coupled_files.extend(matches);
                }
            }
            ManifestType::DockerCompose => {
                let contexts = scan_docker_compose_build_contexts(&content);
                for ctx in &contexts {
                    let dockerfile_path = if ctx == "." {
                        "Dockerfile".to_string()
                    } else {
                        let ctx_norm = ctx.trim_start_matches("./").trim_end_matches('/');
                        format!("{}/Dockerfile", ctx_norm)
                    };
                    let dockerfile_path_norm = dockerfile_path.trim_start_matches("./");
                    if changed_paths.iter().any(|p| {
                        let p_norm = p.trim_start_matches("./");
                        p_norm == dockerfile_path_norm || p_norm.ends_with(dockerfile_path_norm)
                    }) {
                        risk_tier = risk_tier.saturating_add(1);
                        coupled_files.push(dockerfile_path);
                    }
                }
            }
            ManifestType::Terraform => {
                let resources = scan_terraform_resources(&content);
                for (rtype, is_high) in resources {
                    if is_high {
                        high_blast_resources.push(rtype);
                    }
                }
                if !high_blast_resources.is_empty() {
                    risk_tier = risk_tier.saturating_add(1);
                }
            }
            _ => {}
        }

        changes.push(DeployManifestChange {
            file: file.path.clone(),
            manifest_type,
            risk_tier,
            coupled_files,
            high_blast_resources,
            service_name: None,
            owner: None,
        });
    }

    // Multi-manifest dedup: 2+ manifest types → High (tier 3)
    if manifest_types_seen.len() >= 2 {
        for change in &mut changes {
            if change.risk_tier < 3 {
                change.risk_tier = 3;
            }
        }
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::FileAnalysisStatus;
    use std::io::Write;
    use std::path::PathBuf;

    fn changed_file(path: &str) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }
    }

    #[test]
    fn test_classify_dockerfile() {
        assert_eq!(
            classify_deploy_manifest(Path::new("Dockerfile")),
            Some(ManifestType::Dockerfile)
        );
        assert_eq!(
            classify_deploy_manifest(Path::new("Dockerfile.prod")),
            Some(ManifestType::Dockerfile)
        );
    }

    #[test]
    fn test_classify_docker_compose_yml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("docker-compose.yml");
        std::fs::write(&path, "services:\n  app:\n    image: nginx\n").unwrap();
        assert_eq!(
            classify_deploy_manifest(&path),
            Some(ManifestType::DockerCompose)
        );
    }

    #[test]
    fn test_classify_main_tf() {
        assert_eq!(
            classify_deploy_manifest(Path::new("main.tf")),
            Some(ManifestType::Terraform)
        );
        assert_eq!(
            classify_deploy_manifest(Path::new("vars.tfvars")),
            Some(ManifestType::Terraform)
        );
    }

    #[test]
    fn test_classify_k8s_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let k8s_dir = dir.path().join("k8s");
        std::fs::create_dir(&k8s_dir).unwrap();
        let path = k8s_dir.join("deployment.yaml");
        std::fs::write(&path, "apiVersion: apps/v1\nkind: Deployment\n").unwrap();
        assert_eq!(
            classify_deploy_manifest(&path),
            Some(ManifestType::Kubernetes)
        );
    }

    #[test]
    fn test_classify_chart_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Chart.yaml");
        std::fs::write(&path, "apiVersion: v2\nname: my-chart\n").unwrap();
        assert_eq!(classify_deploy_manifest(&path), Some(ManifestType::Helm));
    }

    #[test]
    fn test_classify_ci_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let workflows = dir.path().join(".github").join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        let path = workflows.join("ci.yml");
        std::fs::write(&path, "name: CI\non: push\n").unwrap();
        assert_eq!(
            classify_deploy_manifest(&path),
            Some(ManifestType::CiWorkflow)
        );
    }

    #[test]
    fn test_classify_non_manifest_returns_none() {
        assert_eq!(classify_deploy_manifest(Path::new("src/main.rs")), None);
        assert_eq!(classify_deploy_manifest(Path::new("README.md")), None);
    }

    #[test]
    fn test_classify_binary_yaml_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&[0x00, 0x01, 0xFF, 0xFE]).unwrap();
        drop(f);
        assert_eq!(classify_deploy_manifest(&path), None);
    }

    #[test]
    fn test_scan_dockerfile_copy_detected() {
        let content = "COPY src/ ./src/\nADD lib/ ./lib/\n";
        let result = scan_dockerfile_directives(content, &["src/main.rs", "lib/foo.rs"]);
        assert!(result.contains(&"src/".to_string()));
        assert!(result.contains(&"lib/".to_string()));
    }

    #[test]
    fn test_scan_dockerfile_copy_not_detected() {
        let content = "COPY src/ ./src/\n";
        let result = scan_dockerfile_directives(content, &["lib/foo.rs"]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_scan_dockerfile_json_array_form() {
        let content = "COPY [\"src/\", \"./src/\"]\n";
        let result = scan_dockerfile_directives(content, &["src/main.rs"]);
        assert!(result.contains(&"src/".to_string()));
    }

    #[test]
    fn test_scan_terraform_rds_flagged() {
        let content = r#"resource "aws_rds_cluster" "db" {
  engine = "aurora"
}
resource "aws_s3_bucket" "b" {
  bucket = "foo"
}"#;
        let result = scan_terraform_resources(content);
        assert!(result.iter().any(|(t, h)| t == "aws_rds_cluster" && *h));
        assert!(result.iter().any(|(t, h)| t == "aws_s3_bucket" && !*h));
    }

    #[test]
    fn test_scan_terraform_s3_not_flagged() {
        let content = r#"resource "aws_s3_bucket" "b" {
  bucket = "foo"
}"#;
        let result = scan_terraform_resources(content);
        assert_eq!(result, vec![("aws_s3_bucket".to_string(), false)]);
    }

    #[test]
    fn test_scan_docker_compose_build_contexts() {
        let content = r#"services:
  api:
    build: ./api
  web:
    build:
      context: ./web
"#;
        let result = scan_docker_compose_build_contexts(content);
        assert!(result.contains(&"./api".to_string()));
        assert!(result.contains(&"./web".to_string()));
    }

    #[test]
    fn test_detect_deploy_changes_basic() {
        let files = vec![changed_file("Dockerfile"), changed_file("src/main.rs")];
        let patterns = vec!["**/Dockerfile*".to_string()];
        let changes = detect_deploy_manifest_changes(&files, &patterns, Path::new("."));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].manifest_type, ManifestType::Dockerfile);
        assert_eq!(changes[0].risk_tier, 1);
    }

    #[test]
    fn test_detect_deploy_changes_no_match() {
        let files = vec![changed_file("src/main.rs")];
        let patterns = vec!["**/Dockerfile*".to_string()];
        let changes = detect_deploy_manifest_changes(&files, &patterns, Path::new("."));
        assert!(changes.is_empty());
    }

    #[test]
    fn test_multi_manifest_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let dockerfile = dir.path().join("Dockerfile");
        std::fs::write(&dockerfile, "FROM alpine\n").unwrap();
        let compose = dir.path().join("docker-compose.yml");
        std::fs::write(&compose, "services:\n  app:\n    image: nginx\n").unwrap();

        let files = vec![
            changed_file("Dockerfile"),
            changed_file("docker-compose.yml"),
        ];
        let patterns = vec![
            "**/Dockerfile*".to_string(),
            "**/docker-compose*.yml".to_string(),
        ];
        let changes = detect_deploy_manifest_changes(&files, &patterns, dir.path());
        assert_eq!(changes.len(), 2);
        // Both should be tier 3 because two manifest types are present.
        assert!(changes.iter().all(|c| c.risk_tier == 3));
    }

    #[test]
    fn test_docker_compose_dockerfile_coupling() {
        let dir = tempfile::tempdir().unwrap();
        let api_dir = dir.path().join("api");
        std::fs::create_dir(&api_dir).unwrap();
        std::fs::write(api_dir.join("Dockerfile"), "FROM alpine\n").unwrap();
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  api:\n    build: ./api\n",
        )
        .unwrap();

        let files = vec![
            changed_file("docker-compose.yml"),
            changed_file("api/Dockerfile"),
        ];
        let patterns = vec![
            "**/docker-compose*.yml".to_string(),
            "**/Dockerfile*".to_string(),
        ];
        let changes = detect_deploy_manifest_changes(&files, &patterns, dir.path());
        let compose_change = changes
            .iter()
            .find(|c| c.manifest_type == ManifestType::DockerCompose)
            .expect("docker-compose change expected");
        assert!(!compose_change.coupled_files.is_empty());
        assert!(
            compose_change
                .coupled_files
                .iter()
                .any(|f| f.contains("Dockerfile"))
        );
    }

    #[test]
    fn test_dockerfile_copy_coupling() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Dockerfile"),
            "FROM alpine\nCOPY src/ ./src/\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let files = vec![changed_file("Dockerfile"), changed_file("src/main.rs")];
        let patterns = vec!["**/Dockerfile*".to_string()];
        let changes = detect_deploy_manifest_changes(&files, &patterns, dir.path());
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].risk_tier, 2); // tier 1 + 1 for COPY match
        assert!(changes[0].coupled_files.contains(&"src/".to_string()));
    }

    #[test]
    fn test_terraform_high_blast() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tf"),
            r#"resource "aws_rds_cluster" "db" {
  engine = "aurora"
}
"#,
        )
        .unwrap();

        let files = vec![changed_file("main.tf")];
        let patterns = vec!["**/*.tf".to_string()];
        let changes = detect_deploy_manifest_changes(&files, &patterns, dir.path());
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].risk_tier, 3); // tier 2 + 1 for high blast
        assert!(
            changes[0]
                .high_blast_resources
                .contains(&"aws_rds_cluster".to_string())
        );
    }
}
