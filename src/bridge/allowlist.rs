//! Bridge `provider_command` allowlist (track 0073 / RT-A3).
//!
//! Only `ai-brains` / `ai-brains.exe` (basename) may be spawned. Absolute paths
//! are accepted only when the basename resolves to that allowlist. Evil
//! commands (`powershell`, `cmd`, shells) are rejected **before** spawn via
//! [`crate::platform::process_policy::check_policy`] with `strict: true`.

use crate::platform::process_policy::{ProcessPolicy, ProcessPolicyError, check_policy};
use std::path::Path;

/// Allowed basenames for bridge provider_command (case-insensitive on Windows).
const ALLOWED_BASENAMES: &[&str] = &["ai-brains", "ai-brains.exe"];

/// Build the strict process policy used for bridge provider commands.
pub fn bridge_provider_process_policy() -> ProcessPolicy {
    ProcessPolicy {
        allowed_commands: ALLOWED_BASENAMES.iter().map(|s| (*s).to_string()).collect(),
        denied_commands: Vec::new(),
        default_timeout_secs: 5,
        strict: true,
    }
}

/// Extract basename of a command path (handles `/` and `\` on Windows).
pub fn provider_command_basename(command: &str) -> &str {
    let trimmed = command.trim().trim_matches('"');
    Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(trimmed)
}

/// Return true if the basename alone is on the allowlist.
pub fn basename_is_allowed(command: &str) -> bool {
    let base = provider_command_basename(command);
    ALLOWED_BASENAMES
        .iter()
        .any(|allowed| base.eq_ignore_ascii_case(allowed))
}

/// Reject evil `provider_command` before spawn.
///
/// Checks basename allowlist first (fast fail for `powershell` / `cmd`), then
/// process_policy strict allowlist (covers absolute paths when resolvable).
pub fn check_bridge_provider_command(command: &str) -> Result<(), ProcessPolicyError> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(ProcessPolicyError::Denied {
            command: trimmed.to_string(),
        });
    }

    // Fast basename gate — process_policy path resolution can be slow / miss
    // non-existent absolute shell paths; we still deny by basename.
    if !basename_is_allowed(trimmed) {
        return Err(ProcessPolicyError::Denied {
            command: trimmed.to_string(),
        });
    }

    // Strict process_policy: basename or resolved absolute path must match.
    check_policy(trimmed, &bridge_provider_process_policy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_ai_brains_basename() {
        assert!(check_bridge_provider_command("ai-brains").is_ok());
        assert!(check_bridge_provider_command("ai-brains.exe").is_ok());
    }

    #[test]
    fn denies_powershell() {
        assert!(check_bridge_provider_command("powershell").is_err());
        assert!(check_bridge_provider_command("powershell.exe").is_err());
        assert!(check_bridge_provider_command("PowerShell").is_err());
    }

    #[test]
    fn denies_cmd() {
        assert!(check_bridge_provider_command("cmd").is_err());
        assert!(check_bridge_provider_command("cmd.exe").is_err());
    }

    #[test]
    fn denies_absolute_shell_paths_windows_style() {
        // Even if the path does not exist on this machine, basename gate denies.
        assert!(
            check_bridge_provider_command(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            )
            .is_err()
        );
        assert!(check_bridge_provider_command(r"C:\Windows\System32\cmd.exe").is_err());
    }

    #[test]
    fn denies_empty() {
        assert!(check_bridge_provider_command("").is_err());
        assert!(check_bridge_provider_command("   ").is_err());
    }

    #[test]
    fn denies_other_binaries() {
        assert!(check_bridge_provider_command("bash").is_err());
        assert!(check_bridge_provider_command("sh").is_err());
        assert!(check_bridge_provider_command("python").is_err());
        assert!(check_bridge_provider_command("node").is_err());
    }

    #[test]
    fn basename_helper_handles_paths() {
        assert_eq!(
            provider_command_basename(r"C:\tools\ai-brains.exe"),
            "ai-brains.exe"
        );
        assert_eq!(
            provider_command_basename("/usr/local/bin/ai-brains"),
            "ai-brains"
        );
    }
}
