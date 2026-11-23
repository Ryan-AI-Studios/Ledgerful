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

/// Process-wide cache: exe name → resolved `PathBuf` after a successful
/// `--version` probe (`None` = probed, not capable).
fn capability_cache() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
static RUSTUP_WHICH_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Clear the capability cache (tests only).
#[cfg(test)]
pub fn clear_capability_cache_for_test() {
    if let Ok(mut guard) = capability_cache().lock() {
        guard.clear();
    }
    RUSTUP_WHICH_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScipToolchain {
    RustAnalyzer,
    ScipTypescript,
    ScipPython,
}

pub(crate) const SCIP_TOOLCHAINS: [ScipToolchain; 3] = [
    ScipToolchain::RustAnalyzer,
    ScipToolchain::ScipTypescript,
    ScipToolchain::ScipPython,
];

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    Imported,
    Automatic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionStatus {
    MissingRootMarker,
    PolicyDenied,
    Unavailable,
    Selected,
    NotProbedAfterSelection,
}

impl SelectionStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MissingRootMarker => "missing_root_marker",
            Self::PolicyDenied => "policy_denied",
            Self::Unavailable => "unavailable",
            Self::Selected => "selected",
            Self::NotProbedAfterSelection => "not_probed_after_selection",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SelectionRow {
    pub toolchain: &'static str,
    pub root_markers: Vec<&'static str>,
    pub status: SelectionStatus,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScipSelection {
    pub mode: SelectionMode,
    pub selected_toolchain: Option<&'static str>,
    pub rows: Vec<SelectionRow>,
}

impl ScipSelection {
    pub(crate) fn imported() -> Self {
        Self {
            mode: SelectionMode::Imported,
            selected_toolchain: None,
            rows: Vec::new(),
        }
    }
}

pub(crate) fn select_auto(
    root: &Path,
    policy: &ProcessPolicy,
) -> (ScipSelection, Option<(ScipToolchain, PathBuf)>) {
    select_auto_with(root, |tool| resolve_for_selection(tool, policy))
}

fn select_auto_with(
    root: &Path,
    mut probe: impl FnMut(ScipToolchain) -> Result<PathBuf, SelectionStatus>,
) -> (ScipSelection, Option<(ScipToolchain, PathBuf)>) {
    let mut selected = None;
    let rows = SCIP_TOOLCHAINS
        .into_iter()
        .map(|tool| {
            let (root_markers, marker_error) = tool.root_markers(root);
            let status = if root_markers.is_empty() {
                if marker_error {
                    SelectionStatus::Unavailable
                } else {
                    SelectionStatus::MissingRootMarker
                }
            } else if selected.is_some() {
                SelectionStatus::NotProbedAfterSelection
            } else {
                match probe(tool) {
                    Ok(path) => {
                        selected = Some((tool, path));
                        SelectionStatus::Selected
                    }
                    Err(status) => status,
                }
            };
            SelectionRow {
                toolchain: tool.exe_name(),
                root_markers,
                status,
            }
        })
        .collect();
    let selection = ScipSelection {
        mode: SelectionMode::Automatic,
        selected_toolchain: selected.as_ref().map(|(tool, _)| tool.exe_name()),
        rows,
    };
    (selection, selected)
}

fn resolve_for_selection(
    tool: ScipToolchain,
    policy: &ProcessPolicy,
) -> Result<PathBuf, SelectionStatus> {
    resolve_for_selection_with(
        tool,
        policy,
        crate::util::which::which,
        probe_version_at,
        rustup_which_at,
    )
}

fn resolve_for_selection_with(
    tool: ScipToolchain,
    policy: &ProcessPolicy,
    mut which: impl FnMut(&str) -> Option<PathBuf>,
    mut probe: impl FnMut(&Path) -> bool,
    mut rustup: impl FnMut(&Path, &str) -> Option<PathBuf>,
) -> Result<PathBuf, SelectionStatus> {
    let denied = |_| SelectionStatus::PolicyDenied;
    let logical = tool.exe_name();
    check_policy(logical, policy).map_err(denied)?;
    if let Some(path) = which(logical) {
        check_selected_executable(logical, &path, policy).map_err(denied)?;
        if probe(&path) {
            return Ok(path);
        }
    }
    if tool != ScipToolchain::RustAnalyzer {
        return Err(SelectionStatus::Unavailable);
    }
    check_policy("rustup", policy).map_err(denied)?;
    let rustup_path = which("rustup").ok_or(SelectionStatus::Unavailable)?;
    check_selected_executable("rustup", &rustup_path, policy).map_err(denied)?;
    let path = rustup(&rustup_path, logical)
        .filter(|path| path.is_absolute())
        .ok_or(SelectionStatus::Unavailable)?;
    check_selected_executable(logical, &path, policy).map_err(denied)?;
    if probe(&path) {
        Ok(path)
    } else {
        Err(SelectionStatus::Unavailable)
    }
}

fn check_selected_executable(
    logical: &str,
    path: &Path,
    policy: &ProcessPolicy,
) -> Result<(), crate::platform::process_policy::ProcessPolicyError> {
    use crate::platform::process_policy::ProcessPolicyError;
    check_policy(logical, policy)?;
    let spelling = path
        .to_str()
        .filter(|_| path.is_absolute())
        .ok_or_else(|| ProcessPolicyError::denied(logical, "unverifiable resolved executable"))?;
    check_policy(spelling, policy)
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

    pub(crate) fn marker_names(self) -> &'static [&'static str] {
        match self {
            Self::RustAnalyzer => &["Cargo.toml"],
            Self::ScipTypescript => &["package.json", "tsconfig.json"],
            Self::ScipPython => &["pyproject.toml", "requirements.txt"],
        }
    }

    pub(crate) fn root_markers(self, root: &Path) -> (Vec<&'static str>, bool) {
        let mut found = Vec::new();
        let mut error = false;
        for &name in self.marker_names() {
            match std::fs::symlink_metadata(root.join(name)) {
                Ok(meta) if meta.is_file() => found.push(name),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => error = true,
            }
        }
        (found, error)
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Self::RustAnalyzer => "rustup component add rust-analyzer",
            Self::ScipTypescript => "npm install -g @sourcegraph/scip-typescript",
            Self::ScipPython => "npm install -g @sourcegraph/scip-python",
        }
    }

    /// Return **all** available toolchains for this repo, ordered by language
    /// priority (Rust → TypeScript → Python). Used by `doctor`.
    ///
    /// `--auto-scip` generation still runs only the first entry (same priority);
    /// multi-language generation in one run is out of scope for 0095.
    pub fn detect_all(repo_root: &Path) -> Vec<Self> {
        SCIP_TOOLCHAINS
            .into_iter()
            .filter(|tool| {
                tool.marker_names()
                    .iter()
                    .any(|marker| repo_root.join(marker).exists())
                    && is_capable(tool.exe_name())
            })
            .collect()
    }

    /// First available toolchain by language priority (for `--auto-scip` generate).
    pub fn detect(repo_root: &Path) -> Option<Self> {
        Self::detect_all(repo_root).into_iter().next()
    }

    /// Probe every known SCIP base executable (regardless of repo markers).
    /// Used by `doctor` for per-language availability lines.
    pub fn probe_all_languages() -> Vec<(Self, bool)> {
        SCIP_TOOLCHAINS
            .into_iter()
            .map(|t| (t, is_capable(t.exe_name())))
            .collect()
    }

    /// Generate a SCIP index. Uses the **configured** process policy (DoD-12).
    pub fn generate(&self, repo_root: &Path, policy: &ProcessPolicy) -> Result<PathBuf> {
        let exe = self.exe_name();
        check_policy(exe, policy).into_diagnostic()?;
        let resolved = resolve_for_selection(*self, policy).map_err(|status| {
            miette!(
                "SCIP indexer '{}': {}. Setup help: `{}`.",
                exe,
                status.as_str(),
                self.install_hint()
            )
        })?;
        self.generate_at(repo_root, policy, &resolved)
    }

    pub(crate) fn generate_at(
        &self,
        repo_root: &Path,
        policy: &ProcessPolicy,
        resolved: &Path,
    ) -> Result<PathBuf> {
        check_selected_executable(self.exe_name(), resolved, policy).into_diagnostic()?;
        let temp_filename = "ledgerful.temp.scip";
        let output_path = repo_root.join(temp_filename);
        let exe_args = match self {
            Self::RustAnalyzer => vec!["scip", ".", "--output", temp_filename],
            Self::ScipTypescript => vec!["index", "--output", temp_filename],
            Self::ScipPython => {
                let project_name = repo_root
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("ledgerful-project");
                vec![
                    "index",
                    ".",
                    "--project-name",
                    project_name,
                    "--output",
                    temp_filename,
                ]
            }
        };

        let mut cmd = Command::new(resolved);
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
/// Results are cached per process as the resolved `PathBuf` after `--version`.
pub fn is_capable(binary: &str) -> bool {
    resolve_scip_binary(binary).is_some()
}

/// Resolve a SCIP indexer executable: PATH first, then `rustup which` for
/// rust-analyzer (component lives in the toolchain bin, which is often not on
/// PATH on Windows). A PATH hit that fails `--version` (broken shim / missing
/// DLLs) is ignored so rustup can still succeed.
///
/// Cached after the first `--version` probe so `rustup which` / `probe_version_at`
/// run at most once per binary per process.
pub(crate) fn resolve_scip_binary(binary: &str) -> Option<PathBuf> {
    if let Ok(guard) = capability_cache().lock()
        && let Some(cached) = guard.get(binary)
    {
        return cached.clone();
    }

    let resolved = resolve_scip_binary_uncached(binary);

    if let Ok(mut guard) = capability_cache().lock() {
        guard.insert(binary.to_string(), resolved.clone());
    }
    resolved
}

fn resolve_scip_binary_uncached(binary: &str) -> Option<PathBuf> {
    if let Some(p) = crate::util::which::which(binary)
        && probe_version_at(&p)
    {
        return Some(p);
    }
    let base = binary
        .strip_suffix(".exe")
        .or_else(|| binary.strip_suffix(".EXE"))
        .unwrap_or(binary);
    if base.eq_ignore_ascii_case("rust-analyzer") {
        let p = rustup_which("rust-analyzer").filter(|p| p.is_file())?;
        if probe_version_at(&p) {
            return Some(p);
        }
    }
    None
}

/// Parse `rustup which <component>` stdout (first non-empty trimmed line).
pub(crate) fn parse_rustup_which_stdout(stdout: &str) -> Option<PathBuf> {
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(PathBuf::from(line))
}

fn rustup_which(component: &str) -> Option<PathBuf> {
    #[cfg(test)]
    {
        RUSTUP_WHICH_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    rustup_which_at(Path::new("rustup"), component)
}

fn rustup_which_at(executable: &Path, component: &str) -> Option<PathBuf> {
    let mut cmd = Command::new(executable);
    cmd.args(["which", component]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return None,
    };
    let stdout_handle = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = stdout_handle {
            use std::io::Read;
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });

    let timeout = Duration::from_secs(SCIP_PROBE_TIMEOUT_SECS);
    let status = match wait_timeout::ChildExt::wait_timeout(&mut child, timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.kill();
            let _ = reader.join();
            return None;
        }
        Err(_) => {
            let _ = child.kill();
            let _ = reader.join();
            return None;
        }
    };
    let stdout = reader.join().unwrap_or_default();
    if !status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&stdout);
    parse_rustup_which_stdout(&text).filter(|p| p.is_file())
}

fn probe_version_at(exe: &Path) -> bool {
    let mut cmd = Command::new(exe);
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
    let path = available.then(|| PathBuf::from(format!("__ledgerful_test_capable__/{binary}")));
    set_resolved_path_for_test(binary, path);
}

/// Inject a resolved SCIP path (tests only).
#[cfg(test)]
pub fn set_resolved_path_for_test(binary: &str, path: Option<PathBuf>) {
    if let Ok(mut guard) = capability_cache().lock() {
        guard.insert(binary.to_string(), path);
    }
}

#[cfg(test)]
fn rustup_which_call_count() -> usize {
    RUSTUP_WHICH_CALLS.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::process_policy::ProcessPolicy;

    #[test]
    #[serial_test::serial(scip_capability_cache)]
    fn detect_all_empty_repo_returns_empty() {
        clear_capability_cache_for_test();
        set_capability_for_test("rust-analyzer", false);
        set_capability_for_test("scip-typescript", false);
        set_capability_for_test("scip-python", false);
        let tmp = tempfile::tempdir().unwrap();
        assert!(ScipToolchain::detect_all(tmp.path()).is_empty());
    }

    #[test]
    #[serial_test::serial(scip_capability_cache)]
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
    #[serial_test::serial(scip_capability_cache)]
    fn capability_cache_returns_injected_value() {
        clear_capability_cache_for_test();
        set_capability_for_test("totally-fake-scip-tool-0095", false);
        assert!(!is_capable("totally-fake-scip-tool-0095"));
        set_capability_for_test("totally-fake-scip-tool-0095", true);
        assert!(is_capable("totally-fake-scip-tool-0095"));
    }

    #[test]
    fn parse_rustup_which_stdout_takes_first_nonempty_line() {
        let p = parse_rustup_which_stdout(
            "\n  C:\\Users\\me\\.rustup\\toolchains\\stable-x86_64-pc-windows-msvc\\bin\\rust-analyzer.exe  \n",
        )
        .expect("path");
        // Path::ends_with is component-based; Windows rustup output is one
        // component on Unix because `\` is not a separator.
        let displayed = p.to_string_lossy();
        assert!(
            displayed.ends_with("rust-analyzer.exe"),
            "unexpected parse: {displayed}"
        );
    }

    #[test]
    fn parse_rustup_which_stdout_empty_is_none() {
        assert!(parse_rustup_which_stdout("   \n\n").is_none());
    }

    #[test]
    #[serial_test::serial(scip_capability_cache)]
    fn resolve_rust_analyzer_via_rustup_when_component_installed() {
        clear_capability_cache_for_test();
        if rustup_which("rust-analyzer").is_none() {
            return;
        }
        let resolved = resolve_scip_binary("rust-analyzer")
            .expect("rustup which should resolve rust-analyzer");
        assert!(
            resolved.is_file(),
            "resolved rust-analyzer missing: {}",
            resolved.display()
        );
        assert!(
            is_capable("rust-analyzer"),
            "capability probe must succeed via rustup which when component is installed"
        );
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    #[serial_test::serial(scip_capability_cache)]
    fn scip_resolve_path_cache__second_call__reuses_pathbuf_no_rustup() {
        clear_capability_cache_for_test();
        let injected = PathBuf::from("C:\\injected\\rust-analyzer.exe");
        set_resolved_path_for_test("rust-analyzer", Some(injected.clone()));
        let rustup_before = rustup_which_call_count();
        let first = resolve_scip_binary("rust-analyzer");
        let second = resolve_scip_binary("rust-analyzer");
        let rustup_after = rustup_which_call_count();
        assert_eq!(first.as_ref(), Some(&injected));
        assert_eq!(second, first, "second resolve must reuse cached PathBuf");
        assert_eq!(
            rustup_before, rustup_after,
            "second resolve must not invoke rustup which"
        );
    }

    #[test]
    fn scip_python_install_hint_is_npm_not_pip() {
        let hint = ScipToolchain::ScipPython.install_hint();
        assert!(
            hint.contains("npm") && hint.contains("@sourcegraph/scip-python"),
            "scip-python must advertise npm package, got: {hint}"
        );
        assert!(
            !hint.contains("pip"),
            "scip-python is not on PyPI; install_hint must not say pip: {hint}"
        );
    }
    #[test]
    fn scip_selection_stops_probing_after_first_permitted_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        for marker in ["Cargo.toml", "tsconfig.json", "requirements.txt"] {
            std::fs::write(tmp.path().join(marker), "").unwrap();
        }
        let typescript = tmp.path().join(if cfg!(windows) {
            "scip-typescript.exe"
        } else {
            "scip-typescript"
        });
        let mut attempts = Vec::new();
        let (report, selected) = select_auto_with(tmp.path(), |tool| {
            attempts.push(tool);
            match tool {
                ScipToolchain::RustAnalyzer => Err(SelectionStatus::PolicyDenied),
                ScipToolchain::ScipTypescript => Ok(typescript.clone()),
                ScipToolchain::ScipPython => panic!("later candidate must not be probed"),
            }
        });
        assert_eq!(
            attempts,
            [ScipToolchain::RustAnalyzer, ScipToolchain::ScipTypescript]
        );
        assert_eq!(selected, Some((ScipToolchain::ScipTypescript, typescript)));
        assert_eq!(
            serde_json::to_value(report).unwrap(),
            serde_json::json!({
                "mode":"automatic","selected_toolchain":"scip-typescript",
                "rows":[
                    {"toolchain":"rust-analyzer","root_markers":["Cargo.toml"],"status":"policy_denied"},
                    {"toolchain":"scip-typescript","root_markers":["tsconfig.json"],"status":"selected"},
                    {"toolchain":"scip-python","root_markers":["requirements.txt"],"status":"not_probed_after_selection"}
                ]
            })
        );
    }

    #[test]
    fn scip_selection_missing_marker_precedes_unprobed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let (report, _) = select_auto_with(tmp.path(), |tool| {
            assert_eq!(tool, ScipToolchain::RustAnalyzer);
            Ok(tmp.path().join(tool.exe_name()))
        });
        assert_eq!(report.rows[1].status, SelectionStatus::MissingRootMarker);
        assert_eq!(report.rows[2].status, SelectionStatus::MissingRootMarker);
    }

    #[rstest::rstest]
    #[case(false, false, false, 1, 0, SelectionStatus::PolicyDenied)]
    #[case(false, true, false, 0, 0, SelectionStatus::PolicyDenied)]
    #[case(true, false, true, 2, 1, SelectionStatus::Selected)]
    #[case(true, false, false, 2, 1, SelectionStatus::Unavailable)]
    fn scip_selection_checks_fallback_and_primary_before_spawn(
        #[case] allow_rustup: bool,
        #[case] deny_primary: bool,
        #[case] fallback_works: bool,
        #[case] expected_probes: usize,
        #[case] expected_rustup: usize,
        #[case] expected_status: SelectionStatus,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let primary = root.join("rust-analyzer");
        let fallback = root.join("component").join("rust-analyzer");
        let launcher = root.join("rustup");
        let mut policy = ProcessPolicy::default();
        if allow_rustup {
            policy.allowed_commands.push("rustup".into());
        }
        if deny_primary {
            policy.denied_commands.push("rust-analyzer".into());
        }
        let mut probes = Vec::new();
        let mut launches = 0;
        let result = resolve_for_selection_with(
            ScipToolchain::RustAnalyzer,
            &policy,
            |name| Some(root.join(name)),
            |path| {
                probes.push(path.to_owned());
                path == fallback && fallback_works
            },
            |path, logical| {
                assert_eq!(path, launcher);
                assert_eq!(logical, "rust-analyzer");
                launches += 1;
                Some(fallback.clone())
            },
        );
        match result {
            Ok(path) => {
                assert_eq!(expected_status, SelectionStatus::Selected);
                assert_eq!(path, fallback);
            }
            Err(status) => assert_eq!(status, expected_status),
        }
        assert_eq!(launches, expected_rustup);
        assert_eq!(probes.len(), expected_probes);
        if expected_probes > 0 {
            assert_eq!(probes[0], primary);
        }
        if expected_probes > 1 {
            assert_eq!(probes[1], fallback);
        }
    }
    #[test]
    fn scip_selection_refuses_denied_fallback_executable_before_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let launcher = root.join("rustup");
        let mut policy = ProcessPolicy::default();
        policy.allowed_commands.push("rustup".into());
        policy.denied_commands.push("blocked-rust-analyzer".into());
        let mut probes = Vec::new();
        let result = resolve_for_selection_with(
            ScipToolchain::RustAnalyzer,
            &policy,
            |name| Some(root.join(name)),
            |path| {
                probes.push(path.to_owned());
                false
            },
            |path, logical| {
                assert_eq!(path, launcher);
                assert_eq!(logical, "rust-analyzer");
                Some(root.join("blocked-rust-analyzer"))
            },
        );
        assert_eq!(result, Err(SelectionStatus::PolicyDenied));
        assert_eq!(probes, [root.join("rust-analyzer")]);
    }
}
