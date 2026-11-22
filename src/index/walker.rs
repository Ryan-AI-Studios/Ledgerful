use crate::index::docs::parse_markdown;
use camino::Utf8PathBuf;
use ignore::WalkBuilder;
use miette::Result;
use std::collections::HashSet;

pub struct RepoWalker {
    root: Utf8PathBuf,
    supported_extensions: HashSet<String>,
    binary_extensions: HashSet<String>,
}

impl RepoWalker {
    pub fn new(root: Utf8PathBuf, supported: &[&str], binary: &[&str]) -> Self {
        Self {
            root,
            supported_extensions: supported.iter().map(|s| s.to_string()).collect(),
            binary_extensions: binary.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Discover files in the repository, respecting .gitignore and filtering by extensions.
    ///
    /// Directory-walk errors are logged and skipped; the walk therefore cannot
    /// report whether it saw everything. Callers that need that guarantee use
    /// [`Self::discover_files_strict`].
    pub fn discover_files(&self) -> Result<Vec<Utf8PathBuf>> {
        self.discover_files_with_mode(false)
    }

    /// Discover files and fail closed on a directory-walk error.
    pub(crate) fn discover_files_strict(&self) -> Result<Vec<Utf8PathBuf>> {
        self.discover_files_with_mode(true)
    }

    fn discover_files_with_mode(&self, strict: bool) -> Result<Vec<Utf8PathBuf>> {
        let walker = WalkBuilder::new(&self.root)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();
        collect_discovered(
            walker,
            &self.supported_extensions,
            &self.binary_extensions,
            strict,
        )
    }

    pub fn discover_empty_reason(
        &self,
    ) -> (
        crate::index::staleness::EmptyIndexReason,
        crate::index::staleness::EmptyDiscoveryDiagnostics,
    ) {
        use crate::index::staleness::{EmptyDiscoveryDiagnostics, EmptyIndexReason};

        let mut visible_files = 0;
        let mut ignored_indexable = 0;
        let mut scan_complete = false;

        let walker = WalkBuilder::new(&self.root)
            .hidden(true)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .build();

        let mut has_files = false;
        let mut has_supported = false;

        for entry in walker {
            if visible_files > 1000 {
                break;
            }
            if let Ok(entry) = entry
                && entry.file_type().is_some_and(|ft| ft.is_file())
            {
                has_files = true;
                visible_files += 1;

                let path = entry.path();
                if let Some(ext) = path.extension().and_then(|e| e.to_str())
                    && self.supported_extensions.contains(ext)
                    && !self.binary_extensions.contains(ext)
                {
                    has_supported = true;
                    ignored_indexable += 1;
                }
            }
        }

        if visible_files <= 1000 {
            scan_complete = true;
        }

        let reason = if !has_files {
            EmptyIndexReason::RepositoryEmpty
        } else if !has_supported {
            EmptyIndexReason::NoSupportedFiles
        } else {
            EmptyIndexReason::AllIndexableCandidatesIgnored
        };

        (
            reason,
            EmptyDiscoveryDiagnostics {
                visible_files_examined: visible_files,
                ignored_indexable_candidates_lower_bound: ignored_indexable,
                configured_exclusions_lower_bound: 0,
                scan_complete,
                warnings: vec![],
            },
        )
    }

    pub fn discover_doc_files(&self) -> Result<Vec<Utf8PathBuf>> {
        let mut doc_files = Vec::new();
        let priority_files = ["README.md", "CONTRIBUTING.md", "ARCHITECTURE.md"];

        for name in &priority_files {
            let path = self.root.join(name);
            if path.exists() {
                doc_files.push(path);
            }
        }

        // Follow internal links from README.md (one level deep)
        let readme_path = self.root.join("README.md");
        if readme_path.exists()
            && let Ok(content) =
                crate::util::fs::read_to_string_with_encoding(readme_path.as_std_path())
        {
            let parsed = parse_markdown(&content, "README.md");
            for link in &parsed.internal_links {
                let linked_path = self.root.join(&link.target);
                if linked_path.exists()
                    && linked_path.extension().is_some_and(|e| e == "md")
                    && !doc_files.contains(&linked_path)
                {
                    doc_files.push(linked_path);
                }
            }
        }

        // Also check docs/ directory for ARCHITECTURE.md
        let docs_arch = self.root.join("docs").join("ARCHITECTURE.md");
        if docs_arch.exists() && !doc_files.contains(&docs_arch) {
            doc_files.push(docs_arch);
        }

        doc_files.sort();
        doc_files.dedup();
        Ok(doc_files)
    }
}

/// Shared discovery loop for the lenient and strict walkers.
///
/// `strict = false` keeps the shipped `index`/`doctor` behavior: a walk error
/// is logged and skipped. `strict = true` returns the first walk error so a
/// caller that must prove it saw the whole tree can fail closed.
fn collect_discovered<I>(
    entries: I,
    supported_extensions: &HashSet<String>,
    binary_extensions: &HashSet<String>,
    strict: bool,
) -> Result<Vec<Utf8PathBuf>>
where
    I: Iterator<Item = std::result::Result<ignore::DirEntry, ignore::Error>>,
{
    let mut files = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                if strict {
                    return Err(miette::miette!("Directory walk failed: {e}"));
                }
                tracing::warn!("Error walking directory: {}", e);
                continue;
            }
        };

        if strict && let Some(error) = entry.error() {
            return Err(miette::miette!(
                "Directory walk failed at {}: {error}",
                entry.path().display()
            ));
        }

        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            let path = entry.path();
            let utf8_path = Utf8PathBuf::from_path_buf(path.to_path_buf())
                .map_err(|_| miette::miette!("Invalid UTF-8 path: {:?}", path))?;

            if let Some(ext) = utf8_path.extension()
                && supported_extensions.contains(ext)
                && !binary_extensions.contains(ext)
            {
                files.push(utf8_path);
            }
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
pub(crate) fn discovery_with_injected_error(
    blocked: &camino::Utf8Path,
    strict: bool,
) -> Result<Vec<Utf8PathBuf>> {
    let error = ignore::Error::WithPath {
        path: blocked.as_std_path().to_path_buf(),
        err: Box::new(ignore::Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied by test",
        ))),
    };
    collect_discovered(
        std::iter::once(Err(error)),
        &HashSet::new(),
        &HashSet::new(),
        strict,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use std::path::PathBuf;

    fn supported() -> HashSet<String> {
        ["rs".to_string()].into_iter().collect()
    }

    fn binary() -> HashSet<String> {
        HashSet::new()
    }

    fn io_error(root: &Utf8Path) -> ignore::Error {
        ignore::Error::WithPath {
            path: root.join("blocked").into_std_path_buf(),
            err: Box::new(ignore::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied by test",
            ))),
        }
    }

    fn entries_with_error(
        root: &Utf8Path,
        bad: bool,
    ) -> Vec<std::result::Result<ignore::DirEntry, ignore::Error>> {
        let walker = WalkBuilder::new(root)
            .hidden(true)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .build();
        let mut out: Vec<_> = walker.into_iter().collect();
        if bad {
            out.insert(0, Err(io_error(root)));
        }
        out
    }

    #[test]
    fn walker_strict_returns_error_on_walk_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let root: Utf8PathBuf = PathBuf::from(tmp.path()).try_into().unwrap();
        std::fs::create_dir_all(root.join("src").as_std_path()).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}").unwrap();
        let entries = entries_with_error(&root, true);
        let err = collect_discovered(entries.into_iter(), &supported(), &binary(), true)
            .expect_err("strict discovery must surface the walk error");
        assert!(
            err.to_string().contains("blocked") && err.to_string().contains("denied by test"),
            "error must name the cause: {err}"
        );
    }

    #[test]
    fn walker_lenient_keeps_errors_non_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let root: Utf8PathBuf = PathBuf::from(tmp.path()).try_into().unwrap();
        std::fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        let entries = entries_with_error(&root, true);
        let files = collect_discovered(entries.into_iter(), &supported(), &binary(), false)
            .expect("lenient discovery keeps walking");
        assert_eq!(
            files,
            vec![root.join("a.rs")],
            "lenient discovery must keep the discoverable files"
        );
    }

    #[test]
    fn walker_strict_surfaces_nested_ignore_errors_only_in_strict_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let root: Utf8PathBuf = PathBuf::from(tmp.path()).try_into().unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/.gitignore"), b"invalid\\\n").unwrap();
        std::fs::write(root.join("nested/a.rs"), "fn a() {}").unwrap();
        let walker = RepoWalker::new(root.clone(), &["rs"], &[]);

        assert_eq!(
            walker.discover_files().unwrap(),
            vec![root.join("nested/a.rs")]
        );
        let error = walker.discover_files_strict().unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("nested") && message.contains(".gitignore"),
            "{message}"
        );
    }

    #[test]
    fn walker_strict_matches_lenient_when_no_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let root: Utf8PathBuf = PathBuf::from(tmp.path()).try_into().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}").unwrap();
        std::fs::write(root.join("src/b.rs"), "fn b() {}").unwrap();
        let lenient = RepoWalker::new(root.clone(), &["rs"], &[])
            .discover_files()
            .unwrap();
        let strict = RepoWalker::new(root.clone(), &["rs"], &[])
            .discover_files_strict()
            .unwrap();
        assert_eq!(lenient, strict);
        assert_eq!(strict.len(), 2);
    }
}
