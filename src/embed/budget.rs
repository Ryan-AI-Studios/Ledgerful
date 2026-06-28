pub fn enforce_budget(text: &str, max_tokens: usize) -> String {
    let max_chars = max_tokens * 4;

    if text.len() <= max_chars {
        return text.to_string();
    }

    let boundary = if is_char_boundary(text, max_chars) {
        max_chars
    } else {
        (0..max_chars)
            .rev()
            .find(|&i| is_char_boundary(text, i))
            .unwrap_or(0)
    };

    let truncated = &text[..boundary];

    if let Some(last_space) = truncated.rfind(' ') {
        truncated[..last_space].to_string()
    } else {
        truncated.to_string()
    }
}

fn is_char_boundary(s: &str, index: usize) -> bool {
    s.get(index..).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforce_budget_short_text_unchanged() {
        let text = "hello world";
        let result = enforce_budget(text, 100);
        assert_eq!(result, text);
    }

    #[test]
    fn enforce_budget_truncates_long_text() {
        let text = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";
        let result = enforce_budget(text, 5);
        assert!(result.len() < text.len());
        assert!(!result.is_empty());
        let original_words: Vec<&str> = text.split_whitespace().collect();
        let result_words: Vec<&str> = result.split_whitespace().collect();
        assert!(result_words.len() < original_words.len());
    }

    #[test]
    fn enforce_budget_no_mid_word_truncation() {
        let text = "hello world beautiful";
        // budget of 2 tokens = 8 chars, which falls mid-word in "world"
        // truncates to "hello" (last complete word before boundary)
        let result = enforce_budget(text, 2);
        assert_eq!(result, "hello");
    }
}
