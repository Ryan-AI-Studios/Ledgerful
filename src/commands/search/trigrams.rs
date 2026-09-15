//! Trigram path search (`search-trigrams`).

use crate::commands::helpers::get_layout;
use crate::search::TantivySearchEngine;
use crate::search::tantivy_engine::normalize_search_path;
use miette::Result;
use serde::Serialize;

const KIND: &str = "searchTrigrams";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClassifiedTrigrams {
    pub query: Vec<String>,
    pub accepted: Vec<String>,
    pub rejected: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchTrigramsHit {
    path: String,
    score: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchTrigramsEnvelope {
    schema_version: u32,
    kind: &'static str,
    query: Vec<String>,
    accepted: Vec<String>,
    rejected: Vec<String>,
    limit: u64,
    result_count: usize,
    total_matching: usize,
    truncated: bool,
    document_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    empty_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
    results: Vec<SearchTrigramsHit>,
}

/// Lowercase first, then accept iff the folded token is exactly 3 chars.
pub(crate) fn classify_trigrams(raw: &[String]) -> ClassifiedTrigrams {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for token in raw {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if lower.chars().count() == 3 {
            accepted.push(lower);
        } else {
            rejected.push(trimmed.to_string());
        }
    }
    ClassifiedTrigrams {
        query: raw.to_vec(),
        accepted,
        rejected,
    }
}

fn is_empty_query(classified: &ClassifiedTrigrams) -> bool {
    classified.accepted.is_empty() && classified.rejected.is_empty()
}

fn search_next(accepted: &[String]) -> Option<String> {
    if accepted.is_empty() {
        None
    } else {
        Some(format!("ledgerful search {}", accepted.join(" ")))
    }
}

fn next_action(empty_reason: Option<&str>, accepted: &[String]) -> Option<String> {
    if empty_reason == Some("emptyIndex") {
        Some("ledgerful index".to_string())
    } else {
        search_next(accepted)
    }
}

fn rejected_csv(rejected: &[String]) -> String {
    rejected.join(", ")
}

struct DiagnosticView<'a> {
    empty_reason: Option<&'a str>,
    accepted: &'a [String],
    rejected: &'a [String],
    result_count: usize,
    total_matching: usize,
    document_count: usize,
    limit: u64,
    truncated: bool,
}

fn human_diagnostic(view: DiagnosticView<'_>) -> String {
    let n = view.accepted.len();
    let mut line = match view.empty_reason {
        Some("emptyQuery") => {
            return "search-trigrams: empty query (each argument is one 3-character content trigram, AND). Ordinary source search is ledgerful search <query> (not a copy-paste argv)"
                .to_string();
        }
        Some("invalidTrigrams") => {
            return format!(
                "search-trigrams: rejected token(s) (not 3 characters): {}. Ordinary source search is ledgerful search <query> (not a copy-paste argv)",
                rejected_csv(view.rejected)
            );
        }
        Some("emptyIndex") => {
            return "search-trigrams: empty index (documentCount=0). Next: ledgerful index"
                .to_string();
        }
        Some("noMatches") => format!(
            "search-trigrams: no matching paths (AND of {n} trigram(s); documentCount={})",
            view.document_count
        ),
        _ if view.truncated => format!(
            "search-trigrams: showing {} of {} matching paths (limit={}; AND of {n} trigram(s))",
            view.result_count, view.total_matching, view.limit
        ),
        _ => format!(
            "search-trigrams: {} matching path(s) (AND of {n} trigram(s); documentCount={})",
            view.result_count, view.document_count
        ),
    };
    if !view.rejected.is_empty() && !view.accepted.is_empty() {
        line.push_str(" rejected: ");
        line.push_str(&rejected_csv(view.rejected));
    }
    if let Some(cmd) = search_next(view.accepted) {
        line.push_str(". Ordinary search: ");
        line.push_str(&cmd);
    }
    line
}

struct EmitArgs<'a> {
    classified: &'a ClassifiedTrigrams,
    limit: u64,
    results: Vec<SearchTrigramsHit>,
    total_matching: usize,
    document_count: usize,
    truncated: bool,
    empty_reason: Option<&'static str>,
    json: bool,
    suppress_diag: bool,
}

pub fn execute_search_trigrams(
    trigrams: Vec<String>,
    limit: u64,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let classified = classify_trigrams(&trigrams);
    let suppress_diag = json || quiet;

    if classified.accepted.is_empty() {
        let empty_reason = if is_empty_query(&classified) {
            "emptyQuery"
        } else {
            "invalidTrigrams"
        };
        return emit(EmitArgs {
            classified: &classified,
            limit,
            results: Vec::new(),
            total_matching: 0,
            document_count: 0,
            truncated: false,
            empty_reason: Some(empty_reason),
            json,
            suppress_diag,
        });
    }

    let layout = get_layout()?;
    let index_path = layout.search_index_dir();
    let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;
    let document_count = engine.document_count();
    if document_count == 0 {
        return emit(EmitArgs {
            classified: &classified,
            limit,
            results: Vec::new(),
            total_matching: 0,
            document_count,
            truncated: false,
            empty_reason: Some("emptyIndex"),
            json,
            suppress_diag,
        });
    }

    let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
    let page = engine.search_trigrams_page(&classified.accepted, limit_usize)?;
    let truncated = page.hits.len() < page.total_matching;
    let empty_reason = if page.hits.is_empty() {
        Some("noMatches")
    } else {
        None
    };
    emit(EmitArgs {
        classified: &classified,
        limit,
        results: page
            .hits
            .into_iter()
            .map(|hit| SearchTrigramsHit {
                path: normalize_search_path(&hit.path),
                score: hit.score,
            })
            .collect(),
        total_matching: page.total_matching,
        document_count: page.document_count,
        truncated,
        empty_reason,
        json,
        suppress_diag,
    })
}

fn emit(args: EmitArgs<'_>) -> Result<()> {
    let result_count = args.results.len();
    if args.json {
        let envelope = SearchTrigramsEnvelope {
            schema_version: 1,
            kind: KIND,
            query: args.classified.query.clone(),
            accepted: args.classified.accepted.clone(),
            rejected: args.classified.rejected.clone(),
            limit: args.limit,
            result_count,
            total_matching: args.total_matching,
            truncated: args.truncated,
            document_count: args.document_count,
            empty_reason: args.empty_reason,
            next: next_action(args.empty_reason, &args.classified.accepted),
            results: args.results,
        };
        return crate::output::json::emit(&envelope);
    }

    for hit in &args.results {
        println!("{}", hit.path);
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    if !args.suppress_diag {
        let line = human_diagnostic(DiagnosticView {
            empty_reason: args.empty_reason,
            accepted: &args.classified.accepted,
            rejected: &args.classified.rejected,
            result_count,
            total_matching: args.total_matching,
            document_count: args.document_count,
            limit: args.limit,
            truncated: args.truncated,
        });
        eprintln!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_trigrams_rejects_non_three_char_tokens() {
        let c = classify_trigrams(&["ledger".into(), "led".into(), "GER".into()]);
        assert_eq!(c.accepted, vec!["led", "ger"]);
        assert_eq!(c.rejected, vec!["ledger"]);
    }

    #[test]
    fn search_trigrams_empty_query_is_empty_reason() {
        let empty = classify_trigrams(&[]);
        assert!(is_empty_query(&empty));
        let ws = classify_trigrams(&["  ".into(), "".into()]);
        assert!(is_empty_query(&ws));
        assert!(next_action(Some("emptyQuery"), &[]).is_none());
    }

    #[test]
    fn search_trigrams_invalid_then_empty_index_precedence() {
        let c = classify_trigrams(&["led".into(), "ger".into()]);
        assert!(!is_empty_query(&c));
        assert_eq!(
            next_action(Some("emptyIndex"), &c.accepted).as_deref(),
            Some("ledgerful index")
        );
        assert_eq!(
            next_action(Some("noMatches"), &c.accepted).as_deref(),
            Some("ledgerful search led ger")
        );
    }

    #[test]
    fn search_trigrams_mixed_accept_reject_rejected_clause() {
        let c = classify_trigrams(&["led".into(), "ledger".into()]);
        let line = human_diagnostic(DiagnosticView {
            empty_reason: None,
            accepted: &c.accepted,
            rejected: &c.rejected,
            result_count: 1,
            total_matching: 1,
            document_count: 2,
            limit: 100,
            truncated: false,
        });
        assert!(
            line.contains(" rejected: ledger. Ordinary search:"),
            "line={line}"
        );
        assert!(line.contains("ledgerful search led"), "line={line}");
    }
}
