use miette::{Diagnostic, Result};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPolicy {
    pub allowed_commands: Vec<String>,
    pub denied_commands: Vec<String>,
    pub default_timeout_secs: u64,
    /// When true, an empty allowlist means no commands are permitted.
    /// Defaults to false for backward compatibility.
    pub strict: bool,
}

impl Default for ProcessPolicy {
    fn default() -> Self {
        Self {
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: false,
        }
    }
}

#[derive(Debug, Error, Diagnostic)]
pub enum ProcessPolicyError {
    #[error("Command '{command}' is denied by process policy")]
    Denied { command: String },
}

pub fn check_policy(command: &str, policy: &ProcessPolicy) -> Result<(), ProcessPolicyError> {
    let normalized = command.trim();

    if policy
        .denied_commands
        .iter()
        .any(|denied| matches_command(normalized, denied))
    {
        return Err(ProcessPolicyError::Denied {
            command: normalized.to_string(),
        });
    }

    if policy.allowed_commands.is_empty() {
        if policy.strict {
            return Err(ProcessPolicyError::Denied {
                command: normalized.to_string(),
            });
        }
        return Ok(());
    }

    if policy
        .allowed_commands
        .iter()
        .any(|allowed| matches_command(normalized, allowed))
    {
        Ok(())
    } else {
        Err(ProcessPolicyError::Denied {
            command: normalized.to_string(),
        })
    }
}

fn matches_command(command: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    let command = command.trim();

    if pattern.eq_ignore_ascii_case(command) {
        return true;
    }

    let command_path = resolve_executable(command);
    let pattern_path = resolve_executable(pattern);

    if let (Some(cmd_path), Some(pat_path)) = (command_path.as_deref(), pattern_path.as_deref())
        && cmd_path == pat_path
    {
        return true;
    }

    false
}

fn resolve_executable(name: &str) -> Option<PathBuf> {
    let path = std::path::Path::new(name);
    if path.is_absolute() || path.components().count() > 1 {
        return std::fs::canonicalize(path).ok();
    }

    crate::util::which::which(name).and_then(|p| std::fs::canonicalize(p).ok())
}

pub fn force_unlock_processes() -> miette::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        // Kill any background ledgerful processes that might hold file locks
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "ledgerful.exe", "/T"])
            .status();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy_allows_anything_when_not_strict() {
        let policy = ProcessPolicy::default();
        assert!(check_policy("cargo test", &policy).is_ok());
    }

    #[test]
    fn test_default_policy_denies_everything_when_strict() {
        let policy = ProcessPolicy {
            strict: true,
            ..ProcessPolicy::default()
        };
        assert!(check_policy("cargo test", &policy).is_err());
    }

    #[test]
    fn test_deny_list_blocks_command() {
        let policy = ProcessPolicy {
            denied_commands: vec!["rm -rf /".to_string()],
            ..ProcessPolicy::default()
        };
        assert!(check_policy("rm -rf /", &policy).is_err());
    }

    #[test]
    fn test_allow_list_permits_only_allowed_commands() {
        let policy = ProcessPolicy {
            allowed_commands: vec!["cargo".to_string(), "cargo.exe".to_string()],
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: false,
        };
        assert!(check_policy("cargo", &policy).is_ok());
        assert!(check_policy("npm", &policy).is_err());
    }

    #[test]
    fn test_canonical_path_matches_allowlisted_name() {
        let policy = ProcessPolicy {
            allowed_commands: vec!["cargo".to_string()],
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: false,
        };
        let cargo_path = resolve_executable("cargo").unwrap();
        assert!(check_policy(cargo_path.to_string_lossy().as_ref(), &policy).is_ok());
    }
}
