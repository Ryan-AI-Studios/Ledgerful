//! On-disk FTS format stamp for tokenizer/analyzer revisions that do not
//! change the Tantivy schema (so schema-mismatch reopen alone is insufficient).
//!
//! Stamp file sits beside index segments under the search_index directory.

use miette::{IntoDiagnostic, Result};
use std::path::Path;

/// Sibling of Tantivy index files under `search_index_dir`.
pub const SEARCH_FORMAT_STAMP_NAME: &str = "ledgerful_search_format";

/// Content written when the code-identifier dual-emit tokenizer is active.
pub const SEARCH_FORMAT_STAMP_CONTENT: &str = "code_tokenizer_v2";

/// Absolute path of the stamp file for `index_dir`.
pub fn stamp_path(index_dir: &Path) -> std::path::PathBuf {
    index_dir.join(SEARCH_FORMAT_STAMP_NAME)
}

/// True when stamp exists and matches the current format id.
pub fn stamp_matches(index_dir: &Path) -> bool {
    let path = stamp_path(index_dir);
    match std::fs::read_to_string(&path) {
        Ok(contents) => contents.trim() == SEARCH_FORMAT_STAMP_CONTENT,
        Err(_) => false,
    }
}

/// Write the current format stamp (creates parent dirs if needed).
pub fn write_stamp(index_dir: &Path) -> Result<()> {
    if !index_dir.exists() {
        std::fs::create_dir_all(index_dir).into_diagnostic()?;
    }
    let path = stamp_path(index_dir);
    std::fs::write(&path, SEARCH_FORMAT_STAMP_CONTENT).into_diagnostic()?;
    Ok(())
}

/// True when the on-disk index must be rebuilt for the current tokenizer format.
pub fn needs_format_rebuild(index_dir: &Path) -> bool {
    !stamp_matches(index_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn format_stamp_missing_needs_rebuild() {
        let dir = TempDir::new().expect("tempdir");
        assert!(needs_format_rebuild(dir.path()));
        assert!(!stamp_matches(dir.path()));
    }

    #[test]
    fn format_stamp_write_and_match() {
        let dir = TempDir::new().expect("tempdir");
        write_stamp(dir.path()).expect("write_stamp");
        assert!(stamp_matches(dir.path()));
        assert!(!needs_format_rebuild(dir.path()));
        let raw = std::fs::read_to_string(stamp_path(dir.path())).expect("read");
        assert_eq!(raw.trim(), SEARCH_FORMAT_STAMP_CONTENT);
    }

    #[test]
    fn format_stamp_stale_content_needs_rebuild() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(stamp_path(dir.path()), "code_tokenizer_v1").expect("write stale");
        assert!(!stamp_matches(dir.path()));
        assert!(needs_format_rebuild(dir.path()));
    }
}
