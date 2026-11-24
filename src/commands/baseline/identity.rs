//! Baseline payload, identity and canonical-JSON helpers (Track 0601).
//!
//! Every stored byte comes from `serde_json::to_string` (compact) over these
//! structs, with fields declared in the order the schema documents. Emission
//! to stdout is separate and pretty (`crate::output::json::emit`); the two are
//! never mixed, otherwise the payload digest would differ between write and
//! read.

use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};

pub(crate) const BASELINE_SCHEMA: u32 = 1;
pub(crate) const PAYLOAD_KIND: &str = "fileDependencyEdges";

/// One resolved file-to-file dependency pair weighted by its stored rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileEdge {
    pub src_path: String,
    pub dst_path: String,
    pub weight: u32,
}

/// Scope of a baseline. v1 writes only `{"kind":"repo"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct BaselineScope {
    pub kind: String,
}

impl BaselineScope {
    pub(crate) fn repo() -> Self {
        Self {
            kind: "repo".to_string(),
        }
    }
}

/// Completeness state, serialized as `COMPLETE` / `INCOMPLETE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum CompletenessState {
    Complete,
    Incomplete,
}

/// Completeness verdict plus its sorted reason tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct InputCompleteness {
    pub state: CompletenessState,
    pub reasons: Vec<String>,
}

impl InputCompleteness {
    pub(crate) fn complete() -> Self {
        Self {
            state: CompletenessState::Complete,
            reasons: Vec::new(),
        }
    }

    pub(crate) fn incomplete(mut reasons: Vec<String>) -> Self {
        reasons.sort();
        reasons.dedup();
        Self {
            state: CompletenessState::Incomplete,
            reasons,
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.state == CompletenessState::Complete
    }
}

/// Raw counts read from the index. These are row counts, not claims.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct EdgeCoverage {
    pub files_indexed: u64,
    pub files_parse_failed: u64,
    pub edges_resolved_cross_file: u64,
    pub edges_resolved_intra_file: u64,
    pub edges_ambiguous: u64,
    pub edges_unresolved: u64,
    pub edges_capped: u64,
}

/// Identity bound to a baseline payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct BaselineIdentity {
    pub baseline_schema: u32,
    pub payload_kind: String,
    pub scope: BaselineScope,
    pub parser_version: String,
    pub edge_sources: Vec<String>,
    pub ledgerful_version: String,
    pub index_head_hash: Option<String>,
    pub input_digest: String,
    pub input_completeness: InputCompleteness,
}

/// Identity + coverage + canonical edge list for one side of a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileEdgeSet {
    pub identity: BaselineIdentity,
    pub coverage: EdgeCoverage,
    pub edges: Vec<FileEdge>,
}

/// The `payload_json` body: coverage rides here because the table has no
/// coverage column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredPayload {
    pub coverage: EdgeCoverage,
    pub edges: Vec<FileEdge>,
}

/// Compact canonical identity bytes, used for storage and the payload digest.
pub(crate) fn canonical_identity_json(identity: &BaselineIdentity) -> Result<String> {
    serde_json::to_string(identity).into_diagnostic()
}

/// Compact canonical payload bytes, used for storage and the payload digest.
pub(crate) fn canonical_payload_json(payload: &StoredPayload) -> Result<String> {
    serde_json::to_string(payload).into_diagnostic()
}

/// `"blake3:" + hex` over `file_path \0 content_hash \0 parser_version \n`
/// rows ordered bytewise by path.
pub(crate) fn input_digest(rows: &[(String, Option<String>, String)]) -> String {
    let mut sorted: Vec<&(String, Option<String>, String)> = rows.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut hasher = blake3::Hasher::new();
    for (path, content_hash, parser_version) in sorted {
        hasher.update(path.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(content_hash.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0u8]);
        hasher.update(parser_version.as_bytes());
        hasher.update(&[0x0Au8]);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_identity() -> BaselineIdentity {
        BaselineIdentity {
            baseline_schema: BASELINE_SCHEMA,
            payload_kind: PAYLOAD_KIND.to_string(),
            scope: BaselineScope::repo(),
            parser_version: "1".to_string(),
            edge_sources: vec!["native".to_string()],
            ledgerful_version: "0.2.18".to_string(),
            index_head_hash: Some("abc".to_string()),
            input_digest: "blake3:deadbeef".to_string(),
            input_completeness: InputCompleteness::complete(),
        }
    }

    #[test]
    fn baseline_identity_canonical_json_is_compact_camel_case_and_ordered() {
        let json = canonical_identity_json(&sample_identity()).unwrap();
        assert!(
            !json.contains("\n"),
            "canonical form must be compact: {json}"
        );
        assert!(json.starts_with(r#"{"baselineSchema":1,"payloadKind":"fileDependencyEdges""#));
        assert!(json.contains(r#""scope":{"kind":"repo"}"#));
        assert!(
            json.contains(r#""inputCompleteness":{"state":"COMPLETE","reasons":[]}"#),
            "completeness token must be SCREAMING_SNAKE_CASE: {json}"
        );
    }

    #[test]
    fn baseline_identity_incomplete_state_serializes_as_incomplete() {
        let value =
            serde_json::to_value(InputCompleteness::incomplete(vec!["CONTENT_DRIFT".into()]))
                .unwrap();
        assert_eq!(value["state"], "INCOMPLETE");
        assert_eq!(value["reasons"][0], "CONTENT_DRIFT");
    }

    #[test]
    fn baseline_input_digest_is_order_insensitive_and_content_sensitive() {
        let a = (
            "src/a.rs".to_string(),
            Some("hash-a".to_string()),
            "1".to_string(),
        );
        let b = (
            "src/b.rs".to_string(),
            Some("hash-b".to_string()),
            "1".to_string(),
        );
        let forward = input_digest(&[a.clone(), b.clone()]);
        let reversed = input_digest(&[b.clone(), a.clone()]);
        assert_eq!(forward, reversed, "row order must not change the digest");
        assert!(forward.starts_with("blake3:"));

        let changed = input_digest(&[(a.0.clone(), Some("hash-a2".to_string()), a.2.clone())]);
        assert_ne!(
            changed,
            input_digest(&[a]),
            "hash change must change digest"
        );
    }

    #[test]
    fn baseline_input_digest_treats_missing_hash_as_empty() {
        let row = ("src/a.rs".to_string(), None, "1".to_string());
        let empty = ("src/a.rs".to_string(), Some(String::new()), "1".to_string());
        assert_eq!(input_digest(&[row]), input_digest(&[empty]));
    }
}
