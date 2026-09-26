use crate::types::{AdrStatus, LedgerEntry};

fn lifecycle_status_token(lifecycle: AdrStatus) -> &'static str {
    match lifecycle {
        AdrStatus::Proposed => "proposed",
        AdrStatus::Accepted => "accepted",
        AdrStatus::Rejected => "rejected",
        AdrStatus::Deprecated => "deprecated",
        AdrStatus::Superseded => "superseded",
    }
}

pub fn slugify_summary(summary: &str) -> String {
    summary
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn generate_madr_content(entry: &LedgerEntry, lifecycle: AdrStatus) -> String {
    let mut content = format!("# {}. {}\n\n", entry.id, entry.summary);

    content.push_str(&format!(
        "- **Status**: {}\n",
        lifecycle_status_token(lifecycle)
    ));
    content.push_str(&format!("- **Change type**: {}\n", entry.change_type));
    content.push_str(&format!("- **Category**: {:?}\n", entry.category));
    content.push_str(&format!(
        "- **Breaking**: {}\n",
        if entry.is_breaking { "yes" } else { "no" }
    ));
    content.push_str(&format!("- **Date**: {}\n", entry.committed_at));
    content.push('\n');

    content.push_str("## Context\n\n");
    content.push_str(&format!("Entity: `{}`\n\n", entry.entity_normalized));
    content.push_str(&format!("{}\n\n", entry.reason));

    content.push_str("## Decision\n\n");
    content.push_str(&format!("{}\n\n", entry.summary));

    content.push_str("## Consequences\n\n");
    if let Some(ref notes) = entry.outcome_notes {
        content.push_str(&format!("{}\n\n", notes));
    } else {
        content.push_str("None recorded.\n\n");
    }

    if let (Some(status), Some(basis)) = (entry.verification_status, entry.verification_basis) {
        content.push_str("## Validation\n\n");
        content.push_str(&format!("* Status: {:?}\n", status));
        content.push_str(&format!("* Basis: {:?}\n", basis));
        content.push('\n');
    }

    content
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use crate::types::*;

    #[test]
    fn test_slugify_summary() {
        assert_eq!(
            slugify_summary("Use UUID for transactions"),
            "use-uuid-for-transactions"
        );
        assert_eq!(
            slugify_summary("Breaking: Change API!!"),
            "breaking-change-api"
        );
        assert_eq!(slugify_summary("   Space  Test   "), "space-test");
    }

    #[test]
    fn test_generate_madr_content() {
        let entry = LedgerEntry {
            id: 1,
            tx_id: "tx-123".to_string(),
            category: Category::Architecture,
            entry_type: EntryType::Architecture,
            entity: "src/lib.rs".to_string(),
            entity_normalized: "src/lib.rs".to_string(),
            change_type: ChangeType::Modify,
            summary: "Standardize error handling".to_string(),
            reason: "We need consistent errors across the CLI.".to_string(),
            is_breaking: true,
            committed_at: "2023-10-27T10:00:00Z".to_string(),
            verification_status: Some(VerificationStatus::Verified),
            verification_basis: Some(VerificationBasis::Tests),
            outcome_notes: Some("All modules now use thiserror.".to_string()),
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            related_tickets: None,
            author: "Test User".to_string(),
            observed: None,
            prev_hash: None,
            sig_version: 1,
        };

        let content = generate_madr_content(&entry, AdrStatus::Proposed);
        assert!(content.contains("# 1. Standardize error handling"));
        assert!(content.contains("- **Status**: proposed"));
        assert!(content.contains("- **Change type**: MODIFY"));
        let status_idx = content.find("- **Status**: proposed").expect("status line");
        let change_idx = content
            .find("- **Change type**: MODIFY")
            .expect("change type line");
        assert!(status_idx < change_idx);
        assert!(content.contains("- **Category**: Architecture"));
        assert!(content.contains("- **Breaking**: yes"));
        assert!(content.contains("Entity: `src/lib.rs`"));
        assert!(content.contains("We need consistent errors across the CLI."));
        assert!(content.contains("## Decision"));
        assert!(content.contains("Standardize error handling"));
        assert!(content.contains("## Consequences"));
        assert!(content.contains("All modules now use thiserror."));
        assert!(content.contains("Status: Verified"));
        assert!(content.contains("Basis: Tests"));
    }

    #[rstest::rstest]
    #[case(AdrStatus::Proposed, "proposed")]
    #[case(AdrStatus::Accepted, "accepted")]
    #[case(AdrStatus::Rejected, "rejected")]
    #[case(AdrStatus::Deprecated, "deprecated")]
    #[case(AdrStatus::Superseded, "superseded")]
    fn generate_madr_content__lifecycle_token__matches_serde(
        #[case] lifecycle: AdrStatus,
        #[case] token: &str,
    ) {
        let serialized = serde_json::to_string(&lifecycle).expect("serialize AdrStatus");
        assert_eq!(serialized.trim_matches('"'), token);
        let entry = LedgerEntry {
            id: 1,
            tx_id: "tx-123".to_string(),
            category: Category::Architecture,
            entry_type: EntryType::Architecture,
            entity: "src/lib.rs".to_string(),
            entity_normalized: "src/lib.rs".to_string(),
            change_type: ChangeType::Modify,
            summary: "token check".to_string(),
            reason: "table".to_string(),
            is_breaking: false,
            committed_at: "2023-10-27T10:00:00Z".to_string(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            related_tickets: None,
            author: "Test User".to_string(),
            observed: None,
            prev_hash: None,
            sig_version: 1,
        };
        let content = generate_madr_content(&entry, lifecycle);
        assert!(content.contains(&format!("- **Status**: {token}\n")));
        assert!(content.contains("- **Change type**: MODIFY\n"));
        assert!(!content.contains(&format!("- **Change type**: {token}")));
        let status_idx = content.find("- **Status**:").expect("status line present");
        let change_idx = content
            .find("- **Change type**:")
            .expect("change type line present");
        assert!(status_idx < change_idx);
    }
}
