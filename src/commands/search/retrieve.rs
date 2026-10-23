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

/// Explicit `--regex` truncation: result over-limit, or the candidate cap
/// fired and at least one match was returned.
pub(crate) fn regex_cli_marks_truncated(
    over_limit: bool,
    candidates_truncated: bool,
    had_hits: bool,
) -> bool {
    over_limit || (candidates_truncated && had_hits)
}

pub fn is_identifier_likely(query: &str) -> bool {
    !query.is_empty()
        && !query.contains(' ')
        && query
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
}

/// Whitespace tokens for FTS interleave (E017 `"configuration provenance"`).
pub(crate) fn tokenize_search_query(query: &str) -> Vec<String> {
    let mut tokens: Vec<String> = query
        .split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .to_string()
        })
        .filter(|t| t.len() >= 2)
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

/// Semantic slice that will remain if one reserved lexical seat is taken.
/// Overfetch / the evicted last displayed hit are **not** in this set, so they
/// stay eligible for the reserved BM25 seat.
pub(crate) fn displayed_semantic_for_reserved<T>(results: &[T], limit: usize) -> &[T] {
    let cap = limit.saturating_sub(1);
    &results[..results.len().min(cap)]
}

/// Lexical (path, score) not already in `semantic_paths`, highest score first,
/// stable path tie-break, capped at `remaining`.
pub(crate) fn lexical_paths_to_interleave(
    semantic_paths: &[String],
    lexical: Vec<(String, f32)>,
    remaining: usize,
) -> Vec<(String, f32)> {
    let mut seen: std::collections::HashSet<String> = semantic_paths
        .iter()
        .map(|p| p.replace('\\', "/"))
        .collect();
    let mut extra: Vec<(String, f32)> = lexical
        .into_iter()
        .map(|(p, s)| (p.replace('\\', "/"), s))
        .filter(|(p, _)| seen.insert(p.clone()))
        .collect();
    extra.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    extra.truncate(remaining);
    extra
}

/// Token FTS hits whose paths are not already in `semantic` results.
pub(crate) fn collect_token_lexical_hits(
    layout: &crate::state::layout::Layout,
    query: &str,
    semantic: &[(String, String, usize, f32)],
    remaining: usize,
) -> Vec<(String, f32, String, Option<usize>)> {
    if remaining == 0 {
        return Vec::new();
    }
    let Ok(engine) =
        crate::search::TantivySearchEngine::open_or_create(layout.search_index_dir().as_std_path())
    else {
        return Vec::new();
    };
    let mut lexical: Vec<(String, f32, String, Option<usize>)> = Vec::new();
    let mut queries = vec![query.to_string()];
    queries.extend(tokenize_search_query(query));
    for q in queries {
        let Ok(hits) = engine.search(&q, remaining.saturating_add(4)) else {
            continue;
        };
        for hit in hits {
            lexical.push((
                hit.path,
                hit.score,
                hit.snippet.unwrap_or_default(),
                hit.line_number,
            ));
        }
    }
    let semantic_paths: Vec<String> = semantic.iter().map(|(p, _, _, _)| p.clone()).collect();
    let scored: Vec<(String, f32)> = lexical.iter().map(|(p, s, _, _)| (p.clone(), *s)).collect();
    let chosen = lexical_paths_to_interleave(&semantic_paths, scored, remaining);
    let mut by_path: std::collections::BTreeMap<String, (f32, String, Option<usize>)> =
        std::collections::BTreeMap::new();
    for (path, score, snippet, line) in lexical {
        let path = path.replace('\\', "/");
        by_path
            .entry(path)
            .and_modify(|e| {
                if score > e.0 {
                    *e = (score, snippet.clone(), line);
                }
            })
            .or_insert((score, snippet, line));
    }
    chosen
        .into_iter()
        .filter_map(|(path, _)| {
            by_path
                .remove(&path)
                .map(|(score, snippet, line)| (path, score, snippet, line))
        })
        .collect()
}

/// One ordinary or seated hybrid hit. Score ties use path, then line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HybridRow {
    pub path: String,
    pub line_number: Option<usize>,
    /// Plain fragment for JSON and for emphasis application at print time.
    pub content: String,
    /// Byte ranges into `content` for gated owo_colors emphasis (human only).
    pub highlight_ranges: Vec<(usize, usize)>,
    pub score: Option<f32>,
    pub is_regex: bool,
}

/// Existing +5 / +15 regex boost. `.d.ts` is 15 because it ends with `.ts`.
/// `.java` stays 5. The extension set is not widened.
pub(crate) fn code_extension_boost(path: &str) -> f32 {
    let path = path.replace('\\', "/");
    if path.ends_with(".rs")
        || path.ends_with(".ts")
        || path.ends_with(".js")
        || path.ends_with(".go")
        || path.ends_with(".py")
    {
        15.0
    } else {
        5.0
    }
}

/// Declaration inserted at index 0 when the ordinary merge has no matching row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WitnessSeat {
    pub path: String,
    pub line: usize,
    pub content: String,
}

/// What an empty seated hybrid list does. A seated witness is not empty, so it
/// emits and does not call the identifier fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HybridEmptyAction {
    Emit,
    IdentifierFallback,
    Fuzzy,
}

pub(crate) fn hybrid_empty_action(
    seated_empty: bool,
    identifier_likely: bool,
) -> HybridEmptyAction {
    if !seated_empty {
        HybridEmptyAction::Emit
    } else if identifier_likely {
        HybridEmptyAction::IdentifierFallback
    } else {
        HybridEmptyAction::Fuzzy
    }
}

fn norm_search_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn line_sort_key(line: Option<usize>) -> (bool, usize) {
    match line {
        Some(number) => (false, number),
        None => (true, 0),
    }
}

pub(crate) fn sort_hybrid_rows(rows: &mut [HybridRow]) {
    rows.sort_by(|left, right| {
        let score_left = left.score.unwrap_or(0.0);
        let score_right = right.score.unwrap_or(0.0);
        score_right.total_cmp(&score_left).then_with(|| {
            norm_search_path(&left.path)
                .cmp(&norm_search_path(&right.path))
                .then_with(|| {
                    line_sort_key(left.line_number).cmp(&line_sort_key(right.line_number))
                })
        })
    });
}

/// Sort, move or insert the witness at index 0, then truncate.
/// `truncated` is decided from the seated length (and the existing regex
/// candidate-cap signal) before truncation.
pub(crate) fn seat_definition_witness(
    mut rows: Vec<HybridRow>,
    witness: Option<&WitnessSeat>,
    limit: usize,
    regex_candidates_truncated: bool,
    regex_had_hits: bool,
) -> (Vec<HybridRow>, bool) {
    sort_hybrid_rows(&mut rows);
    if let Some(witness) = witness {
        let key_path = norm_search_path(&witness.path);
        if let Some(idx) = rows.iter().position(|row| {
            norm_search_path(&row.path) == key_path && row.line_number == Some(witness.line)
        }) {
            let row = rows.remove(idx);
            rows.insert(0, row);
        } else {
            rows.insert(
                0,
                HybridRow {
                    path: key_path.clone(),
                    line_number: Some(witness.line),
                    content: witness.content.clone(),
                    highlight_ranges: Vec::new(),
                    score: Some(code_extension_boost(&key_path)),
                    is_regex: true,
                },
            );
        }
    }
    let truncated = rows.len() > limit || (regex_candidates_truncated && regex_had_hits);
    rows.truncate(limit);
    (rows, truncated)
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

        let mut merged: std::collections::HashMap<(String, Option<usize>), HybridRow> =
            std::collections::HashMap::new();

        for r in bm25_results {
            // Seed from plain fragment (not pre-rendered highlighted). Emphasis
            // is applied only on the human path; machine content stays plain (DoD-5).
            let content = r.snippet.clone().unwrap_or_default();
            let highlight_ranges = r.highlight_ranges.unwrap_or_default();
            merged.insert(
                (r.path.clone(), r.line_number),
                HybridRow {
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
            let boost = code_extension_boost(&m.path);
            if let Some(existing) = merged.get_mut(&key) {
                // Boost existing BM25 score
                let current_score = existing.score.unwrap_or(0.0);
                existing.score = Some(current_score + boost);
                existing.is_regex = true;
            } else {
                merged.insert(
                    key,
                    HybridRow {
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

        let merged_results: Vec<HybridRow> = merged.into_values().collect();
        let witness = if is_identifier_likely(&args.query) {
            filter
                .definition_witness(root, &args.query)
                .ok()
                .flatten()
                .map(|hit| WitnessSeat {
                    path: hit.path,
                    line: hit.line,
                    content: hit.content,
                })
        } else {
            None
        };
        let (merged_results, truncated) = seat_definition_witness(
            merged_results,
            witness.as_ref(),
            args.limit,
            regex_candidates_truncated,
            regex_had_hits,
        );
        collector.set_truncated(truncated);

        match hybrid_empty_action(merged_results.is_empty(), is_identifier_likely(&args.query)) {
            HybridEmptyAction::IdentifierFallback => {
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
            }
            HybridEmptyAction::Fuzzy => {
                handle_fuzzy_fallback(&engine, args, collector);
            }
            HybridEmptyAction::Emit => {
                if args.is_machine() {
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
                            engine: None,
                            preview_unavailable_reason: None,
                        });
                    }
                } else {
                    println!(
                        "\n{}",
                        "Hybrid Search Results (BM25 + Regex):"
                            .if_supports_color(Stream::Stdout, |s| s
                                .style(Style::new().bold().cyan()))
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
            }
        }
    } else if use_regex {
        if !args.is_machine() {
            println!("[Search Mode: Regex]");
        }
        let filter = RegexFilter::new(&engine);
        let result =
            filter.search_with(root, &args.query, overfetch, RegexCandidateSource::Auto)?;
        let over_limit = result.matches.len() > args.limit;
        let had_hits = !result.matches.is_empty();
        let truncated =
            regex_cli_marks_truncated(over_limit, result.candidates_truncated, had_hits);
        let mut matches = result.matches;
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
                    engine: None,
                    preview_unavailable_reason: None,
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
                    engine: None,
                    preview_unavailable_reason: None,
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
                engine: None,
                preview_unavailable_reason: None,
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
                    engine: None,
                    preview_unavailable_reason: None,
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
mod regex_cli_truncated_tests {
    use super::regex_cli_marks_truncated;

    #[test]
    fn regex_cli_marks_truncated_four_corners() {
        assert!(regex_cli_marks_truncated(false, true, true));
        assert!(!regex_cli_marks_truncated(false, true, false));
        assert!(regex_cli_marks_truncated(true, false, true));
        assert!(!regex_cli_marks_truncated(false, false, true));
    }
}

#[cfg(test)]
mod tokenize_interleave_tests {
    use super::{lexical_paths_to_interleave, tokenize_search_query};

    #[test]
    fn tokenize_search_query_splits_configuration_provenance() {
        let tokens = tokenize_search_query("configuration provenance");
        assert!(tokens.contains(&"configuration".to_string()));
        assert!(tokens.contains(&"provenance".to_string()));
        assert!(!is_identifier_likely_for_query());
    }

    fn is_identifier_likely_for_query() -> bool {
        super::is_identifier_likely("configuration provenance")
    }

    #[test]
    fn lexical_paths_to_interleave_skips_semantic_and_caps() {
        let semantic = vec!["src/ledger.rs".to_string()];
        let lexical = vec![
            ("src/ledger.rs".to_string(), 9.0),
            ("src/commands/config_verify.rs".to_string(), 4.0),
            ("src/other.rs".to_string(), 5.0),
        ];
        let extra = lexical_paths_to_interleave(&semantic, lexical, 1);
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "src/other.rs");
    }

    #[test]
    #[allow(non_snake_case)]
    fn collect_token_lexical_hits__overfetch_or_evicted_path__eligible_for_reserved_slot() {
        use super::displayed_semantic_for_reserved;

        let results = vec![
            ("src/a.rs".to_string(), "a".to_string(), 0usize, 0.1_f32),
            ("src/b.rs".to_string(), "b".to_string(), 0, 0.2),
            ("src/c.rs".to_string(), "c".to_string(), 0, 0.3),
            ("src/d.rs".to_string(), "d".to_string(), 0, 0.4),
        ];
        let limit = 3;
        let displayed = displayed_semantic_for_reserved(&results, limit);
        assert_eq!(displayed.len(), 2, "limit-1 displayed when reserving one");
        let displayed_paths: Vec<String> = displayed.iter().map(|(p, _, _, _)| p.clone()).collect();
        assert_eq!(displayed_paths, vec!["src/a.rs", "src/b.rs"]);
        let lexical = vec![
            ("src/c.rs".to_string(), 8.0),
            ("src/d.rs".to_string(), 7.0),
            ("src/a.rs".to_string(), 9.0),
        ];
        let extra = lexical_paths_to_interleave(&displayed_paths, lexical, 1);
        assert_eq!(extra.len(), 1);
        assert!(
            extra[0].0 == "src/c.rs" || extra[0].0 == "src/d.rs",
            "evicted/overfetch path must be eligible, got {}",
            extra[0].0
        );
        assert_ne!(extra[0].0, "src/a.rs", "no duplicate of displayed semantic");
    }

    #[test]
    fn lexical_paths_to_interleave_includes_config_verify() {
        let semantic = vec!["src/ledger/db.rs".to_string()];
        let lexical = vec![
            ("src/commands/config_verify.rs".to_string(), 8.0),
            ("src/ledger/db.rs".to_string(), 12.0),
        ];
        let extra = lexical_paths_to_interleave(&semantic, lexical, 2);
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "src/commands/config_verify.rs");
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

#[cfg(test)]
mod definition_seat_tests {
    use super::{
        HybridEmptyAction, HybridRow, WitnessSeat, code_extension_boost, hybrid_empty_action,
        seat_definition_witness, sort_hybrid_rows,
    };

    fn row(path: &str, line: Option<usize>, score: f32, is_regex: bool) -> HybridRow {
        HybridRow {
            path: path.to_string(),
            line_number: line,
            content: path.to_string(),
            highlight_ranges: Vec::new(),
            score: Some(score),
            is_regex,
        }
    }

    #[test]
    fn code_extension_boost_js_is_15_java_is_5_dts_is_15() {
        assert_eq!(code_extension_boost("pkg/widget.js"), 15.0);
        assert_eq!(code_extension_boost("types/widget.d.ts"), 15.0);
        assert_eq!(code_extension_boost("src/Widget.java"), 5.0);
        assert_eq!(code_extension_boost("pkg/widget.jsx"), 5.0);
    }

    #[test]
    fn seat_inserted_js_is_regex_match_score_15() {
        let (rows, truncated) = seat_definition_witness(
            vec![row("tests/other.js", Some(2), 40.0, false)],
            Some(&WitnessSeat {
                path: "pkg/widget.js".to_string(),
                line: 4,
                content: "widget_open(source) {".to_string(),
            }),
            5,
            false,
            false,
        );
        assert!(!truncated);
        assert!(rows[0].is_regex);
        assert_eq!(rows[0].score, Some(15.0));
        assert_eq!(rows[0].line_number, Some(4));
        assert_eq!(rows[0].content, "widget_open(source) {");
        assert_eq!(rows[1].path, "tests/other.js");
    }

    #[test]
    fn seat_inserted_java_is_regex_match_score_5() {
        let (rows, _) = seat_definition_witness(
            Vec::new(),
            Some(&WitnessSeat {
                path: "src/Widget.java".to_string(),
                line: 3,
                content: "void widget_open() {".to_string(),
            }),
            5,
            false,
            false,
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_regex);
        assert_eq!(rows[0].score, Some(5.0));
        assert_eq!(
            hybrid_empty_action(false, true),
            HybridEmptyAction::Emit,
            "a seated witness does not take the identifier fallback"
        );
    }

    #[test]
    fn seat_moved_row_keeps_kind_and_score() {
        let (rows, _) = seat_definition_witness(
            vec![row("pkg/widget.js", Some(4), 5.0, false)],
            Some(&WitnessSeat {
                path: "pkg/widget.js".to_string(),
                line: 4,
                content: "widget_open(source) {".to_string(),
            }),
            5,
            false,
            false,
        );
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].is_regex, "moved row stays bm25_match");
        assert_eq!(rows[0].score, Some(5.0));
        assert_eq!(rows[0].content, "pkg/widget.js");
    }

    #[test]
    fn equal_scores_sort_path_then_line_missing_last() {
        let mut rows = vec![
            row("b.rs", Some(1), 10.0, false),
            row("a.rs", None, 10.0, false),
            row("a.rs", Some(5), 10.0, false),
            row("a.rs", Some(2), 10.0, true),
            row("c.rs", Some(1), 11.0, false),
        ];
        sort_hybrid_rows(&mut rows);
        let order: Vec<_> = rows
            .iter()
            .map(|row| (row.path.as_str(), row.line_number))
            .collect();
        assert_eq!(
            order,
            vec![
                ("c.rs", Some(1)),
                ("a.rs", Some(2)),
                ("a.rs", Some(5)),
                ("a.rs", None),
                ("b.rs", Some(1)),
            ]
        );
    }

    #[test]
    fn seat_at_limit_sets_truncated_and_drops_last_ordinary_row() {
        let ordinary = vec![
            row("a.js", Some(1), 3.0, false),
            row("b.js", Some(1), 2.0, false),
            row("c.js", Some(1), 1.0, false),
        ];
        let (rows, truncated) = seat_definition_witness(
            ordinary,
            Some(&WitnessSeat {
                path: "pkg/widget.js".to_string(),
                line: 8,
                content: "widget_open(source) {".to_string(),
            }),
            3,
            false,
            false,
        );
        assert!(truncated);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].path, "pkg/widget.js");
        assert!(rows[0].is_regex);
        assert!(rows.iter().all(|row| row.path != "c.js"));
        assert_eq!(rows[1].path, "a.js");
        assert_eq!(rows[2].path, "b.js");
    }

    #[test]
    fn seat_keeps_second_row_for_the_same_path() {
        let (rows, _) = seat_definition_witness(
            vec![
                row("pkg/widget.js", Some(1), 20.0, false),
                row("pkg/widget.js", Some(9), 8.0, true),
            ],
            Some(&WitnessSeat {
                path: "pkg/widget.js".to_string(),
                line: 4,
                content: "widget_open(source) {".to_string(),
            }),
            10,
            false,
            false,
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].line_number, Some(4));
        assert!(rows.iter().any(|row| row.line_number == Some(1)));
        assert!(rows.iter().any(|row| row.line_number == Some(9)));
    }

    #[test]
    fn empty_merge_branches_stay_distinct() {
        assert_eq!(
            hybrid_empty_action(true, true),
            HybridEmptyAction::IdentifierFallback
        );
        assert_eq!(hybrid_empty_action(true, false), HybridEmptyAction::Fuzzy);
        assert_eq!(hybrid_empty_action(false, true), HybridEmptyAction::Emit);
        assert_eq!(hybrid_empty_action(false, false), HybridEmptyAction::Emit);
    }
}
