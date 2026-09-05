use crate::ledger::Category;
use std::fs;

pub fn extract_trailers(msg: &str) -> String {
    let lines: Vec<&str> = msg.lines().collect();
    let mut trailer_lines = Vec::new();
    let mut in_trailer_block = true;

    for line in lines.iter().rev() {
        if line.trim().is_empty() {
            // Hit the blank line preceding the trailer block
            break;
        }

        if !in_trailer_block {
            break;
        }

        if let Some(pos) = line.find(':') {
            let token = line[..pos].trim();
            // Git trailers are typically Alphanumeric and dashes, e.g., Signed-off-by, Co-authored-by
            if !token.is_empty()
                && !token.contains(' ')
                && token.chars().all(|c| c.is_alphanumeric() || c == '-')
            {
                trailer_lines.push(*line);
            } else {
                // Not a valid trailer token format, meaning this isn't a true trailer block
                trailer_lines.clear();
                in_trailer_block = false;
            }
        } else {
            // No colon, not a trailer block
            trailer_lines.clear();
            in_trailer_block = false;
        }
    }
    trailer_lines.reverse();
    trailer_lines.join("\n")
}

pub fn is_trivial_commit(msg: &str) -> bool {
    let msg_lower = msg.to_lowercase();
    msg_lower.starts_with("chore:")
        || msg_lower.starts_with("docs:")
        || msg_lower.starts_with("style:")
        || msg_lower.starts_with("test:")
}

pub fn is_well_formed_conventional(msg: &str) -> bool {
    let lines: Vec<&str> = msg.lines().collect();
    if lines.is_empty() {
        return false;
    }
    let subject = lines[0].trim();

    // Standard conventional commit prefixes
    let prefixes = [
        "feat", "fix", "chore", "docs", "refactor", "perf", "ci", "build", "test", "revert",
        "style",
    ];

    let has_prefix = prefixes.iter().any(|&p| {
        subject.starts_with(p)
            && (subject[p.len()..].starts_with(':') || subject[p.len()..].starts_with('('))
            && subject.contains(':')
    });

    // Also require a body for "well-formed" bypass to ensure sufficient intent
    let has_body = lines.iter().skip(1).any(|l| !l.trim().is_empty());

    has_prefix && has_body
}

pub(super) fn are_files_trivial(files: &[String]) -> bool {
    files
        .iter()
        .all(|f| f.ends_with(".md") || f.contains(".ledgerful/") || f.contains("ignore_patterns"))
}

pub(super) fn load_skip_history(path: &camino::Utf8Path) -> SkipHistory {
    if path.exists()
        && let Ok(data) = fs::read_to_string(path.as_std_path())
        && let Ok(history) = serde_json::from_str(&data)
    {
        return history;
    }
    SkipHistory::default()
}

pub(super) fn save_skip_history(path: &camino::Utf8Path, history: &SkipHistory) {
    if let Ok(data) = serde_json::to_string(history) {
        let _ = fs::write(path.as_std_path(), data);
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub(super) struct SkipHistory {
    pub consecutive_skips: u32,
    pub bypass_remaining: u32,
}

pub fn parse_category_from_message(msg: &str) -> Category {
    let msg_lower = msg.to_lowercase();
    if msg_lower.starts_with("feat") {
        Category::Feature
    } else if msg_lower.starts_with("fix") || msg_lower.starts_with("bug") {
        Category::Bugfix
    } else if msg_lower.starts_with("docs") {
        Category::Docs
    } else if msg_lower.starts_with("refactor") || msg_lower.starts_with("perf") {
        Category::Refactor
    } else if msg_lower.starts_with("chore") {
        Category::Chore
    } else if msg_lower.starts_with("ci")
        || msg_lower.starts_with("infra")
        || msg_lower.starts_with("build")
    {
        Category::Infra
    } else if msg_lower.starts_with("style") {
        Category::Tooling
    } else if msg_lower.starts_with("revert") {
        Category::Bugfix
    } else if msg_lower.starts_with("security") {
        Category::Security
    } else if msg_lower.starts_with("breaking") {
        Category::Architecture
    } else {
        tracing::debug!(
            "No conventional commit prefix found in message; falling back to Category::Chore: {}",
            msg
        );
        Category::Chore
    }
}

pub fn risk_from_category(cat: Category) -> &'static str {
    match cat {
        Category::Architecture
        | Category::Feature
        | Category::Bugfix
        | Category::Infra
        | Category::Security => "HIGH",
        Category::Refactor | Category::Tooling => "MEDIUM",
        Category::Docs | Category::Chore => "TRIVIAL",
    }
}
