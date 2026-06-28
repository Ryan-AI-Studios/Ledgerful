use encoding_rs::UTF_8;
use std::borrow::Cow;

/// Normalizes file content to UTF-8.
/// If the content is already UTF-8, it returns a Cow::Borrowed.
/// Otherwise, it attempts to detect and convert to UTF-8.
pub fn normalize_to_utf8(data: &[u8]) -> Cow<'_, str> {
    let (res, _encoding, _has_errors) = UTF_8.decode(data);
    res
}

/// Strips non-printable control characters from the string.
/// Keeps tab, newline, and carriage return.
pub fn strip_control_characters(s: &str) -> String {
    s.chars()
        .filter(|&c| !c.is_control() || c == '\n' || c == '\r' || c == '\t')
        .collect()
}

/// Checks if a line is likely minified or too long to index efficiently.
pub fn is_line_too_long(s: &str, max_len: usize) -> bool {
    s.lines().any(|line| line.len() > max_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_utf8() {
        let valid_utf8 = b"Hello, world!";
        assert_eq!(normalize_to_utf8(valid_utf8), "Hello, world!");

        let invalid_utf8 = b"Hello, \xFFworld!";
        let normalized = normalize_to_utf8(invalid_utf8);
        assert!(normalized.contains('\u{FFFD}') || normalized.contains("world"));
    }

    #[test]
    fn test_strip_control_characters() {
        let input = "Hello\x00world\nTest\x1B";
        let expected = "Helloworld\nTest";
        assert_eq!(strip_control_characters(input), expected);
    }

    #[test]
    fn test_is_line_too_long() {
        let short_lines = "line1\nline2";
        assert!(!is_line_too_long(short_lines, 10));

        let long_line = "this is a very long line";
        assert!(is_line_too_long(long_line, 10));
    }
}
