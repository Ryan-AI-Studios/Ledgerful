//! SCIP toolchain detection and generation.
//!
//! Detection is a **capability probe** (base exe + `--version`), not a PATH
//! lookup. A rustup shim that exits non-zero is reported unavailable (0095).

use crate::platform::process_policy::{ProcessPolicy, check_policy};
use miette::{IntoDiagnostic, Result, miette};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tracing::info;

const SCIP_INDEX_TIMEOUT_SECS: u64 = 600;
/// Short timeout for capability probes (`exe --version`).
const SCIP_PROBE_TIMEOUT_SECS: u64 = 5;

/// Process-wide cache of capability probe results (base executable name → ok).
fn capability_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Clear the capability cache (tests only).
#[cfg(test)]
pub fn clear_capability_cache_for_test() {
    if let Ok(mut guard) = capability_cache().lock() {
        guard.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScipToolchain {
    RustAnalyzer,
    ScipTypescript,
    ScipPython,
}

impl ScipToolchain {
    pub fn language_label(self) -> &'static str {
        match self {
            Self::RustAnalyzer => "Rust",
            Self::ScipTypescript => "TypeScript",
            Self::ScipPython => "Python",
        }
    }

    pub fn exe_name(self) -> &'static str {
        match self {
            Self::RustAnalyzer => "rust-analyzer",
            Self::ScipTypescript => "scip-typescript",
            Self::ScipPython => "scip-python",
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Self::RustAnalyzer => "rustup component add rust-analyzer",
            Self::ScipTypescript => "npm install -g @sourcegraph/scip-typescript",
            Self::ScipPython => "pip install scip-python",
        }
    }

    /// Return **all** available toolchains for this repo, ordered by language
    /// priority (Rust → TypeScript → Python). Used by `doctor`.
    ///
    /// `--auto-scip` generation still runs only the first entry (same priority);
    /// multi-language generation in one run is out of scope for 0095.
    pub fn detect_all(repo_root: &Path) -> Vec<Self> {
        let mut out = Vec::new();
        if repo_root.join("Cargo.toml").exists() && is_capable(Self::RustAnalyzer.exe_name()) {
            out.push(Self::RustAnalyzer);
        }
        if (repo_root.join("tsconfig.json").exists() || repo_root.join("package.json").exists())
            && is_capable(Self::ScipTypescript.exe_name())
        {
            out.push(Self::ScipTypescript);
        }
        if (repo_root.join("requirements.txt").exists()
            || repo_root.join("pyproject.toml").exists())
            && is_capable(Self::ScipPython.exe_name())
        {
            out.push(Self::ScipPython);
        }
        out
    }

    /// First available toolchain by language priority (for `--auto-scip` generate).
    pub fn detect(repo_root: &Path) -> Option<Self> {
        Self::detect_all(repo_root).into_iter().next()
    }

    /// Probe every known SCIP base executable (regardless of repo markers).
    /// Used by `doctor` for per-language availability lines.
    pub fn probe_all_languages() -> Vec<(Self, bool)> {
        let tools = [Self::RustAnalyzer, Self::ScipTypescript, Self::ScipPython];
        tools
            .into_iter()
            .map(|t| (t, is_capable(t.exe_name())))
            .collect()
    }

    /// Generate a SCIP index. Uses the **configured** process policy (DoD-12).
    pub fn generate(&self, repo_root: &Path, policy: &ProcessPolicy) -> Result<PathBuf> {
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

        check_policy(exe, policy).into_diagnostic()?;

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

/// Capability probe: run **base executable** + `--version` under a short timeout.
///
/// Never probes the composite ingestion command (`rust-analyzer scip --version`
/// is invalid). Exit 0 → available; anything else → not available.
///
/// Results are cached per process.
pub fn is_capable(binary: &str) -> bool {
    if let Ok(guard) = capability_cache().lock()
        && let Some(&cached) = guard.get(binary)
    {
        return cached;
    }

    let ok = probe_base_exe_version(binary);

    if let Ok(mut guard) = capability_cache().lock() {
        guard.insert(binary.to_string(), ok);
    }
    ok
}

fn probe_base_exe_version(binary: &str) -> bool {
    // PATH presence alone is not enough (rustup shim); must execute.
    if crate::util::which::which(binary).is_none() {
        return false;
    }

    let mut cmd = Command::new(binary);
    cmd.arg("--version");
    cmd.stdout(Stdio::null()).stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return false,
    };

    let timeout = Duration::from_secs(SCIP_PROBE_TIMEOUT_SECS);
    match wait_timeout::ChildExt::wait_timeout(&mut child, timeout) {
        Ok(Some(status)) => status.success(),
        Ok(None) => {
            let _ = child.kill();
            false
        }
        Err(_) => {
            let _ = child.kill();
            false
        }
    }
}

/// Inject a capability result for tests (bypasses real spawn).
#[cfg(test)]
pub fn set_capability_for_test(binary: &str, available: bool) {
    if let Ok(mut guard) = capability_cache().lock() {
        guard.insert(binary.to_string(), available);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::process_policy::ProcessPolicy;

    #[test]
    fn detect_all_empty_repo_returns_empty() {
        clear_capability_cache_for_test();
        set_capability_for_test("rust-analyzer", false);
        set_capability_for_test("scip-typescript", false);
        set_capability_for_test("scip-python", false);
        let tmp = tempfile::tempdir().unwrap();
        assert!(ScipToolchain::detect_all(tmp.path()).is_empty());
    }

    #[test]
    fn detect_all_respects_capability_not_just_manifest() {
        clear_capability_cache_for_test();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();
        set_capability_for_test("rust-analyzer", false);
        assert!(ScipToolchain::detect_all(tmp.path()).is_empty());
        set_capability_for_test("rust-analyzer", true);
        let found = ScipToolchain::detect_all(tmp.path());
        assert_eq!(found, vec![ScipToolchain::RustAnalyzer]);
    }

    #[test]
    fn generate_denied_by_process_policy() {
        let policy = ProcessPolicy {
            allowed_commands: vec!["cargo".to_string()],
            denied_commands: vec!["rust-analyzer".to_string()],
            default_timeout_secs: 30,
            strict: true,
        };
        let tmp = tempfile::tempdir().unwrap();
        let err = ScipToolchain::RustAnalyzer
            .generate(tmp.path(), &policy)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("denied") || msg.contains("policy") || msg.contains("rust-analyzer"),
            "expected policy denial, got: {msg}"
        );
    }

    #[test]
    fn generate_not_on_allowlist_refused() {
        let policy = ProcessPolicy {
            allowed_commands: vec!["cargo".to_string()],
            denied_commands: Vec::new(),
            default_timeout_secs: 30,
            strict: true,
        };
        let tmp = tempfile::tempdir().unwrap();
        let err = ScipToolchain::RustAnalyzer
            .generate(tmp.path(), &policy)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("allowed") || msg.contains("denied") || msg.contains("rust-analyzer"),
            "expected allowlist refusal, got: {msg}"
        );
    }

    #[test]
    fn capability_cache_returns_injected_value() {
        clear_capability_cache_for_test();
        set_capability_for_test("totally-fake-scip-tool-0095", false);
        assert!(!is_capable("totally-fake-scip-tool-0095"));
        set_capability_for_test("totally-fake-scip-tool-0095", true);
        assert!(is_capable("totally-fake-scip-tool-0095"));
    }
}
