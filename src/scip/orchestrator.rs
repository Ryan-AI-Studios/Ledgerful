use miette::{IntoDiagnostic, Result, miette};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::info;

const SCIP_INDEX_TIMEOUT_SECS: u64 = 300;

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
        let (exe, exe_args) = match self {
            Self::RustAnalyzer => (
                "rust-analyzer",
                vec!["scip", ".", "--output", temp_filename],
            ),
            Self::ScipTypescript => ("scip-typescript", vec!["index", "--output", temp_filename]),
            Self::ScipPython => {
                let project_name = repo_root
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("ledgerful-project");
                (
                    "scip-python",
                    vec![
                        "index",
                        ".",
                        "--project-name",
                        project_name,
                        "--output",
                        temp_filename,
                    ],
                )
            }
        };

        crate::platform::process_policy::check_policy(
            exe,
            &crate::platform::process_policy::ProcessPolicy {
                allowed_commands: vec![exe.to_string()],
                denied_commands: Vec::new(),
                default_timeout_secs: SCIP_INDEX_TIMEOUT_SECS,
                strict: true,
            },
        )
        .into_diagnostic()?;

        let mut cmd = Command::new(exe);
        cmd.args(exe_args);
        cmd.current_dir(repo_root);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        info!("Running SCIP indexer: {:?}", cmd);

        let mut child = cmd.spawn().into_diagnostic()?;
        let timeout = Duration::from_secs(SCIP_INDEX_TIMEOUT_SECS);
        let status =
            match wait_timeout::ChildExt::wait_timeout(&mut child, timeout).into_diagnostic()? {
                Some(status) => status,
                None => {
                    let _ = child.kill();
                    return Err(miette!(
                        "SCIP indexer timed out after {} seconds",
                        SCIP_INDEX_TIMEOUT_SECS
                    ));
                }
            };

        if !status.success() {
            let stderr = child
                .stderr
                .take()
                .map(|mut r| {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut r, &mut buf).ok();
                    buf
                })
                .unwrap_or_default();
            return Err(miette!(
                "SCIP indexer failed with status: {}: {}",
                status,
                stderr
            ));
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
