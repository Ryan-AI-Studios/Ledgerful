use miette::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Expand a leading home tilde using an injected `home` (hermetic tests).
///
/// - `"~"` → `home`
/// - `"~/…"` → `home.join(rest)`
/// - `"~\…"` → `home.join(rest)` (Windows-style separator)
///
/// Does **not** expand `~user` (no username tilde). Non-tilde strings are
/// returned unchanged.
pub(crate) fn expand_leading_tilde_with_home(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.join(rest);
    }
    if let Some(rest) = raw.strip_prefix("~\\") {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Expand a leading home tilde via `dirs::home_dir`.
///
/// Fails with miette when expansion is needed and the home directory cannot
/// be determined. Non-tilde strings never consult home.
pub(crate) fn expand_leading_tilde(raw: &str) -> Result<PathBuf> {
    if leading_tilde_needs_home(raw) {
        let home = dirs::home_dir()
            .ok_or_else(|| miette::miette!("could not determine home directory"))?;
        Ok(expand_leading_tilde_with_home(raw, &home))
    } else {
        Ok(PathBuf::from(raw))
    }
}

fn leading_tilde_needs_home(raw: &str) -> bool {
    raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\")
}

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
    use super::{expand_leading_tilde, expand_leading_tilde_with_home};
    use std::path::{Path, PathBuf};

    #[test]
    fn expand_leading_tilde_with_home_bare_and_slash_forms() {
        let home = Path::new("fake-home");
        assert_eq!(
            expand_leading_tilde_with_home("~", home),
            PathBuf::from("fake-home")
        );
        assert_eq!(
            expand_leading_tilde_with_home("~/dev", home),
            home.join("dev")
        );
        assert_eq!(
            expand_leading_tilde_with_home("~\\foo", home),
            home.join("foo")
        );
        assert_eq!(
            expand_leading_tilde_with_home("~/dev/nested", home),
            home.join("dev").join("nested")
        );
    }

    #[test]
    fn expand_leading_tilde_with_home_does_not_expand_username_or_literals() {
        let home = Path::new("fake-home");
        assert_eq!(
            expand_leading_tilde_with_home("~user", home),
            PathBuf::from("~user")
        );
        assert_eq!(
            expand_leading_tilde_with_home("~user/dev", home),
            PathBuf::from("~user/dev")
        );
        assert_eq!(
            expand_leading_tilde_with_home("C:\\dev", home),
            PathBuf::from("C:\\dev")
        );
        assert_eq!(
            expand_leading_tilde_with_home("/abs/path", home),
            PathBuf::from("/abs/path")
        );
        assert_eq!(
            expand_leading_tilde_with_home("relative/path", home),
            PathBuf::from("relative/path")
        );
    }

    #[test]
    fn expand_leading_tilde_non_tilde_does_not_need_home() {
        assert_eq!(
            expand_leading_tilde("~user").expect("literal ~user"),
            PathBuf::from("~user")
        );
        assert_eq!(
            expand_leading_tilde("C:\\dev").expect("literal path"),
            PathBuf::from("C:\\dev")
        );
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    use super::PathKind;
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    use super::classify_path;

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
