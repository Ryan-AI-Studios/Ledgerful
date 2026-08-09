//! Pure completion-text resolution: strip thinking tags, extract final answers,
//! never promote raw chain-of-thought as product content (track 0159).

use std::fmt;

/// Allowlisted thinking-tag pairs (case-sensitive open/close as commonly emitted).
const TAG_PAIRS: &[(&str, &str)] = &[
    ("<think>", "</think>"),
    ("<thinking>", "</thinking>"),
    ("<thought>", "</thought>"),
    ("<reasoning>", "</reasoning>"),
    ("<|begin_of_thought|>", "<|end_of_thought|>"),
];

/// Line-anchored final-answer markers (case-insensitive match).
/// Longer forms first so `## Answer:` is preferred over `## Answer` at the same site.
const ANSWER_MARKERS: &[&str] = &["Final answer:", "Answer:", "## Answer:", "## Answer"];

/// Error from [`resolve_completion_text`].
///
/// Display text is greppable: **`reasoning only`** (with char count) or empty content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionTextError {
    /// Non-empty reasoning/thinking with no extractable product answer.
    ReasoningOnly { chars: usize },
    /// Both content and reasoning empty after strip/extract.
    Empty,
}

impl fmt::Display for CompletionTextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompletionTextError::ReasoningOnly { chars } => {
                write!(f, "reasoning only: {chars} chars")
            }
            CompletionTextError::Empty => write!(f, "empty content"),
        }
    }
}

impl std::error::Error for CompletionTextError {}

/// Resolve user-facing completion text from `content` + optional `reasoning`
/// (OpenAI `reasoning` / `reasoning_content`, or Ollama `thinking` mapped at call site).
///
/// Resolution order:
/// 1. Strip think tags from `content` (M1/M2); non-empty remainder → Ok
/// 2. Else if `reasoning` present: strip / marker-extract; extractable → Ok
/// 3. Else if both empty after strip → Err empty
/// 4. Pure reasoning-only → Err reasoning only
pub fn resolve_completion_text(
    content: &str,
    reasoning: Option<&str>,
) -> Result<String, CompletionTextError> {
    let (content_stripped, tags_found) = strip_thinking_tags(content);

    if tags_found {
        // After strip, trim is OK (tags often leave surrounding whitespace).
        let content_rem = content_stripped.trim();
        if !content_rem.is_empty() {
            return Ok(content_rem.to_string());
        }
        // Empty remainder after strip → fall through to reasoning / errors.
    } else if !content.trim().is_empty() {
        // DoD-3: non-empty plain content (no tags) unchanged — do not trim Ok value.
        return Ok(content.to_string());
    }
    // No tags + whitespace-only/empty, or tags + empty remainder → reasoning path.

    if let Some(r) = reasoning.filter(|s| !s.is_empty()) {
        if let Some(answer) = extract_from_reasoning(r) {
            return Ok(answer);
        }
        return Err(CompletionTextError::ReasoningOnly {
            chars: r.chars().count(),
        });
    }

    // Only thinking tags / unclosed think and no extractable answer → reasoning only.
    // Whitespace-only content (no tags) is Empty, not ReasoningOnly.
    if tags_found {
        return Err(CompletionTextError::ReasoningOnly {
            chars: content.chars().count(),
        });
    }

    Err(CompletionTextError::Empty)
}

/// Map a helper error to the existing endpoint-labeled parse error string.
pub fn format_completion_text_error(label: &str, err: CompletionTextError) -> String {
    match err {
        CompletionTextError::ReasoningOnly { chars } => {
            format!("{label} returned empty content (reasoning only: {chars} chars)")
        }
        CompletionTextError::Empty => {
            format!("{label} returned empty message content")
        }
    }
}

/// M1: left-to-right non-greedy multi-block strip.
/// M2: unclosed open tag → discard from open through EOF.
///
/// Returns `(stripped_text, any_tag_open_found)`.
fn strip_thinking_tags(input: &str) -> (String, bool) {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;
    let mut tags_found = false;

    loop {
        let mut earliest: Option<(usize, &'static str, &'static str)> = None;
        for &(open, close) in TAG_PAIRS {
            if let Some(pos) = rest.find(open)
                && earliest.is_none_or(|(ep, _, _)| pos < ep)
            {
                earliest = Some((pos, open, close));
            }
        }

        let Some((pos, open, close)) = earliest else {
            result.push_str(rest);
            break;
        };

        tags_found = true;
        result.push_str(&rest[..pos]);
        let after_open = &rest[pos + open.len()..];
        if let Some(close_rel) = after_open.find(close) {
            // Non-greedy: first close after this open; continue after close.
            rest = &after_open[close_rel + close.len()..];
        } else {
            // M2: no matching close — discard open through EOF.
            break;
        }
    }

    (result, tags_found)
}

/// Extract product answer from a reasoning/thinking field.
///
/// - If think tags were present and strip left non-empty text → that remainder.
/// - Else last line-anchored marker (M3).
/// - Pure CoT with neither → None (caller fails closed).
fn extract_from_reasoning(reasoning: &str) -> Option<String> {
    let (stripped, tags_found) = strip_thinking_tags(reasoning);

    if tags_found {
        let rem = stripped.trim();
        if !rem.is_empty() {
            return Some(rem.to_string());
        }
        // Tags swallowed everything; markers only on post-strip (empty) → none.
        return extract_marker_answer(&stripped);
    }

    // No tags: do not promote full CoT; only marker extract.
    extract_marker_answer(reasoning)
}

/// M3: last line-anchored `Final answer:` / `Answer:` / `## Answer` / `## Answer:`.
/// Line-anchored = start-of-string or after `\n`. Remainder after marker must be non-empty.
fn extract_marker_answer(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut best: Option<(usize, usize)> = None; // (marker_start, marker_len)

    for marker in ANSWER_MARKERS {
        let marker_lower = marker.to_ascii_lowercase();
        let mut search_from = 0;
        while search_from <= lower.len() {
            let Some(rel) = lower[search_from..].find(&marker_lower) else {
                break;
            };
            let pos = search_from + rel;
            let line_anchored = pos == 0 || text.as_bytes().get(pos - 1) == Some(&b'\n');
            if line_anchored {
                let marker_len = marker.len();
                let replace = match best {
                    None => true,
                    Some((best_start, best_len)) => {
                        // Prefer last occurrence; at same start prefer longer marker.
                        pos > best_start || (pos == best_start && marker_len > best_len)
                    }
                };
                if replace {
                    best = Some((pos, marker_len));
                }
            }
            search_from = pos + 1;
        }
    }

    let (start, len) = best?;
    let remainder = text[start + len..].trim();
    if remainder.is_empty() {
        return None;
    }
    Some(remainder.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_closed_think_keeps_trailing() {
        let out = resolve_completion_text("<think>abc</think> Hello", None).unwrap();
        assert_eq!(out, "Hello");
    }

    #[test]
    fn multi_block_m1_preserves_inter_block_text() {
        let out = resolve_completion_text(
            "<thought>t1</thought> Part 1 <thought>t2</thought> Part 2",
            None,
        )
        .unwrap();
        assert!(
            out.contains("Part 1") && out.contains("Part 2"),
            "expected both parts, got: {out:?}"
        );
    }

    #[test]
    fn unclosed_m2_does_not_ok_cot() {
        let err = resolve_completion_text("<think>Unfinished thoughts", None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("reasoning only") || msg.contains("empty"),
            "expected reasoning only or empty, got: {msg}"
        );
        assert!(matches!(
            err,
            CompletionTextError::ReasoningOnly { .. } | CompletionTextError::Empty
        ));
    }

    #[test]
    fn empty_content_plain_reasoning_is_reasoning_only() {
        let err = resolve_completion_text("", Some("Just thoughts")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("reasoning only"),
            "expected greppable reasoning only, got: {msg}"
        );
        assert_eq!(
            err,
            CompletionTextError::ReasoningOnly {
                chars: "Just thoughts".chars().count()
            }
        );
    }

    #[test]
    fn plain_content_preserves_surrounding_whitespace() {
        // DoD-3: no tags → Ok value is content unchanged (no trim).
        let out = resolve_completion_text("  answer  ", None).unwrap();
        assert_eq!(out, "  answer  ");
    }

    #[test]
    fn whitespace_only_content_is_empty() {
        let err = resolve_completion_text("   ", None).unwrap_err();
        assert_eq!(err, CompletionTextError::Empty);
    }

    #[test]
    fn newline_only_content_is_empty() {
        let err = resolve_completion_text("\n", None).unwrap_err();
        assert_eq!(err, CompletionTextError::Empty);
    }

    #[test]
    fn tags_present_trims_stripped_remainder() {
        // Tags found → trim after strip is OK.
        let out = resolve_completion_text("<think>x</think>  hi  ", None).unwrap();
        assert_eq!(out, "hi");
    }

    #[test]
    fn reasoning_only_chars_is_unicode_scalar_count() {
        // "思考" is 2 Unicode scalars, 6 UTF-8 bytes — count must be chars not bytes.
        let err = resolve_completion_text("", Some("思考")).unwrap_err();
        assert_eq!(err, CompletionTextError::ReasoningOnly { chars: 2 });
        assert_eq!(err.to_string(), "reasoning only: 2 chars");
    }

    #[test]
    fn marker_m3_final_answer_line() {
        let out = resolve_completion_text("", Some("Step 1...\nFinal answer: Done")).unwrap();
        assert_eq!(out, "Done");
    }

    #[test]
    fn marker_m3_last_line_anchored_wins() {
        // Mid-monologue first "Answer:" must not win over a later line-anchored marker.
        let reasoning = "I considered answer: it might be wrong\n\
             Answer: intermediate guess\n\
             Final answer: correct result";
        let out = resolve_completion_text("", Some(reasoning)).unwrap();
        assert_eq!(out, "correct result");
    }

    #[test]
    fn plain_content_wins_over_reasoning() {
        let out = resolve_completion_text("The real answer", Some("secret CoT monologue")).unwrap();
        assert_eq!(out, "The real answer");
    }

    #[test]
    fn unclosed_think_keeps_text_before_open() {
        let out = resolve_completion_text("Keep me <think>Unfinished thoughts", None).unwrap();
        assert_eq!(out, "Keep me");
    }

    #[test]
    fn reasoning_closed_tags_trailing_answer() {
        let out = resolve_completion_text(
            "",
            Some("<think>long chain of thought</think>\nThe capital is Paris."),
        )
        .unwrap();
        assert_eq!(out, "The capital is Paris.");
    }

    #[test]
    fn both_empty_is_empty_error() {
        let err = resolve_completion_text("", None).unwrap_err();
        assert_eq!(err, CompletionTextError::Empty);
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn empty_content_empty_reasoning_is_empty() {
        let err = resolve_completion_text("", Some("")).unwrap_err();
        assert_eq!(err, CompletionTextError::Empty);
    }

    #[test]
    fn closed_think_only_content_is_reasoning_only() {
        let err = resolve_completion_text("<think>only thoughts</think>", None).unwrap_err();
        assert!(err.to_string().contains("reasoning only"));
    }

    #[test]
    fn thinking_and_begin_of_thought_tags() {
        let out = resolve_completion_text(
            "<thinking>x</thinking> mid <|begin_of_thought|>y<|end_of_thought|> end",
            None,
        )
        .unwrap();
        assert!(out.contains("mid"));
        assert!(out.contains("end"));
        assert!(!out.contains("x"));
        assert!(!out.contains("y"));
    }

    #[test]
    fn marker_not_line_anchored_does_not_match() {
        // Mid-line "answer:" is not a marker; bare CoT fails closed.
        let err =
            resolve_completion_text("", Some("I think the answer: forty-two maybe")).unwrap_err();
        assert!(err.to_string().contains("reasoning only"));
    }

    #[test]
    fn marker_m3_hash_answer_colon_line() {
        let out =
            resolve_completion_text("", Some("long monologue...\n## Answer:\n42 is the answer"))
                .unwrap();
        assert_eq!(out, "42 is the answer");
    }

    #[test]
    fn marker_m3_hash_answer_without_colon_line() {
        let out =
            resolve_completion_text("", Some("scratchpad\n## Answer\nplain hash form")).unwrap();
        assert_eq!(out, "plain hash form");
    }

    #[test]
    fn marker_m3_hash_answer_last_wins_over_answer() {
        // Earlier line-anchored Answer: must lose to later ## Answer: (last-wins).
        let reasoning = "Answer: intermediate\n## Answer:\nfinal hash form";
        let out = resolve_completion_text("", Some(reasoning)).unwrap();
        assert_eq!(out, "final hash form");
    }

    #[test]
    fn format_error_preserves_greppable_strings() {
        let s =
            format_completion_text_error("local", CompletionTextError::ReasoningOnly { chars: 42 });
        assert!(s.contains("reasoning only"));
        assert!(s.contains("42"));
        assert!(s.contains("local"));
        let s2 = format_completion_text_error("cloud", CompletionTextError::Empty);
        assert!(s2.contains("empty message content"));
        assert!(s2.contains("cloud"));
    }
}
