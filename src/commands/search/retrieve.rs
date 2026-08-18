//! Retrieve / present / fallback helpers for `ledgerful search`.

use super::{HitEmit, SearchArgs, SearchCollector};
use crate::search::{
    RegexCandidateSource, RegexFilter, RegexMatch, RegexSearchResult, TantivySearchEngine,
};
use camino::Utf8Path;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};

pub fn is_regex_likely(query: &str) -> bool {
    query.chars().any(|c| {
        matches!(
            c,
            '^' | '$' | '.' | '*' | '+' | '?' | '[' | ']' | '(' | ')' | '|'
        )
    })
}

pub fn is_identifier_likely(query: &str) -> bool {
    !query.is_empty()
        && !query.contains(' ')
        && query
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
}

pub(crate) fn perform_search(
    engine: TantivySearchEngine,
    root: &Utf8Path,
    args: &SearchArgs,
    collector: &mut SearchCollector,
    use_regex: bool,
    use_hybrid: bool,
) -> Result<()> {
    // Overfetch by one so human path can emit a truncation affordance without
    // claiming an exact "K more" total (0100 DoD-7). Envelope uses the same
    // signal for `truncated`.
    let overfetch = args.limit.saturating_add(1);

    if use_hybrid {
        if !args.is_machine() {
            println!("[Search Mode: Hybrid]");
        }
        let filter = RegexFilter::new(&engine);
        // Identifier-likely hybrid: escape as literal so meta chars in ids are safe.
        // Explicit --regex path keeps raw query (handled below).
        let regex_pattern = if is_identifier_likely(&args.query) {
            regex::escape(&args.query)
        } else {
            args.query.clone()
        };
        let regex_result = filter
            .search_with(root, &regex_pattern, overfetch, RegexCandidateSource::Auto)
            .ok();
        let regex_candidates_truncated = regex_result
            .as_ref()
            .map(|r| r.candidates_truncated)
            .unwrap_or(false);
        let regex_matches = regex_result
            .as_ref()
            .map(|r| r.matches.clone())
            .unwrap_or_default();
        let regex_had_hits = !regex_matches.is_empty();
        let bm25_results = engine.search(&args.query, overfetch).unwrap_or_default();

        struct MergedResult {
            path: String,
            line_number: Option<usize>,
            /// Plain fragment for JSON and for emphasis application at print time.
            content: String,
            /// Byte ranges into `content` for gated owo_colors emphasis (human only).
            highlight_ranges: Vec<(usize, usize)>,
            score: Option<f32>,
            is_regex: bool,
        }

        let mut merged: std::collections::HashMap<(String, Option<usize>), MergedResult> =
            std::collections::HashMap::new();

        for r in bm25_results {
            // Seed from plain fragment (not pre-rendered highlighted). Emphasis
            // is applied only on the human path; machine content stays plain (DoD-5).
            let content = r.snippet.clone().unwrap_or_default();
            let highlight_ranges = r.highlight_ranges.unwrap_or_default();
            merged.insert(
                (r.path.clone(), r.line_number),
                MergedResult {
                    path: r.path,
                    line_number: r.line_number,
                    content,
                    highlight_ranges,
                    score: Some(r.score),
                    is_regex: false,
                },
            );
        }

        for m in regex_matches {
            let key = (m.path.clone(), Some(m.line_number));
            let mut boost = 5.0;
            if m.path.ends_with(".rs")
                || m.path.ends_with(".ts")
                || m.path.ends_with(".js")
                || m.path.ends_with(".go")
                || m.path.ends_with(".py")
            {
                boost += 10.0;
            }
            if let Some(existing) = merged.get_mut(&key) {
                // Boost existing BM25 score
                let current_score = existing.score.unwrap_or(0.0);
                existing.score = Some(current_score + boost);
                existing.is_regex = true;
            } else {
                merged.insert(
                    key,
                    MergedResult {
                        path: m.path,
                        line_number: Some(m.line_number),
                        content: m.content,
                        highlight_ranges: Vec::new(),
                        score: Some(boost),
                        is_regex: true,
                    },
                );
            }
        }

        let mut merged_results: Vec<MergedResult> = merged.into_values().collect();
        merged_results.sort_by(|a, b| {
            let score_a = a.score.unwrap_or(0.0);
            let score_b = b.score.unwrap_or(0.0);
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let truncated =
            merged_results.len() > args.limit || (regex_candidates_truncated && regex_had_hits);
        merged_results.truncate(args.limit);
        collector.set_truncated(truncated);

        if merged_results.is_empty() && is_identifier_likely(&args.query) {
            // Empty hybrid fallback: escaped literal + all_paths candidates.
            match try_identifier_literal_fallback(&filter, root, &args.query, overfetch) {
                Ok(Some(fallback)) => {
                    if !args.is_machine() {
                        println!(
                            "{} identifier literal fallback (all_paths candidates)",
                            "INFO".if_supports_color(Stream::Stdout, |s| s
                                .style(Style::new().yellow().bold()))
                        );
                    }
                    collector.set_fallback_used("identifier_literal");
                    if fallback.candidates_truncated {
                        collector.set_truncated(true);
                    }
                    let mut matches = fallback.matches;
                    let truncated_hits = matches.len() > args.limit;
                    matches.truncate(args.limit);
                    if truncated_hits {
                        collector.set_truncated(true);
                    }
                    emit_regex_style_hits(args, collector, matches, truncated_hits);
                }
                _ => {
                    handle_fuzzy_fallback(&engine, args, collector);
                }
            }
        } else if merged_results.is_empty() {
            handle_fuzzy_fallback(&engine, args, collector);
        } else if args.is_machine() {
            for res in merged_results {
                let kind = if res.is_regex {
                    "regex_match"
                } else {
                    "bm25_match"
                };
                let score = res.score.map(|s| s as f64);
                let bridge_content = if res.is_regex {
                    format!(
                        "{}:{}: {}",
                        res.path,
                        res.line_number.unwrap_or(0),
                        res.content
                    )
                } else {
                    format!("{} ({})", res.path, res.content)
                };
                let memory_id = if let Some(line) = res.line_number {
                    format!("{}::{}", res.path, line)
                } else {
                    res.path.clone()
                };
                let relevance = res.score.unwrap_or(1.0) as f64;
                collector.push_hit(HitEmit {
                    kind,
                    path: res.path,
                    line: res.line_number,
                    score,
                    content: res.content,
                    bridge_content,
                    bridge_relevance: relevance,
                    bridge_memory_id: memory_id,
                });
            }
        } else {
            println!(
                "\n{}",
                "Hybrid Search Results (BM25 + Regex):"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            for res in merged_results {
                let line_info = if let Some(line) = res.line_number {
                    format!(
                        ":{}",
                        line.to_string()
                            .if_supports_color(Stream::Stdout, |s| s.yellow())
                    )
                } else {
                    String::new()
                };
                let source_label = if res.is_regex {
                    "[Regex]"
                        .if_supports_color(Stream::Stdout, |s| s.magenta())
                        .to_string()
                } else {
                    "[BM25]"
                        .if_supports_color(Stream::Stdout, |s| s.green())
                        .to_string()
                };
                let score_info = if let Some(score) = res.score {
                    format!(" [score: {:.2}]", score)
                } else {
                    String::new()
                };
                // Apply emphasis at print time via if_supports_color (0131 colour
                // gate). Under NO_COLOR / non-TTY, escapes are suppressed.
                let display = emphasize_snippet(&res.content, &res.highlight_ranges);
                println!(
                    "{} {}{} {}",
                    source_label,
                    format!(
                        "{}{}",
                        res.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                        line_info
                    )
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
                    score_info.if_supports_color(Stream::Stdout, |s| s.yellow()),
                    display.trim()
                );
            }
            if truncated {
                print_search_truncation_affordance();
            }
            println!();
        }
    } else if use_regex {
        if !args.is_machine() {
            println!("[Search Mode: Regex]");
        }
        let filter = RegexFilter::new(&engine);
        let mut matches = filter.search(root, &args.query, overfetch)?;
        let truncated = matches.len() > args.limit;
        matches.truncate(args.limit);
        collector.set_truncated(truncated);

        if args.is_machine() {
            for m in matches {
                let bridge_content = format!("{}:{}: {}", m.path, m.line_number, m.content);
                let memory_id = format!("{}::{}", m.path, m.line_number);
                collector.push_hit(HitEmit {
                    kind: "regex_match",
                    path: m.path,
                    line: Some(m.line_number),
                    score: Some(1.0),
                    content: m.content,
                    bridge_content,
                    bridge_relevance: 1.0,
                    bridge_memory_id: memory_id,
                });
            }
        } else {
            println!(
                "\n{}",
                "Regex Search Results:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            if matches.is_empty() {
                println!("No matches found.");
                println!(
                    "{} Check your regex syntax or run {} if changes are missing.",
                    "HINT".if_supports_color(Stream::Stdout, |s| s
                        .style(Style::new().yellow().bold())),
                    "ledgerful index"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
                );
            } else {
                for m in matches {
                    println!(
                        "{}:{}: {}",
                        m.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                        m.line_number
                            .to_string()
                            .if_supports_color(Stream::Stdout, |s| s.yellow()),
                        m.content.trim()
                    );
                }
                if truncated {
                    print_search_truncation_affordance();
                }
            }
            println!();
        }
    } else {
        if !args.is_machine() {
            println!("[Search Mode: BM25]");
        }
        let mut results = engine.search(&args.query, overfetch)?;
        let truncated = results.len() > args.limit;
        results.truncate(args.limit);
        collector.set_truncated(truncated);

        if results.is_empty() {
            handle_fuzzy_fallback(&engine, args, collector);
        } else if args.is_machine() {
            for r in results {
                let content = r.snippet.unwrap_or_default();
                let bridge_content = format!("{} ({})", r.path, content);
                let memory_id = r.path.clone();
                let score = r.score as f64;
                collector.push_hit(HitEmit {
                    kind: "bm25_match",
                    path: r.path,
                    line: r.line_number,
                    score: Some(score),
                    content,
                    bridge_content,
                    bridge_relevance: score,
                    bridge_memory_id: memory_id,
                });
            }
        } else {
            println!(
                "\n{}",
                "Ranked Search Results (BM25):"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            for r in results {
                let line_info = if let Some(line) = r.line_number {
                    format!(
                        ":{}",
                        line.to_string()
                            .if_supports_color(Stream::Stdout, |s| s.yellow())
                    )
                } else {
                    String::new()
                };
                println!(
                    "{} [score: {:.2}]",
                    format!(
                        "{}{}",
                        r.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                        line_info
                    )
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
                    r.score.if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                if let Some(snippet) = r.snippet {
                    let ranges = r.highlight_ranges.as_deref().unwrap_or(&[]);
                    let display = emphasize_snippet(&snippet, ranges);
                    println!("  {}", display.trim());
                }
            }
            if truncated {
                print_search_truncation_affordance();
            }
            println!();
        }
    }

    Ok(())
}

/// Human-only truncation affordance (0100 DoD-7). No exact remaining count —
/// engines do not always return a total. Machine paths never call this.
/// Emit regex_match hits for hybrid identifier-literal fallback (same shape as regex path).
/// Empty-hybrid identifier fallback: escaped literal + index-bound all_paths.
/// Returns `Ok(Some(...))` only when ≥1 hit was produced (DoD-3 / `fallbackUsed`).
fn try_identifier_literal_fallback(
    filter: &RegexFilter<'_>,
    root: &Utf8Path,
    query: &str,
    limit: usize,
) -> Result<Option<RegexSearchResult>> {
    if !is_identifier_likely(query) {
        return Ok(None);
    }
    let escaped = regex::escape(query);
    let result = filter.search_with(root, &escaped, limit, RegexCandidateSource::AllPaths)?;
    if result.matches.is_empty() {
        Ok(None)
    } else {
        Ok(Some(result))
    }
}

fn emit_regex_style_hits(
    args: &SearchArgs,
    collector: &mut SearchCollector,
    matches: Vec<RegexMatch>,
    truncated: bool,
) {
    if args.is_machine() {
        for m in matches {
            let bridge_content = format!("{}:{}: {}", m.path, m.line_number, m.content);
            let memory_id = format!("{}::{}", m.path, m.line_number);
            collector.push_hit(HitEmit {
                kind: "regex_match",
                path: m.path,
                line: Some(m.line_number),
                score: Some(1.0),
                content: m.content,
                bridge_content,
                bridge_relevance: 1.0,
                bridge_memory_id: memory_id,
            });
        }
    } else {
        println!(
            "\n{}",
            "Hybrid Search Results (identifier literal fallback):"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
        );
        for m in matches {
            println!(
                "{}:{}: {}",
                m.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                m.line_number
                    .to_string()
                    .if_supports_color(Stream::Stdout, |s| s.yellow()),
                m.content.trim()
            );
        }
        if truncated {
            print_search_truncation_affordance();
        }
        println!();
    }
}

pub(crate) fn print_search_truncation_affordance() {
    println!("… and more results (use --limit N to see more)");
}

/// Apply bold emphasis to byte ranges in a plain snippet via owo_colors.
/// Ranges that are out of bounds or mid-character are skipped.
fn emphasize_snippet(fragment: &str, ranges: &[(usize, usize)]) -> String {
    if ranges.is_empty() {
        return fragment.to_string();
    }
    let mut sorted: Vec<(usize, usize)> = ranges
        .iter()
        .copied()
        .filter(|(s, e)| {
            *s <= *e
                && *e <= fragment.len()
                && fragment.is_char_boundary(*s)
                && fragment.is_char_boundary(*e)
        })
        .collect();
    sorted.sort_by_key(|(s, e)| (*s, *e));

    let mut out = String::new();
    let mut last = 0usize;
    for (start, end) in sorted {
        if start < last {
            continue;
        }
        out.push_str(&fragment[last..start]);
        let piece = &fragment[start..end];
        out.push_str(
            &piece
                .if_supports_color(Stream::Stdout, |s| s.bold())
                .to_string(),
        );
        last = end;
    }
    if last <= fragment.len() && fragment.is_char_boundary(last) {
        out.push_str(&fragment[last..]);
    }
    out
}

fn handle_fuzzy_fallback(
    engine: &TantivySearchEngine,
    args: &SearchArgs,
    collector: &mut SearchCollector,
) {
    if !args.is_machine() {
        println!(
            "{}",
            "Falling back to fuzzy search...".if_supports_color(Stream::Stdout, |s| s.yellow())
        );
    }
    // Overfetch limit+1 for human truncation affordance (0100 DoD-8; codex R1).
    let fuzzy_fetch = args.limit.saturating_add(1);
    let mut fuzzy_matches = match engine.search_fuzzy(&args.query, fuzzy_fetch) {
        Ok(matches) => matches,
        Err(e) => {
            tracing::warn!("Fuzzy search fallback failed: {}", e);
            Vec::new()
        }
    };

    if fuzzy_matches.is_empty() {
        if !args.is_machine() {
            println!("No matches found.");
            println!(
                "{} No exact symbols found. Try {} or {}.",
                "HINT:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
                "--regex"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold())),
                "ledgerful index"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
            );
            println!(
                "      Alternatively, try semantic search instead: {}",
                format!("ledgerful ask \"{}\"", args.query)
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
        }
        // Envelope: empty results still finish with resultCount 0 (caller finishes).
    } else {
        let truncated = fuzzy_matches.len() > args.limit;
        fuzzy_matches.truncate(args.limit);
        collector.set_truncated(truncated);
        if args.is_machine() {
            for m in fuzzy_matches {
                let content = m.snippet.as_deref().unwrap_or_default().to_string();
                let line = m.line_number;
                let bridge_content = format!("{}:{}: {}", m.path, line.unwrap_or(1), content);
                let memory_id = format!("{}::{}", m.path, line.unwrap_or(1));
                let score = m.score as f64;
                collector.push_hit(HitEmit {
                    kind: "fuzzy_match",
                    path: m.path,
                    line,
                    score: Some(score),
                    content,
                    bridge_content,
                    bridge_relevance: score,
                    bridge_memory_id: memory_id,
                });
            }
        } else {
            println!(
                "\n{}",
                "Fuzzy Search Results:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            for m in fuzzy_matches {
                let line_info = if let Some(line) = m.line_number {
                    format!(
                        ":{}",
                        line.to_string()
                            .if_supports_color(Stream::Stdout, |s| s.yellow())
                    )
                } else {
                    String::new()
                };
                println!(
                    "{} [score: {:.2}]",
                    format!(
                        "{}{}",
                        m.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                        line_info
                    )
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
                    m.score.if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                if let Some(snippet) = m.snippet {
                    let ranges = m.highlight_ranges.as_deref().unwrap_or(&[]);
                    let display = emphasize_snippet(&snippet, ranges);
                    println!("  {}", display.trim());
                }
            }
            if truncated {
                print_search_truncation_affordance();
            }
            println!();
        }
    }
}

#[cfg(test)]
mod identifier_fallback_tests {
    use super::*;
    use crate::search::trigram::extract_trigrams;
    use tantivy::schema::TantivyDocument;
    use tempfile::TempDir;

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

    /// Forces the empty-BM25 case: FTS content does not contain the identifier,
    /// but the live file does and the path is in the index so AllPaths fallback
    /// recovers it. Fails if `try_identifier_literal_fallback` is unwired.
    #[test]
    fn identifier_literal_fallback_recovers_when_fts_content_misses() {
        let dir = TempDir::new().expect("tempdir");
        let fts_dir = dir.path().join("fts");
        let engine = TantivySearchEngine::open_or_create(&fts_dir).expect("engine");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        let file_rel = "src/miss.rs";
        let live = "fn verify_step_key() { /* live */ }";
        let indexed = "fn totally_unrelated_helper() {}";
        std::fs::write(root.join(file_rel), live).expect("write live");
        index_doc(&engine, file_rel, indexed);

        let bm25 = engine
            .search("verify_step_key", 10)
            .expect("bm25")
            .is_empty();
        assert!(bm25, "precondition: BM25 empty on mismatched index content");

        let filter = RegexFilter::new(&engine);
        let root_utf8 = Utf8Path::from_path(&root).expect("utf8");
        let recovered = try_identifier_literal_fallback(&filter, root_utf8, "verify_step_key", 10)
            .expect("fallback ok")
            .expect("fallback must produce hits");
        assert!(
            !recovered.matches.is_empty(),
            "AllPaths literal fallback must find live identifier"
        );
        assert_eq!(recovered.matches[0].path, file_rel);
        assert!(
            recovered.matches[0].content.contains("verify_step_key"),
            "hit content must reflect live file"
        );

        assert!(
            try_identifier_literal_fallback(&filter, root_utf8, "hello world", 10)
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn is_identifier_likely_snake_case() {
        assert!(is_identifier_likely("verify_step_key"));
        assert!(!is_identifier_likely("hello world"));
        assert!(!is_identifier_likely(""));
    }
}
