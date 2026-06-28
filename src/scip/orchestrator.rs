use miette::{IntoDiagnostic, Result, miette};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

pub enum ScipToolchain {
    RustAnalyzer,
    ScipTypescript,
    ScipPython,
}

impl ScipToolchain {
    pub fn detect(repo_root: &Path) -> Option<Self> {
        // Rust detection
        if repo_root.join("Cargo.toml").exists() && is_on_path("rust-analyzer") {
            return Some(Self::RustAnalyzer);
        }
        // TS detection
        if (repo_root.join("tsconfig.json").exists() || repo_root.join("package.json").exists())
            && is_on_path("scip-typescript")
        {
            return Some(Self::ScipTypescript);
        }
        // Python detection
        if (repo_root.join("requirements.txt").exists()
            || repo_root.join("pyproject.toml").exists())
            && is_on_path("scip-python")
        {
            return Some(Self::ScipPython);
        }

        None
    }

    pub fn generate(&self, repo_root: &Path) -> Result<PathBuf> {
        let temp_filename = "ledgerful.temp.scip";
        let output_path = repo_root.join(temp_filename);
        let mut cmd = match self {
            Self::RustAnalyzer => {
                let mut c = Command::new("rust-analyzer");
                c.args(["scip", ".", "--output", temp_filename]);
                c
            }
            Self::ScipTypescript => {
                let mut c = Command::new("scip-typescript");
                c.args(["index", "--output", temp_filename]);
                c
            }
            Self::ScipPython => {
                let mut c = Command::new("scip-python");
                let project_name = repo_root
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("ledgerful-project");
                c.args([
                    "index",
                    ".",
                    "--project-name",
                    project_name,
                    "--output",
                    temp_filename,
                ]);
                c
            }
        };

        cmd.current_dir(repo_root);
        info!("Running SCIP indexer: {:?}", cmd);

        let status = cmd.status().into_diagnostic()?;

        if !status.success() {
            return Err(miette!("SCIP indexer failed with status: {}", status));
        }

        if !output_path.exists() {
            return Err(miette!(
                "SCIP indexer succeeded but {} was not generated",
                temp_filename
            ));
        }

        Ok(output_path)
    }
}

fn is_on_path(binary: &str) -> bool {
    crate::util::which::which(binary).is_some()
}
