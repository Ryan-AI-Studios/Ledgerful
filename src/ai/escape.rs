//! Shared DATA-fence utilities for untrusted repository content in LLM prompts.

/// Escape untrusted code so it cannot form a Markdown fence inside a prompt's
/// own ``` block. Replaces every backtick with U+02CB (modifier letter grave
/// accent). Content remains visually recognizable while preventing
/// prompt-injection breakouts.
pub fn escape_code_chunk(chunk: &str) -> String {
    chunk.replace('`', "\u{02CB}")
}

/// Maximum characters retained from a bridge insight before truncation.
pub const BRIDGE_INSIGHT_MAX_CHARS: usize = 2_048;

/// Fence and size-cap bridge insight content for re-injection into ask context.
pub fn fence_bridge_insight(content: &str) -> String {
    let escaped = escape_code_chunk(content);
    let char_count = escaped.chars().count();
    if char_count <= BRIDGE_INSIGHT_MAX_CHARS {
        return escaped;
    }
    let truncated: String = escaped.chars().take(BRIDGE_INSIGHT_MAX_CHARS).collect();
    format!("{truncated}\n[...bridge insight truncated...]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_replaces_backticks() {
        let chunk = "```\ninjection\n```";
        let escaped = escape_code_chunk(chunk);
        assert!(!escaped.contains('`'));
        assert!(escaped.contains("\u{02CB}\u{02CB}\u{02CB}"));
    }

    #[test]
    fn fence_bridge_insight_caps_size() {
        let huge = "x".repeat(BRIDGE_INSIGHT_MAX_CHARS + 500);
        let fenced = fence_bridge_insight(&huge);
        assert!(fenced.contains("[...bridge insight truncated...]"));
        assert!(fenced.chars().count() < huge.chars().count());
    }
}
