/// Sanitize a query string for FTS5 phrase search.
/// Wraps the query in double quotes and escapes any internal double quotes by doubling them.
/// This prevents syntax errors like "fts5: syntax error near '?'" when queries contain special characters.
pub fn sanitize_fts5_query(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_fts5_query() {
        assert_eq!(sanitize_fts5_query("foo"), "\"foo\"");
        assert_eq!(sanitize_fts5_query("foo?"), "\"foo?\"");
        assert_eq!(sanitize_fts5_query("cross-project"), "\"cross-project\"");
        assert_eq!(sanitize_fts5_query("a:b"), "\"a:b\"");
        assert_eq!(
            sanitize_fts5_query("double \"quote\""),
            "\"double \"\"quote\"\"\""
        );
    }
}
