pub fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Normalize a commit message the way git's `commit.cleanup=whitespace` does,
/// so the hash computed before the commit object is written (in the commit-msg
/// hook) matches the hash computed after (in the post-commit hook via
/// `git log -1 --format=%B`).
///
/// Git's whitespace cleanup:
///   1. Strip trailing whitespace from each line.
///   2. Collapse runs of 2+ consecutive blank lines into a single blank line.
///   3. Trim leading and trailing blank lines.
///
/// We also strip `#`-comment lines (git does this in the default editor flow
/// via the `#` comment marker, and the commit-msg hook receives the message
/// after those are removed — but `clean_commit_msg` is called on both sides
/// so it must be idempotent).
pub fn clean_commit_msg(msg: &str) -> String {
    let lines: Vec<String> = msg
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| line.trim_end().to_string())
        .collect();

    // Collapse runs of 2+ blank lines into one.
    let mut result = Vec::with_capacity(lines.len());
    let mut prev_blank = false;
    for line in &lines {
        let is_blank = line.is_empty();
        if is_blank && prev_blank {
            continue;
        }
        result.push(line.clone());
        prev_blank = is_blank;
    }

    // Trim leading/trailing blank lines.
    while !result.is_empty() && result[0].is_empty() {
        result.remove(0);
    }
    while !result.is_empty() && result.last().map(|s| s.is_empty()).unwrap_or(false) {
        result.pop();
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_comment_lines() {
        let msg = "# Please enter the commit message\nfeat: add function\n# Lines starting with # are removed";
        assert_eq!(clean_commit_msg(msg), "feat: add function");
    }

    #[test]
    fn clean_strips_trailing_whitespace() {
        let msg = "feat: add function   \n   body line  \n";
        assert_eq!(clean_commit_msg(msg), "feat: add function\n   body line");
    }

    #[test]
    fn clean_collapses_blank_line_runs() {
        let msg = "feat: add function\n\n\n\nbody text\n\n\n";
        assert_eq!(clean_commit_msg(msg), "feat: add function\n\nbody text");
    }

    #[test]
    fn clean_trims_leading_and_trailing_blanks() {
        let msg = "\n\nfeat: add function\nbody\n\n\n";
        assert_eq!(clean_commit_msg(msg), "feat: add function\nbody");
    }

    #[test]
    fn clean_preserves_single_blank_line() {
        let msg = "feat: add function\n\nbody text";
        assert_eq!(clean_commit_msg(msg), "feat: add function\n\nbody text");
    }

    #[test]
    fn clean_idempotent() {
        let msg = "feat: add function\n\n  body with trailing space   \n\n\n\n";
        let once = clean_commit_msg(msg);
        let twice = clean_commit_msg(&once);
        assert_eq!(once, twice, "clean_commit_msg must be idempotent");
    }

    #[test]
    fn clean_handles_llm_draft_with_double_blanks() {
        let pre_cleanup = "Adds a subtraction function and a new icon asset.\n\nThe change introduces a new mathematical operation (subtraction) to the library and updates the application's assets by adding a new icon, likely to represent the new functionality.\n\n  \n\nfeat: add sub and binary icon";
        let post_cleanup = "Adds a subtraction function and a new icon asset.\n\nThe change introduces a new mathematical operation (subtraction) to the library and updates the application's assets by adding a new icon, likely to represent the new functionality.\n\nfeat: add sub and binary icon";
        assert_eq!(
            clean_commit_msg(pre_cleanup),
            clean_commit_msg(post_cleanup)
        );
    }

    #[test]
    fn clean_empty_msg() {
        assert_eq!(clean_commit_msg(""), "");
        assert_eq!(clean_commit_msg("\n\n\n"), "");
    }
}
