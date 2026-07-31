//! Top-N agent platform path resolution and format metadata.

use std::path::{Path, PathBuf};

/// Exactly four platform ids (shared by clap parser, help, and errors).
pub const PLATFORM_IDS: &[&str] = &["claude-code", "cursor", "codex", "copilot"];

/// Stable server entry name in host configs.
pub const SERVER_NAME: &str = crate::commands::mcp::merge::SERVER_NAME;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformId {
    ClaudeCode,
    Cursor,
    Codex,
    Copilot,
}

impl PlatformId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "claude-code" => Ok(Self::ClaudeCode),
            "cursor" => Ok(Self::Cursor),
            "codex" => Ok(Self::Codex),
            "copilot" => Ok(Self::Copilot),
            other => Err(format!(
                "unknown platform `{other}`; supported: {}",
                PLATFORM_IDS.join(", ")
            )),
        }
    }

    pub fn all() -> &'static [PlatformId] {
        &[
            Self::ClaudeCode,
            Self::Cursor,
            Self::Codex,
            Self::Copilot,
        ]
    }

    /// Default scope when `--scope` is omitted.
    pub fn default_scope(self) -> crate::cli::args::McpScope {
        match self {
            Self::Copilot => crate::cli::args::McpScope::Project,
            _ => crate::cli::args::McpScope::User,
        }
    }

    /// Host binary names used for detection (any present counts).
    pub fn detection_binaries(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &["claude"],
            Self::Cursor => &["cursor"],
            Self::Codex => &["codex"],
            Self::Copilot => {
                if cfg!(windows) {
                    &["code", "code.cmd"]
                } else {
                    &["code"]
                }
            }
        }
    }

    pub fn config_format(self) -> ConfigFormat {
        match self {
            Self::Codex => ConfigFormat::Toml,
            _ => ConfigFormat::Json,
        }
    }

    /// JSON parent key (`mcpServers` or `servers`). None for TOML.
    pub fn json_parent_key(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode | Self::Cursor => Some("mcpServers"),
            Self::Copilot => Some("servers"),
            Self::Codex => None,
        }
    }

    pub fn include_type_stdio(self) -> bool {
        matches!(self, Self::Copilot)
    }

    /// Host-trust honesty message after a successful write (scope-aware).
    pub fn host_trust_message(self, scope: crate::cli::args::McpScope) -> &'static str {
        match (self, scope) {
            (Self::Codex, crate::cli::args::McpScope::Project) => {
                "Config written; Codex project MCP may stay inert until you confirm project trust in a Codex session (written ≠ connected)"
            }
            (Self::ClaudeCode, crate::cli::args::McpScope::Project) => {
                "Config written; Claude Code may prompt to approve this server on first use (written ≠ connected)"
            }
            (Self::Copilot, _) => {
                "Config written; VS Code / Copilot may show an MCP trust dialog on first start (written ≠ connected)"
            }
            _ => "Config written; restart or reload the host agent to pick up tools (written ≠ connected)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Toml,
}

/// Paths for a platform under injected `home` + `cwd` (and optional Windows APPDATA).
#[derive(Debug, Clone)]
pub struct PlatformPaths {
    pub user: PathBuf,
    pub project: PathBuf,
}

/// Resolve user/project config paths.
///
/// `home` — user home directory.
/// `cwd` — project root / current working directory.
/// `appdata` — Windows `%APPDATA%` (or None on non-Windows / when unset). Used for Copilot user scope.
pub fn resolve_paths(
    id: PlatformId,
    home: &Path,
    cwd: &Path,
    appdata: Option<&Path>,
) -> Result<PlatformPaths, String> {
    match id {
        PlatformId::ClaudeCode => Ok(PlatformPaths {
            user: home.join(".claude.json"),
            project: cwd.join(".mcp.json"),
        }),
        PlatformId::Cursor => Ok(PlatformPaths {
            user: home.join(".cursor").join("mcp.json"),
            project: cwd.join(".cursor").join("mcp.json"),
        }),
        PlatformId::Codex => Ok(PlatformPaths {
            user: home.join(".codex").join("config.toml"),
            project: cwd.join(".codex").join("config.toml"),
        }),
        PlatformId::Copilot => {
            let user = copilot_user_mcp_path(home, appdata)?;
            Ok(PlatformPaths {
                user,
                project: cwd.join(".vscode").join("mcp.json"),
            })
        }
    }
}

fn copilot_user_mcp_path(home: &Path, appdata: Option<&Path>) -> Result<PathBuf, String> {
    if cfg!(windows) {
        let appdata = appdata.ok_or_else(|| {
            "Copilot user scope requires APPDATA (default VS Code profile only; \
             use --scope project for .vscode/mcp.json, or set APPDATA)"
                .to_string()
        })?;
        Ok(appdata.join("Code").join("User").join("mcp.json"))
    } else if cfg!(target_os = "macos") {
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("mcp.json"))
    } else {
        // Linux / other Unix: XDG-style default Code path
        let _ = home;
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(config_home.join("Code").join("User").join("mcp.json"))
    }
}

/// Path for the effective scope.
pub fn path_for_scope(paths: &PlatformPaths, scope: crate::cli::args::McpScope) -> &Path {
    match scope {
        crate::cli::args::McpScope::User => &paths.user,
        crate::cli::args::McpScope::Project => &paths.project,
    }
}

/// Detection: config exists OR any host binary on PATH.
pub fn is_detected(
    id: PlatformId,
    paths: &PlatformPaths,
    binary_present: impl Fn(&str) -> bool,
) -> bool {
    if paths.user.exists() || paths.project.exists() {
        return true;
    }
    id.detection_binaries()
        .iter()
        .any(|b| binary_present(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::McpScope;
    use std::path::PathBuf;

    #[test]
    fn platform_ids_exactly_four() {
        assert_eq!(PLATFORM_IDS.len(), 4);
        assert_eq!(PlatformId::all().len(), 4);
    }

    #[test]
    fn parse_platform_unknown_lists_four() {
        let err = PlatformId::parse("windsurf").unwrap_err();
        assert!(err.contains("claude-code"));
        assert!(err.contains("cursor"));
        assert!(err.contains("codex"));
        assert!(err.contains("copilot"));
        assert!(!err.contains("windsurf") || err.contains("unknown"));
    }

    #[test]
    fn paths_claude_user_is_home_claude_json() {
        let home = PathBuf::from("/home/u");
        let cwd = PathBuf::from("/repo");
        let p = resolve_paths(PlatformId::ClaudeCode, &home, &cwd, None).expect("paths");
        assert_eq!(p.user, PathBuf::from("/home/u/.claude.json"));
        assert_eq!(p.project, PathBuf::from("/repo/.mcp.json"));
    }

    #[test]
    fn paths_cursor_and_codex() {
        let home = PathBuf::from("/home/u");
        let cwd = PathBuf::from("/repo");
        let c = resolve_paths(PlatformId::Cursor, &home, &cwd, None).unwrap();
        assert!(c.user.ends_with(".cursor/mcp.json") || c.user.ends_with(".cursor\\mcp.json"));
        let x = resolve_paths(PlatformId::Codex, &home, &cwd, None).unwrap();
        assert!(
            x.user.ends_with(".codex/config.toml") || x.user.ends_with(".codex\\config.toml")
        );
    }

    #[test]
    fn paths_copilot_project_vscode_mcp_json() {
        let home = PathBuf::from("/home/u");
        let cwd = PathBuf::from("/repo");
        let appdata = PathBuf::from(r"C:\Users\u\AppData\Roaming");
        let p = resolve_paths(PlatformId::Copilot, &home, &cwd, Some(&appdata)).unwrap();
        assert!(
            p.project.ends_with(".vscode/mcp.json") || p.project.ends_with(".vscode\\mcp.json")
        );
        if cfg!(windows) {
            assert!(
                p.user.ends_with("Code\\User\\mcp.json")
                    || p.user.ends_with("Code/User/mcp.json")
            );
        }
    }

    #[test]
    fn default_scope_copilot_project_others_user() {
        assert_eq!(PlatformId::Copilot.default_scope(), McpScope::Project);
        assert_eq!(PlatformId::ClaudeCode.default_scope(), McpScope::User);
        assert_eq!(PlatformId::Cursor.default_scope(), McpScope::User);
        assert_eq!(PlatformId::Codex.default_scope(), McpScope::User);
    }

    #[test]
    fn copilot_uses_servers_key() {
        assert_eq!(PlatformId::Copilot.json_parent_key(), Some("servers"));
        assert!(PlatformId::Copilot.include_type_stdio());
        assert_eq!(
            PlatformId::ClaudeCode.json_parent_key(),
            Some("mcpServers")
        );
    }
}
