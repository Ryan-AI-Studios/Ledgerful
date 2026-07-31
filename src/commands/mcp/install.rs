//! MCP host install / uninstall / status orchestration.
//!
//! File merge is the source of truth — never shells out to host CLIs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use toml_edit::DocumentMut;

use super::launcher::{ResolvedLauncher, resolve_launcher_from_path};
use super::merge::{
    ServerEntry, get_json_server, get_toml_server, merge_json_server, merge_toml_server,
    parse_jsonc, remove_json_server, remove_toml_server, serialize_json,
};
use super::platforms::{
    ConfigFormat, PLATFORM_IDS, PlatformId, is_detected, path_for_scope, resolve_paths,
};
use crate::cli::args::{McpLauncher, McpScope};

// ── Clap value parser ───────────────────────────────────────────────────────

/// Clap `value_parser` for `--platform` ids.
pub fn parse_platform_id(s: &str) -> Result<String, String> {
    PlatformId::parse(s)?;
    Ok(s.to_string())
}

// ── Report types (schemaVersion 1) ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpInstallReport {
    pub schema_version: u32,
    pub action: String,
    pub platforms: Vec<PlatformReport>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformReport {
    pub id: String,
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ── Public execute entry points ─────────────────────────────────────────────

pub fn execute_install(
    platforms: Vec<String>,
    scope: Option<McpScope>,
    launcher: McpLauncher,
    dry_run: bool,
    force: bool,
    backup: bool,
    json: bool,
) -> miette::Result<()> {
    let env = InstallEnv::from_process()?;
    let resolved = resolve_launcher_from_path(launcher).map_err(|e| miette::miette!("{e}"))?;
    let targets = select_targets(&platforms, &env, false)?;
    let mut reports = Vec::new();

    for id in targets {
        let sc = scope.unwrap_or_else(|| id.default_scope());
        let report = install_one(id, sc, &resolved, dry_run, force, backup, &env);
        reports.push(report);
    }

    emit_report("install", reports, json, resolved.message.as_deref())
}

pub fn execute_uninstall(
    platforms: Vec<String>,
    scope: Option<McpScope>,
    dry_run: bool,
    json: bool,
) -> miette::Result<()> {
    let env = InstallEnv::from_process()?;
    let targets = select_targets(&platforms, &env, true)?;
    let mut reports = Vec::new();

    for id in targets {
        let sc = scope.unwrap_or_else(|| id.default_scope());
        let report = uninstall_one(id, sc, dry_run, &env);
        reports.push(report);
    }

    emit_report("uninstall", reports, json, None)
}

pub fn execute_status(json: bool) -> miette::Result<()> {
    let env = InstallEnv::from_process()?;
    let mut reports = Vec::new();

    for &id in PlatformId::all() {
        for sc in [McpScope::User, McpScope::Project] {
            // Copilot user may fail path resolution without APPDATA — report error row.
            let report = status_one(id, sc, &env);
            reports.push(report);
        }
    }

    // Stable order
    reports.sort_by(|a, b| (&a.id, &a.path).cmp(&(&b.id, &b.path)));
    emit_report("status", reports, json, None)
}

// ── Environment injection ───────────────────────────────────────────────────

/// Paths and probes injectable for tests (prefer over public env vars).
#[derive(Debug, Clone)]
pub struct InstallEnv {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub appdata: Option<PathBuf>,
    /// When set, used instead of real PATH probes for detection.
    pub binary_probe: Option<fn(&str) -> bool>,
}

impl InstallEnv {
    pub fn from_process() -> miette::Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| miette::miette!("could not determine home directory"))?;
        let cwd = std::env::current_dir()
            .map_err(|e| miette::miette!("could not determine current directory: {e}"))?;
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        Ok(Self {
            home,
            cwd,
            appdata,
            binary_probe: None,
        })
    }

    fn binary_present(&self, name: &str) -> bool {
        if let Some(probe) = self.binary_probe {
            return probe(name);
        }
        crate::util::which::which(name).is_some()
    }
}

fn select_targets(
    platforms: &[String],
    env: &InstallEnv,
    for_uninstall: bool,
) -> miette::Result<Vec<PlatformId>> {
    if !platforms.is_empty() {
        let mut ids = Vec::with_capacity(platforms.len());
        for p in platforms {
            ids.push(PlatformId::parse(p).map_err(|e| miette::miette!("{e}"))?);
        }
        // Deterministic order following PLATFORM_IDS
        ids.sort_by_key(|id| {
            PLATFORM_IDS
                .iter()
                .position(|s| *s == id.as_str())
                .unwrap_or(usize::MAX)
        });
        ids.dedup();
        return Ok(ids);
    }

    // Detect
    let mut found = Vec::new();
    for &id in PlatformId::all() {
        let paths = match resolve_paths(id, &env.home, &env.cwd, env.appdata.as_deref()) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if is_detected(id, &paths, |b| env.binary_present(b)) {
            found.push(id);
        }
    }

    if found.is_empty() {
        if for_uninstall {
            // Uninstall with no detection: still walk all four with default scopes
            // so idempotent absent reports are useful — or error?
            // Spec: when --platform omitted, detect; if none → non-zero with help.
            return Err(miette::miette!(
                "no agent platforms detected. Specify one explicitly, e.g.\n  \
                 ledgerful mcp uninstall --platform cursor\n\
                 Supported: {}",
                PLATFORM_IDS.join(", ")
            ));
        }
        return Err(miette::miette!(
            "no agent platforms detected (no config file and no host binary on PATH).\n  \
             Install with an explicit platform, e.g.\n  \
             ledgerful mcp install --platform cursor\n\
             Supported: {}",
            PLATFORM_IDS.join(", ")
        ));
    }
    Ok(found)
}

// ── Per-platform operations ─────────────────────────────────────────────────

fn install_one(
    id: PlatformId,
    scope: McpScope,
    launcher: &ResolvedLauncher,
    dry_run: bool,
    force: bool,
    backup: bool,
    env: &InstallEnv,
) -> PlatformReport {
    let paths = match resolve_paths(id, &env.home, &env.cwd, env.appdata.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            return PlatformReport {
                id: id.as_str().to_string(),
                path: String::new(),
                status: "error".to_string(),
                launcher: Some(launcher_mode_str(launcher)),
                command: Some(launcher.command.clone()),
                args: Some(launcher.args.clone()),
                message: Some(e),
            };
        }
    };
    let path = path_for_scope(&paths, scope).to_path_buf();
    let entry = ServerEntry {
        command: launcher.command.clone(),
        args: launcher.args.clone(),
        include_type_stdio: id.include_type_stdio(),
    };

    // Check existing without force
    match read_existing_entry(id, &path) {
        Ok(Some(existing)) if !force && !existing.same_launcher_shape(&entry) => {
            return PlatformReport {
                id: id.as_str().to_string(),
                path: path.display().to_string(),
                status: "skipped".to_string(),
                launcher: Some(launcher_mode_str(launcher)),
                command: Some(existing.command),
                args: Some(existing.args),
                message: Some(
                    "existing ledgerful entry has a different command/args; re-run with --force to replace"
                        .to_string(),
                ),
            };
        }
        Ok(Some(existing)) if !force && existing.same_launcher_shape(&entry) => {
            // Spec: without force, different launcher shape → skipped above.
            // Same launcher shape may re-write (idempotent refresh); fall through
            // to dry-run would_write / live written — not skipped.
            drop(existing);
        }
        Ok(_) => {}
        Err(e) => {
            return PlatformReport {
                id: id.as_str().to_string(),
                path: path.display().to_string(),
                status: "error".to_string(),
                launcher: Some(launcher_mode_str(launcher)),
                command: Some(launcher.command.clone()),
                args: Some(launcher.args.clone()),
                message: Some(e),
            };
        }
    }

    // Explicit --platform still writes when the host binary is missing; warn only.
    let host_binary_missing = !id
        .detection_binaries()
        .iter()
        .any(|b| env.binary_present(b));

    if dry_run {
        let msg = compose_install_message(id, scope, launcher, None, host_binary_missing);
        return PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "would_write".to_string(),
            launcher: Some(launcher_mode_str(launcher)),
            command: Some(launcher.command.clone()),
            args: Some(launcher.args.clone()),
            message: Some(msg),
        };
    }

    match write_merged(id, &path, &entry, backup) {
        Ok(backup_msg) => {
            let msg = compose_install_message(
                id,
                scope,
                launcher,
                backup_msg.as_deref(),
                host_binary_missing,
            );
            PlatformReport {
                id: id.as_str().to_string(),
                path: path.display().to_string(),
                status: "written".to_string(),
                launcher: Some(launcher_mode_str(launcher)),
                command: Some(launcher.command.clone()),
                args: Some(launcher.args.clone()),
                message: Some(msg),
            }
        }
        Err(e) => PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "error".to_string(),
            launcher: Some(launcher_mode_str(launcher)),
            command: Some(launcher.command.clone()),
            args: Some(launcher.args.clone()),
            message: Some(e),
        },
    }
}

/// Build install platform message: optional backup + launcher notes + host-trust + optional PATH warn.
fn compose_install_message(
    id: PlatformId,
    scope: McpScope,
    launcher: &ResolvedLauncher,
    backup_msg: Option<&str>,
    host_binary_missing: bool,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = backup_msg {
        parts.push(b.to_string());
    }
    if let Some(ref m) = launcher.message {
        parts.push(m.clone());
    }
    parts.push(id.host_trust_message(scope).to_string());
    if host_binary_missing {
        parts.push(format!(
            "warn: host binary for `{}` not detected on PATH; config still written",
            id.as_str()
        ));
    }
    parts.join("; ")
}

fn uninstall_one(
    id: PlatformId,
    scope: McpScope,
    dry_run: bool,
    env: &InstallEnv,
) -> PlatformReport {
    let paths = match resolve_paths(id, &env.home, &env.cwd, env.appdata.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            return PlatformReport {
                id: id.as_str().to_string(),
                path: String::new(),
                status: "error".to_string(),
                launcher: None,
                command: None,
                args: None,
                message: Some(e),
            };
        }
    };
    let path = path_for_scope(&paths, scope).to_path_buf();

    if !path.exists() {
        return PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "absent".to_string(),
            launcher: None,
            command: None,
            args: None,
            message: Some("config file does not exist".to_string()),
        };
    }

    let existing = match read_existing_entry(id, &path) {
        Ok(e) => e,
        Err(e) => {
            return PlatformReport {
                id: id.as_str().to_string(),
                path: path.display().to_string(),
                status: "error".to_string(),
                launcher: None,
                command: None,
                args: None,
                message: Some(e),
            };
        }
    };

    if existing.is_none() {
        return PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "absent".to_string(),
            launcher: None,
            command: None,
            args: None,
            message: Some("no ledgerful server entry".to_string()),
        };
    }

    if dry_run {
        return PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "would_write".to_string(),
            launcher: None,
            command: existing.as_ref().map(|e| e.command.clone()),
            args: existing.as_ref().map(|e| e.args.clone()),
            message: Some(
                "would remove ledgerful entry only; other servers preserved; written ≠ connected (host may still cache tools until reload)"
                    .to_string(),
            ),
        };
    }

    match write_removed(id, &path, true) {
        Ok(_) => PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "written".to_string(),
            launcher: None,
            command: None,
            args: None,
            message: Some(
                "removed ledgerful entry only; other servers preserved; written ≠ connected (host may still cache tools until reload)"
                    .to_string(),
            ),
        },
        Err(e) => PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "error".to_string(),
            launcher: None,
            command: None,
            args: None,
            message: Some(e),
        },
    }
}

fn status_one(id: PlatformId, scope: McpScope, env: &InstallEnv) -> PlatformReport {
    let paths = match resolve_paths(id, &env.home, &env.cwd, env.appdata.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            return PlatformReport {
                id: id.as_str().to_string(),
                path: format!("({scope:?})"),
                status: "error".to_string(),
                launcher: None,
                command: None,
                args: None,
                message: Some(e),
            };
        }
    };
    let path = path_for_scope(&paths, scope).to_path_buf();
    let detected = is_detected(id, &paths, |b| env.binary_present(b));

    if !path.exists() {
        return PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "absent".to_string(),
            launcher: None,
            command: None,
            args: None,
            message: Some(if detected {
                format!("scope={scope:?}; host detected but config path missing; not connected")
            } else {
                format!("scope={scope:?}; no config and host not detected")
            }),
        };
    }

    match read_existing_entry(id, &path) {
        Ok(Some(entry)) => PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "written".to_string(),
            launcher: None,
            command: Some(entry.command),
            args: Some(entry.args),
            message: Some(format!(
                "scope={scope:?}; ledgerful entry present; host_detected={detected}; file presence only, not a live host connection"
            )),
        },
        Ok(None) => PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "absent".to_string(),
            launcher: None,
            command: None,
            args: None,
            message: Some(format!(
                "scope={scope:?}; config exists but no ledgerful entry; host_detected={detected}; file presence only, not a live host connection"
            )),
        },
        Err(e) => PlatformReport {
            id: id.as_str().to_string(),
            path: path.display().to_string(),
            status: "error".to_string(),
            launcher: None,
            command: None,
            args: None,
            message: Some(e),
        },
    }
}

// ── Read / write ────────────────────────────────────────────────────────────

fn read_existing_entry(id: PlatformId, path: &Path) -> Result<Option<ServerEntry>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    match id.config_format() {
        ConfigFormat::Json => {
            let key = id
                .json_parent_key()
                .ok_or_else(|| "internal: json platform missing parent key".to_string())?;
            let root = parse_jsonc(&content)?;
            Ok(get_json_server(&root, key))
        }
        ConfigFormat::Toml => {
            let doc: DocumentMut = content
                .parse()
                .map_err(|e| format!("TOML parse error in {}: {e}", path.display()))?;
            Ok(get_toml_server(&doc))
        }
    }
}

fn write_merged(
    id: PlatformId,
    path: &Path,
    entry: &ServerEntry,
    backup: bool,
) -> Result<Option<String>, String> {
    let content = if path.exists() {
        fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?
    } else {
        String::new()
    };

    let new_content = match id.config_format() {
        ConfigFormat::Json => {
            let key = id
                .json_parent_key()
                .ok_or_else(|| "internal: json platform missing parent key".to_string())?;
            let mut root = parse_jsonc(&content)?;
            merge_json_server(&mut root, key, entry)?;
            serialize_json(&root)?
        }
        ConfigFormat::Toml => {
            let mut doc: DocumentMut = if content.trim().is_empty() {
                DocumentMut::new()
            } else {
                content
                    .parse()
                    .map_err(|e| format!("TOML parse error in {}: {e}", path.display()))?
            };
            merge_toml_server(&mut doc, entry)?;
            doc.to_string()
        }
    };

    let backup_msg = if backup && path.exists() {
        Some(write_sibling_backup(path)?)
    } else {
        None
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create parent {}: {e}", parent.display()))?;
    }

    atomic_write(path, new_content.as_bytes())?;
    Ok(backup_msg)
}

fn write_removed(id: PlatformId, path: &Path, backup: bool) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;

    let new_content = match id.config_format() {
        ConfigFormat::Json => {
            let key = id
                .json_parent_key()
                .ok_or_else(|| "internal: json platform missing parent key".to_string())?;
            let mut root = parse_jsonc(&content)?;
            let _ = remove_json_server(&mut root, key)?;
            serialize_json(&root)?
        }
        ConfigFormat::Toml => {
            let mut doc: DocumentMut = content
                .parse()
                .map_err(|e| format!("TOML parse error in {}: {e}", path.display()))?;
            let _ = remove_toml_server(&mut doc);
            doc.to_string()
        }
    };

    if backup {
        let _ = write_sibling_backup(path)?;
    }
    atomic_write(path, new_content.as_bytes())
}

fn write_sibling_backup(path: &Path) -> Result<String, String> {
    let bak = sibling_bak(path);
    let bytes = fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| format!("stat {}: {e}", path.display()))?;
    fs::copy(path, &bak).map_err(|e| format!("backup to {}: {e}", bak.display()))?;
    Ok(format!("backed up {bytes} bytes to {}", bak.display()))
}

fn sibling_bak(path: &Path) -> PathBuf {
    let mut bak = path.as_os_str().to_os_string();
    bak.push(".bak");
    PathBuf::from(bak)
}

/// Same-directory temp + rename. Never deletes the only copy of the target.
///
/// Replace sequence when `path` exists:
/// 1. Write + sync `*.ledgerful-mcp-tmp`
/// 2. Rename target → `*.ledgerful-mcp-prev`
/// 3. Rename tmp → path; on success remove prev (best-effort)
/// 4. On step-3 failure: rename prev back to path (restore), clean tmp (best-effort)
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("invalid path {}", path.display()))?;

    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".ledgerful-mcp-tmp");
    let tmp_path = parent.join(tmp_name);

    let mut prev_name = file_name.to_os_string();
    prev_name.push(".ledgerful-mcp-prev");
    let prev_path = parent.join(prev_name);

    {
        let mut f = fs::File::create(&tmp_path)
            .map_err(|e| format!("create temp {}: {e}", tmp_path.display()))?;
        f.write_all(data)
            .map_err(|e| format!("write temp {}: {e}", tmp_path.display()))?;
        f.sync_all()
            .map_err(|e| format!("sync temp {}: {e}", tmp_path.display()))?;
    }

    if !path.exists() {
        return match fs::rename(&tmp_path, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                Err(format!("rename temp over {}: {e}", path.display()))
            }
        };
    }

    // Drop a stale prev from a prior interrupted replace (best-effort).
    if prev_path.exists() {
        let _ = fs::remove_file(&prev_path);
    }

    if let Err(e) = fs::rename(path, &prev_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "rename target aside before replace {}: {e}",
            path.display()
        ));
    }

    match fs::rename(&tmp_path, path) {
        Ok(()) => {
            let _ = fs::remove_file(&prev_path);
            Ok(())
        }
        Err(e) => {
            // Restore the only prior copy; never leave the target deleted.
            let restore_err = fs::rename(&prev_path, path).err();
            let _ = fs::remove_file(&tmp_path);
            match restore_err {
                None => Err(format!(
                    "rename temp over {}: {e} (original restored)",
                    path.display()
                )),
                Some(re) => Err(format!(
                    "rename temp over {}: {e}; restore failed: {re} (original at {})",
                    path.display(),
                    prev_path.display()
                )),
            }
        }
    }
}

fn launcher_mode_str(launcher: &ResolvedLauncher) -> String {
    match launcher.mode {
        super::launcher::LauncherMode::Path => "path".to_string(),
        super::launcher::LauncherMode::Npx => "npx".to_string(),
    }
}

fn emit_report(
    action: &str,
    platforms: Vec<PlatformReport>,
    json: bool,
    global_message: Option<&str>,
) -> miette::Result<()> {
    let report = McpInstallReport {
        schema_version: 1,
        action: action.to_string(),
        platforms,
    };

    if json {
        let s = serde_json::to_string_pretty(&report)
            .map_err(|e| miette::miette!("serialize report: {e}"))?;
        println!("{s}");
    } else {
        if let Some(m) = global_message {
            println!("note: {m}");
        }
        println!("MCP {action} (schemaVersion {})", report.schema_version);
        for p in &report.platforms {
            let msg = p.message.as_deref().unwrap_or("");
            println!(
                "  [{}] {} @ {}{}",
                p.status,
                p.id,
                p.path,
                if msg.is_empty() {
                    String::new()
                } else {
                    format!(" — {msg}")
                }
            );
            if let (Some(cmd), Some(args)) = (&p.command, &p.args) {
                println!("      command: {cmd} {}", args.join(" "));
            }
        }
        println!(
            "\nNote: status values describe config files only (written ≠ host tools connected)."
        );
    }

    // Non-zero if any error
    if report.platforms.iter().any(|p| p.status == "error") {
        miette::bail!("one or more platforms reported error status");
    }
    Ok(())
}

// ── Integration-style unit tests (temp dirs) ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::launcher::resolve_launcher;
    use super::*;
    use crate::cli::args::McpLauncher;

    /// Secure unique temp root (`tempfile`), not shared `std::env::temp_dir()` fixed names.
    fn unique_temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn env_at(root: &Path) -> InstallEnv {
        let home = root.join("home");
        let cwd = root.join("cwd");
        let appdata = root.join("appdata");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&appdata).unwrap();
        InstallEnv {
            home,
            cwd,
            appdata: Some(appdata),
            binary_probe: Some(|_| false),
        }
    }

    fn path_launcher() -> ResolvedLauncher {
        let bin = if cfg!(windows) {
            PathBuf::from(r"C:\bin\ledgerful.exe")
        } else {
            PathBuf::from("/usr/bin/ledgerful")
        };
        resolve_launcher(McpLauncher::Path, Some(bin), None, None).expect("launcher")
    }

    #[test]
    fn install_cursor_user_multi_server_preserves_foreign() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::Cursor,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::create_dir_all(paths.user.parent().unwrap()).unwrap();
        fs::write(
            &paths.user,
            r#"{"mcpServers":{"other":{"command":"other-bin"}}}"#,
        )
        .unwrap();

        let r = install_one(
            PlatformId::Cursor,
            McpScope::User,
            &path_launcher(),
            false,
            false,
            true,
            &env,
        );
        assert_eq!(r.status, "written", "{:?}", r.message);
        let content = fs::read_to_string(&paths.user).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v["mcpServers"]["other"].is_object());
        assert!(v["mcpServers"]["ledgerful"].is_object());
        assert_eq!(
            v["mcpServers"]["ledgerful"]["args"],
            serde_json::json!(["mcp"])
        );
        // backup created
        assert!(
            paths.user.with_extension("json.bak").exists() || {
                let mut bak = paths.user.as_os_str().to_os_string();
                bak.push(".bak");
                PathBuf::from(bak).exists()
            }
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_claude_user_top_level_not_projects() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::ClaudeCode,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::write(
            &paths.user,
            r#"{
  "projects": {
    "/repo": { "mcpServers": { "local": { "command": "local" } } }
  },
  "mcpServers": { "peer": { "command": "peer" } }
}"#,
        )
        .unwrap();

        let r = install_one(
            PlatformId::ClaudeCode,
            McpScope::User,
            &path_launcher(),
            false,
            true,
            false,
            &env,
        );
        assert_eq!(r.status, "written");
        let content = fs::read_to_string(&paths.user).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v["mcpServers"]["ledgerful"].is_object());
        assert!(v["mcpServers"]["peer"].is_object());
        assert!(
            v["projects"]["/repo"]["mcpServers"]
                .get("ledgerful")
                .is_none()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_copilot_project_servers_and_type_stdio() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::Copilot,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();

        let r = install_one(
            PlatformId::Copilot,
            McpScope::Project,
            &path_launcher(),
            false,
            false,
            false,
            &env,
        );
        assert_eq!(r.status, "written", "{:?}", r.message);
        let content = fs::read_to_string(&paths.project).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v.get("mcpServers").is_none());
        assert_eq!(v["servers"]["ledgerful"]["type"], "stdio");
        assert!(v["servers"]["ledgerful"]["command"].is_string());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_codex_toml_args_array_peer_preserved() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::Codex,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::create_dir_all(paths.user.parent().unwrap()).unwrap();
        fs::write(
            &paths.user,
            r#"
[mcp_servers.peer]
command = "peer"
args = ["a"]
"#,
        )
        .unwrap();

        let r = install_one(
            PlatformId::Codex,
            McpScope::User,
            &path_launcher(),
            false,
            false,
            true,
            &env,
        );
        assert_eq!(r.status, "written", "{:?}", r.message);
        let content = fs::read_to_string(&paths.user).unwrap();
        assert!(content.contains("[mcp_servers.ledgerful]") || content.contains("ledgerful"));
        assert!(content.contains("peer"));
        let doc: DocumentMut = content.parse().unwrap();
        let entry = get_toml_server(&doc).unwrap();
        assert_eq!(entry.args, vec!["mcp".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_dry_run_no_file_change() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::Cursor,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::create_dir_all(paths.user.parent().unwrap()).unwrap();
        let original = r#"{"mcpServers":{}}"#;
        fs::write(&paths.user, original).unwrap();

        let r = install_one(
            PlatformId::Cursor,
            McpScope::User,
            &path_launcher(),
            true,
            false,
            true,
            &env,
        );
        assert_eq!(r.status, "would_write");
        assert_eq!(fs::read_to_string(&paths.user).unwrap(), original);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn uninstall_only_ledgerful_and_idempotent() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::Cursor,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::create_dir_all(paths.user.parent().unwrap()).unwrap();
        fs::write(
            &paths.user,
            r#"{"mcpServers":{"ledgerful":{"command":"x","args":["mcp"]},"other":{"command":"y"}}}"#,
        )
        .unwrap();

        let r1 = uninstall_one(PlatformId::Cursor, McpScope::User, false, &env);
        assert_eq!(r1.status, "written");
        let msg = r1.message.as_deref().unwrap_or("");
        assert!(
            msg.contains("removed ledgerful entry only") && msg.contains("other servers preserved"),
            "uninstall honesty base: {msg}"
        );
        assert!(
            msg.contains("written ≠ connected") || msg.contains("written != connected"),
            "uninstall written≠connected clause: {msg}"
        );
        let content = fs::read_to_string(&paths.user).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v["mcpServers"].get("ledgerful").is_none());
        assert!(v["mcpServers"]["other"].is_object());

        let r2 = uninstall_one(PlatformId::Cursor, McpScope::User, false, &env);
        assert_eq!(r2.status, "absent");
        // file still exists
        assert!(paths.user.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn uninstall_dry_run_honesty_message() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::Cursor,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::create_dir_all(paths.user.parent().unwrap()).unwrap();
        fs::write(
            &paths.user,
            r#"{"mcpServers":{"ledgerful":{"command":"x","args":["mcp"]}}}"#,
        )
        .unwrap();
        let r = uninstall_one(PlatformId::Cursor, McpScope::User, true, &env);
        assert_eq!(r.status, "would_write");
        let msg = r.message.as_deref().unwrap_or("");
        assert!(
            msg.contains("would remove ledgerful entry only"),
            "dry-run base: {msg}"
        );
        assert!(
            msg.contains("written ≠ connected") || msg.contains("written != connected"),
            "dry-run written≠connected: {msg}"
        );
        // File unchanged
        assert!(
            fs::read_to_string(&paths.user)
                .unwrap()
                .contains("ledgerful")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_force_false_skips_mismatched_entry() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::Cursor,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::create_dir_all(paths.user.parent().unwrap()).unwrap();
        fs::write(
            &paths.user,
            r#"{"mcpServers":{"ledgerful":{"command":"old-bin","args":["mcp"]}}}"#,
        )
        .unwrap();

        let r = install_one(
            PlatformId::Cursor,
            McpScope::User,
            &path_launcher(),
            false,
            false,
            false,
            &env,
        );
        assert_eq!(r.status, "skipped");
        let content = fs::read_to_string(&paths.user).unwrap();
        assert!(content.contains("old-bin"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_jsonc_comment_fixture() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let paths = resolve_paths(
            PlatformId::ClaudeCode,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::write(
            &paths.user,
            r#"{
  // agent config
  "mcpServers": {
    "x": { "command": "x" },
  },
}"#,
        )
        .unwrap();

        let r = install_one(
            PlatformId::ClaudeCode,
            McpScope::User,
            &path_launcher(),
            false,
            true,
            false,
            &env,
        );
        assert_eq!(r.status, "written", "{:?}", r.message);
        let content = fs::read_to_string(&paths.user).unwrap();
        assert!(content.contains("ledgerful"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_explicit_platform_warns_when_host_binary_missing() {
        // env_at probes always return false (host not on PATH) but explicit install still writes.
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root);
        let r = install_one(
            PlatformId::Cursor,
            McpScope::User,
            &path_launcher(),
            false,
            false,
            false,
            &env,
        );
        assert_eq!(r.status, "written", "{:?}", r.message);
        let msg = r.message.as_deref().unwrap_or("");
        assert!(
            msg.contains("host binary") && msg.contains("not detected on PATH"),
            "expected host-binary warn in message: {msg}"
        );
        assert!(
            msg.contains("written ≠ connected")
                || msg.contains("written != connected")
                || msg.contains("Restart")
                || msg.contains("reload"),
            "host-trust still present: {msg}"
        );
        // Config file was created despite missing host binary.
        let paths = resolve_paths(
            PlatformId::Cursor,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        assert!(paths.user.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_no_host_binary_warn_when_probe_finds_binary() {
        let tmp = unique_temp();
        let root = tmp.path();
        let mut env = env_at(&root);
        env.binary_probe = Some(|_| true);
        let r = install_one(
            PlatformId::Cursor,
            McpScope::User,
            &path_launcher(),
            true, // dry-run
            false,
            false,
            &env,
        );
        assert_eq!(r.status, "would_write", "{:?}", r.message);
        let msg = r.message.as_deref().unwrap_or("");
        assert!(
            !msg.contains("not detected on PATH"),
            "should not warn when host binary probe succeeds: {msg}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_platform_id_rejects_unknown() {
        let err = parse_platform_id("windsurf").unwrap_err();
        assert!(err.contains("claude-code"));
    }

    #[test]
    fn path_launcher_args_distinct_from_command() {
        let l = path_launcher();
        assert!(l.command.ends_with("ledgerful") || l.command.ends_with("ledgerful.exe"));
        assert_eq!(l.args, vec!["mcp".to_string()]);
    }

    #[test]
    fn atomic_write_create_and_replace_happy_path() {
        let tmp = unique_temp();
        let root = tmp.path();
        let path = root.join("mcp.json");

        atomic_write(&path, br#"{"v":1}"#).expect("create");
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"v":1}"#);
        // No leftover temp/prev after success
        assert!(!root.join("mcp.json.ledgerful-mcp-tmp").exists());
        assert!(!root.join("mcp.json.ledgerful-mcp-prev").exists());

        atomic_write(&path, br#"{"v":2}"#).expect("replace");
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"v":2}"#);
        assert!(!root.join("mcp.json.ledgerful-mcp-tmp").exists());
        assert!(!root.join("mcp.json.ledgerful-mcp-prev").exists());

        // Document restore contract: if prev were left behind, a subsequent
        // replace must not treat it as the live config (happy path cleans prev).
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn status_one_includes_host_detected_when_config_exists() {
        let tmp = unique_temp();
        let root = tmp.path();
        let env = env_at(&root); // binary probe false; path existence still counts as detected
        let paths = resolve_paths(
            PlatformId::Cursor,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        )
        .unwrap();
        fs::create_dir_all(paths.user.parent().unwrap()).unwrap();

        // Entry present — is_detected true because config path exists
        fs::write(
            &paths.user,
            r#"{"mcpServers":{"ledgerful":{"command":"x","args":["mcp"]}}}"#,
        )
        .unwrap();
        let present = status_one(PlatformId::Cursor, McpScope::User, &env);
        assert_eq!(present.status, "written");
        let msg = present.message.as_deref().unwrap_or("");
        assert!(
            msg.contains("host_detected=true") && msg.contains("ledgerful entry present"),
            "present message: {msg}"
        );
        assert!(
            msg.contains("file presence only") || msg.contains("not a live host connection"),
            "honesty: {msg}"
        );

        // Entry absent, file exists — still host_detected=true (path exists)
        fs::write(&paths.user, r#"{"mcpServers":{"other":{"command":"y"}}}"#).unwrap();
        let absent = status_one(PlatformId::Cursor, McpScope::User, &env);
        assert_eq!(absent.status, "absent");
        let msg = absent.message.as_deref().unwrap_or("");
        assert!(
            msg.contains("host_detected=true") && msg.contains("no ledgerful entry"),
            "absent-entry message: {msg}"
        );

        // No config file + probe false → host_detected=false path (missing-file branch)
        fs::remove_file(&paths.user).unwrap();
        let missing = status_one(PlatformId::Cursor, McpScope::User, &env);
        assert_eq!(missing.status, "absent");
        let msg = missing.message.as_deref().unwrap_or("");
        assert!(
            msg.contains("host not detected") || msg.contains("host_detected=false"),
            "missing config, no binary: {msg}"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
