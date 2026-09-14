//! Inventory writer for `deploy_manifests`.
//!
//! `risk_tier` stored here is the **base** map (Dockerfile/Unknown = 1,
//! other classified types = 2). It is **not** `deploy impact`'s coupled /
//! high-blast / multi-type bump. Checklist and surfaces read `COUNT(*)`;
//! `graph_loader` reads `file_path` only.
//!
//! Walk is independent of `SUPPORTED_EXTENSIONS` (Dockerfile has none).
//! Coverage flags do not gate the write.

use crate::coverage::deploy::classify_deploy_manifest;
use crate::impact::packet::ManifestType;
use crate::state::storage::StorageManager;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use miette::{IntoDiagnostic, Result};
use std::path::PathBuf;
use tracing::warn;

/// Copied from `commands::doctor::checks::optional::SCIP_SKIP_SEGS`.
/// Do not import doctor into index (0343).
const SKIP_SEGS: &[&str] = &[
    "vendor",
    "deps_src",
    "node_modules",
    "target",
    "third_party",
    "tests",
    "test",
    "examples",
    "benches",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployManifestStats {
    pub rows_written: usize,
}

struct DeployRow {
    file_path: String,
    manifest_type: String,
    risk_tier: u8,
}

pub struct DeployManifestExtractor<'a> {
    storage: &'a StorageManager,
    repo_path: PathBuf,
    patterns: Vec<String>,
}

impl<'a> DeployManifestExtractor<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: PathBuf, patterns: Vec<String>) -> Self {
        Self {
            storage,
            repo_path,
            patterns,
        }
    }

    pub fn extract(&self) -> Result<DeployManifestStats> {
        let glob_set = match build_glob_set(&self.patterns) {
            Some(set) => set,
            None => {
                self.replace_rows(&[])?;
                return Ok(DeployManifestStats { rows_written: 0 });
            }
        };

        let mut rows = self.discover(&glob_set);
        rows.sort_by(|a, b| a.file_path.cmp(&b.file_path));
        let written = rows.len();
        self.replace_rows(&rows)?;
        Ok(DeployManifestStats {
            rows_written: written,
        })
    }

    fn discover(&self, glob_set: &GlobSet) -> Vec<DeployRow> {
        let walker = WalkBuilder::new(&self.repo_path)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();

        let mut rows = Vec::new();
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Error walking directory for deploy manifests: {}", e);
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let full_path = entry.path();
            let Ok(relative) = full_path.strip_prefix(&self.repo_path) else {
                continue;
            };
            let rel = relative.to_string_lossy().replace('\\', "/");
            if skip_non_product_rel(&rel) {
                continue;
            }
            if !glob_set.is_match(rel.as_str()) {
                continue;
            }
            let Some(manifest_type) = classify_deploy_manifest(full_path) else {
                continue;
            };
            rows.push(DeployRow {
                file_path: rel,
                manifest_type: manifest_type_label(&manifest_type).to_string(),
                risk_tier: base_risk_tier(&manifest_type),
            });
        }
        rows
    }

    fn replace_rows(&self, rows: &[DeployRow]) -> Result<()> {
        let conn = self.storage.get_connection();
        let tx = conn.unchecked_transaction().into_diagnostic()?;
        tx.execute("DELETE FROM deploy_manifests", [])
            .into_diagnostic()?;
        let now = chrono::Utc::now().to_rfc3339();
        for row in rows {
            tx.execute(
                "INSERT INTO deploy_manifests \
                 (file_path, manifest_type, risk_tier, service_name, owner, last_indexed_at) \
                 VALUES (?1, ?2, ?3, NULL, NULL, ?4)",
                rusqlite::params![row.file_path, row.manifest_type, row.risk_tier, now],
            )
            .into_diagnostic()?;
        }
        tx.commit().into_diagnostic()?;
        Ok(())
    }
}

fn build_glob_set(patterns: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for pat in patterns {
        match Glob::new(pat) {
            Ok(glob) => {
                builder.add(glob);
                any = true;
            }
            Err(e) => {
                warn!("Invalid deploy manifest glob pattern '{}': {}", pat, e);
            }
        }
    }
    if !any {
        return None;
    }
    match builder.build() {
        Ok(set) => Some(set),
        Err(e) => {
            warn!("Failed to build deploy manifest glob set: {}", e);
            None
        }
    }
}

fn skip_non_product_rel(rel: &str) -> bool {
    rel.to_ascii_lowercase()
        .split('/')
        .filter(|s| !s.is_empty())
        .any(|seg| SKIP_SEGS.contains(&seg))
}

fn manifest_type_label(mt: &ManifestType) -> &'static str {
    match mt {
        ManifestType::Dockerfile => "Dockerfile",
        ManifestType::DockerCompose => "DockerCompose",
        ManifestType::Kubernetes => "Kubernetes",
        ManifestType::Terraform => "Terraform",
        ManifestType::Helm => "Helm",
        ManifestType::CiWorkflow => "CiWorkflow",
        ManifestType::Unknown => "Unknown",
    }
}

fn base_risk_tier(mt: &ManifestType) -> u8 {
    match mt {
        ManifestType::Dockerfile | ManifestType::Unknown => 1,
        ManifestType::DockerCompose
        | ManifestType::Kubernetes
        | ManifestType::Terraform
        | ManifestType::Helm
        | ManifestType::CiWorkflow => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;
    use std::fs;
    use std::path::Path;

    fn in_memory_storage() -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn count_rows(storage: &StorageManager) -> i64 {
        storage
            .get_connection()
            .query_row("SELECT COUNT(*) FROM deploy_manifests", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn extract_at(root: &Path, storage: &StorageManager) -> DeployManifestStats {
        let patterns = Config::default().coverage.deploy.patterns;
        DeployManifestExtractor::new(storage, root.to_path_buf(), patterns)
            .extract()
            .unwrap()
    }

    fn row(storage: &StorageManager) -> (String, String, i64, Option<String>) {
        storage
            .get_connection()
            .query_row(
                "SELECT file_path, manifest_type, risk_tier, service_name FROM deploy_manifests",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
    }

    #[test]
    fn extract_deploy_manifests_writes_root_dockerfile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("Dockerfile"), "FROM alpine:3.20\n").unwrap();
        let storage = in_memory_storage();
        assert_eq!(count_rows(&storage), 0);
        let stats = extract_at(root, &storage);
        assert_eq!(stats.rows_written, 1);
        assert_eq!(count_rows(&storage), 1);
        let (path, kind, tier, service) = row(&storage);
        assert_eq!(path, "Dockerfile");
        assert!(!path.contains('\\'), "stored path must use /: {path}");
        assert_eq!(kind, "Dockerfile");
        assert_eq!(tier, 1);
        assert!(service.is_none());
    }

    #[test]
    fn extract_deploy_manifests_skips_tests_dockerfile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("tests").join("Dockerfile"), "FROM alpine:3.20\n").unwrap();
        let storage = in_memory_storage();
        let stats = extract_at(root, &storage);
        assert_eq!(stats.rows_written, 0);
        assert_eq!(count_rows(&storage), 0);
    }

    #[test]
    fn extract_deploy_manifests_skips_directory_named_dockerfile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("Dockerfile")).unwrap();
        let storage = in_memory_storage();
        let stats = extract_at(root, &storage);
        assert_eq!(stats.rows_written, 0);
        assert_eq!(count_rows(&storage), 0);
    }

    #[test]
    fn extract_deploy_manifests_writes_k8s_yaml_with_slash_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("k8s")).unwrap();
        fs::write(root.join("k8s").join("app.yaml"), "kind: Deployment\n").unwrap();
        let storage = in_memory_storage();
        let stats = extract_at(root, &storage);
        assert_eq!(stats.rows_written, 1);
        let (path, kind, tier, _) = row(&storage);
        assert_eq!(path, "k8s/app.yaml");
        assert!(!path.contains('\\'), "stored path must use /: {path}");
        assert_eq!(kind, "Kubernetes");
        assert_eq!(tier, 2);
    }

    #[test]
    fn extract_deploy_manifests_delete_clears_removed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let docker = root.join("Dockerfile");
        fs::write(&docker, "FROM alpine:3.20\n").unwrap();
        let storage = in_memory_storage();
        extract_at(root, &storage);
        assert_eq!(count_rows(&storage), 1);
        fs::remove_file(&docker).unwrap();
        extract_at(root, &storage);
        assert_eq!(count_rows(&storage), 0);
    }

    #[test]
    fn extract_deploy_manifests_writes_when_coverage_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("Dockerfile"), "FROM alpine:3.20\n").unwrap();
        let storage = in_memory_storage();
        let mut config = Config::default();
        assert!(!config.coverage.enabled);
        assert!(!config.coverage.deploy.enabled);
        config.coverage.enabled = false;
        config.coverage.deploy.enabled = false;
        let stats = DeployManifestExtractor::new(
            &storage,
            root.to_path_buf(),
            config.coverage.deploy.patterns,
        )
        .extract()
        .unwrap();
        assert_eq!(stats.rows_written, 1);
        assert_eq!(count_rows(&storage), 1);
    }

    #[test]
    fn extract_deploy_manifests_wired_on_graph_run_policy() {
        let src = include_str!("orchestrator/graph.rs");
        let run = src
            .find("if policy == SqliteExtractPolicy::Run")
            .expect("Run policy");
        let after = &src[run..];
        let ci = after
            .find("indexer.extract_ci_gates()")
            .expect("extract_ci_gates");
        let deploy = after
            .find("indexer.extract_deploy_manifests()")
            .expect("extract_deploy_manifests");
        assert!(
            deploy > ci,
            "extract_deploy_manifests must follow extract_ci_gates in Run"
        );
    }
}
