use serde::Serialize;
use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[serde(tag = "status", content = "path")]
pub enum ExecutableStatus {
    Found(PathBuf),
    NotFound,
}

pub fn find_executable<P: AsRef<Path>>(name: P) -> ExecutableStatus {
    let name = name.as_ref();

    // Extensions to check on Windows
    let extensions: &[&str] = if cfg!(target_os = "windows") {
        &["exe", "cmd", "bat", "ps1"]
    } else {
        &[""]
    };

    if let Ok(path_var) = env::var("PATH") {
        for path in env::split_paths(&path_var) {
            for ext in extensions {
                let mut exec_path = path.join(name);
                if !ext.is_empty() {
                    exec_path.set_extension(ext);
                }

                if exec_path.exists() && exec_path.is_file() {
                    return ExecutableStatus::Found(exec_path);
                }
            }
        }
    }

    ExecutableStatus::NotFound
}

pub fn check_tools() -> Vec<(String, ExecutableStatus)> {
    let mut tools = Vec::new();

    tools.push(("git".to_string(), find_executable("git")));

    // Check both gemini-cli and gemini
    let gemini_cli = find_executable("gemini-cli");
    if let ExecutableStatus::Found(_) = gemini_cli {
        tools.push(("gemini-cli".to_string(), gemini_cli));
    } else {
        let gemini = find_executable("gemini");
        tools.push(("gemini".to_string(), gemini));
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_executable_git() {
        // git is usually present in test environments
        let status = find_executable("git");
        // We don't assert Found because it might not be in some weird envs,
        // but we can at least check it doesn't crash.
        match status {
            ExecutableStatus::Found(p) => println!("Found git at {:?}", p),
            ExecutableStatus::NotFound => println!("git not found"),
        }
    }
}
