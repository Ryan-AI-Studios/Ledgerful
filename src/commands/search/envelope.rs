//! Search machine-output envelope (0136) and BridgeRecord NDJSON lines mode.
//!
//! `--json` → single camelCase envelope (`schemaVersion: 1`).
//! `--json-lines` → legacy NDJSON BridgeRecord stream (pre-0136 `--json`).

use crate::bridge::model::{BridgeDirection, BridgePayload, BridgeRecord, Privacy};
use crate::semantic::{BackendStatus, SemanticReadiness};
use serde::Serialize;

/// Machine JSON mode for `search` (prefer enum over a second bare bool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchJsonMode {
    #[default]
    Off,
    /// Single agent envelope on stdout (`--json`).
    Envelope,
    /// NDJSON BridgeRecord lines (`--json-lines`; legacy pre-0136 `--json`).
    Lines,
}

impl SearchJsonMode {
    #[inline]
    pub fn is_machine(self) -> bool {
        !matches!(self, Self::Off)
    }

    #[inline]
    pub fn is_envelope(self) -> bool {
        matches!(self, Self::Envelope)
    }

    #[inline]
    pub fn is_lines(self) -> bool {
        matches!(self, Self::Lines)
    }
}

/// Wire shape for `search --json` (v1).
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchEnvelope {
    pub schema_version: u32,
    pub query: String,
    pub mode: String,
    pub limit: usize,
    pub truncated: bool,
    pub result_count: usize,
    pub results: Vec<SearchHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_index_status: Option<SearchIndexStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SearchSemantic>,
    /// Set when hybrid empty path used identifier-literal AllPaths fallback.
    /// Wire: `fallbackUsed` (e.g. `"identifier_literal"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_used: Option<String>,
}

/// One match hit under the envelope (status/semantic meta are side fields).
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub kind: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub content: String,
}

/// Empty-index / FTS honesty (typed; never serialize-then-parse Insight content).
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Semantic readiness + optional query error under `--semantic`.
///
/// Residual honesty (0152): when query-time work-root filter drops foreign
/// path hits, `filtered_foreign_count` is set on the envelope only (omit when
/// 0/None; never null). `--json-lines` does **not** carry this field —
/// envelope-only honesty.
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchSemantic {
    pub backend_status: String,
    pub model_name: String,
    pub dimensions: usize,
    pub vector_count: usize,
    pub zero_vector_count: usize,
    pub is_stale: bool,
    pub dimension_mismatch: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Foreign path hits dropped by work-root isolation (0152). Omit when zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filtered_foreign_count: Option<u64>,
}

impl SearchSemantic {
    pub fn from_readiness(readiness: &SemanticReadiness) -> Self {
        Self {
            backend_status: backend_status_wire(readiness.backend_status),
            model_name: readiness.model_name.clone(),
            dimensions: readiness.dimensions,
            vector_count: readiness.vector_count,
            zero_vector_count: readiness.zero_vector_count,
            is_stale: readiness.is_stale,
            dimension_mismatch: readiness.dimension_mismatch,
            error: None,
            filtered_foreign_count: None,
        }
    }

    pub fn with_error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }
}

fn backend_status_wire(status: BackendStatus) -> String {
    match status {
        BackendStatus::NotConfigured => "not_configured".to_string(),
        BackendStatus::Unreachable => "unreachable".to_string(),
        BackendStatus::Ready => "ready".to_string(),
    }
}

/// Arguments for [`SearchCollector::push_hit`] (keeps call sites readable).
#[derive(Debug)]
pub struct HitEmit {
    pub kind: &'static str,
    pub path: String,
    pub line: Option<usize>,
    pub score: Option<f64>,
    pub content: String,
    /// Legacy BridgeRecord content (path-prefixed etc.); used only for lines.
    pub bridge_content: String,
    pub bridge_relevance: f64,
    pub bridge_memory_id: String,
}

/// Collects machine output for envelope mode, or prints NDJSON in lines mode.
#[derive(Debug)]
pub struct SearchCollector {
    mode: SearchJsonMode,
    project_id: String,
    query: String,
    engine_mode: String,
    limit: usize,
    truncated: bool,
    results: Vec<SearchHit>,
    search_index_status: Option<SearchIndexStatus>,
    semantic: Option<SearchSemantic>,
    fallback_used: Option<String>,
}

impl SearchCollector {
    pub fn new(mode: SearchJsonMode, project_id: String, query: String, limit: usize) -> Self {
        Self {
            mode,
            project_id,
            query,
            engine_mode: "bm25".to_string(),
            limit,
            truncated: false,
            results: Vec::new(),
            search_index_status: None,
            semantic: None,
            fallback_used: None,
        }
    }

    #[inline]
    pub fn mode(&self) -> SearchJsonMode {
        self.mode
    }

    #[inline]
    pub fn is_machine(&self) -> bool {
        self.mode.is_machine()
    }

    #[inline]
    pub fn is_envelope(&self) -> bool {
        self.mode.is_envelope()
    }

    #[inline]
    pub fn is_lines(&self) -> bool {
        self.mode.is_lines()
    }

    pub fn set_engine_mode(&mut self, engine_mode: &str) {
        self.engine_mode = engine_mode.to_string();
    }

    pub fn set_truncated(&mut self, truncated: bool) {
        self.truncated = truncated;
    }

    /// Record which search fallback produced hits (envelope `fallbackUsed`).
    pub fn set_fallback_used(&mut self, value: impl Into<String>) {
        self.fallback_used = Some(value.into());
    }

    /// Record a match hit. Lines mode also prints a BridgeRecord immediately.
    pub fn push_hit(&mut self, hit: HitEmit) {
        match self.mode {
            SearchJsonMode::Off => {}
            SearchJsonMode::Envelope => {
                self.results.push(SearchHit {
                    kind: hit.kind.to_string(),
                    path: hit.path,
                    line: hit.line,
                    score: hit.score,
                    content: hit.content,
                });
            }
            SearchJsonMode::Lines => {
                let record = BridgeRecord {
                    bridge_version: BridgeRecord::VERSION.to_string(),
                    direction: BridgeDirection::Outbound,
                    timestamp: chrono::Utc::now(),
                    parent_hash: None,
                    project_id: self.project_id.clone(),
                    session_id: None,
                    tx_id: None,
                    record_kind: hit.kind.to_string(),
                    payload: BridgePayload::Insight {
                        memory_id: hit.bridge_memory_id,
                        relevance: hit.bridge_relevance,
                        content: hit.bridge_content,
                    },
                    privacy: Privacy::ProjectLocal,
                };
                Self::print_bridge(&record);
            }
        }
    }

    /// Whether envelope mode already has a `searchIndexStatus` (single slot).
    #[inline]
    pub fn has_search_index_status(&self) -> bool {
        self.search_index_status.is_some()
    }

    pub fn set_search_index_status(&mut self, status: SearchIndexStatus) {
        match self.mode {
            SearchJsonMode::Off => {}
            SearchJsonMode::Envelope => {
                self.search_index_status = Some(status);
            }
            SearchJsonMode::Lines => {
                // Legacy: embed status JSON in Insight content (snake_case keys).
                let mut content = serde_json::json!({
                    "state": status.state,
                });
                if let Some(n) = status.document_count {
                    content["document_count"] = serde_json::json!(n);
                }
                if let Some(rem) = &status.remediation {
                    content["remediation"] = serde_json::Value::String(rem.clone());
                }
                if let Some(err) = &status.error {
                    content["error"] = serde_json::Value::String(err.clone());
                }
                let content_str = serde_json::to_string(&content).unwrap_or_else(|_| "{}".into());
                let record = BridgeRecord {
                    bridge_version: BridgeRecord::VERSION.to_string(),
                    direction: BridgeDirection::Outbound,
                    timestamp: chrono::Utc::now(),
                    parent_hash: None,
                    project_id: self.project_id.clone(),
                    session_id: None,
                    tx_id: None,
                    record_kind: "search_index_status".to_string(),
                    payload: BridgePayload::Insight {
                        memory_id: "search_index_status".to_string(),
                        relevance: 0.0,
                        content: content_str,
                    },
                    privacy: Privacy::ProjectLocal,
                };
                Self::print_bridge(&record);
            }
        }
    }

    pub fn set_semantic_readiness(&mut self, readiness: &SemanticReadiness) {
        match self.mode {
            SearchJsonMode::Off => {}
            SearchJsonMode::Envelope => {
                self.semantic = Some(SearchSemantic::from_readiness(readiness));
            }
            SearchJsonMode::Lines => {
                let content = serde_json::to_string(readiness).unwrap_or_default();
                let record = BridgeRecord {
                    bridge_version: BridgeRecord::VERSION.to_string(),
                    direction: BridgeDirection::Outbound,
                    timestamp: chrono::Utc::now(),
                    parent_hash: None,
                    project_id: self.project_id.clone(),
                    session_id: None,
                    tx_id: None,
                    record_kind: "semantic_readiness".to_string(),
                    payload: BridgePayload::Insight {
                        memory_id: "readiness".to_string(),
                        relevance: 1.0,
                        content,
                    },
                    privacy: Privacy::ProjectLocal,
                };
                Self::print_bridge(&record);
            }
        }
    }

    pub fn set_semantic_error(&mut self, failure_msg: String) {
        match self.mode {
            SearchJsonMode::Off => {}
            SearchJsonMode::Envelope => {
                if let Some(sem) = self.semantic.as_mut() {
                    sem.error = Some(failure_msg);
                } else {
                    self.semantic = Some(SearchSemantic {
                        backend_status: "unknown".to_string(),
                        model_name: String::new(),
                        dimensions: 0,
                        vector_count: 0,
                        zero_vector_count: 0,
                        is_stale: false,
                        dimension_mismatch: false,
                        error: Some(failure_msg),
                        filtered_foreign_count: None,
                    });
                }
            }
            SearchJsonMode::Lines => {
                let record = BridgeRecord {
                    bridge_version: BridgeRecord::VERSION.to_string(),
                    direction: BridgeDirection::Outbound,
                    timestamp: chrono::Utc::now(),
                    parent_hash: None,
                    project_id: self.project_id.clone(),
                    session_id: None,
                    tx_id: None,
                    record_kind: "semantic_error".to_string(),
                    payload: BridgePayload::Insight {
                        memory_id: "semantic_error".to_string(),
                        relevance: 0.0,
                        content: failure_msg,
                    },
                    privacy: Privacy::ProjectLocal,
                };
                Self::print_bridge(&record);
            }
        }
    }

    /// Set residual foreign-path filter count (0152). Envelope-only; omit when 0.
    /// No-op for Off / Lines (lines mode does not carry residual honesty).
    pub fn set_filtered_foreign_count(&mut self, count: u64) {
        if count == 0 {
            return;
        }
        if !self.mode.is_envelope() {
            return;
        }
        if let Some(sem) = self.semantic.as_mut() {
            sem.filtered_foreign_count = Some(count);
        } else {
            self.semantic = Some(SearchSemantic {
                backend_status: "unknown".to_string(),
                model_name: String::new(),
                dimensions: 0,
                vector_count: 0,
                zero_vector_count: 0,
                is_stale: false,
                dimension_mismatch: false,
                error: None,
                filtered_foreign_count: Some(count),
            });
        }
    }

    /// Build and emit the envelope once. No-op for Off / Lines.
    pub fn finish(self) {
        if !self.mode.is_envelope() {
            return;
        }
        let envelope = SearchEnvelope {
            schema_version: 1,
            query: self.query,
            mode: self.engine_mode,
            limit: self.limit,
            truncated: self.truncated,
            result_count: self.results.len(),
            results: self.results,
            search_index_status: self.search_index_status,
            semantic: self.semantic,
            fallback_used: self.fallback_used,
        };
        match serde_json::to_string(&envelope) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                // Never leave agents with partial multi-doc output; stderr only.
                eprintln!("search --json: failed to serialize envelope: {e}");
            }
        }
    }

    fn print_bridge(record: &BridgeRecord) {
        match serde_json::to_string(record) {
            Ok(s) => println!("{s}"),
            Err(_) => println!("{{}}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_omits_optional_nulls() {
        let env = SearchEnvelope {
            schema_version: 1,
            query: "q".into(),
            mode: "bm25".into(),
            limit: 10,
            truncated: false,
            result_count: 1,
            results: vec![SearchHit {
                kind: "bm25_match".into(),
                path: "a.rs".into(),
                line: None,
                score: Some(1.5),
                content: "plain".into(),
            }],
            search_index_status: None,
            semantic: None,
            fallback_used: None,
        };
        let s = serde_json::to_string(&env).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["resultCount"], 1);
        assert!(v.get("searchIndexStatus").is_none());
        assert!(v.get("semantic").is_none());
        assert!(v.get("fallbackUsed").is_none());
        assert!(v["results"][0].get("line").is_none());
        assert!(v["results"][0].get("score").is_some());
        assert!(!s.contains("null"));
    }

    /// 0152: `filteredForeignCount` omit-when-zero / None; never null; camelCase when set.
    #[test]
    fn envelope_omits_filtered_foreign_count_when_zero_or_none() {
        let mut sem = SearchSemantic::from_readiness(&SemanticReadiness {
            backend_status: BackendStatus::Ready,
            model_name: "test".into(),
            dimensions: 384,
            vector_count: 10,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        });
        // Default None → key absent
        let s = serde_json::to_string(&sem).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert!(v.get("filteredForeignCount").is_none());
        assert!(!s.contains("null"));
        assert!(!s.contains("filtered_foreign_count"));

        // Explicit None still omitted
        sem.filtered_foreign_count = None;
        let s = serde_json::to_string(&sem).expect("serialize");
        assert!(!s.contains("filteredForeignCount"));

        // Non-zero present as camelCase
        sem.filtered_foreign_count = Some(3);
        let s = serde_json::to_string(&sem).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(v["filteredForeignCount"], 3);
        assert!(!s.contains("filtered_foreign_count"));
        assert!(!s.contains("null"));
    }

    #[test]
    fn collector_set_filtered_foreign_count_zero_is_noop() {
        let mut c = SearchCollector::new(SearchJsonMode::Envelope, "proj".into(), "q".into(), 10);
        c.set_semantic_readiness(&SemanticReadiness {
            backend_status: BackendStatus::Ready,
            model_name: "m".into(),
            dimensions: 3,
            vector_count: 1,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        });
        c.set_filtered_foreign_count(0);
        let sem = c.semantic.as_ref().expect("semantic set");
        assert!(sem.filtered_foreign_count.is_none());
        c.set_filtered_foreign_count(2);
        assert_eq!(c.semantic.as_ref().unwrap().filtered_foreign_count, Some(2));
    }

    #[test]
    fn empty_envelope_round_trip() {
        let env = SearchEnvelope {
            schema_version: 1,
            query: "zzzz".into(),
            mode: "bm25".into(),
            limit: 5,
            truncated: false,
            result_count: 0,
            results: vec![],
            search_index_status: None,
            semantic: None,
            fallback_used: None,
        };
        let s = serde_json::to_string(&env).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(v["results"], serde_json::json!([]));
        assert_eq!(v["resultCount"], 0);
    }

    #[test]
    fn envelope_fallback_used_camel_case() {
        let env = SearchEnvelope {
            schema_version: 1,
            query: "verify_step_key".into(),
            mode: "hybrid".into(),
            limit: 5,
            truncated: false,
            result_count: 1,
            results: vec![SearchHit {
                kind: "regex_match".into(),
                path: "a.rs".into(),
                line: Some(1),
                score: Some(1.0),
                content: "fn verify_step_key".into(),
            }],
            search_index_status: None,
            semantic: None,
            fallback_used: Some("identifier_literal".into()),
        };
        let s = serde_json::to_string(&env).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(v["fallbackUsed"], "identifier_literal");
        assert!(!s.contains("fallback_used"));
    }

    #[test]
    fn index_status_camel_case() {
        let status = SearchIndexStatus {
            state: "was_empty".into(),
            document_count: Some(3),
            remediation: None,
            error: None,
        };
        let s = serde_json::to_string(&status).expect("serialize");
        assert!(s.contains("documentCount"));
        assert!(!s.contains("document_count"));
    }

    #[test]
    fn search_json_mode_machine() {
        assert!(!SearchJsonMode::Off.is_machine());
        assert!(SearchJsonMode::Envelope.is_machine());
        assert!(SearchJsonMode::Lines.is_machine());
    }
}
