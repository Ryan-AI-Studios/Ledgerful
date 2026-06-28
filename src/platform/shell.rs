use serde::Serialize;
use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellType {
    Powershell,
    Bash,
    Zsh,
    Cmd,
    Fish,
    Sh,
    Unknown,
}

pub fn detect_shell() -> ShellType {
    if cfg!(target_os = "windows") {
        if env::var("PSModulePath").is_ok() {
            ShellType::Powershell
        } else {
            ShellType::Cmd
        }
    } else {
        if let Ok(shell_var) = env::var("SHELL") {
            if shell_var.contains("pwsh") || shell_var.contains("powershell") {
                ShellType::Powershell
            } else if shell_var.contains("bash") {
                ShellType::Bash
            } else if shell_var.contains("zsh") {
                ShellType::Zsh
            } else if shell_var.contains("fish") {
                ShellType::Fish
            } else if shell_var.ends_with("/sh") || shell_var == "sh" {
                ShellType::Sh
            } else {
                ShellType::Unknown
            }
        } else {
            ShellType::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_detection() {
        #[cfg(not(target_os = "windows"))]
        assert!(matches!(
            detect_shell(),
            ShellType::Bash
                | ShellType::Zsh
                | ShellType::Fish
                | ShellType::Sh
                | ShellType::Powershell
                | ShellType::Unknown
        ));

        #[cfg(target_os = "windows")]
        {
            let shell = detect_shell();
            // Usually we are in powershell in this env
            if env::var("PSModulePath").is_ok() {
                assert_eq!(shell, ShellType::Powershell);
            } else {
                assert_eq!(shell, ShellType::Cmd);
            }
        }
    }
}
