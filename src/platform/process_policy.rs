use miette::{Diagnostic, Result};
use std::collections::BTreeSet;
use std::path::PathBuf;
use thiserror::Error;

/// Built-in toolchain allowlist used by [`ProcessPolicy::default`] and
/// [`crate::config::model::VerifyConfig::effective_process_policy`].
///
/// Includes common build/test toolchains plus Windows `.exe`/`.cmd`/`.bat`
/// basename variants so path-resolved shims still match.
pub fn builtin_allowed_commands() -> Vec<String> {
    const BASES: &[&str] = &[
        "cargo", "npm", "pnpm", "yarn", "npx", "node", "bun", "deno", "python", "python3",
        "pytest", "pip", "go", "make", "git",
    ];
    let mut out = Vec::with_capacity(BASES.len() * 4);
    for base in BASES {
        out.push((*base).to_string());
        out.push(format!("{base}.exe"));
        out.push(format!("{base}.cmd"));
        out.push(format!("{base}.bat"));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPolicy {
    pub allowed_commands: Vec<String>,
    pub denied_commands: Vec<String>,
    pub default_timeout_secs: u64,
    /// When true, an empty allowlist means no commands are permitted.
    /// Defaults to true (fail-closed) with a non-empty built-in allowlist.
    pub strict: bool,
}

impl Default for ProcessPolicy {
    fn default() -> Self {
        Self {
            allowed_commands: builtin_allowed_commands(),
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: true,
        }
    }
}

#[derive(Debug, Error, Diagnostic)]
pub enum ProcessPolicyError {
    /// Three-part diagnostic: blocked command, reason, and a paste-ready
    /// `config.toml` snippet to permit the command (spec §3.4b).
    #[error(
        "Command '{command}' is denied by process policy ({reason}).\n\
         To allow this command, add to config.toml:\n\
         {fix_snippet}"
    )]
    #[diagnostic(code(ledgerful::process_policy::denied))]
    Denied {
        command: String,
        reason: String,
        fix_snippet: String,
    },
}

impl ProcessPolicyError {
    /// Build a denial for an un-allowlisted / denied executable.
    pub fn denied(command: impl Into<String>, reason: impl Into<String>) -> Self {
        let command = command.into();
        let reason = reason.into();
        let fix_snippet = format!("[verify]\nallowed_commands = [\"{command}\"]");
        Self::Denied {
            command,
            reason,
            fix_snippet,
        }
    }

    /// Build a denial for config-declared shell steps when the flag is off.
    pub fn shell_steps_disabled(command: impl Into<String>) -> Self {
        let command = command.into();
        Self::Denied {
            command,
            reason: "shell_steps_disabled".to_string(),
            fix_snippet: "[verify]\nallow_shell_steps = true".to_string(),
        }
    }
}

pub fn check_policy(command: &str, policy: &ProcessPolicy) -> Result<(), ProcessPolicyError> {
    let normalized = command.trim();

    if policy
        .denied_commands
        .iter()
        .any(|denied| matches_command(normalized, denied))
    {
        return Err(ProcessPolicyError::denied(normalized, "explicitly denied"));
    }

    if policy.allowed_commands.is_empty() {
        if policy.strict {
            return Err(ProcessPolicyError::denied(
                normalized,
                "not in allowed_commands (strict policy with empty allowlist)",
            ));
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
        Err(ProcessPolicyError::denied(
            normalized,
            "not in allowed_commands",
        ))
    }
}

fn matches_command(command: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    let command = command.trim();

    if pattern.eq_ignore_ascii_case(command) {
        return true;
    }

    // Basename-only comparison (path components stripped) so allowlist entries
    // like "cargo" match "cargo.exe" and absolute resolved paths.
    let command_base = command_basename(command);
    let pattern_base = command_basename(pattern);
    if !command_base.is_empty()
        && !pattern_base.is_empty()
        && command_base.eq_ignore_ascii_case(pattern_base)
    {
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

/// Basename of a command path (handles `/` and `\` on all platforms).
pub fn command_basename(command: &str) -> &str {
    let trimmed = command.trim().trim_matches('"');
    trimmed
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
}

fn resolve_executable(name: &str) -> Option<PathBuf> {
    let path = std::path::Path::new(name);
    if path.is_absolute() || path.components().count() > 1 {
        return std::fs::canonicalize(path).ok();
    }

    crate::util::which::which(name).and_then(|p| std::fs::canonicalize(p).ok())
}

/// Merge a built-in allowlist with config allow/deny lists (extend-not-replace).
///
/// Effective allowlist = (`built_in` ∪ `extra_allowed`) − `denied`.
/// `strict` defaults to `true` when `config_strict` is `None`.
pub fn merge_process_policy(
    built_in: &[String],
    extra_allowed: &[String],
    denied: &[String],
    config_strict: Option<bool>,
    default_timeout_secs: u64,
) -> ProcessPolicy {
    let denied_set: BTreeSet<String> = denied
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let mut allowed: BTreeSet<String> = BTreeSet::new();
    for entry in built_in.iter().chain(extra_allowed.iter()) {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if denied_set
            .iter()
            .any(|d| d.eq_ignore_ascii_case(trimmed) || matches_command(trimmed, d))
        {
            continue;
        }
        allowed.insert(trimmed.to_string());
    }

    ProcessPolicy {
        allowed_commands: allowed.into_iter().collect(),
        denied_commands: denied_set.into_iter().collect(),
        default_timeout_secs,
        strict: config_strict.unwrap_or(true),
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
    use rstest::rstest;

    #[test]
    fn test_default_policy_is_strict_with_builtin_allowlist() {
        let policy = ProcessPolicy::default();
        assert!(policy.strict);
        assert!(!policy.allowed_commands.is_empty());
        assert!(check_policy("cargo", &policy).is_ok());
        assert!(check_policy("npm", &policy).is_ok());
        assert!(check_policy("pytest", &policy).is_ok());
        assert!(check_policy("git", &policy).is_ok());
        // Auto-policy generates bun/deno steps; must not break under default-strict.
        assert!(check_policy("bun", &policy).is_ok());
        assert!(check_policy("deno", &policy).is_ok());
        assert!(check_policy("bun.exe", &policy).is_ok());
        assert!(check_policy("deno.cmd", &policy).is_ok());
        assert!(check_policy("curl", &policy).is_err());
    }

    #[test]
    fn test_empty_allowlist_strict_refuses() {
        let policy = ProcessPolicy {
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: true,
        };
        assert!(check_policy("cargo", &policy).is_err());
    }

    #[test]
    fn test_empty_allowlist_non_strict_allows() {
        let policy = ProcessPolicy {
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: false,
        };
        assert!(check_policy("anything", &policy).is_ok());
    }

    #[test]
    fn test_deny_list_blocks_command() {
        let policy = ProcessPolicy {
            denied_commands: vec!["rm".to_string()],
            ..ProcessPolicy::default()
        };
        assert!(check_policy("rm", &policy).is_err());
    }

    #[test]
    fn test_allow_list_permits_only_allowed_commands() {
        let policy = ProcessPolicy {
            allowed_commands: vec!["cargo".to_string(), "cargo.exe".to_string()],
            denied_commands: Vec::new(),
            default_timeout_secs: 300,
            strict: true,
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
            strict: true,
        };
        let Some(cargo_path) = resolve_executable("cargo") else {
            // cargo not on PATH in this environment — skip path match
            return;
        };
        assert!(check_policy(cargo_path.to_string_lossy().as_ref(), &policy).is_ok());
    }

    #[test]
    fn denial_message_has_three_parts() {
        let err = check_policy(
            "curl",
            &ProcessPolicy {
                allowed_commands: vec!["cargo".to_string()],
                denied_commands: Vec::new(),
                default_timeout_secs: 300,
                strict: true,
            },
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("curl"), "command: {text}");
        assert!(text.contains("not in allowed_commands"), "reason: {text}");
        assert!(text.contains("[verify]"), "snippet header: {text}");
        assert!(
            text.contains("allowed_commands = [\"curl\"]"),
            "snippet: {text}"
        );
    }

    #[rstest]
    #[case::extends_without_dropping_builtins(
        &["my-tool".to_string()] as &[String],
        &[] as &[String],
        None,
        true,  // cargo still allowed
        true   // my-tool allowed
    )]
    #[case::deny_overrides_builtin(
        &[] as &[String],
        &["cargo".to_string()] as &[String],
        None,
        false, // cargo denied
        false  // my-tool not present
    )]
    fn merge_extend_not_replace(
        #[case] extra: &[String],
        #[case] denied: &[String],
        #[case] strict: Option<bool>,
        #[case] cargo_ok: bool,
        #[case] my_tool_ok: bool,
    ) {
        let policy = merge_process_policy(&builtin_allowed_commands(), extra, denied, strict, 300);
        assert_eq!(
            check_policy("cargo", &policy).is_ok(),
            cargo_ok,
            "cargo policy mismatch"
        );
        if extra.iter().any(|s| s == "my-tool") {
            assert_eq!(
                check_policy("my-tool", &policy).is_ok(),
                my_tool_ok,
                "my-tool policy mismatch"
            );
        }
    }

    #[test]
    fn merge_strict_false_empty_allowlist_is_permissive_footgun() {
        // Documented footgun: empty built-in + no extras + strict=false
        // re-opens "allow anything" (deny list still wins if populated).
        let policy = merge_process_policy(&[], &[], &[], Some(false), 300);
        assert!(!policy.strict);
        assert!(policy.allowed_commands.is_empty());
        assert!(check_policy("curl", &policy).is_ok());
        assert!(check_policy("cargo", &policy).is_ok());
    }

    #[test]
    fn merge_deny_overrides_allow() {
        let policy = merge_process_policy(
            &builtin_allowed_commands(),
            &["curl".to_string()],
            &["curl".to_string()],
            Some(true),
            120,
        );
        assert!(check_policy("curl", &policy).is_err());
        assert!(check_policy("cargo", &policy).is_ok());
        assert_eq!(policy.default_timeout_secs, 120);
        assert!(policy.strict);
    }

    #[test]
    fn merge_strict_defaults_true() {
        let policy = merge_process_policy(&[], &[], &[], None, 300);
        assert!(policy.strict);
        assert!(check_policy("anything", &policy).is_err());
    }
}
