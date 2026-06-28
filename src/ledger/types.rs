use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ValueEnum, Default,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[value(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Category {
    Architecture,
    #[default]
    Feature,
    Bugfix,
    Refactor,
    Infra,
    Security,
    Tooling,
    Docs,
    Chore,
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Category::Architecture => "ARCHITECTURE",
            Category::Feature => "FEATURE",
            Category::Bugfix => "BUGFIX",
            Category::Refactor => "REFACTOR",
            Category::Infra => "INFRA",
            Category::Security => "SECURITY",
            Category::Tooling => "TOOLING",
            Category::Docs => "DOCS",
            Category::Chore => "CHORE",
        };
        write!(f, "{}", s)
    }
}

impl Category {
    const ALL: [Category; 9] = [
        Category::Architecture,
        Category::Feature,
        Category::Bugfix,
        Category::Refactor,
        Category::Infra,
        Category::Security,
        Category::Tooling,
        Category::Docs,
        Category::Chore,
    ];

    pub fn parse_flexible(input: &str) -> Option<Self> {
        <Self as ValueEnum>::from_str(input, true)
            .ok()
            .or_else(|| Self::suggestions_for(input).first().copied())
    }

    pub fn suggestions_for(input: &str) -> Vec<Self> {
        let normalized = normalize_category_input(input);
        if normalized.is_empty() {
            return Vec::new();
        }

        let mut scored = Self::ALL
            .iter()
            .filter_map(|category| {
                let score = category_match_score(&normalized, *category);
                (score <= 3).then_some((*category, score))
            })
            .collect::<Vec<_>>();
        scored.sort_by(
            |(left_category, left_score), (right_category, right_score)| {
                left_score
                    .cmp(right_score)
                    .then_with(|| left_category.cmp(right_category))
            },
        );
        scored.into_iter().map(|(category, _)| category).collect()
    }
}

fn normalize_category_input(input: &str) -> String {
    input
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn category_aliases(category: Category) -> &'static [&'static str] {
    match category {
        Category::Architecture => &["architecture", "arch", "design"],
        Category::Feature => &["feature", "feat", "new"],
        Category::Bugfix => &["bugfix", "bug", "fix"],
        Category::Refactor => &["refactor", "cleanup", "rework"],
        Category::Infra => &["infra", "infrastructure", "ops", "ci"],
        Category::Tooling => &["tooling", "tool", "dev", "devex"],
        Category::Docs => &["docs", "doc", "documentation"],
        Category::Chore => &["chore", "maintenance", "maint"],
        Category::Security => &["security", "sec", "auth", "authz"],
    }
}

fn category_match_score(input: &str, category: Category) -> usize {
    category_aliases(category)
        .iter()
        .map(|alias| {
            if *alias == input {
                return 0;
            }
            if alias.starts_with(input) || input.starts_with(alias) {
                return 1;
            }
            levenshtein(input, alias)
        })
        .min()
        .unwrap_or(usize::MAX)
}

fn levenshtein(left: &str, right: &str) -> usize {
    if left == right {
        return 0;
    }
    if left.is_empty() {
        return right.chars().count();
    }
    if right.is_empty() {
        return left.chars().count();
    }

    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];

    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_char != *right_char);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            current[right_index + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right_chars.len()]
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ValueEnum, Default,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[value(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChangeType {
    Create,
    #[default]
    Modify,
    Deprecate,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[value(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EntryType {
    Implementation,
    Architecture,
    Lesson,
    Reconciliation,
    Rollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum VerificationStatus {
    Verified,
    Unverified,
    PartiallyVerified,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum VerificationBasis {
    Tests,
    Build,
    Lint,
    Runtime,
    ManualInspection,
    Inferred,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransactionRequest {
    pub category: Category,
    pub entity: String,
    pub planned_action: Option<String>,
    pub issue_ref: Option<String>,
    pub operation_id: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommitRequest {
    pub change_type: ChangeType,
    pub summary: String,
    pub reason: String,
    pub is_breaking: bool,
    pub committed_at: Option<String>,
    pub verification_status: Option<VerificationStatus>,
    pub verification_basis: Option<VerificationBasis>,
    pub outcome_notes: Option<String>,
    pub issue_ref: Option<String>,
    pub signature: Option<String>,
    pub public_key: Option<String>,
    pub risk: Option<String>,
    pub related_tickets: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<Vec<String>>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub tx_id: String,
    pub operation_id: Option<String>,
    pub status: String,
    pub category: Category,
    pub entity: String,
    pub entity_normalized: String,
    pub planned_action: Option<String>,
    pub session_id: String,
    pub source: String,
    pub started_at: String,
    pub resolved_at: Option<String>,
    pub detected_at: Option<String>,
    pub drift_count: i32,
    pub first_seen_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub issue_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: i64,
    pub tx_id: String,
    pub category: Category,
    pub entry_type: EntryType,
    pub entity: String,
    pub entity_normalized: String,
    pub change_type: ChangeType,
    pub summary: String,
    pub reason: String,
    pub is_breaking: bool,
    pub committed_at: String,
    pub verification_status: Option<VerificationStatus>,
    pub verification_basis: Option<VerificationBasis>,
    pub outcome_notes: Option<String>,
    pub origin: String,
    pub trace_id: Option<String>,
    pub signature: Option<String>,
    pub public_key: Option<String>,
    pub risk: Option<String>,
    pub related_tickets: Option<String>,
    pub author: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum AdrStatus {
    #[default]
    Proposed,
    Accepted,
    Rejected,
    Deprecated,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdrMetadata {
    pub adr_id: String,
    pub status: AdrStatus,
    pub owner: Option<String>,
    pub reviewers: Option<String>,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub affected_entities: Option<String>,
    pub decision_scope: Option<String>,
    pub reviewed_at: Option<String>,
    pub review_interval_days: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdrMetadataUpdate {
    pub status: Option<AdrStatus>,
    pub owner: Option<String>,
    pub reviewers: Option<String>,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub affected_entities: Option<String>,
    pub decision_scope: Option<String>,
    pub reviewed_at: Option<String>,
    pub review_interval_days: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_category_serialization() {
        assert_eq!(
            serde_json::to_string(&Category::Architecture).unwrap(),
            "\"ARCHITECTURE\""
        );
        assert_eq!(
            serde_json::from_str::<Category>("\"FEATURE\"").unwrap(),
            Category::Feature
        );
    }

    #[test]
    fn category_suggestions_rank_common_aliases() {
        assert_eq!(Category::parse_flexible("doc").unwrap(), Category::Docs);
        assert_eq!(Category::parse_flexible("bug").unwrap(), Category::Bugfix);
        assert_eq!(Category::parse_flexible("dev").unwrap(), Category::Tooling);

        let suggestions = Category::suggestions_for("infr");
        assert_eq!(suggestions.first().copied(), Some(Category::Infra));
    }

    #[test]
    fn test_change_type_serialization() {
        assert_eq!(
            serde_json::to_string(&ChangeType::Create).unwrap(),
            "\"CREATE\""
        );
        assert_eq!(
            serde_json::from_str::<ChangeType>("\"MODIFY\"").unwrap(),
            ChangeType::Modify
        );
    }

    #[test]
    fn test_entry_type_serialization() {
        assert_eq!(
            serde_json::to_string(&EntryType::Implementation).unwrap(),
            "\"IMPLEMENTATION\""
        );
        assert_eq!(
            serde_json::from_str::<EntryType>("\"ARCHITECTURE\"").unwrap(),
            EntryType::Architecture
        );
    }

    #[test]
    fn test_verification_status_serialization() {
        assert_eq!(
            serde_json::to_string(&VerificationStatus::Verified).unwrap(),
            "\"verified\""
        );
        assert_eq!(
            serde_json::from_str::<VerificationStatus>("\"partially_verified\"").unwrap(),
            VerificationStatus::PartiallyVerified
        );
    }

    #[test]
    fn test_verification_basis_serialization() {
        assert_eq!(
            serde_json::to_string(&VerificationBasis::ManualInspection).unwrap(),
            "\"manual_inspection\""
        );
        assert_eq!(
            serde_json::from_str::<VerificationBasis>("\"tests\"").unwrap(),
            VerificationBasis::Tests
        );
    }
}
