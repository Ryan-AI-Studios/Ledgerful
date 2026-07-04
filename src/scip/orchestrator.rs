use miette::{IntoDiagnostic, Result, miette};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::info;

const SCIP_INDEX_TIMEOUT_SECS: u64 = 600;

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

        // Drain stdout/stderr in background threads to prevent pipe-buffer
        // deadlock. rust-analyzer scip emits thousands of warning lines on
        // stderr; if the OS pipe buffer (~64 KB on Windows) fills and nobody
        // is reading, the child blocks on write() and appears to "hang" —
        // causing a false timeout. Drain continuously and capture the tail
        // for error reporting.
        let stderr_handle = child.stderr.take();
        let stdout_handle = child.stdout.take();
        let stderr_thread = std::thread::spawn(move || {
            let mut buf = Vec::with_capacity(8192);
            if let Some(mut r) = stderr_handle {
                let mut chunk = [0u8; 4096];
                loop {
                    use std::io::Read;
                    match r.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            // Keep only the last 8 KB for error messages
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.len() > 8192 {
                                buf.drain(0..buf.len() - 8192);
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
            buf
        });
        let stdout_thread = std::thread::spawn(move || {
            let mut buf = Vec::with_capacity(8192);
            if let Some(mut r) = stdout_handle {
                let mut chunk = [0u8; 4096];
                loop {
                    use std::io::Read;
                    match r.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.len() > 8192 {
                                buf.drain(0..buf.len() - 8192);
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
            buf
        });

        let timeout = Duration::from_secs(SCIP_INDEX_TIMEOUT_SECS);
        let wait_result = wait_timeout::ChildExt::wait_timeout(&mut child, timeout);
        let status = match wait_result {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = child.kill();
                let _ = stderr_thread.join();
                let _ = stdout_thread.join();
                return Err(miette!(
                    "SCIP indexer timed out after {} seconds",
                    SCIP_INDEX_TIMEOUT_SECS
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = stderr_thread.join();
                let _ = stdout_thread.join();
                return Err(miette!("Failed to wait for SCIP indexer: {}", e));
            }
        };

        // Ensure drain threads finish
        let stderr_buf = stderr_thread.join().unwrap_or_default();
        let _stdout_buf = stdout_thread.join().unwrap_or_default();

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_buf).to_string();
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
