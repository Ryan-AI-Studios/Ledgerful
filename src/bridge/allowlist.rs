//! Bridge `provider_command` allowlist (track 0073 / RT-A3).
//!
//! Only `ai-brains` / `ai-brains.exe` (basename) may be spawned. Absolute paths
//! are accepted only when the basename resolves to that allowlist. Evil
//! commands (`powershell`, `cmd`, shells) are rejected **before** spawn via
//! [`crate::platform::process_policy::check_policy`] with `strict: true`.

use crate::platform::process_policy::{ProcessPolicy, ProcessPolicyError, check_policy};

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

/// Extract basename of a command path (handles `/` and `\` on all platforms).
///
/// Config may store Windows-style paths; CI also runs on Unix where `\` is not a
/// path separator, so we split on both separators rather than using `Path` alone.
pub fn provider_command_basename(command: &str) -> &str {
    let trimmed = command.trim().trim_matches('"');
    trimmed
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
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
/// Bridge-only remediation: provider_command is not controlled by `verify.allowed_commands`.
fn bridge_denied(command: &str, reason: &str) -> ProcessPolicyError {
    ProcessPolicyError::denied_with_fix(
        command,
        reason,
        "bridge.provider_command must resolve to basename 'ai-brains' (or 'ai-brains.exe'); \
         see bridge config (not verify.allowed_commands)",
    )
}

pub fn check_bridge_provider_command(command: &str) -> Result<(), ProcessPolicyError> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(bridge_denied(trimmed, "empty provider_command"));
    }

    // Fast basename gate — process_policy path resolution can be slow / miss
    // non-existent absolute shell paths; we still deny by basename.
    if !basename_is_allowed(trimmed) {
        return Err(bridge_denied(
            trimmed,
            "basename not on bridge provider allowlist (ai-brains only)",
        ));
    }

    // Strict process_policy: basename or resolved absolute path must match.
    // Map generic verify-style diagnostics to bridge-specific remediation.
    check_policy(trimmed, &bridge_provider_process_policy()).map_err(|e| match e {
        ProcessPolicyError::Denied {
            command, reason, ..
        } => bridge_denied(&command, &reason),
    })
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
    fn bridge_denial_does_not_suggest_verify_allowed_commands() {
        let err = check_bridge_provider_command("powershell").unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("[verify]"),
            "bridge denial must not point at verify.allowed_commands: {msg}"
        );
        assert!(
            !msg.contains("allowed_commands = [\"powershell\"]"),
            "bridge denial must not suggest allowlisting the denied binary via verify: {msg}"
        );
        assert!(
            msg.contains("bridge.provider_command") || msg.contains("ai-brains"),
            "bridge denial must name bridge remediation: {msg}"
        );
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
