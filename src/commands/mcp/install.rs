//! MCP host install / uninstall / status orchestration.
//!
//! File merge is the source of truth — never shells out to host CLIs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use toml_edit::DocumentMut;

use super::launcher::{resolve_launcher_from_path, ResolvedLauncher};
use super::merge::{
    get_json_server, get_toml_server, merge_json_server, merge_toml_server, parse_jsonc,
    remove_json_server, remove_toml_server, serialize_json, ServerEntry,
};
use super::platforms::{
    is_detected, path_for_scope, resolve_paths, ConfigFormat, PlatformId, PLATFORM_IDS,
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
        let paths = match resolve_paths(
            id,
            &env.home,
            &env.cwd,
            env.appdata.as_deref(),
        ) {
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
            // Idempotent already-correct: still report would_write/written semantics.
            // Treat as already present — report written if not dry-run would be no-op,
            // but re-writing is fine; for honesty use skipped with message when identical.
            // Spec: without force if different → skipped. Same shape can re-write.
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

    if dry_run {
        let mut msg = id.host_trust_message(scope).to_string();
        if let Some(ref m) = launcher.message {
            msg = format!("{m}; {msg}");
        }
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
            let mut msg = id.host_trust_message(scope).to_string();
            if let Some(b) = backup_msg {
                msg = format!("{b}; {msg}");
            }
            if let Some(ref m) = launcher.message {
                msg = format!("{m}; {msg}");
            }
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
            message: Some("would remove ledgerful entry only".to_string()),
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
            message: Some("removed ledgerful entry only; other servers preserved".to_string()),
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
                format!(
                    "scope={scope:?}; host detected but config path missing; not connected"
                )
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
                "scope={scope:?}; ledgerful entry present in config (file presence only; not a live host connection)"
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
                "scope={scope:?}; config exists but no ledgerful entry"
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
    let content =
        fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;

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
    Ok(format!(
        "backed up {bytes} bytes to {}",
        bak.display()
    ))
}

fn sibling_bak(path: &Path) -> PathBuf {
    let mut bak = path.as_os_str().to_os_string();
    bak.push(".bak");
    PathBuf::from(bak)
}

/// Same-directory temp + rename.
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("invalid path {}", path.display()))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".ledgerful-mcp-tmp");
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path)
            .map_err(|e| format!("create temp {}: {e}", tmp_path.display()))?;
        f.write_all(data)
            .map_err(|e| format!("write temp {}: {e}", tmp_path.display()))?;
        f.sync_all()
            .map_err(|e| format!("sync temp {}: {e}", tmp_path.display()))?;
    }

    // Windows: rename over existing may fail — remove target first if needed.
    if path.exists() {
        // Try rename; on failure remove and retry.
        match fs::rename(&tmp_path, path) {
            Ok(()) => Ok(()),
            Err(_) => {
                fs::remove_file(path)
                    .map_err(|e| format!("replace {}: {e}", path.display()))?;
                fs::rename(&tmp_path, path)
                    .map_err(|e| format!("rename temp over {}: {e}", path.display()))
            }
        }
    } else {
        fs::rename(&tmp_path, path)
            .map_err(|e| format!("rename temp over {}: {e}", path.display()))
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
    use super::*;
    use crate::cli::args::McpLauncher;
    use super::super::launcher::resolve_launcher;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!(
            "ledgerful-mcp-install-{}-{}",
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("temp");
        base
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
        let root = unique_temp();
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
        assert!(paths.user.with_extension("json.bak").exists() || {
            let mut bak = paths.user.as_os_str().to_os_string();
            bak.push(".bak");
            PathBuf::from(bak).exists()
        });
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_claude_user_top_level_not_projects() {
        let root = unique_temp();
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
        let root = unique_temp();
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
        let root = unique_temp();
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
        let root = unique_temp();
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
        let root = unique_temp();
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
    fn install_force_false_skips_mismatched_entry() {
        let root = unique_temp();
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
        let root = unique_temp();
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
}
