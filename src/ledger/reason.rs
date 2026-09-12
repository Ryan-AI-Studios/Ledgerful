//! Emit-time reason/risk labels and write-side trailer stripping.
//!
//! No SQLite columns. Do not put computed fields on the rusqlite `LedgerEntry`
//! mapper. Read surfaces (search / audit / MCP / REST) call these helpers;
//! they must not import `hook_commit_msg`.

use crate::ledger::types::{Category, LedgerEntry};
use serde::Serialize;

const ALLOWLISTED_TRAILER_KEYS: &[&str] = &[
    "Co-authored-by",
    "Signed-off-by",
    "Acked-by",
    "Reviewed-by",
    "Reported-by",
    "Suggested-by",
    "Tested-by",
];

const CONVENTIONAL_TYPE_PREFIXES: &[&str] = &[
    "feat",
    "fix",
    "bug",
    "docs",
    "refactor",
    "perf",
    "chore",
    "ci",
    "infra",
    "build",
    "style",
    "test",
    "revert",
    "security",
    "breaking",
    "architecture",
    "feature",
    "tooling",
];

/// Category → stored risk mapping (HIGH / MEDIUM / TRIVIAL). Do not remap.
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

fn is_token_value_line(line: &str) -> bool {
    let line = line.trim_end();
    if line.trim().is_empty() {
        return false;
    }
    let Some(pos) = line.find(':') else {
        return false;
    };
    let token = line[..pos].trim();
    !token.is_empty()
        && !token.contains(' ')
        && token.chars().all(|c| c.is_alphanumeric() || c == '-')
}

fn trailer_key(line: &str) -> Option<&str> {
    let pos = line.find(':')?;
    Some(line[..pos].trim())
}

fn is_allowlisted_key(key: &str) -> bool {
    ALLOWLISTED_TRAILER_KEYS
        .iter()
        .any(|k| k.eq_ignore_ascii_case(key))
}

fn is_conventional_type_prefix(key: &str) -> bool {
    CONVENTIONAL_TYPE_PREFIXES
        .iter()
        .any(|k| k.eq_ignore_ascii_case(key))
}

/// Stored `reason` is trailer-only: every nonempty line is `Token: value`
/// and at least one key is a well-known git trailer. Historical
/// `Co-authored-by: …` (no blank line) is `trailer`. `Refactor: extract helper`
/// is not.
pub fn is_trailer_only_reason(reason: &str) -> bool {
    let lines: Vec<&str> = reason
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }
    let mut saw_allowlist = false;
    for line in &lines {
        if !is_token_value_line(line) {
            return false;
        }
        let key = trailer_key(line).unwrap_or("");
        if is_conventional_type_prefix(key) {
            return false;
        }
        if is_allowlisted_key(key) {
            saw_allowlist = true;
        }
    }
    saw_allowlist
}

/// Emit `reason_kind`: `"trailer"` or omit. Never `"body"` / `"subject"` / `"explicit"`.
pub fn classify_reason_kind(reason: &str) -> Option<&'static str> {
    if is_trailer_only_reason(reason) {
        Some("trailer")
    } else {
        None
    }
}

/// Emit `risk_source`: `"category"` when risk matches the mapping, `"explicit"`
/// when present and different, omit when risk is null.
pub fn classify_risk_source(risk: Option<&str>, category: Category) -> Option<&'static str> {
    let risk = risk?;
    if risk == risk_from_category(category) {
        Some("category")
    } else {
        Some("explicit")
    }
}

fn strip_blank_preceded_allowlisted_paragraph<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let last_blank = lines.iter().rposition(|l| l.trim().is_empty());
    let Some(blank_idx) = last_blank else {
        return lines.to_vec();
    };
    let after: Vec<&str> = lines[blank_idx + 1..]
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if after.is_empty() {
        return lines.to_vec();
    }
    if !after.iter().all(|l| is_token_value_line(l)) {
        return lines.to_vec();
    }
    let has_allowlist = after
        .iter()
        .any(|l| trailer_key(l).map(is_allowlisted_key).unwrap_or(false));
    if !has_allowlist {
        return lines.to_vec();
    }
    lines[..blank_idx].to_vec()
}

fn strip_trailing_allowlisted(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
    loop {
        while out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        let Some(last) = out.last() else {
            break;
        };
        if is_token_value_line(last) && trailer_key(last).is_some_and(is_allowlisted_key) {
            out.pop();
            continue;
        }
        break;
    }
    out
}

/// Subject/body split + trailer strip for **new** hook writes.
///
/// Fallback order: remaining body → subject (never a trailer block) →
/// `fallback_summary`.
pub fn substantive_reason_from_commit_msg(msg: &str, fallback_summary: Option<&str>) -> String {
    let lines: Vec<&str> = msg.lines().collect();
    let without_block = strip_blank_preceded_allowlisted_paragraph(&lines);
    let subject = without_block
        .first()
        .map(|s| s.trim())
        .unwrap_or("")
        .to_string();
    let body_lines: Vec<&str> = if without_block.len() > 1 {
        without_block[1..].to_vec()
    } else {
        Vec::new()
    };
    let stripped = strip_trailing_allowlisted(&body_lines);
    let body = stripped.join("\n").trim().to_string();

    if !body.is_empty() {
        return body;
    }
    if !subject.is_empty() && !is_trailer_only_reason(&subject) {
        return subject;
    }
    if let Some(summary) = fallback_summary.map(str::trim).filter(|s| !s.is_empty()) {
        return summary.to_string();
    }
    if !subject.is_empty() {
        return subject;
    }
    String::new()
}

/// Flatten wrapper so search / MCP stay a bare array with omit-empty labels.
#[derive(Serialize)]
pub struct LabeledLedgerEntry<'a> {
    #[serde(flatten)]
    pub entry: &'a LedgerEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_source: Option<&'static str>,
}

impl<'a> LabeledLedgerEntry<'a> {
    pub fn from_entry(entry: &'a LedgerEntry) -> Self {
        Self {
            entry,
            reason_kind: classify_reason_kind(&entry.reason),
            risk_source: classify_risk_source(entry.risk.as_deref(), entry.category),
        }
    }
}

pub fn labeled_search_items(entries: &[LedgerEntry]) -> Vec<LabeledLedgerEntry<'_>> {
    entries.iter().map(LabeledLedgerEntry::from_entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substantive_reason_strips_coauthored_trailer() {
        let msg = "feat: bind review evidence\n\nCo-authored-by: Cursor <cursoragent@cursor.com>";
        let why = substantive_reason_from_commit_msg(msg, Some("pending summary"));
        assert_eq!(why, "feat: bind review evidence");
        assert!(!why.contains("Co-authored-by"));
    }

    #[test]
    fn substantive_reason_keeps_body_drops_trailer_block() {
        let msg = "feat: bind review evidence\n\nStore a substantive why.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>";
        let why = substantive_reason_from_commit_msg(msg, None);
        assert_eq!(why, "Store a substantive why.");
        assert!(!why.contains("Co-authored-by"));
    }

    #[test]
    fn substantive_reason_keeps_trailing_prose_colon_line() {
        let msg = "feat: bind review evidence\n\nStore a substantive why.\nNote: important";
        let why = substantive_reason_from_commit_msg(msg, None);
        assert!(
            why.contains("Note: important"),
            "prose colon line must stay: {why}"
        );
        assert!(why.contains("Store a substantive why."));
    }

    #[test]
    fn classify_reason_kind_trailer_only() {
        assert_eq!(
            classify_reason_kind("Co-authored-by: Cursor <cursoragent@cursor.com>"),
            Some("trailer")
        );
        assert_eq!(classify_reason_kind(""), None);
        assert_eq!(classify_reason_kind("Store a substantive why."), None);
    }

    #[test]
    fn classify_reason_kind_prose_with_colon_is_omitted() {
        assert_eq!(classify_reason_kind("Refactor: extract helper"), None);
        assert_eq!(classify_reason_kind("Note: important"), None);
    }

    #[test]
    fn substantive_reason_strips_crlf_coauthored_trailer() {
        let msg =
            "feat: bind review evidence\r\n\r\nCo-authored-by: Cursor <cursoragent@cursor.com>";
        let why = substantive_reason_from_commit_msg(msg, None);
        assert_eq!(why, "feat: bind review evidence");
        assert!(!why.contains("Co-authored-by"));
    }

    #[test]
    fn classify_risk_source_category_for_bugfix_high() {
        assert_eq!(
            classify_risk_source(Some("HIGH"), Category::Bugfix),
            Some("category")
        );
        assert_eq!(
            classify_risk_source(Some("LOW"), Category::Bugfix),
            Some("explicit")
        );
        assert_eq!(classify_risk_source(None, Category::Bugfix), None);
    }

    #[test]
    fn search_json_reason_kind_omitted_on_body() {
        let mut entry = sample_entry();
        entry.reason = "Store a substantive why.".to_string();
        entry.risk = Some("HIGH".to_string());
        let v = serde_json::to_value(LabeledLedgerEntry::from_entry(&entry)).unwrap();
        assert!(v.is_object(), "item must stay an object in a bare array");
        assert!(
            v.get("reason_kind").is_none(),
            "prose why must omit reason_kind: {v}"
        );
        assert_eq!(v["reason"], "Store a substantive why.");
        assert_eq!(v["risk_source"], "category");

        entry.reason = "Co-authored-by: Cursor <cursoragent@cursor.com>".to_string();
        let trailer = serde_json::to_value(LabeledLedgerEntry::from_entry(&entry)).unwrap();
        assert_eq!(trailer["reason_kind"], "trailer");
    }

    fn sample_entry() -> LedgerEntry {
        LedgerEntry {
            id: 1,
            tx_id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
            category: Category::Bugfix,
            entry_type: crate::ledger::types::EntryType::Implementation,
            entity: "0319-fixture-track".to_string(),
            entity_normalized: "0319-fixture-track".to_string(),
            change_type: crate::ledger::types::ChangeType::Modify,
            summary: "summary".to_string(),
            reason: "why".to_string(),
            is_breaking: false,
            committed_at: "2026-09-12T00:00:00Z".to_string(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: Some("HIGH".to_string()),
            related_tickets: None,
            author: "test".to_string(),
            observed: None,
            prev_hash: None,
            sig_version: 2,
        }
    }
}
