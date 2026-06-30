use crate::index::symbols::SymbolKind;
use crate::ledger::types::{Category, ChangeType, EntryType};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublicInterface {
    pub symbol: String,
    pub file: String,
    pub kind: SymbolKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FederatedLedgerEntry {
    pub tx_id: String,
    pub category: Category,
    pub entry_type: EntryType,
    pub entity: String,
    pub change_type: ChangeType,
    pub summary: String,
    pub reason: String,
    pub is_breaking: bool,
    pub committed_at: String,
    // M8 added `author`; `#[serde(default)]` keeps us forward-compat
    // with pre-M8 siblings that serialized entries without this field
    // (see `output/m8-opencode-1.md` H2). On import, empty strings are
    // coalesced to `"unknown"` to match the m43 `DEFAULT 'unknown'`.
    #[serde(default)]
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedSchema {
    pub schema_version: String,
    pub repo_name: String,
    pub public_interfaces: Vec<PublicInterface>,
    pub ledger: Option<Vec<FederatedLedgerEntry>>,
    /// ISO-8601 (RFC 3339) timestamp of when this schema.json was last
    /// exported. `#[serde(default)]` keeps old schema.json files (written
    /// before TA31) deserializing with an empty string (TA31 R4).
    #[serde(default)]
    pub generated_at: String,
    /// `CARGO_PKG_VERSION` of the binary that produced this schema.json.
    /// `#[serde(default)]` keeps old schema.json files deserializing with
    /// an empty string (TA31 R4).
    #[serde(default)]
    pub binary_version: String,
}

impl FederatedSchema {
    /// Current on-the-wire schema version. Bumped to `"1.1"` in the
    /// M8 opencode-1 resolve to advertise the new `author` field. We
    /// still accept `"1.0"` siblings in `validate()` for
    /// backward-compat: the `#[serde(default)]` on
    /// `FederatedLedgerEntry.author` makes the wire format
    /// forward-compatible regardless of the version string, but the
    /// self-describing version is bumped so a future reader can detect
    /// pre-M8 vs post-M8 packets without trying to deserialize the
    /// entries first.
    pub const VERSION: &'static str = "1.1";

    /// Legacy version published by pre-M8 binaries (no `author` field
    /// on `FederatedLedgerEntry`). `validate()` still accepts this
    /// version — the `#[serde(default)]` attribute above makes the
    /// `author` field optional on the wire, so a `"1.0"` packet
    /// deserializes with `author == ""` and the import path
    /// (`import_federated_entries`) coalesces that to `"unknown"` to
    /// match the m43 `DEFAULT 'unknown'`.
    pub const LEGACY_VERSION: &'static str = "1.0";

    /// Splits validation findings into hard errors (security/compat
    /// violations that must always reject the schema) and warnings
    /// (data-quality issues that can be surfaced without rejecting the
    /// sibling).
    ///
    /// TA31 R1: callers that need the old strict all-or-nothing behavior
    /// should keep using `validate()`, which errors if *either* vec is
    /// non-empty. `scan_siblings()` calls this directly so it can keep
    /// discovering a sibling whose only problems are data-quality
    /// warnings (e.g. an empty `entity` on one ledger entry — the
    /// AI-Brains case) while still hard-rejecting security violations
    /// (path traversal, absolute paths) and schema_version mismatches.
    pub fn validation_issues(&self) -> (Vec<String>, Vec<String>) {
        let mut hard_errors = Vec::new();
        let mut warnings = Vec::new();

        if self.schema_version != Self::VERSION && self.schema_version != Self::LEGACY_VERSION {
            hard_errors.push(format!(
                "Unsupported schema version: {}. Expected: {} (or legacy {})",
                self.schema_version,
                Self::VERSION,
                Self::LEGACY_VERSION
            ));
        }
        if self.repo_name.trim().is_empty() {
            warnings.push("Invalid schema: repo_name must not be empty".to_string());
        }
        for interface in &self.public_interfaces {
            if interface.symbol.trim().is_empty() {
                warnings
                    .push("Invalid schema: public interface symbol must not be empty".to_string());
            }
            if interface.file.trim().is_empty() {
                warnings
                    .push("Invalid schema: public interface file must not be empty".to_string());
            }
        }
        if let Some(entries) = &self.ledger {
            for entry in entries {
                if entry.tx_id.trim().is_empty() {
                    warnings.push("Invalid schema: ledger tx_id must not be empty".to_string());
                }
                if entry.entity.trim().is_empty() {
                    warnings.push("Invalid schema: ledger entity must not be empty".to_string());
                }
                let entity_path = Path::new(&entry.entity);
                let is_traversal = entity_path.components().any(|c| c == Component::ParentDir);
                let is_absolute = entity_path.is_absolute();
                if is_traversal {
                    hard_errors.push(format!(
                        "Security violation: ledger entity '{}' contains path traversal",
                        entry.entity
                    ));
                }
                if is_absolute {
                    hard_errors.push(format!(
                        "Security violation: ledger entity '{}' is an absolute path",
                        entry.entity
                    ));
                }
            }
        }

        (hard_errors, warnings)
    }

    /// Strict all-or-nothing validation: errors if there is *any* hard
    /// error or warning. Preserves pre-TA31 behavior for callers that
    /// have not opted into the warning/hard-error split (`impact.rs`,
    /// and the existing integration test suite).
    pub fn validate(&self) -> miette::Result<()> {
        let (hard_errors, warnings) = self.validation_issues();
        if !hard_errors.is_empty() {
            return Err(miette::miette!(hard_errors.join("; ")));
        }
        if !warnings.is_empty() {
            return Err(miette::miette!(warnings.join("; ")));
        }
        Ok(())
    }

    pub fn new(repo_name: String, mut public_interfaces: Vec<PublicInterface>) -> Self {
        // Engineering standard: deterministic sorting
        public_interfaces.sort();
        Self {
            schema_version: Self::VERSION.to_string(),
            repo_name,
            public_interfaces,
            ledger: None,
            generated_at: String::new(),
            binary_version: String::new(),
        }
    }

    pub fn with_ledger(mut self, mut ledger: Vec<FederatedLedgerEntry>) -> Self {
        ledger.sort();
        self.ledger = Some(ledger);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TA31 R4: a schema.json written before `generated_at`/`binary_version`
    /// existed must still deserialize, with both new fields defaulting to
    /// the empty string via `#[serde(default)]`.
    #[test]
    fn old_format_json_deserializes_with_empty_defaults() {
        let old_format_json = r#"{
            "schema_version": "1.1",
            "repo_name": "old-sibling",
            "public_interfaces": [],
            "ledger": null
        }"#;

        let parsed: FederatedSchema =
            serde_json::from_str(old_format_json).expect("old-format schema must deserialize");

        assert_eq!(parsed.generated_at, "");
        assert_eq!(parsed.binary_version, "");
        assert_eq!(parsed.repo_name, "old-sibling");
    }

    /// TA31 R1: `validation_issues()` must classify an empty `repo_name`,
    /// empty interface symbol/file, and empty ledger tx_id/entity as
    /// warnings, not hard errors — these are data-quality problems, not
    /// security or compatibility violations.
    #[test]
    fn empty_entity_is_a_warning_not_a_hard_error() {
        let schema = FederatedSchema {
            schema_version: FederatedSchema::VERSION.to_string(),
            repo_name: "ai-brains".to_string(),
            public_interfaces: vec![],
            ledger: Some(vec![FederatedLedgerEntry {
                tx_id: "tx-1".to_string(),
                category: Category::Feature,
                entry_type: EntryType::Implementation,
                entity: String::new(),
                change_type: ChangeType::Create,
                summary: "summary".to_string(),
                reason: "reason".to_string(),
                is_breaking: false,
                committed_at: "2026-06-24T00:00:00Z".to_string(),
                author: String::new(),
            }]),
            generated_at: String::new(),
            binary_version: String::new(),
        };

        let (hard_errors, warnings) = schema.validation_issues();
        assert!(
            hard_errors.is_empty(),
            "empty entity must not be a hard error, got {:?}",
            hard_errors
        );
        assert!(
            warnings.iter().any(|w| w.contains("entity")),
            "expected a warning mentioning 'entity', got {:?}",
            warnings
        );

        // The strict `validate()` wrapper must still reject (preserves
        // existing all-or-nothing behavior for callers like impact.rs).
        assert!(schema.validate().is_err());
    }

    /// TA31 R1: path traversal in a ledger entity must remain a hard
    /// error — this is the security boundary `scan_siblings()` must
    /// keep enforcing even after loosening data-quality checks.
    #[test]
    fn path_traversal_entity_is_a_hard_error() {
        let schema = FederatedSchema {
            schema_version: FederatedSchema::VERSION.to_string(),
            repo_name: "malicious-sibling".to_string(),
            public_interfaces: vec![],
            ledger: Some(vec![FederatedLedgerEntry {
                tx_id: "tx-1".to_string(),
                category: Category::Feature,
                entry_type: EntryType::Implementation,
                entity: "../outside.rs".to_string(),
                change_type: ChangeType::Create,
                summary: "summary".to_string(),
                reason: "reason".to_string(),
                is_breaking: false,
                committed_at: "2026-06-24T00:00:00Z".to_string(),
                author: String::new(),
            }]),
            generated_at: String::new(),
            binary_version: String::new(),
        };

        let (hard_errors, _warnings) = schema.validation_issues();
        assert!(
            hard_errors.iter().any(|e| e.contains("path traversal")),
            "expected a hard error mentioning 'path traversal', got {:?}",
            hard_errors
        );
        assert!(schema.validate().is_err());
    }

    /// TA31 R1: an unsupported schema_version must remain a hard error.
    #[test]
    fn unsupported_schema_version_is_a_hard_error() {
        let schema = FederatedSchema::new("repo".to_string(), vec![]);
        let mut schema = schema;
        schema.schema_version = "2.0".to_string();

        let (hard_errors, _warnings) = schema.validation_issues();
        assert!(
            hard_errors
                .iter()
                .any(|e| e.contains("Unsupported schema version")),
            "expected a hard error about unsupported schema version, got {:?}",
            hard_errors
        );
        assert!(schema.validate().is_err());
    }
}
