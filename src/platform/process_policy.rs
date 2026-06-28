use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPolicy {
    pub allowed_commands: Vec<String>,
    pub denied_commands: Vec<String>,
    pub default_timeout_secs: u64,
}

impl Default for ProcessPolicy {
    fn default() -> Self {
        Self {
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
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
        .any(|denied| denied.eq_ignore_ascii_case(normalized))
    {
        return Err(ProcessPolicyError::Denied {
            command: normalized.to_string(),
        });
    }

    if policy.allowed_commands.is_empty()
        || policy
            .allowed_commands
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(normalized))
    {
        Ok(())
    } else {
        Err(ProcessPolicyError::Denied {
            command: normalized.to_string(),
        })
    }
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
    fn test_default_policy_allows_anything() {
        let policy = ProcessPolicy::default();
        assert!(check_policy("cargo test", &policy).is_ok());
    }

    #[test]
    fn test_deny_list_blocks_command() {
        let policy = ProcessPolicy {
            denied_commands: vec!["rm -rf /".to_string()],
            ..ProcessPolicy::default()
        };
        assert!(check_policy("rm -rf /", &policy).is_err());
    }
}
