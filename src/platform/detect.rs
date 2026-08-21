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

/// Linux userspace classifiers. Compiled on Linux production and on all
/// `cfg(test)` builds so Windows CI can lock the Docker-vs-WSL matrix.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn osrelease_looks_like_wsl(osrelease: &str) -> bool {
    let s = osrelease.to_lowercase();
    s.contains("microsoft") || s.contains("wsl")
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn container_markers_present(dockerenv: bool, containerenv: bool, cgroup: &str) -> bool {
    if dockerenv || containerenv {
        return true;
    }
    let c = cgroup.to_lowercase();
    [
        "docker",
        "containerd",
        "kubepods",
        "libpod",
        "podman",
        "/lxc",
    ]
    .iter()
    .any(|needle| c.contains(needle))
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn classify_linux(osrelease: &str, in_container: bool) -> PlatformType {
    if osrelease_looks_like_wsl(osrelease) && !in_container {
        PlatformType::Wsl
    } else {
        PlatformType::Linux
    }
}

pub fn is_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        let osrelease = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        classify_linux(&osrelease, in_container()) == PlatformType::Wsl
    }
    #[cfg(not(target_os = "linux"))]
    false
}

#[cfg(target_os = "linux")]
fn in_container() -> bool {
    use std::path::Path;
    let dockerenv = Path::new("/.dockerenv").exists();
    let containerenv = Path::new("/run/.containerenv").exists();
    let cgroup = fs::read_to_string("/proc/1/cgroup")
        .or_else(|_| fs::read_to_string("/proc/self/cgroup"))
        .unwrap_or_default();
    container_markers_present(dockerenv, containerenv, &cgroup)
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

    /// DoD-1 composed matrix: `container_markers_present` then `classify_linux`.
    /// Row 7 is the cgroup-vs-WSL lock; row 6 is the non-WSL-kernel documentation cell.
    #[test]
    fn classify_linux_composed_container_vs_wsl_kernel() {
        const WSL2: &str = "6.6.87.2-microsoft-standard-WSL2";
        const GENERIC: &str = "6.8.0-generic";
        const NON_WSL: &str = "5.15.0";
        const EMPTY_CGROUP: &str = "0::/";
        const DOCKER_CGROUP: &str = "1:name=systemd:/docker/abc";
        const USER_SLICE: &str = "1:name=systemd:/user.slice/user-1000.slice/...";

        let rows: [(&str, bool, bool, &str, PlatformType); 8] = [
            (WSL2, true, false, EMPTY_CGROUP, PlatformType::Linux),
            (WSL2, false, false, EMPTY_CGROUP, PlatformType::Wsl),
            (GENERIC, false, false, EMPTY_CGROUP, PlatformType::Linux),
            (GENERIC, true, false, EMPTY_CGROUP, PlatformType::Linux),
            (WSL2, false, true, EMPTY_CGROUP, PlatformType::Linux),
            (NON_WSL, false, false, DOCKER_CGROUP, PlatformType::Linux),
            (WSL2, false, false, DOCKER_CGROUP, PlatformType::Linux),
            (WSL2, false, false, USER_SLICE, PlatformType::Wsl),
        ];

        for (i, (osrelease, dockerenv, containerenv, cgroup, expected)) in rows.iter().enumerate() {
            let in_container = container_markers_present(*dockerenv, *containerenv, cgroup);
            let got = classify_linux(osrelease, in_container);
            assert_eq!(
                got,
                *expected,
                "row {} osrelease={osrelease:?} dockerenv={dockerenv} containerenv={containerenv} cgroup={cgroup:?} in_container={in_container}",
                i + 1
            );
        }
    }

    #[test]
    fn container_markers_present_needles() {
        let needles = [
            "docker",
            "containerd",
            "kubepods",
            "libpod",
            "podman",
            "/lxc",
        ];
        for needle in needles {
            assert!(
                container_markers_present(false, false, needle),
                "needle {needle:?} must flip true"
            );
        }
        assert!(!container_markers_present(false, false, "0::/"));
        assert!(!container_markers_present(
            false,
            false,
            "1:name=systemd:/user.slice/user-1000.slice/..."
        ));
    }
}
