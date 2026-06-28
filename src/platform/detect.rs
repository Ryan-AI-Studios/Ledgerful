use serde::Serialize;
#[cfg(target_os = "linux")]
use std::fs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlatformType {
    Windows,
    Linux,
    Wsl,
    Unknown,
}

pub fn current_platform() -> PlatformType {
    if cfg!(target_os = "windows") {
        PlatformType::Windows
    } else if cfg!(target_os = "linux") {
        if is_wsl() {
            PlatformType::Wsl
        } else {
            PlatformType::Linux
        }
    } else {
        PlatformType::Unknown
    }
}

pub fn is_wsl() -> bool {
    #[cfg(target_os = "linux")]
    if let Ok(osrelease) = fs::read_to_string("/proc/sys/kernel/osrelease") {
        let osrelease = osrelease.to_lowercase();
        return osrelease.contains("microsoft") || osrelease.contains("wsl");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_detection() {
        let platform = current_platform();
        #[cfg(target_os = "windows")]
        assert_eq!(platform, PlatformType::Windows);

        #[cfg(target_os = "linux")]
        {
            if is_wsl() {
                assert_eq!(platform, PlatformType::Wsl);
            } else {
                assert_eq!(platform, PlatformType::Linux);
            }
        }

        // On other platforms (e.g. macOS), this just smoke-tests that
        // current_platform() doesn't panic; there's no specific value to
        // assert against.
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let _ = platform;
    }
}
