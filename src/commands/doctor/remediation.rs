//! Pure remediation builders for doctor findings (0125).
//!
//! Builders take already-resolved inputs (hex, counts) and emit both
//! `message` and structured `remediation` without I/O, so unit tests stay
//! deterministic and message/remediation cannot drift.

use super::SIG_PIN_WARNING;
use super::finding::{DoctorCategory, DoctorFinding, DoctorSeverity};

/// Build the `sig-pin` finding when `intent.trusted_public_keys` is empty.
///
/// When `pub_key_hex` is `Some`, remediation includes a PowerShell-safe
/// `config set` with **outer single quotes** around the key=value argument.
/// When `None`, never invent a hex; remediation describes honest next steps.
pub fn build_sig_pin_finding(pub_key_hex: Option<&str>) -> DoctorFinding {
    match pub_key_hex {
        Some(hex) => {
            let remediation = format!(
                "ledgerful config set 'intent.trusted_public_keys=[\"{hex}\"]'\n\
                 ledgerful doctor --json\n\
                 ledgerful verify --signatures"
            );
            DoctorFinding {
                code: "sig-pin".to_string(),
                severity: DoctorSeverity::Warn,
                category: DoctorCategory::Signing,
                message: format!(
                    "{SIG_PIN_WARNING} Next: pin the current identity via config set (see remediation)."
                ),
                remediation: Some(remediation),
            }
        }
        None => {
            let remediation = "\
ledgerful init
# or complete first signing so ~/.ledgerful/keys/public.pem exists, then:
ledgerful doctor --json
# follow the sig-pin remediation once the public key is readable"
                .to_string();
            DoctorFinding {
                code: "sig-pin".to_string(),
                severity: DoctorSeverity::Warn,
                category: DoctorCategory::Signing,
                message: format!(
                    "{SIG_PIN_WARNING} Local signing identity not found under ~/.ledgerful/keys; complete init/signing before pinning."
                ),
                remediation: Some(remediation),
            }
        }
    }
}

/// Build the `sig-version` finding when `min_sig_version < 2`.
///
/// `v1_count` is the number of LOCAL rows with `sig_version < 2` when known.
/// On SQL error pass `None` — remediation is still Some and must not claim
/// a false zero count.
pub fn build_sig_version_finding(min_sig_version: u32, v1_count: Option<i64>) -> DoctorFinding {
    let (message, remediation) = match v1_count {
        Some(n) if n > 0 => {
            let message = format!(
                "intent.min_sig_version={min_sig_version} still accepts legacy v1 signatures. \
                 {n} LOCAL row(s) have sig_version < 2. Upgrade with `ledger re-sign --all`, \
                 then set min_sig_version=2 to close the downgrade path."
            );
            let remediation = "\
ledgerful ledger re-sign --all --dry-run
ledgerful ledger re-sign --all --yes
ledgerful config set intent.min_sig_version=2
ledgerful verify --signatures"
                .to_string();
            (message, remediation)
        }
        Some(0) => {
            let message = format!(
                "intent.min_sig_version={min_sig_version} still accepts legacy v1 signatures. \
                 All LOCAL rows already have sig_version >= 2; set min_sig_version=2 to close \
                 the downgrade path."
            );
            let remediation = "\
ledgerful config set intent.min_sig_version=2
ledgerful verify --signatures"
                .to_string();
            (message, remediation)
        }
        // None, or defensive catch-all for unexpected negative counts
        _ => {
            let message = format!(
                "intent.min_sig_version={min_sig_version} still accepts legacy v1 signatures. \
                 After upgrading any remaining v1 rows with `ledger re-sign --all`, set \
                 min_sig_version=2 to close the downgrade path."
            );
            let remediation = "\
ledgerful ledger re-sign --all --dry-run
ledgerful ledger re-sign --all --yes
ledgerful config set intent.min_sig_version=2
ledgerful verify --signatures"
                .to_string();
            (message, remediation)
        }
    };

    DoctorFinding {
        code: "sig-version".to_string(),
        severity: DoctorSeverity::Warn,
        category: DoctorCategory::Signing,
        message,
        remediation: Some(remediation),
    }
}

/// Normative human Index Health line when the search index exists but has
/// zero documents (0126). Must never contain the substring `OK`.
pub fn search_empty_index_health_line() -> &'static str {
    "Search index: Empty (0 documents — run 'ledgerful index')"
}

/// Pure classification of a clean Tantivy `document_count` for doctor (0126).
///
/// Call only after open+integrity succeed. Missing / load-failed / corrupt are
/// separate probe arms and never reach this helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchDocsClassification {
    /// `docs == 0` → `search-empty` finding + non-OK health line.
    Empty,
    /// `docs > 0` → healthy OK health line only (no finding).
    Populated { docs: usize },
}

/// Classify document count for the doctor success arm. Pure: no I/O.
pub fn classify_search_document_count(docs: usize) -> SearchDocsClassification {
    if docs == 0 {
        SearchDocsClassification::Empty
    } else {
        SearchDocsClassification::Populated { docs }
    }
}

/// Build the `search-empty` finding when Tantivy opens clean with 0 documents.
///
/// Mutually exclusive with search-missing / search-load-failed / search-corrupt
/// and with the healthy `OK (N documents)` index_health line when N > 0.
pub fn build_search_empty_finding() -> DoctorFinding {
    let remediation = "\
ledgerful index
# first search also rebuilds when empty:
# ledgerful search \"<query>\"
ledgerful doctor --json"
        .to_string();
    DoctorFinding::warn(
        "search-empty",
        DoctorCategory::Index,
        "Search index: present but empty (0 documents); full-text search unusable until populated",
    )
    .with_remediation(remediation)
}

/// Healthy human Index Health line when `docs > 0`.
pub fn search_ok_index_health_line(docs: usize) -> String {
    format!("Search index: OK ({docs} documents)")
}

// ── 0133 Graph Index Health (age + content) ───────────────────────────────

/// Pure age-path inputs for [`classify_graph_index_health`] (from
/// `check_index_staleness` → `StalenessWarning` fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphAgeInputs {
    /// Never indexed / missing floor → `graph-empty`.
    pub is_missing: bool,
    /// Age-stale file count shown in `graph-stale` message.
    pub stale_files: usize,
}

/// Pure content-hash drift inputs (from `count_content_hash_drift`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentHashDriftInputs {
    /// `changed_or_unindexed` from drift walk; dirty when > 0.
    pub changed_or_unindexed: usize,
}

impl ContentHashDriftInputs {
    pub fn is_dirty(&self) -> bool {
        self.changed_or_unindexed > 0
    }
}

/// SQLite-floor Graph Index Health decision (0133). Mutually exclusive variants.
///
/// Orthogonal Cozo-native findings (`graph-error`, `graph-not-initialized`) are
/// **not** classified here — they co-occur on a separate axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphIndexHealth {
    /// Age path: never indexed → `graph-empty`.
    AgeEmpty,
    /// Age path: time-stale → `graph-stale` only (content drift not evaluated).
    AgeStale { stale_files: usize },
    /// Age-fresh + content-hash dirty → `graph-content-stale` (N files).
    ContentStale { n: usize },
    /// Age-fresh + drift walk failed → `graph-drift-check-failed` (never Current).
    DriftCheckFailed { truncated_err: String },
    /// Age-fresh + content-clean + Cozo has nodes/edges → success Current.
    CurrentPopulated,
    /// Age-fresh + content-clean + Cozo empty/None counts → analyze-graph Current hint.
    CurrentEmptyCozo,
}

/// Classify doctor Graph Index Health from pure inputs (0126-style).
///
/// **Age first (STOP):** when `age` is `Some`, return AgeEmpty / AgeStale and
/// **never** inspect `drift` (avoids double findings + wasted decision branches).
///
/// **Else** evaluate `drift`:
/// - `None` — drift not evaluated (caller wiring); fall through to Current* by Cozo counts
/// - `Ok(dirty)` → ContentStale
/// - `Ok(clean)` → CurrentPopulated / CurrentEmptyCozo by `total_nodes`/`total_edges`
/// - `Err` → DriftCheckFailed with display truncated to 80 chars
pub fn classify_graph_index_health(
    age: Option<&GraphAgeInputs>,
    drift: Option<Result<ContentHashDriftInputs, String>>,
    total_nodes: i64,
    total_edges: i64,
) -> GraphIndexHealth {
    if let Some(age) = age {
        if age.is_missing {
            return GraphIndexHealth::AgeEmpty;
        }
        return GraphIndexHealth::AgeStale {
            stale_files: age.stale_files,
        };
    }

    match drift {
        Some(Ok(d)) if d.is_dirty() => GraphIndexHealth::ContentStale {
            n: d.changed_or_unindexed,
        },
        Some(Err(e)) => GraphIndexHealth::DriftCheckFailed {
            truncated_err: e.chars().take(80).collect(),
        },
        Some(Ok(_)) | None => {
            if total_nodes == 0 && total_edges == 0 {
                GraphIndexHealth::CurrentEmptyCozo
            } else {
                GraphIndexHealth::CurrentPopulated
            }
        }
    }
}

/// Max display length for drift-check error messages (matches completion probe).
const GRAPH_DRIFT_ERR_DISPLAY_CHARS: usize = 80;

/// Build `graph-content-stale` when age-fresh index has content-hash drift.
///
/// Message includes N and greppable content/drift/stale vocabulary. Severity
/// Warn / Index — does **not** flip `readyForPublish`.
pub fn build_graph_content_stale_finding(n: usize) -> DoctorFinding {
    let remediation = "\
ledgerful index --incremental
ledgerful index --check --json
ledgerful doctor --json"
        .to_string();
    DoctorFinding::warn(
        "graph-content-stale",
        DoctorCategory::Index,
        format!(
            "Graph state: content-stale ({n} files with content drift) — run 'ledgerful index --incremental'"
        ),
    )
    .with_remediation(remediation)
}

/// Human Index Health line for content-hash drift. Must **not** contain bare success `Current`.
pub fn graph_content_stale_index_health_line(n: usize) -> String {
    format!("Graph state: Content-stale ({n} files) - run 'ledgerful index --incremental'")
}

/// Build `graph-drift-check-failed` when the content-hash walk errors.
///
/// Display message truncates `err` with `chars().take(80)`; log the full error
/// at the call site via `tracing::debug!`.
pub fn build_graph_drift_check_failed_finding(err: &str) -> DoctorFinding {
    let truncated: String = err.chars().take(GRAPH_DRIFT_ERR_DISPLAY_CHARS).collect();
    let remediation = "\
ledgerful index --check --json
ledgerful index --incremental
ledgerful doctor --json"
        .to_string();
    DoctorFinding::warn(
        "graph-drift-check-failed",
        DoctorCategory::Index,
        format!("Graph state: drift check failed ({truncated})"),
    )
    .with_remediation(remediation)
}

/// Human Index Health line when drift check failed. Must **not** claim `Current`.
pub fn graph_drift_check_failed_index_health_line() -> &'static str {
    "Graph state: Drift check failed — run 'ledgerful index --check'"
}

/// Success Index Health when age-fresh, content-clean, and Cozo has nodes/edges.
pub fn graph_current_populated_index_health_line() -> &'static str {
    "Graph state: Current"
}

/// Success Index Health when age-fresh, content-clean, Cozo empty (analyze-graph hint).
pub fn graph_current_empty_cozo_index_health_line() -> &'static str {
    "Graph state: Current (run 'ledgerful index --analyze-graph' to populate the knowledge graph)"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::finding::{DoctorSeverity, dashboard_failures, ready_for_publish};

    #[test]
    fn sig_pin_some_hex_has_outer_single_quotes_and_hex() {
        let hex = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
        let f = build_sig_pin_finding(Some(hex));
        assert_eq!(f.code, "sig-pin");
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert_eq!(f.category, DoctorCategory::Signing);
        let rem = f.remediation.as_deref().expect("remediation Some");
        assert!(
            rem.contains(&format!("'intent.trusted_public_keys=[\"{hex}\"]'")),
            "outer single quotes + hex required: {rem}"
        );
        assert!(rem.contains("ledgerful config set"));
        assert!(rem.contains("ledgerful doctor --json"));
        assert!(rem.contains("ledgerful verify --signatures"));
        // Vocabulary anchors still present in message
        let msg_lc = f.message.to_ascii_lowercase();
        assert!(msg_lc.contains("unknown key"));
        assert!(msg_lc.contains("pin") || f.message.contains("Pin"));
        assert!(msg_lc.contains("trusted"));
    }

    #[test]
    fn sig_pin_none_hex_never_invents_hex() {
        let f = build_sig_pin_finding(None);
        assert_eq!(f.code, "sig-pin");
        let rem = f
            .remediation
            .as_deref()
            .expect("remediation preferred Some");
        // No 64-char hex blob invented
        let has_long_hex = rem.split_whitespace().any(|tok| {
            let cleaned = tok.trim_matches(|c: char| !c.is_ascii_hexdigit());
            cleaned.len() == 64 && cleaned.chars().all(|c| c.is_ascii_hexdigit())
        });
        assert!(
            !has_long_hex,
            "must not invent a public key hex when missing: {rem}"
        );
        let blob = format!("{} {}", f.message, rem).to_ascii_lowercase();
        assert!(
            blob.contains("key") || blob.contains("init") || blob.contains("missing"),
            "should mention keys/identity missing or init: {blob}"
        );
    }

    #[test]
    fn sig_version_count_gt_zero_orders_re_sign_before_min_sig() {
        let f = build_sig_version_finding(1, Some(12));
        assert_eq!(f.code, "sig-version");
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert!(f.message.contains("12"));
        let rem = f.remediation.as_deref().expect("remediation Some");
        let re_sign_pos = rem
            .find("ledger re-sign --all")
            .expect("must mention re-sign --all");
        let min_sig_pos = rem
            .find("intent.min_sig_version=2")
            .expect("must set min_sig_version=2");
        assert!(
            re_sign_pos < min_sig_pos,
            "re-sign --all must come before min_sig_version=2: {rem}"
        );
        assert!(rem.contains("re-sign --all --dry-run"));
        assert!(rem.contains("re-sign --all --yes"));
        assert!(rem.contains("verify --signatures"));
    }

    #[test]
    fn sig_version_count_zero_is_config_only() {
        let f = build_sig_version_finding(1, Some(0));
        let rem = f.remediation.as_deref().expect("remediation Some");
        assert!(
            !rem.contains("re-sign"),
            "count==0 must not recommend re-sign: {rem}"
        );
        assert!(rem.contains("intent.min_sig_version=2"));
        assert!(rem.contains("verify --signatures"));
        assert!(!f.message.contains("0 LOCAL") || f.message.contains("already"));
    }

    #[test]
    fn sig_version_count_none_still_some_remediation_no_false_zero() {
        let f = build_sig_version_finding(1, None);
        let rem = f.remediation.expect("remediation always Some");
        assert!(rem.contains("re-sign --all") || rem.contains("min_sig_version=2"));
        // Must not claim a false "0 v1" count
        assert!(
            !f.message.contains("0 LOCAL") && !f.message.contains("0 row"),
            "must not claim false zero count: {}",
            f.message
        );
    }

    #[test]
    fn sig_version_never_emits_nonexistent_flag_name_alone() {
        // The flag is real now (`--all`); ensure remediation uses full command path.
        let f = build_sig_version_finding(1, Some(3));
        let rem = f.remediation.unwrap();
        assert!(rem.contains("ledgerful ledger re-sign --all"));
    }

    // ── 0126 search-empty ────────────────────────────────────────────────

    #[test]
    fn search_empty_finding_is_warn_index_with_remediation() {
        let f = build_search_empty_finding();
        assert_eq!(f.code, "search-empty");
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert_eq!(f.category, DoctorCategory::Index);
        let rem = f.remediation.as_deref().expect("remediation Some");
        assert!(
            rem.contains("ledgerful index"),
            "remediation must contain exact ledgerful index: {rem}"
        );
        assert!(
            !f.message.contains("OK"),
            "message must not claim OK when empty: {}",
            f.message
        );
        assert!(
            f.message.to_ascii_lowercase().contains("empty")
                || f.message.to_ascii_lowercase().contains("unusable"),
            "message should diagnose empty/unusable: {}",
            f.message
        );
    }

    #[test]
    fn search_empty_health_line_has_no_ok_substring() {
        let line = search_empty_index_health_line();
        assert!(
            !line.contains("OK"),
            "empty index_health must not contain OK: {line}"
        );
        assert!(
            line.contains("Empty") || line.contains("empty"),
            "should say Empty: {line}"
        );
        assert!(
            line.contains("ledgerful index") || line.contains("'ledgerful index'"),
            "should point at ledgerful index: {line}"
        );
        // Grep-gate: never re-introduce healthy OK-with-zero wording.
        assert!(!line.contains("OK (0 documents)"));
    }

    #[test]
    fn search_empty_serde_remediation_serializes() {
        let f = build_search_empty_finding();
        let v = serde_json::to_value(&f).expect("serialize");
        assert_eq!(v["code"], "search-empty");
        assert_eq!(v["severity"], "warn");
        assert_eq!(v["category"], "index");
        assert!(
            v["remediation"]
                .as_str()
                .expect("remediation present")
                .contains("ledgerful index")
        );
    }

    #[test]
    fn classify_search_document_count_zero_is_empty() {
        assert_eq!(
            classify_search_document_count(0),
            SearchDocsClassification::Empty
        );
        let f = build_search_empty_finding();
        assert_eq!(f.code, "search-empty");
        assert!(f.remediation.is_some());
        let line = search_empty_index_health_line();
        assert!(!line.contains("OK"));
    }

    #[test]
    fn classify_search_document_count_positive_is_populated_not_search_empty() {
        match classify_search_document_count(12) {
            SearchDocsClassification::Populated { docs } => {
                assert_eq!(docs, 12);
                let line = search_ok_index_health_line(docs);
                assert!(line.contains("OK (12 documents)"));
                assert!(!line.contains("OK (0 documents)"));
            }
            SearchDocsClassification::Empty => {
                panic!("docs>0 must not classify as Empty / search-empty")
            }
        }
        // Builder is for the empty arm only — positive path never emits search-empty code.
        assert_ne!(
            search_ok_index_health_line(1),
            search_empty_index_health_line()
        );
    }

    // ── 0133 graph index health ───────────────────────────────────────────

    fn age_missing() -> GraphAgeInputs {
        GraphAgeInputs {
            is_missing: true,
            stale_files: 0,
        }
    }

    fn age_stale(n: usize) -> GraphAgeInputs {
        GraphAgeInputs {
            is_missing: false,
            stale_files: n,
        }
    }

    fn dirty_drift(n: usize) -> ContentHashDriftInputs {
        ContentHashDriftInputs {
            changed_or_unindexed: n,
        }
    }

    fn clean_drift() -> ContentHashDriftInputs {
        ContentHashDriftInputs {
            changed_or_unindexed: 0,
        }
    }

    #[test]
    fn classify_graph_dirty_is_content_stale_health_no_current() {
        let h = classify_graph_index_health(None, Some(Ok(dirty_drift(7))), 100, 200);
        assert_eq!(h, GraphIndexHealth::ContentStale { n: 7 });
        let f = build_graph_content_stale_finding(7);
        assert_eq!(f.code, "graph-content-stale");
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert_eq!(f.category, DoctorCategory::Index);
        assert!(
            f.message.contains('7'),
            "message must include N: {}",
            f.message
        );
        let msg_lc = f.message.to_ascii_lowercase();
        assert!(
            msg_lc.contains("content") || msg_lc.contains("drift") || msg_lc.contains("stale"),
            "greppable content/drift/stale: {}",
            f.message
        );
        let line = graph_content_stale_index_health_line(7);
        assert!(
            !line.contains("Current"),
            "content-stale health must not claim Current: {line}"
        );
        assert!(line.contains('7'));
    }

    #[test]
    fn classify_graph_clean_populated_is_current_no_content_stale() {
        let h = classify_graph_index_health(None, Some(Ok(clean_drift())), 10, 20);
        assert_eq!(h, GraphIndexHealth::CurrentPopulated);
        let line = graph_current_populated_index_health_line();
        assert_eq!(line, "Graph state: Current");
        // Positive clean path never emits content-stale code.
        assert_ne!(
            build_graph_content_stale_finding(1).code.as_str(),
            "graph-current"
        );
    }

    #[test]
    fn classify_graph_clean_zero_nodes_edges_is_analyze_graph_current() {
        let h = classify_graph_index_health(None, Some(Ok(clean_drift())), 0, 0);
        assert_eq!(h, GraphIndexHealth::CurrentEmptyCozo);
        let line = graph_current_empty_cozo_index_health_line();
        assert!(line.contains("Current"));
        assert!(
            line.contains("analyze-graph"),
            "empty Cozo must hint analyze-graph: {line}"
        );
    }

    #[test]
    fn classify_graph_age_stale_ignores_would_be_dirty_drift() {
        // Age Some → STOP; even if drift would be dirty, only AgeStale.
        let age = age_stale(42);
        let h = classify_graph_index_health(Some(&age), Some(Ok(dirty_drift(99))), 0, 0);
        assert_eq!(h, GraphIndexHealth::AgeStale { stale_files: 42 });
        assert!(
            !matches!(h, GraphIndexHealth::ContentStale { .. }),
            "age-stale must not emit content-stale"
        );
    }

    #[test]
    fn classify_graph_never_indexed_is_age_empty_only() {
        let age = age_missing();
        let h = classify_graph_index_health(Some(&age), Some(Ok(dirty_drift(5))), 0, 0);
        assert_eq!(h, GraphIndexHealth::AgeEmpty);
        assert!(
            !matches!(h, GraphIndexHealth::ContentStale { .. }),
            "never-indexed must not emit content-stale"
        );
    }

    #[test]
    fn classify_graph_drift_err_truncates_to_80_chars() {
        let long = "x".repeat(200);
        let h = classify_graph_index_health(None, Some(Err(long.clone())), 1, 1);
        match h {
            GraphIndexHealth::DriftCheckFailed { truncated_err } => {
                assert!(
                    truncated_err.chars().count() <= 80,
                    "classifier trunc ≤80: {}",
                    truncated_err.chars().count()
                );
            }
            other => panic!("expected DriftCheckFailed, got {other:?}"),
        }
        let f = build_graph_drift_check_failed_finding(&long);
        assert_eq!(f.code, "graph-drift-check-failed");
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert_eq!(f.category, DoctorCategory::Index);
        // Message must not embed the full 200-char blob beyond 80.
        let in_parens = f
            .message
            .find('(')
            .and_then(|i| f.message.get(i + 1..))
            .unwrap_or("");
        let display = in_parens.trim_end_matches(')');
        assert!(
            display.chars().count() <= 80,
            "finding display trunc ≤80: count={} msg={}",
            display.chars().count(),
            f.message
        );
        let line = graph_drift_check_failed_index_health_line();
        assert!(
            !line.contains("Current"),
            "drift-failed health must not claim Current: {line}"
        );
    }

    #[test]
    fn graph_content_stale_remediation_non_empty() {
        let f = build_graph_content_stale_finding(3);
        let rem = f.remediation.as_deref().expect("remediation Some");
        assert!(!rem.is_empty());
        assert!(
            rem.contains("ledgerful index --incremental"),
            "primary remediation: {rem}"
        );
        assert!(
            rem.contains("ledgerful index --check --json"),
            "check verification: {rem}"
        );
    }

    #[test]
    fn graph_content_stale_warn_keeps_ready_for_publish() {
        let f = build_graph_content_stale_finding(2);
        assert!(
            ready_for_publish(std::slice::from_ref(&f)),
            "content-stale warn must not flip readyForPublish"
        );
        // Non-optional Index warn still counts for dashboard failures.
        assert_eq!(dashboard_failures(std::slice::from_ref(&f)), 1);
    }

    #[test]
    fn graph_content_stale_serde_remediation_serializes() {
        let f = build_graph_content_stale_finding(4);
        let v = serde_json::to_value(&f).expect("serialize");
        assert_eq!(v["code"], "graph-content-stale");
        assert_eq!(v["severity"], "warn");
        assert_eq!(v["category"], "index");
        assert!(
            v["remediation"]
                .as_str()
                .expect("remediation present")
                .contains("ledgerful index --incremental")
        );
        assert!(v["message"].as_str().expect("message").contains('4'));
    }

    #[test]
    fn graph_drift_check_failed_serde_and_ready_for_publish() {
        let f = build_graph_drift_check_failed_finding("io error: permission denied on path");
        assert!(ready_for_publish(std::slice::from_ref(&f)));
        let v = serde_json::to_value(&f).expect("serialize");
        assert_eq!(v["code"], "graph-drift-check-failed");
        assert_eq!(v["severity"], "warn");
        assert!(v["remediation"].as_str().is_some());
    }

    #[test]
    fn content_stale_wins_over_empty_cozo_counts() {
        // Dirty drift must not fall through to analyze-graph Current even when Cozo empty.
        let h = classify_graph_index_health(None, Some(Ok(dirty_drift(1))), 0, 0);
        assert_eq!(h, GraphIndexHealth::ContentStale { n: 1 });
        let line = graph_content_stale_index_health_line(1);
        assert!(!line.contains("Current"));
        assert!(!line.contains("analyze-graph"));
    }
}
