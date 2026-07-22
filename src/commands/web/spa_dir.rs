//! Containment checks for `--spa-dir` (RT-W1).

use camino::{Utf8Path, Utf8PathBuf};
use miette::{Result, miette};
use std::path::{Component, Path, PathBuf};

/// Environment override for the SPA allow-root (canonicalized directory prefix).
pub const SPA_ROOT_ENV: &str = "LEDGERFUL_SPA_ROOT";

/// Validate, canonicalize, and contain a `--spa-dir` path.
///
/// Rules:
/// - Path must exist and be a directory after symlink/junction resolution.
/// - Drive roots (`/`, `C:\`) are refused.
/// - Paths under the user home directory tree (`$HOME` / `%USERPROFILE%`) are
///   refused unless they fall under an explicit allow-root (see below).
/// - Allowed when under `LEDGERFUL_SPA_ROOT` (if set) or under the process CWD /
///   package directory heuristics, **or** (outside home) when a dashboard marker
///   is present: `index.html` with a ledgerful/dashboard fingerprint **or** a
///   common static asset-layout sibling (`_next`, `assets`, `static`, `out`).
pub fn validate_spa_dir(spa_dir: &Utf8Path) -> Result<Utf8PathBuf> {
    let roots = spa_allow_roots()?;
    validate_spa_dir_with_roots(spa_dir, &roots)
}

/// Pure containment check with injected allow-roots (production + tests).
///
/// Callers supply already-canonicalized allow-root prefixes. Production uses
/// [`spa_allow_roots`]; tests inject a dedicated empty root so fixtures outside
/// that root are never accidentally accepted via CWD heuristics.
pub fn validate_spa_dir_with_roots(
    spa_dir: &Utf8Path,
    allow_roots: &[Utf8PathBuf],
) -> Result<Utf8PathBuf> {
    if !spa_dir.exists() {
        return Err(miette!(
            "SPA directory does not exist: {spa_dir}. Provide an existing directory \
             containing a dashboard build (index.html + ledgerful/dashboard fingerprint \
             or asset layout)."
        ));
    }
    if !spa_dir.is_dir() {
        return Err(miette!(
            "SPA path is not a directory: {spa_dir}. --spa-dir must point at a directory."
        ));
    }

    let canonical_std = std::fs::canonicalize(spa_dir.as_std_path()).map_err(|e| {
        miette!("Failed to resolve --spa-dir '{spa_dir}' (symlink/junction canonicalize): {e}")
    })?;
    let canonical = utf8_path_from_std(strip_verbatim_prefix(&canonical_std))?;

    if is_filesystem_root(canonical.as_std_path()) {
        return Err(miette!(
            "Refusing --spa-dir at filesystem root '{}'. Serving a drive root would \
             expose the entire tree unauthenticated on the static fallback.",
            canonical
        ));
    }

    // Explicit allow-root wins even when the path sits under $HOME (operators
    // may set LEDGERFUL_SPA_ROOT to a project directory inside the home tree).
    if is_under_any_root(&canonical, allow_roots) {
        return Ok(canonical);
    }

    if is_under_home_directory(canonical.as_std_path()) {
        return Err(miette!(
            "Refusing --spa-dir under home directory tree '{}'. Serving paths under \
             $HOME/%USERPROFILE% would expose personal files unauthenticated on the \
             static fallback. Set LEDGERFUL_SPA_ROOT to an allow-root that contains the \
             SPA, or place the dashboard outside the home tree with a dashboard marker \
             (index.html containing 'ledgerful'/'dashboard', or asset layout siblings).",
            canonical
        ));
    }

    if has_dashboard_marker(&canonical) {
        return Ok(canonical);
    }

    Err(miette!(
        "Refusing --spa-dir '{}': path is outside the SPA allow-root and does not look \
         like a dashboard build. Set LEDGERFUL_SPA_ROOT to an allow-root directory, run \
         from the package/cwd that contains the SPA, or pass a directory with index.html \
         containing a ledgerful/dashboard fingerprint or asset-layout siblings \
         (_next/assets/static/out). Generic HTML alone is not accepted.",
        canonical
    ))
}

fn is_under_any_root(canonical: &Utf8Path, allow_roots: &[Utf8PathBuf]) -> bool {
    allow_roots
        .iter()
        .any(|root| path_is_under(canonical.as_std_path(), root.as_std_path()))
}

fn spa_allow_roots() -> Result<Vec<Utf8PathBuf>> {
    let mut roots = Vec::new();

    if let Ok(raw) = std::env::var(SPA_ROOT_ENV) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(miette!(
                "{SPA_ROOT_ENV} is set but empty. Unset it or set it to an existing directory."
            ));
        }
        let path = Utf8PathBuf::from(trimmed);
        if !path.is_dir() {
            return Err(miette!(
                "{SPA_ROOT_ENV} is not an existing directory: {path}"
            ));
        }
        let canon = std::fs::canonicalize(path.as_std_path())
            .map_err(|e| miette!("Failed to canonicalize {SPA_ROOT_ENV} '{path}': {e}"))?;
        roots.push(utf8_path_from_std(strip_verbatim_prefix(&canon))?);
        return Ok(roots);
    }

    // Heuristics when LEDGERFUL_SPA_ROOT is unset: CWD and current executable dir.
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(canon) = std::fs::canonicalize(&cwd)
        && let Ok(p) = utf8_path_from_std(strip_verbatim_prefix(&canon))
    {
        roots.push(p);
    }
    // Heuristic only (not security identity): optional package-dir allow-root
    // when LEDGERFUL_SPA_ROOT is unset. Path is re-validated via canonicalize
    // containment; not used as an auth decision.
    // nosemgrep: rust.lang.security.current-exe.current-exe
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
        && let Ok(canon) = std::fs::canonicalize(parent)
        && let Ok(p) = utf8_path_from_std(strip_verbatim_prefix(&canon))
    {
        roots.push(p);
    }

    // Dedup while preserving order.
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// Dashboard marker: `index.html` present **and** either a content fingerprint
/// (`ledgerful` / `dashboard`, case-insensitive) **or** a common static
/// asset-layout sibling directory.
///
/// Bare `<!doctype` / non-empty generic HTML alone is **not** sufficient — that
/// previously allowed arbitrary static trees outside the allow-root.
pub fn has_dashboard_marker(dir: &Utf8Path) -> bool {
    let index = dir.join("index.html");
    if !index.is_file() {
        return false;
    }
    let content = std::fs::read_to_string(index.as_std_path()).unwrap_or_default();
    let lower = content.to_ascii_lowercase();
    if lower.contains("ledgerful") || lower.contains("dashboard") {
        return true;
    }
    // Asset-layout siblings used by Next.js / static exports.
    for sibling in ["_next", "assets", "static", "out"] {
        if dir.join(sibling).is_dir() {
            return true;
        }
    }
    false
}

fn is_filesystem_root(path: &Path) -> bool {
    let mut components = path.components();
    match components.next() {
        Some(Component::Prefix(_)) => {
            // Windows: `C:\` → Prefix + RootDir only.
            matches!(components.next(), Some(Component::RootDir)) && components.next().is_none()
        }
        Some(Component::RootDir) => components.next().is_none(),
        _ => false,
    }
}

/// True when `path` is the home directory itself or any descendant of it.
fn is_under_home_directory(path: &Path) -> bool {
    let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) else {
        return false;
    };
    let home_path = PathBuf::from(home);
    let Ok(home_canon) = std::fs::canonicalize(&home_path) else {
        return false;
    };
    let home_canon = strip_verbatim_prefix(&home_canon);
    path_is_under(path, home_canon.as_path())
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

fn utf8_path_from_std(path: PathBuf) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path)
        .map_err(|p| miette!("path is not valid UTF-8: {}", p.display()))
}

/// Create a dedicated empty allow-root temp directory (canonicalized Utf8).
#[cfg(test)]
fn dedicated_allow_root() -> (tempfile::TempDir, Utf8PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(tmp.path()).expect("canonicalize allow-root");
    let utf8 = utf8_path_from_std(strip_verbatim_prefix(&canon)).expect("utf8 allow-root");
    (tmp, utf8)
}

#[cfg(test)]
fn utf8_temp(tmp: &tempfile::TempDir) -> Utf8PathBuf {
    let canon = std::fs::canonicalize(tmp.path()).expect("canonicalize temp");
    utf8_path_from_std(strip_verbatim_prefix(&canon)).expect("utf8 temp")
}

#[cfg(test)]
fn write_marker_spa(dir: &Utf8Path) {
    std::fs::write(
        dir.join("index.html"),
        "<!DOCTYPE html><html><body>Ledgerful Dashboard</body></html>",
    )
    .expect("write marker index.html");
}

#[cfg(test)]
fn write_generic_html(dir: &Utf8Path) {
    std::fs::write(
        dir.join("index.html"),
        "<!DOCTYPE html><html><body>hello world</body></html>",
    )
    .expect("write generic index.html");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_missing_dir() {
        let (_allow_tmp, allow) = dedicated_allow_root();
        let err = validate_spa_dir_with_roots(
            Utf8Path::new("/nonexistent/spa/dir/ledgerful-0078"),
            &[allow],
        )
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("does not exist"), "{msg}");
    }

    #[test]
    fn accepts_spa_with_ledgerful_fingerprint_outside_allow_root() {
        let (_allow_tmp, allow) = dedicated_allow_root();
        // Fixture lives outside the dedicated allow-root (sibling temp dir).
        let spa_tmp = tempfile::tempdir().unwrap();
        let spa = utf8_temp(&spa_tmp);
        write_marker_spa(&spa);
        // Marker alone is only accepted outside home. Temp dirs usually sit under
        // home on Windows/macOS — force outside-home by using allow-root instead
        // for the positive marker path when under home, or accept via allow-root.
        if is_under_home_directory(spa.as_std_path()) {
            // Place SPA under a second allow-root that contains it.
            let roots = vec![allow, spa.clone()];
            let got = validate_spa_dir_with_roots(&spa, &roots).unwrap();
            assert!(got.as_std_path().is_dir());
        } else {
            let got = validate_spa_dir_with_roots(&spa, &[allow]).unwrap();
            assert!(got.as_std_path().is_dir());
        }
    }

    #[test]
    fn accepts_spa_under_allow_root_without_marker() {
        let (allow_tmp, allow) = dedicated_allow_root();
        let spa = allow.join("my-spa");
        std::fs::create_dir_all(spa.as_std_path()).unwrap();
        // No index.html / marker — still allowed because under allow-root.
        let got = validate_spa_dir_with_roots(&spa, std::slice::from_ref(&allow)).unwrap();
        assert!(got.as_str().contains("my-spa") || got.ends_with("my-spa"));
        // Keep allow_tmp alive for the path lifetime.
        drop(allow_tmp);
    }

    #[test]
    fn accepts_asset_layout_sibling_marker() {
        let (_allow_tmp, allow) = dedicated_allow_root();
        let spa_tmp = tempfile::tempdir().unwrap();
        let spa = utf8_temp(&spa_tmp);
        std::fs::write(spa.join("index.html"), "<!DOCTYPE html><html></html>").unwrap();
        std::fs::create_dir(spa.join("_next").as_std_path()).unwrap();
        assert!(has_dashboard_marker(&spa));
        // Containment: under home needs allow-root; fingerprint-free asset layout
        // still counts as marker for outside-home only.
        if is_under_home_directory(spa.as_std_path()) {
            let roots = vec![allow, spa.clone()];
            assert!(validate_spa_dir_with_roots(&spa, &roots).is_ok());
        } else {
            assert!(validate_spa_dir_with_roots(&spa, &[allow]).is_ok());
        }
    }

    #[test]
    fn refuses_generic_html_without_fingerprint_or_asset_layout() {
        let (_allow_tmp, allow) = dedicated_allow_root();
        let spa_tmp = tempfile::tempdir().unwrap();
        let spa = utf8_temp(&spa_tmp);
        write_generic_html(&spa);
        assert!(
            !has_dashboard_marker(&spa),
            "bare doctype / non-empty HTML must not count as marker"
        );
        let err = validate_spa_dir_with_roots(&spa, &[allow]).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("Refusing") || msg.contains("home") || msg.contains("dashboard"),
            "{msg}"
        );
    }

    #[test]
    fn refuses_dir_without_marker_outside_allow_root() {
        let (_allow_tmp, allow) = dedicated_allow_root();
        let spa_tmp = tempfile::tempdir().unwrap();
        let spa = utf8_temp(&spa_tmp);
        // No index.html → no marker.
        assert!(!has_dashboard_marker(&spa));
        let result = validate_spa_dir_with_roots(&spa, &[allow]);
        assert!(
            result.is_err(),
            "no-marker spa outside dedicated allow-root must refuse"
        );
    }

    #[test]
    fn refuses_home_directory_and_under_home_without_allow_root() {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .ok();
        let Some(home) = home else {
            return;
        };
        let home_path = Utf8PathBuf::from(&home);
        if !home_path.is_dir() {
            return;
        }
        let (_allow_tmp, allow) = dedicated_allow_root();
        // Exact home — must refuse (hard assert; silent Ok would pass the old test).
        let result = validate_spa_dir_with_roots(&home_path, std::slice::from_ref(&allow));
        assert!(
            result.is_err(),
            "exact $HOME as --spa-dir must refuse when not under allow-root"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("home") || msg.contains("HOME") || msg.contains("Refusing"),
            "{msg}"
        );
        // Under-home path without allow covering it: use a subdir of home if
        // present (Documents / itself already covered). Prefer a real child.
        let child = home_path.join("Documents");
        if child.is_dir() {
            let result = validate_spa_dir_with_roots(&child, std::slice::from_ref(&allow));
            assert!(
                result.is_err(),
                "under-home without allow-root must refuse even if path exists"
            );
            let msg = format!("{:?}", result.unwrap_err());
            assert!(
                msg.contains("home") || msg.contains("HOME") || msg.contains("Refusing"),
                "{msg}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_escape_outside_allow_root() {
        use std::os::unix::fs::symlink;

        let (_allow_tmp, allow) = dedicated_allow_root();
        // Secret tree lives outside allow-root and has no dashboard marker.
        let secret_tmp = tempfile::tempdir().unwrap();
        let secret = utf8_temp(&secret_tmp);
        std::fs::write(secret.join("secret.txt"), "nope").unwrap();

        // Link location is also outside allow-root (sibling of allow, not under it).
        let link_tmp = tempfile::tempdir().unwrap();
        let link_path = link_tmp.path().join("escape-link");
        symlink(secret.as_std_path(), &link_path).unwrap();
        let link_utf8 = Utf8Path::from_path(&link_path).expect("utf8 link");

        let result = validate_spa_dir_with_roots(link_utf8, std::slice::from_ref(&allow));
        assert!(
            result.is_err(),
            "symlink escape without marker must refuse; got {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn accepts_symlink_to_marker_spa() {
        use std::os::unix::fs::symlink;

        let (_allow_tmp, allow) = dedicated_allow_root();
        let spa_tmp = tempfile::tempdir().unwrap();
        let spa = utf8_temp(&spa_tmp);
        write_marker_spa(&spa);

        let link_tmp = tempfile::tempdir().unwrap();
        let link_path = link_tmp.path().join("spa-link");
        symlink(spa.as_std_path(), &link_path).unwrap();
        let link_utf8 = Utf8Path::from_path(&link_path).expect("utf8 link");

        // Marker path: if under home, allow-root must cover the *canonical* target.
        let roots = if is_under_home_directory(spa.as_std_path()) {
            vec![allow, spa.clone()]
        } else {
            vec![allow]
        };
        let got = validate_spa_dir_with_roots(link_utf8, &roots).unwrap();
        assert!(got.as_std_path().is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn refuses_junction_escape_without_marker() {
        use std::os::windows::fs::symlink_dir;

        let (_allow_tmp, allow) = dedicated_allow_root();
        let secret_tmp = tempfile::tempdir().unwrap();
        let secret = utf8_temp(&secret_tmp);
        std::fs::write(secret.join("secret.txt"), "nope").unwrap();

        let link_tmp = tempfile::tempdir().unwrap();
        let link_path = link_tmp.path().join("escape-junction");
        // Prefer symlink_dir (works with Developer Mode / elevation). Fall back
        // to `mklink /J` which creates a directory junction without admin on
        // modern Windows.
        let created = symlink_dir(secret.as_std_path(), &link_path).or_else(|e| {
            let status = std::process::Command::new("cmd")
                .args([
                    "/C",
                    "mklink",
                    "/J",
                    link_path.to_str().unwrap_or(""),
                    secret.as_str(),
                ])
                .status();
            match status {
                Ok(s) if s.success() => Ok(()),
                Ok(s) => Err(std::io::Error::other(format!(
                    "symlink_dir failed ({e}); mklink /J exit {s}"
                ))),
                Err(cmd_err) => Err(std::io::Error::other(format!(
                    "symlink_dir failed ({e}); mklink spawn failed ({cmd_err})"
                ))),
            }
        });
        if let Err(e) = created {
            eprintln!(
                "skipping junction escape test: could not create junction/symlink ({e}); \
                 enable Developer Mode or re-run elevated"
            );
            return;
        }

        let link_utf8 = Utf8Path::from_path(&link_path).expect("utf8 junction");
        let result = validate_spa_dir_with_roots(link_utf8, &[allow]);
        assert!(
            result.is_err(),
            "junction escape without marker must refuse; got {result:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn accepts_junction_to_marker_spa() {
        use std::os::windows::fs::symlink_dir;

        let (_allow_tmp, allow) = dedicated_allow_root();
        let spa_tmp = tempfile::tempdir().unwrap();
        let spa = utf8_temp(&spa_tmp);
        write_marker_spa(&spa);

        let link_tmp = tempfile::tempdir().unwrap();
        let link_path = link_tmp.path().join("spa-junction");
        let created = symlink_dir(spa.as_std_path(), &link_path).or_else(|e| {
            let status = std::process::Command::new("cmd")
                .args([
                    "/C",
                    "mklink",
                    "/J",
                    link_path.to_str().unwrap_or(""),
                    spa.as_str(),
                ])
                .status();
            match status {
                Ok(s) if s.success() => Ok(()),
                Ok(s) => Err(std::io::Error::other(format!(
                    "symlink_dir failed ({e}); mklink /J exit {s}"
                ))),
                Err(cmd_err) => Err(std::io::Error::other(format!(
                    "symlink_dir failed ({e}); mklink spawn failed ({cmd_err})"
                ))),
            }
        });
        if let Err(e) = created {
            eprintln!(
                "skipping junction positive-control test: could not create junction/symlink ({e})"
            );
            return;
        }

        let link_utf8 = Utf8Path::from_path(&link_path).expect("utf8 junction");
        // Temp SPAs on Windows live under %USERPROFILE% — cover via allow-root.
        let roots = if is_under_home_directory(spa.as_std_path()) {
            vec![allow, spa.clone()]
        } else {
            vec![allow]
        };
        let got = validate_spa_dir_with_roots(link_utf8, &roots).unwrap();
        assert!(got.as_std_path().is_dir());
    }

    #[test]
    fn marker_rejects_empty_and_doctype_only() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = utf8_temp(&tmp);
        std::fs::write(spa.join("index.html"), "").unwrap();
        assert!(!has_dashboard_marker(&spa));
        std::fs::write(spa.join("index.html"), "<!DOCTYPE html><html></html>").unwrap();
        assert!(!has_dashboard_marker(&spa));
        std::fs::write(
            spa.join("index.html"),
            "<html><title>My Dashboard</title></html>",
        )
        .unwrap();
        assert!(has_dashboard_marker(&spa));
        std::fs::write(
            spa.join("index.html"),
            "<html><body>Welcome to Ledgerful</body></html>",
        )
        .unwrap();
        assert!(has_dashboard_marker(&spa));
    }
}
