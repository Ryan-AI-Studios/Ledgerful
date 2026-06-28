use crate::impact::redact::{DEFAULT_MAX_BYTES, SanitizeResult, sanitize_prompt};

pub fn sanitize_for_gemini(prompt: &str) -> SanitizeResult {
    sanitize_prompt(prompt, DEFAULT_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_for_gemini_removes_secrets() {
        let prompt = "api_key = \"AKIAIOSFODNN7EXAMPLE\"\ntoken = \"ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890\"";
        let result = sanitize_for_gemini(prompt);
        assert!(!result.sanitized.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!result.sanitized.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
        assert!(!result.truncated);
    }

    #[test]
    fn test_sanitize_for_gemini_under_limit() {
        let prompt = "Normal prompt without secrets.";
        let result = sanitize_for_gemini(prompt);
        assert_eq!(result.sanitized, prompt);
        assert!(!result.truncated);
    }
}
