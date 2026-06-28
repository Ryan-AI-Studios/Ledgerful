use crate::search::encoding::{normalize_to_utf8, strip_control_characters};
use crate::search::tantivy_engine::TantivySearchEngine;
use crate::search::trigram::regex_to_trigrams;
use camino::Utf8Path;
use miette::{IntoDiagnostic, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize, Debug)]
pub struct RegexMatch {
    pub path: String,
    pub line_number: usize,
    pub content: String,
}

pub struct RegexFilter<'a> {
    engine: &'a TantivySearchEngine,
}

impl<'a> RegexFilter<'a> {
    pub fn new(engine: &'a TantivySearchEngine) -> Self {
        Self { engine }
    }

    pub fn search(&self, root: &Utf8Path, pattern: &str, limit: usize) -> Result<Vec<RegexMatch>> {
        let regex = Regex::new(pattern).into_diagnostic()?;

        // 1. Pre-filter using trigrams
        let candidates = if let Some(trigrams) = regex_to_trigrams(pattern) {
            self.engine.search_trigrams(&trigrams, 1000)? // Limit candidates
        } else {
            // No trigrams derived, must scan all files
            self.engine.all_paths(1000)?
        };

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

        Ok(matches)
    }
}
