//! Ask-local identifier rewrite and RRF fusion (0395).
//!
//! Do **not** reuse `is_identifier_likely` as a per-token extractor (whole-query
//! predicate). Do **not** key fusion on raw `RankedChunk.source`.

use crate::local_model::pruner::RankedChunk;
use crate::search::tantivy_engine::normalize_search_path;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Cormack/Clarke RRF constant.
pub(crate) const RRF_K: f32 = 60.0;

const STRUCTURAL_CAP: usize = 8;

const STOPWORDS: &[&str] = &[
    "where",
    "is",
    "the",
    "a",
    "an",
    "of",
    "to",
    "for",
    "with",
    "from",
    "how",
    "why",
    "what",
    "who",
    "cite",
    "file",
    "files",
    "path",
    "paths",
    "please",
    "resolved",
    "defined",
    "located",
    "implemented",
];

/// Injectable `project_symbols` JOIN row (0395).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolRow {
    pub name: String,
    pub path: String,
    pub kind: String,
}

/// Which ranked list produced a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateOrigin {
    Vector,
    Fts,
    Structural,
}

/// One item in a pre-fusion ranked list.
#[derive(Debug, Clone)]
pub(crate) struct RankedListItem {
    pub path: String,
    pub symbol_name: Option<String>,
    pub content: String,
}

/// Token-span definition cue (not adjacent phrase, not CG-F20 first-word).
pub(crate) fn is_definition_shaped(query: &str) -> bool {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)\b(where|find|locate)\b.{1,50}\b(defined|resolved|located|implemented)\b")
            .ok()
    });
    re.as_ref().is_some_and(|r| r.is_match(query))
}

/// Explicit identifiers: `_` / `::` / CamelCase (not a mere capitalized word).
pub(crate) fn is_explicit_identifier(tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    if tok.contains('_') || tok.contains("::") {
        return true;
    }
    let mut chars = tok.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let rest: Vec<char> = chars.collect();
    if first.is_uppercase()
        && rest.iter().any(|c| c.is_uppercase())
        && rest.iter().any(|c| c.is_lowercase())
    {
        return true;
    }
    if first.is_lowercase() && rest.iter().any(|c| c.is_uppercase()) {
        return true;
    }
    false
}

/// Explicit identifier tokens already in the sentence (not stopwords).
pub(crate) fn extract_identifier_candidates(query: &str) -> Vec<String> {
    let mut out: Vec<String> = query
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .filter(|t| !t.is_empty() && is_explicit_identifier(t))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn is_stopword(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    STOPWORDS.contains(&lower.as_str())
}

/// Content words for SQL lookup (definition-shaped queries).
pub(crate) fn content_words(query: &str) -> Vec<String> {
    crate::commands::search::tokenize_search_query(query)
        .into_iter()
        .filter(|t| !is_stopword(t) && !is_explicit_identifier(t))
        .collect()
}

fn path_has_config_hint(path: &str) -> bool {
    path.to_ascii_lowercase().contains("config")
}

fn query_has_config_hint(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    lower.contains("config")
}

fn kind_is_function_or_method(kind: &str) -> bool {
    matches!(kind, "Function" | "Method")
}

/// Rank injectable symbol rows; cap 8. Path-hint + Function/Method first.
pub(crate) fn rewrite_identifiers(query: &str, rows: &[SymbolRow]) -> Vec<String> {
    let words = content_words(query);
    let ids = extract_identifier_candidates(query);
    let hint = query_has_config_hint(query);
    let mut scored: Vec<(u8, u8, usize, String, String)> = Vec::new();
    for row in rows {
        let name_l = row.name.to_ascii_lowercase();
        let word_hit = words
            .iter()
            .any(|w| name_l.contains(&w.to_ascii_lowercase()));
        let id_hit = ids.iter().any(|id| id.eq_ignore_ascii_case(&row.name));
        if !word_hit && !id_hit {
            continue;
        }
        let path_rank: u8 = if hint && path_has_config_hint(&row.path) {
            0
        } else {
            1
        };
        let kind_rank: u8 = if kind_is_function_or_method(&row.kind) {
            0
        } else {
            1
        };
        scored.push((
            path_rank,
            kind_rank,
            row.name.len(),
            row.name.clone(),
            row.path.clone(),
        ));
    }
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
            .then(a.4.cmp(&b.4))
    });
    let mut names: Vec<String> = Vec::new();
    for (_, _, _, name, _) in scored {
        if !names.iter().any(|n| n == &name) {
            names.push(name);
        }
        if names.len() >= STRUCTURAL_CAP {
            break;
        }
    }
    for id in ids {
        if !names.iter().any(|n| n == &id) {
            names.push(id);
        }
        if names.len() >= STRUCTURAL_CAP {
            break;
        }
    }
    names.truncate(STRUCTURAL_CAP);
    names
}

fn fusion_key(path: &str) -> String {
    normalize_search_path(path)
}

/// Unweighted RRF over populated lists. Same-path merge keeps structural snippet.
pub(crate) fn rrf_merge(
    lists: &[(CandidateOrigin, Vec<RankedListItem>)],
    limit: usize,
) -> Vec<RankedChunk> {
    #[derive(Clone)]
    struct Acc {
        score: f32,
        path: String,
        symbol_name: Option<String>,
        content: String,
        has_structural: bool,
    }
    let mut by_path: HashMap<String, Acc> = HashMap::new();
    for (origin, items) in lists {
        if items.is_empty() {
            continue;
        }
        for (idx, item) in items.iter().enumerate() {
            let rank = (idx + 1) as f32;
            let add = 1.0 / (RRF_K + rank);
            let key = fusion_key(&item.path);
            let is_structural = matches!(origin, CandidateOrigin::Structural);
            by_path
                .entry(key.clone())
                .and_modify(|acc| {
                    acc.score += add;
                    if is_structural && !acc.has_structural {
                        acc.symbol_name = item.symbol_name.clone();
                        if !item.content.is_empty() {
                            acc.content = item.content.clone();
                        }
                        acc.has_structural = true;
                    } else if !acc.has_structural && item.symbol_name.is_some() {
                        acc.symbol_name = item.symbol_name.clone();
                        if !item.content.is_empty() {
                            acc.content = item.content.clone();
                        }
                    }
                })
                .or_insert(Acc {
                    score: add,
                    path: key,
                    symbol_name: item.symbol_name.clone(),
                    content: item.content.clone(),
                    has_structural: is_structural,
                });
        }
    }
    let mut out: Vec<Acc> = by_path.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| match (&a.symbol_name, &b.symbol_name) {
                (Some(x), Some(y)) => x.cmp(y),
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, None) => std::cmp::Ordering::Equal,
            })
    });
    out.truncate(limit);
    out.into_iter()
        .map(|acc| {
            let source = match acc.symbol_name {
                Some(ref name) if !name.is_empty() => format!("{}::{}", acc.path, name),
                _ => acc.path,
            };
            RankedChunk {
                source,
                content: acc.content,
                score: acc.score,
            }
        })
        .collect()
}

/// Split `path` or `path::symbol` (not KG).
pub(crate) fn split_ranked_source(source: &str) -> (String, Option<String>) {
    if source.starts_with("Knowledge Graph") {
        return (source.to_string(), None);
    }
    if let Some((path, name)) = source.split_once("::") {
        (normalize_search_path(path), Some(name.to_string()))
    } else {
        (normalize_search_path(source), None)
    }
}

pub(crate) fn is_kg_source(source: &str) -> bool {
    source.starts_with("Knowledge Graph")
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    #[test]
    fn is_definition_shaped__resolved_phrase() {
        assert!(is_definition_shaped(
            "Where is configuration provenance resolved? Cite file paths."
        ));
        assert!(is_definition_shaped("where is apply_provenance defined"));
        assert!(!is_definition_shaped(
            "How does apply_provenance handle env vars?"
        ));
    }

    #[test]
    fn extract_identifier_candidates__skips_capitalized_english() {
        let ids = extract_identifier_candidates(
            "Where is configuration provenance resolved? Cite file paths.",
        );
        assert!(
            !ids.iter().any(|t| t == "Where" || t == "Cite" || t == "is"),
            "capitalized English must not count as identifiers: {ids:?}"
        );
        let from_explicit = extract_identifier_candidates("How does apply_provenance handle env?");
        assert!(from_explicit.iter().any(|t| t == "apply_provenance"));
        assert!(is_explicit_identifier("ProvenanceContext"));
        assert!(!is_explicit_identifier("Where"));
    }

    #[test]
    fn rewrite_identifiers__configuration_provenance__includes_apply_provenance() {
        let rows = vec![
            SymbolRow {
                name: "apply_provenance".into(),
                path: "src/commands/config_verify.rs".into(),
                kind: "Function".into(),
            },
            SymbolRow {
                name: "find_transactions_by_file".into(),
                path: "src/ledger/db.rs".into(),
                kind: "Function".into(),
            },
            SymbolRow {
                name: "ProvenanceAction".into(),
                path: "src/ledger/provenance.rs".into(),
                kind: "Enum".into(),
            },
        ];
        let names = rewrite_identifiers(
            "Where is configuration provenance resolved? Cite file paths.",
            &rows,
        );
        assert!(
            names.iter().any(|n| n == "apply_provenance"),
            "path-hint Function must surface: {names:?}"
        );
        assert_eq!(names.first().map(String::as_str), Some("apply_provenance"));
    }

    #[test]
    fn rrf_merge__same_path_fts_and_structural__merges() {
        let fts = vec![RankedListItem {
            path: "src/commands/config_verify.rs".into(),
            symbol_name: None,
            content: "some fts snippet".into(),
        }];
        let structural = vec![RankedListItem {
            path: "src/commands/config_verify.rs".into(),
            symbol_name: Some("apply_provenance".into()),
            content: "fn apply_provenance".into(),
        }];
        let fused = rrf_merge(
            &[
                (CandidateOrigin::Fts, fts),
                (CandidateOrigin::Structural, structural),
            ],
            3,
        );
        assert_eq!(fused.len(), 1);
        assert!(
            fused[0].source.contains("config_verify.rs"),
            "{}",
            fused[0].source
        );
        assert!(
            fused[0].source.contains("apply_provenance"),
            "structural snippet keeps ::name: {}",
            fused[0].source
        );
        assert!(fused[0].content.contains("apply_provenance"));
        let expected = 1.0 / (RRF_K + 1.0) + 1.0 / (RRF_K + 1.0);
        assert!((fused[0].score - expected).abs() < 1e-6);
    }

    #[test]
    fn rrf_merge__vector_db_rs_plus_structural_config_verify__config_verify_in_top_limit() {
        let vectors = vec![RankedListItem {
            path: "src/ledger/db.rs".into(),
            symbol_name: Some("mod".into()),
            content: "mod provenance;".into(),
        }];
        let fts = vec![RankedListItem {
            path: "src/ledger/provenance.rs".into(),
            symbol_name: None,
            content: "provenance provenance provenance".into(),
        }];
        let structural = vec![RankedListItem {
            path: "src/commands/config_verify.rs".into(),
            symbol_name: Some("apply_provenance".into()),
            content: "fn apply_provenance".into(),
        }];
        let fused = rrf_merge(
            &[
                (CandidateOrigin::Vector, vectors),
                (CandidateOrigin::Fts, fts),
                (CandidateOrigin::Structural, structural),
            ],
            3,
        );
        assert!(
            fused.iter().any(|c| c.source.contains("config_verify.rs")),
            "fused: {:?}",
            fused.iter().map(|c| c.source.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rrf_merge__same_path_two_structural__keeps_first_name() {
        let structural = vec![
            RankedListItem {
                path: "src/commands/config_verify.rs".into(),
                symbol_name: Some("apply_provenance".into()),
                content: "fn apply_provenance".into(),
            },
            RankedListItem {
                path: "src/commands/config_verify.rs".into(),
                symbol_name: Some("apply_provenance_preserves_inherited_with_file_origin".into()),
                content: "fn apply_provenance_preserves_inherited_with_file_origin".into(),
            },
        ];
        let fused = rrf_merge(&[(CandidateOrigin::Structural, structural)], 3);
        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].source,
            "src/commands/config_verify.rs::apply_provenance"
        );
        assert!(fused[0].content.contains("fn apply_provenance"));
        assert!(
            !fused[0]
                .content
                .contains("apply_provenance_preserves_inherited_with_file_origin")
        );
    }

    #[test]
    fn rrf_merge__ties_break_path_then_symbol_name() {
        let a = vec![RankedListItem {
            path: "src/b.rs".into(),
            symbol_name: Some("z".into()),
            content: "z".into(),
        }];
        let b = vec![RankedListItem {
            path: "src/a.rs".into(),
            symbol_name: Some("a".into()),
            content: "a".into(),
        }];
        let fused = rrf_merge(
            &[(CandidateOrigin::Fts, a), (CandidateOrigin::Vector, b)],
            2,
        );
        assert_eq!(fused.len(), 2);
        assert!(fused[0].source.contains("src/a.rs") || fused[0].source.contains("src/b.rs"));
        let paths: Vec<_> = fused.iter().map(|c| c.source.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        // Equal RRF (both rank 1 in their lists) → path ASC.
        assert_eq!(paths[0].as_str(), "src/a.rs::a");
        assert_eq!(paths[1].as_str(), "src/b.rs::z");
        let _ = sorted;
    }
}
