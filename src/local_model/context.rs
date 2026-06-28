use crate::gemini::modes::{GeminiMode, build_system_prompt};
use crate::local_model::client::ChatMessage;
use crate::local_model::pruner::RankedChunk;

/// Budget allocation constants:
/// 85% usable for context (system prompt + user query + chunks)
/// 10% reserved for model response generation
/// 5% safety headroom
const USABLE_FRACTION: f64 = 0.85;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveMode {
    ChangesFocus,
    CodebaseFocus,
}

/// Assemble a conversation context for the local model.
///
/// Budget allocation priority:
/// 1. System prompt (fixed, high priority)
/// 2. User message + pruned impact packet (high priority in ChangesFocus)
/// 3. Top-ranked chunks sorted by relevance score until budget exhausted
///
/// In CodebaseFocus mode, chunks are prioritized to fill up to 90% of the budget.
pub fn assemble_context(
    system_prompt: &str,
    user_message: &str,
    relevant_chunks: &[RankedChunk],
    max_context_tokens: usize,
    mode: AdaptiveMode,
) -> Vec<ChatMessage> {
    // 85% of total context window is usable for prompt + context
    let usable_tokens = (max_context_tokens as f64 * USABLE_FRACTION) as usize;
    let char_budget = usable_tokens * 4;

    let mut messages = Vec::new();

    let (final_system_prompt, final_user_message, chunk_budget) = match mode {
        AdaptiveMode::ChangesFocus => {
            let sys_prompt = if system_prompt.len() > char_budget {
                tracing::warn!("System prompt exceeds token budget, truncating");
                system_prompt.chars().take(char_budget).collect::<String>()
            } else {
                system_prompt.to_string()
            };

            let remaining = char_budget.saturating_sub(sys_prompt.len() + user_message.len());
            (sys_prompt, user_message.to_string(), remaining)
        }
        AdaptiveMode::CodebaseFocus => {
            let oracle_prompt = format!(
                "{}\n\nCODEBASE ORACLE MODE: You are an expert on this codebase. Answer the user's question primarily using the provided code snippets. If the provided snippets do not contain the answer, you may use your general knowledge, but you MUST state that you are answering without codebase context. When using snippets, citations are MANDATORY: use [source] format.",
                system_prompt
            );

            let sys_prompt = if oracle_prompt.len() > char_budget {
                oracle_prompt.chars().take(char_budget).collect::<String>()
            } else {
                oracle_prompt
            };

            // In CodebaseFocus, chunks can take up to 90% of the budget.
            // We prioritize chunks over the user message if necessary.
            let chunk_budget = (char_budget as f64 * 0.9) as usize;
            let user_msg_budget = char_budget.saturating_sub(sys_prompt.len() + chunk_budget);

            let trimmed_user_message = if user_message.len() > user_msg_budget {
                user_message
                    .chars()
                    .take(user_msg_budget)
                    .collect::<String>()
            } else {
                user_message.to_string()
            };

            (sys_prompt, trimmed_user_message, chunk_budget)
        }
    };

    messages.push(ChatMessage {
        role: "system".to_string(),
        content: final_system_prompt,
    });

    // Sort chunks by score descending (highest relevance first)
    let mut sorted: Vec<&RankedChunk> = relevant_chunks.iter().collect();
    sorted.sort_unstable_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut included = 0usize;
    let mut used: usize = 0;
    for chunk in &sorted {
        if used + chunk.content.len() + chunk.source.len() + 4 <= chunk_budget {
            used += chunk.content.len() + chunk.source.len() + 4;
            included += 1;
        } else {
            break;
        }
    }

    let trimmed = sorted.len().saturating_sub(included);
    if trimmed > 0 {
        tracing::warn!(
            "Context assembly trimmed {trimmed} context chunk(s) to fit token budget (usable budget: {char_budget} chars, {usable_tokens} tokens)"
        );
    }

    for chunk in sorted.iter().take(included) {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: format!("[{}] {}", chunk.source, chunk.content),
        });
    }

    messages.push(ChatMessage {
        role: "user".to_string(),
        content: final_user_message,
    });

    messages
}

pub fn get_system_prompt(mode: &str) -> String {
    let gemini_mode = match mode {
        "analyze" => GeminiMode::Analyze,
        "suggest" => GeminiMode::Suggest,
        "review" => GeminiMode::ReviewPatch,
        _ => {
            tracing::warn!("Unknown mode '{mode}', defaulting to analyze");
            GeminiMode::Analyze
        }
    };
    build_system_prompt(gemini_mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(content: &str, source: &str, score: f32) -> RankedChunk {
        RankedChunk {
            content: content.to_string(),
            source: source.to_string(),
            score,
        }
    }

    #[test]
    fn assemble_with_chunks_sorted_by_relevance() {
        let chunks = vec![
            make_chunk("Low relevance chunk here", "low.md", 0.4),
            make_chunk("High relevance chunk here", "high.md", 0.95),
            make_chunk("Medium relevance chunk", "medium.md", 0.6),
        ];

        let messages = assemble_context(
            "You are helpful.",
            "What is Rust?",
            &chunks,
            1000,
            AdaptiveMode::ChangesFocus,
        );

        // Should have at least: system + 3 chunks + user = 5 messages
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[0].content, "You are helpful.");

        // First chunk should be highest score (0.95)
        assert!(messages[1].content.contains("high.md"));
        assert!(messages[1].content.contains("High relevance"));
        // Second should be medium (0.6)
        assert!(messages[2].content.contains("medium.md"));
        // Third should be low (0.4)
        assert!(messages[3].content.contains("low.md"));
    }

    #[test]
    fn assemble_correct_order() {
        let chunks = vec![
            make_chunk("Context A", "a.md", 0.9),
            make_chunk("Context B", "b.md", 0.8),
        ];
        let messages = assemble_context(
            "You are helpful.",
            "What is Rust?",
            &chunks,
            1000,
            AdaptiveMode::ChangesFocus,
        );
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[0].content, "You are helpful.");
        assert_eq!(messages[1].role, "system");
        assert!(messages[1].content.contains("Context A"));
        assert_eq!(messages[2].role, "system");
        assert!(messages[2].content.contains("Context B"));
        assert_eq!(messages[3].role, "user");
        assert_eq!(messages[3].content, "What is Rust?");
    }

    #[test]
    fn assemble_budget_trimming() {
        // Budget = 10 tokens -> usable = 8 tokens (85%) -> 32 chars
        // system: 10 chars, user: 10 chars, chunks: 30 + 30 chars each
        // Remaining for chunks: 32 - 20 = 12 chars -> no chunks fit
        let chunks = vec![
            make_chunk("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "a.md", 0.9),
            make_chunk("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", "b.md", 0.8),
        ];

        let messages = assemble_context(
            "0123456789", // 10 chars
            "user query", // 10 chars
            &chunks,
            10, // 10 tokens -> 8 usable -> 32 chars budget
            AdaptiveMode::ChangesFocus,
        );
        // After budget: system(10) + user(10) = 20, leaving 12 for chunks
        // First chunk is 30 > 12, so NO chunks fit
        // Only system + user returned
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
    }

    #[test]
    fn assemble_empty_context() {
        let messages = assemble_context(
            "System prompt",
            "User question",
            &[],
            1000,
            AdaptiveMode::ChangesFocus,
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[0].content, "System prompt");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content, "User question");
    }

    #[test]
    fn assemble_system_prompt_exceeds_budget() {
        let messages = assemble_context(
            "This is a very long system prompt that exceeds token budget",
            "short",
            &[],
            2, // 2 tokens -> 1 usable -> 4 chars
            AdaptiveMode::ChangesFocus,
        );
        // system_prompt truncated to 4 chars
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[0].content, "This");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content, "short");
    }

    #[test]
    fn assemble_one_chunk_fits() {
        // Usable budget: 500 tokens * 4 = 2000 chars * 0.85 = 1700 chars
        // system(10) + user(10) = 20, remainder 1680
        // chunk1: 100 chars -> fits
        let chunks = vec![
            make_chunk(&"A".repeat(100), "a.md", 0.8),
            make_chunk(&"B".repeat(2000), "b.md", 0.5),
        ];

        let messages = assemble_context(
            "0123456789", // 10 chars
            "user query", // 10 chars
            &chunks,
            500,
            AdaptiveMode::ChangesFocus,
        );

        // System + chunk1 (fits) + user = 3
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "system");
        assert!(messages[1].content.contains("a.md"));
        assert_eq!(messages[2].role, "user");
    }

    #[test]
    fn assemble_budget_85_percent_usable() {
        // 1000 token context -> 850 usable tokens -> 3400 chars
        let chunk_content = &"X".repeat(3000);
        let chunks = vec![make_chunk(chunk_content, "big.md", 0.9)];

        let messages = assemble_context(
            "sys",   // 3 chars
            "query", // 5 chars
            &chunks,
            1000,
            AdaptiveMode::ChangesFocus,
        );

        // 3400 - 3 - 5 = 3392 remaining for chunks, 3000 fits -> included
        assert_eq!(messages.len(), 3); // system + chunk + user
        assert_eq!(messages[0].content, "sys");
        assert!(messages[1].content.contains("big.md"));
        assert_eq!(messages[2].content, "query");
    }

    #[test]
    fn get_system_prompt_analyze() {
        let prompt = get_system_prompt("analyze");
        assert!(prompt.contains("impact and risk"));
    }

    #[test]
    fn get_system_prompt_suggest() {
        let prompt = get_system_prompt("suggest");
        assert!(prompt.contains("verification"));
    }

    #[test]
    fn get_system_prompt_review() {
        let prompt = get_system_prompt("review");
        assert!(prompt.contains("code reviewer"));
    }

    #[test]
    fn get_system_prompt_unknown_defaults_to_analyze() {
        let prompt = get_system_prompt("nonexistent");
        assert!(prompt.contains("impact and risk"));
    }
}
