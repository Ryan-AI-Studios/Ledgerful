//! Allowlist CI guard — fails if a sensitive field reaches the public export
//! allowlist.
//!
//! Track 0045 DoD-6: the allowlist can only grow deliberately.  If someone
//! wants to allow a field currently listed in `SENSITIVE_FIELDS`, they must
//! remove it from `SENSITIVE_FIELDS` in the same commit — the test itself is
//! the deliberate gate and is visible in review.

#![cfg(test)]

use std::collections::BTreeSet;

/// Fields that must NEVER appear in the public export allowlist.
/// These contain internal-only context that could leak sensitive info.
const SENSITIVE_FIELDS: &[&str] = &[
    "entity",
    "entity_normalized",
    "change_type",
    "is_breaking",
    "outcome_notes",
    "origin",
    "trace_id",
    "related_tickets",
    "author", // raw author — only author_pseudonym is published
    "observed",
    "prev_hash", // internal chain linkage — only entry_hash is published
    "entry_type",
    "id",
    "operation_id",
    "snapshot_id",
    "tree_hash",
    "issue_ref",
    "verification_basis",
    "verification_status", // raw enum — only verification_result (mapped) is published
];

/// Fields that are intentionally allowed in the public export.
/// Extracted from `src/ledger/public_export.rs` at compile time via
/// `include_str!` so the test always reflects the current source.
fn extract_allowed_fields(source: &str) -> BTreeSet<&str> {
    let mut allowed = BTreeSet::new();

    // The public fields are declared as `field_name: Type` inside the
    // `PublicEntry` struct. We scan the struct body and collect every
    // field name that appears before the first method (`impl` block).
    let Some(struct_start) = source.find("struct PublicEntry") else {
        panic!("PublicEntry struct not found in src/ledger/public_export.rs");
    };
    let struct_body = &source[struct_start..];
    let Some(body_start) = struct_body.find('{') else {
        panic!("PublicEntry struct body not found");
    };
    let body = &struct_body[body_start + 1..];
    let Some(body_end) = body.find('}') else {
        panic!("PublicEntry struct body is not closed");
    };
    let body = &body[..body_end];

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("///") || trimmed.starts_with("//") {
            continue;
        }
        if let Some(colon) = trimmed.find(':') {
            let field = trimmed[..colon].trim();
            if !field.is_empty() && !field.contains(' ') {
                allowed.insert(field);
            }
        }
    }

    allowed
}

/// The set of fields the public export actually publishes.
fn allowed_fields() -> BTreeSet<&'static str> {
    let source = include_str!("../../src/ledger/public_export.rs");
    extract_allowed_fields(source)
}

/// Returns the set of sensitive fields that are currently present in the
/// public export allowlist.
fn sensitive_in_allowlist() -> BTreeSet<&'static str> {
    let allowed = allowed_fields();
    SENSITIVE_FIELDS
        .iter()
        .filter(|field| allowed.contains(*field))
        .copied()
        .collect()
}

#[test]
fn no_sensitive_field_in_allowlist() {
    let violations = sensitive_in_allowlist();

    assert!(
        violations.is_empty(),
        "Sensitive fields found in public export allowlist: {:?}. \
         If this is intentional, remove those fields from SENSITIVE_FIELDS in this file \
         in the same commit so the change is visible in review.",
        violations
    );
}

#[test]
fn allowlist_is_not_empty() {
    let allowed = allowed_fields();
    assert!(
        !allowed.is_empty(),
        "PublicEntry allowlist must not be empty"
    );
}

#[test]
fn published_fields_are_expected() {
    let allowed = allowed_fields();
    let expected: BTreeSet<&str> = [
        "author_pseudonym",
        "category",
        "committed_at",
        "entry_hash",
        "public_key",
        "reason",
        "risk_level",
        "signature",
        "summary",
        "tx_id",
        "verification_result",
    ]
    .into_iter()
    .collect();

    assert_eq!(
        allowed, expected,
        "PublicEntry allowlist does not match the documented published fields"
    );
}
