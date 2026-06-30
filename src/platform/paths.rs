use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    Native,
    WslMounted,
    Network,
    Unknown,
}

pub fn classify_path<P: AsRef<Path>>(path: P) -> PathKind {
    #[cfg(target_os = "windows")]
    {
        let path = path.as_ref();
        if path.is_absolute() {
            let path_str = path.to_string_lossy();
            if path_str.starts_with("\\\\?\\") {
                return PathKind::Native;
            }
            if path_str.starts_with("\\\\.\\") {
                return PathKind::Unknown;
            }
            if path_str.starts_with("\\\\") {
                return PathKind::Network;
            }
        }
        PathKind::Native
    }

    #[cfg(target_os = "linux")]
    {
        use super::detect::is_wsl;
        let path = path.as_ref();
        if is_wsl() {
            let path_str = path.to_string_lossy();
            if path_str.starts_with("/mnt/") {
                // Check if it's followed by a single letter (drive letter)
                let components: Vec<&str> = path_str.split('/').filter(|s| !s.is_empty()).collect();
                if components.len() >= 2 && components[0] == "mnt" && components[1].len() == 1 {
                    return PathKind::WslMounted;
                }
            }
        }
        PathKind::Native
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = path;
        PathKind::Native
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    use super::*;

    #[test]
    fn test_classify_path_windows() {
        #[cfg(target_os = "windows")]
        {
            assert_eq!(classify_path("C:\\Users\\Admin"), PathKind::Native);
            assert_eq!(classify_path("\\\\server\\share"), PathKind::Network);
            assert_eq!(classify_path("\\\\?\\C:\\Users\\Admin"), PathKind::Native);
            assert_eq!(classify_path("\\\\.\\COM1"), PathKind::Unknown);
        }
    }

    #[test]
    fn test_classify_path_wsl() {
        #[cfg(target_os = "linux")]
        {
            use crate::platform::detect::is_wsl;
            if is_wsl() {
                assert_eq!(classify_path("/mnt/c/Users/Admin"), PathKind::WslMounted);
                assert_eq!(classify_path("/home/user"), PathKind::Native);
            }
        }
    }
}
