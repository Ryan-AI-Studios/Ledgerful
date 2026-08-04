use crate::search::encoding::{normalize_to_utf8, strip_control_characters};
use crate::search::tantivy_engine::TantivySearchEngine;
use crate::search::trigram::regex_to_trigrams;
use camino::Utf8Path;
use miette::{IntoDiagnostic, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;

/// Max candidate paths from trigram prefilter or all_paths scan.
pub const REGEX_CANDIDATE_CAP: usize = 5000;

/// How [`RegexFilter`] selects candidate paths before line-level matching.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RegexCandidateSource {
    /// Trigrams when `regex_to_trigrams` yields Some, else all_paths.
    #[default]
    Auto,
    /// Force all_paths (identifier-literal empty fallback).
    AllPaths,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RegexMatch {
    pub path: String,
    pub line_number: usize,
    pub content: String,
}

/// Result of a regex candidate + line scan, including candidate-cap honesty.
#[derive(Debug)]
pub struct RegexSearchResult {
    pub matches: Vec<RegexMatch>,
    /// True when the candidate list hit [`REGEX_CANDIDATE_CAP`].
    pub candidates_truncated: bool,
}

pub struct RegexFilter<'a> {
    engine: &'a TantivySearchEngine,
}

impl<'a> RegexFilter<'a> {
    pub fn new(engine: &'a TantivySearchEngine) -> Self {
        Self { engine }
    }

    /// Search with default [`RegexCandidateSource::Auto`].
    pub fn search(&self, root: &Utf8Path, pattern: &str, limit: usize) -> Result<Vec<RegexMatch>> {
        Ok(self
            .search_with(root, pattern, limit, RegexCandidateSource::Auto)?
            .matches)
    }

    /// Search with an explicit candidate source; reports candidate-cap truncation.
    pub fn search_with(
        &self,
        root: &Utf8Path,
        pattern: &str,
        limit: usize,
        source: RegexCandidateSource,
    ) -> Result<RegexSearchResult> {
        let regex = Regex::new(pattern).into_diagnostic()?;

        let candidates = match source {
            RegexCandidateSource::AllPaths => self.engine.all_paths(REGEX_CANDIDATE_CAP)?,
            RegexCandidateSource::Auto => {
                if let Some(trigrams) = regex_to_trigrams(pattern) {
                    self.engine
                        .search_trigrams(&trigrams, REGEX_CANDIDATE_CAP)?
                } else {
                    self.engine.all_paths(REGEX_CANDIDATE_CAP)?
                }
            }
        };

        let candidates_truncated = candidates.len() >= REGEX_CANDIDATE_CAP;

        let mut matches = Vec::new();
        let mut seen_paths = std::collections::HashSet::new();

        for path_str in candidates {
            if seen_paths.contains(&path_str) {
                continue;
            }
            seen_paths.insert(path_str.clone());

            if matches.len() >= limit {
                break;
            }

            let full_path = root.join(&path_str);
            if let Ok(content_bytes) = fs::read(&full_path) {
                let content = normalize_to_utf8(&content_bytes);
                let clean_content = strip_control_characters(&content);

                for (idx, line) in clean_content.lines().enumerate() {
                    if line.len() > 1000 {
                        // Skip very long lines
                        continue;
                    }

                    if regex.is_match(line) {
                        matches.push(RegexMatch {
                            path: path_str.clone(),
                            line_number: idx + 1,
                            content: line.trim().to_string(),
                        });

                        if matches.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }

        Ok(RegexSearchResult {
            matches,
            candidates_truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::tantivy_engine::TantivySearchEngine;
    use crate::search::trigram::extract_trigrams;
    use tantivy::schema::TantivyDocument;
    use tempfile::TempDir;

    fn make_engine(dir: &TempDir) -> TantivySearchEngine {
        TantivySearchEngine::open_or_create(dir.path()).expect("open_or_create")
    }

    fn index_doc(engine: &TantivySearchEngine, path: &str, content: &str) {
        let schema = engine.schema();
        let path_field = schema.get_field("path").expect("path field");
        let content_field = schema.get_field("content").expect("content field");
        let line_count_field = schema.get_field("line_count").expect("line_count field");
        let trigrams_field = schema.get_field("trigrams").expect("trigrams field");

        let tgrams_str = extract_trigrams(content)
            .into_iter()
            .collect::<Vec<_>>()
            .join(" ");

        let mut writer = engine.get_writer(15_000_000).expect("writer");
        let mut doc = TantivyDocument::default();
        doc.add_text(path_field, path);
        doc.add_text(content_field, content);
        doc.add_u64(line_count_field, 1);
        doc.add_text(trigrams_field, &tgrams_str);
        writer.add_document(doc).expect("add_document");
        writer.commit().expect("commit");
        engine.reload_reader().expect("reload_reader");
    }

    #[test]
    fn identifier_all_paths_fallback_finds_indexed_file() {
        let dir = TempDir::new().expect("tempdir");
        let engine = make_engine(&dir);
        // Write a real file under root so RegexFilter can read it.
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        let file_rel = "src/ident.rs";
        let body = "fn verify_step_key() { /* unique_id_xyz */ }";
        std::fs::write(root.join(file_rel), body).expect("write");
        index_doc(&engine, file_rel, body);

        let filter = RegexFilter::new(&engine);
        let root_utf8 = camino::Utf8Path::from_path(&root).expect("utf8 root");
        let escaped = regex::escape("verify_step_key");
        let result = filter
            .search_with(root_utf8, &escaped, 10, RegexCandidateSource::AllPaths)
            .expect("search_with AllPaths");
        assert!(
            !result.matches.is_empty(),
            "AllPaths identifier literal must find indexed file"
        );
        assert_eq!(result.matches[0].path, file_rel);
        assert!(!result.candidates_truncated);
    }
}
