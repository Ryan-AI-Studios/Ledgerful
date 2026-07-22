use crate::ledger::types::LedgerEntry;

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

pub fn generate_madr_content(entry: &LedgerEntry) -> String {
    let mut content = format!("# {}. {}\n\n", entry.id, entry.summary);

    content.push_str(&format!(
        "- **Status**: {}\n",
        serde_json::to_string(&entry.change_type)
            .unwrap_or_default()
            .trim_matches('"')
    ));
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
mod tests {
    use super::*;
    use crate::ledger::types::*;

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

        let content = generate_madr_content(&entry);
        assert!(content.contains("# 1. Standardize error handling"));
        assert!(content.contains("- **Status**: MODIFY"));
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
}
