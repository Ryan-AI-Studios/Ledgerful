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
    pub fn discover_files(&self) -> Result<Vec<Utf8PathBuf>> {
        let mut files = Vec::new();

        // WalkBuilder handles .gitignore automatically if it's in a git repo.
        let walker = WalkBuilder::new(&self.root)
            .hidden(true) // Skip hidden files/dirs like .git, .env
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Error walking directory: {}", e);
                    continue;
                }
            };

            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                let path = entry.path();
                let utf8_path = Utf8PathBuf::from_path_buf(path.to_path_buf())
                    .map_err(|_| miette::miette!("Invalid UTF-8 path: {:?}", path))?;

                if let Some(ext) = utf8_path.extension()
                    && self.supported_extensions.contains(ext)
                    && !self.binary_extensions.contains(ext)
                {
                    files.push(utf8_path);
                }
            }
        }

        files.sort();
        Ok(files)
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
