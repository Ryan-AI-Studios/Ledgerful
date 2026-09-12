use crate::impact::redact::{DEFAULT_MAX_BYTES, SanitizeResult, sanitize_prompt};

/// Provider-agnostic egress sanitizer (track 0073 / RT-A1).
///
/// Run once before any completion network call on cloud arms (OpenRouter,
/// Ollama Cloud, Gemini). Do not double-call on the same string path when
/// composing Gemini prompts that already went through this function.
pub fn sanitize_for_egress(prompt: &str) -> SanitizeResult {
    sanitize_prompt(prompt, DEFAULT_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_for_egress_removes_api_key_shaped_strings() {
        let prompt = "api_key = \"AKIAIOSFODNN7EXAMPLE\"";
        let result = sanitize_for_egress(prompt);
        assert!(
            !result.sanitized.contains("AKIAIOSFODNN7EXAMPLE"),
            "expected redaction of API-key-shaped content, got: {}",
            result.sanitized
        );
        assert!(result.sanitized.contains("[REDACTED") || !result.redactions.is_empty());
    }
}
