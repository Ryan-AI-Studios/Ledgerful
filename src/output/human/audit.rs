/// Uncolored TOP CHURNED FILES row (count word is `entries`, not `commits`).
/// Audit row (0472); width 40; this is not the velocity `Total Commits:` line.
pub(crate) fn format_churn_line(entity: &str, count: i64) -> String {
    format!("  {entity:<40} {count} entries")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_churn_line_uses_entries_not_commits() {
        let line = format_churn_line("CHANGELOG.md", 12);
        assert!(
            line.contains("entries"),
            "churn unit must be entries: {line}"
        );
        assert!(
            !line.contains("commits"),
            "churn unit must not be commits: {line}"
        );
        assert!(line.contains("CHANGELOG.md"));
        assert!(line.contains("12"));
        assert_eq!(line, format!("  {:<40} {} entries", "CHANGELOG.md", 12));
        assert!(line.starts_with("  CHANGELOG.md"));
        assert_eq!(
            line,
            "  CHANGELOG.md                             12 entries"
        );
        assert_eq!(
            format_churn_line("src/lib.rs", 1),
            format!("  {:<40} {} entries", "src/lib.rs", 1)
        );
        assert_eq!(
            format_churn_line("src/lib.rs", 0),
            format!("  {:<40} {} entries", "src/lib.rs", 0)
        );
    }
}
