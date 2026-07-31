//! MCP server launcher resolution (`path` | `npx` | `auto`).

use std::path::{Path, PathBuf};

/// Resolved command + args for a host MCP server entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLauncher {
    pub command: String,
    pub args: Vec<String>,
    /// Human/JSON message (e.g. npm pin lag when auto falls back to npx).
    pub message: Option<String>,
    /// Which mode produced this resolution.
    pub mode: LauncherMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherMode {
    Path,
    Npx,
}

/// Resolve launcher for install.
///
/// `which_ledgerful` / `which_npx` / `which_npx_cmd` are injectable for tests.
/// On Windows, npx prefers `npx.cmd` when present.
pub fn resolve_launcher(
    mode: crate::cli::args::McpLauncher,
    which_ledgerful: Option<PathBuf>,
    which_npx: Option<PathBuf>,
    which_npx_cmd: Option<PathBuf>,
) -> Result<ResolvedLauncher, String> {
    match mode {
        crate::cli::args::McpLauncher::Path => resolve_path(which_ledgerful),
        crate::cli::args::McpLauncher::Npx => Ok(resolve_npx(which_npx, which_npx_cmd, None)),
        crate::cli::args::McpLauncher::Auto => {
            if which_ledgerful.is_some() {
                resolve_path(which_ledgerful)
            } else {
                Ok(resolve_npx(
                    which_npx,
                    which_npx_cmd,
                    Some(
                        "ledgerful not on PATH; using npx @ledgerful/mcp-server \
                         (published npm pin may lag the engine release)"
                            .to_string(),
                    ),
                ))
            }
        }
    }
}

fn resolve_path(which_ledgerful: Option<PathBuf>) -> Result<ResolvedLauncher, String> {
    let path = which_ledgerful.ok_or_else(|| {
        "ledgerful not found on PATH; install the binary or use --launcher npx".to_string()
    })?;
    let command = path_to_command_string(&path);
    Ok(ResolvedLauncher {
        command,
        args: vec!["mcp".to_string()],
        message: None,
        mode: LauncherMode::Path,
    })
}

fn resolve_npx(
    which_npx: Option<PathBuf>,
    which_npx_cmd: Option<PathBuf>,
    message: Option<String>,
) -> ResolvedLauncher {
    let command = npx_command(which_npx.as_deref(), which_npx_cmd.as_deref());
    ResolvedLauncher {
        command,
        args: vec!["-y".to_string(), "@ledgerful/mcp-server".to_string()],
        message,
        mode: LauncherMode::Npx,
    }
}

/// Windows prefers `npx.cmd` when on PATH; otherwise `npx` (or absolute path).
fn npx_command(which_npx: Option<&Path>, which_npx_cmd: Option<&Path>) -> String {
    if cfg!(windows) {
        if let Some(cmd) = which_npx_cmd {
            return path_to_command_string(cmd);
        }
        if let Some(npx) = which_npx {
            return path_to_command_string(npx);
        }
        "npx.cmd".to_string()
    } else {
        if let Some(npx) = which_npx {
            return path_to_command_string(npx);
        }
        "npx".to_string()
    }
}

fn path_to_command_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Probe PATH via the shared which helper (production entry).
pub fn resolve_launcher_from_path(
    mode: crate::cli::args::McpLauncher,
) -> Result<ResolvedLauncher, String> {
    let which_ledgerful = crate::util::which::which("ledgerful");
    let which_npx = crate::util::which::which("npx");
    let which_npx_cmd = if cfg!(windows) {
        crate::util::which::which("npx.cmd")
    } else {
        None
    };
    resolve_launcher(mode, which_ledgerful, which_npx, which_npx_cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::McpLauncher;
    use std::path::PathBuf;

    #[test]
    fn path_launcher_absolute_binary_args_mcp_only() {
        let bin = if cfg!(windows) {
            PathBuf::from(r"C:\tools\ledgerful.exe")
        } else {
            PathBuf::from("/usr/local/bin/ledgerful")
        };
        let r = resolve_launcher(McpLauncher::Path, Some(bin.clone()), None, None)
            .expect("path should resolve");
        assert_eq!(r.mode, LauncherMode::Path);
        assert_eq!(r.args, vec!["mcp".to_string()]);
        assert!(r.command.ends_with("ledgerful") || r.command.ends_with("ledgerful.exe"));
        assert!(r.message.is_none());
        // Distinct asserts: command path vs args
        assert_ne!(r.command, "mcp");
        assert_eq!(r.args.as_slice(), &["mcp"]);
    }

    #[test]
    fn path_launcher_missing_binary_errors() {
        let err = resolve_launcher(McpLauncher::Path, None, None, None).unwrap_err();
        assert!(err.contains("PATH") || err.contains("npx"));
    }

    #[test]
    fn npx_launcher_args_contain_package() {
        let r = resolve_launcher(McpLauncher::Npx, None, None, None).expect("npx always resolves");
        assert_eq!(r.mode, LauncherMode::Npx);
        assert!(r.args.iter().any(|a| a == "@ledgerful/mcp-server"));
        assert_eq!(
            r.args,
            vec!["-y".to_string(), "@ledgerful/mcp-server".to_string()]
        );
    }

    #[test]
    fn npx_launcher_windows_prefers_npx_cmd() {
        let r = resolve_launcher(
            McpLauncher::Npx,
            None,
            Some(PathBuf::from(r"C:\npm\npx")),
            Some(PathBuf::from(r"C:\npm\npx.cmd")),
        )
        .expect("npx");
        if cfg!(windows) {
            assert!(
                r.command.ends_with("npx.cmd") || r.command.contains("npx.cmd"),
                "expected npx.cmd, got {}",
                r.command
            );
        }
    }

    #[test]
    fn auto_launcher_path_when_present() {
        let bin = PathBuf::from("/opt/ledgerful");
        let r = resolve_launcher(McpLauncher::Auto, Some(bin), None, None).expect("auto");
        assert_eq!(r.mode, LauncherMode::Path);
        assert_eq!(r.args, vec!["mcp".to_string()]);
        assert!(r.message.is_none());
    }

    #[test]
    fn auto_launcher_npx_fallback_with_pin_lag_message() {
        let r = resolve_launcher(McpLauncher::Auto, None, None, None).expect("auto");
        assert_eq!(r.mode, LauncherMode::Npx);
        assert!(
            r.message
                .as_deref()
                .is_some_and(|m| m.contains("npx") || m.contains("pin")),
            "expected pin-lag warn, got {:?}",
            r.message
        );
    }
}
