use crate::ledger::Category;
use std::fs;

pub use crate::ledger::reason::{risk_from_category, substantive_reason_from_commit_msg};

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

/// True when a related / issue_ref token looks like a filesystem path.
///
/// Slash-only is not enough: live dogfood writes `CHANGELOG.md` (no slash).
/// Dotted ids such as `TICKET-1.2` also match — named 0287 trade.
fn is_path_shaped(item: &str) -> bool {
    item.contains('/') || item.contains('\\') || std::path::Path::new(item).extension().is_some()
}

/// Four-digit conductor slug prefix (`0272-ci-…` → `0272`).
///
/// Infallible byte-slice equivalent of `^(\d{4})-[A-Za-z]`. Rejects
/// `2026-09-06` (digit after hyphen) and any `/` or `\\`.
fn ticket_from_entity(entity: &str) -> Option<&str> {
    let s = entity.trim();
    if s.contains('/') || s.contains('\\') {
        return None;
    }
    let b = s.as_bytes();
    if b.len() >= 6
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5].is_ascii_alphabetic()
    {
        Some(&s[..4])
    } else {
        None
    }
}

/// Ticket ids for the hook sidecar / v2 sign input. Never takes file lists.
///
/// Order: remaining explicit `related` (unique, sorted, `", "` join) →
/// entity slug → non-path `issue_ref` → `None`.
pub(crate) fn related_tickets_value(
    related: &[String],
    entity: &str,
    issue_ref: Option<&str>,
) -> Option<String> {
    let mut tickets: Vec<String> = related
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !is_path_shaped(s))
        .collect();
    tickets.sort();
    tickets.dedup();
    if !tickets.is_empty() {
        return Some(tickets.join(", "));
    }
    if let Some(id) = ticket_from_entity(entity) {
        return Some(id.to_string());
    }
    let issue = issue_ref.map(str::trim).filter(|s| !s.is_empty())?;
    if is_path_shaped(issue) {
        None
    } else {
        Some(issue.to_string())
    }
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
