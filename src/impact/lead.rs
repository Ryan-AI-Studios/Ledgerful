//! Docs-mode impact presentation (track 0227).
//!
//! Presentation-only: does not change temporal risk weights or coupling math
//! (0202/0173). Filters crate co-change trivia out of the **lead** while the
//! full coupling list stays on the packet.

use crate::impact::enrichment::test_gaps::TestGapsStatus;
use crate::impact::packet::ImpactPacket;
use crate::impact::path_class::normalize_path;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Cap on JSON `actionableLead` / human Actionable punchlist.
pub const ACTIONABLE_LEAD_CAP: usize = 5;

/// Glossary key for [`TestGapsStatus::NoSourceSeeds`].
pub const GLOSSARY_KEY_NO_SOURCE_SEEDS: &str = "no_source_seeds";

/// Glossary key for mappedCount = 0.
pub const GLOSSARY_KEY_MAPPED_ZERO: &str = "mapped=0";

/// Human explanation for `no_source_seeds`.
pub const EXPLAIN_NO_SOURCE_SEEDS: &str = "No source-code seeds in this change set, so structural test mapping does not apply (typical for docs/governance-only edits).";

/// Human explanation for mappedCount = 0.
pub const EXPLAIN_MAPPED_ZERO: &str = "mappedCount is 0: no changed source symbols have a structural test mapping — often because there were no source seeds.";

const CONDUCTOR_PROCESS_BASENAMES: &[&str] = &[
    "deferred.md",
    "conductor.md",
    "coordination.md",
    "sequencing.md",
    "sequencing2.md",
];

/// Source-code extensions that must never count as documentation-shaped
/// (including `docs/foo.rs`).
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "c", "h", "cpp", "cc", "cxx", "hpp",
    "hh", "hxx", "java", "kt", "kts", "cs", "rb", "php", "swift", "scala", "toml",
];

/// One punchlist row for docs-mode lead (JSON `actionableLead`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionableLeadItem {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_a: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_b: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapped_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<String>,
}

impl PartialEq for ActionableLeadItem {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ActionableLeadItem {}

impl PartialOrd for ActionableLeadItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ActionableLeadItem {
    fn cmp(&self, other: &Self) -> Ordering {
        self.kind
            .cmp(&other.kind)
            .then_with(|| self.file_a.cmp(&other.file_a))
            .then_with(|| self.file_b.cmp(&other.file_b))
            .then_with(|| self.status.cmp(&other.status))
            .then_with(|| self.explain.cmp(&other.explain))
            .then_with(|| self.mapped_count.cmp(&other.mapped_count))
    }
}

/// Returns true when `path` is documentation-shaped for 0227 docs-mode auto-detect.
///
/// A path is documentation-shaped when it is `.md` / `.txt` / `.rst`, lives
/// under `docs/`, or is a conductor process doc (`conductor.md`, `deferred.md`,
/// `coordination.md`, `sequencing.md`, `sequencing2.md`, or under `conductor/`
/// / `coordinated/conductor/`).
///
/// Explicitly **not** documentation-shaped: `.github/**`, `Cargo.toml`,
/// `src/**`, and any source-code extension (including `docs/foo.rs`).
pub fn is_documentation_shaped(path: &str) -> bool {
    let norm = normalize_path(path);
    if norm.is_empty() {
        return false;
    }
    if is_excluded_from_docs_shape(&norm) {
        return false;
    }
    if has_doc_extension(&norm) {
        return true;
    }
    if is_under_docs(&norm) {
        return true;
    }
    is_conductor_process_doc(&norm)
}

/// True when **every** path is documentation-shaped and the set is non-empty.
///
/// Mixed sets (`src/lib.rs` + `docs/installation.md`) return false. An empty
/// set is not docs-only (vacuous truth would auto-detect a clean tree).
pub fn should_auto_detect_docs_mode<I, S>(paths: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut any = false;
    for path in paths {
        any = true;
        if !is_documentation_shaped(path.as_ref()) {
            return false;
        }
    }
    any
}

/// Explicit `--mode docs` or strict auto-detect on the dirty/prospective set.
pub fn docs_mode_active<I, S>(explicit_docs: bool, paths: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    explicit_docs || should_auto_detect_docs_mode(paths)
}

/// True when a temporal pair is docs↔crate co-change trivia: one path is
/// documentation-shaped, the other is crate/source (`src/**`, a source
/// extension, or `Cargo.toml` / `Cargo.lock`), and the score is at least 70%
/// (the human temporal table threshold). 100% `conductor.md`↔crate pairs are
/// the motivating case; lower-but-still-table-visible pairs are the same
/// review-irrelevant class.
pub fn is_docs_crate_cochange_trivia(path_a: &str, path_b: &str, score: f32) -> bool {
    if score < 0.7 {
        return false;
    }
    let a_docs = is_documentation_shaped(path_a);
    let b_docs = is_documentation_shaped(path_b);
    let a_crate = is_crate_source_path(path_a);
    let b_crate = is_crate_source_path(path_b);
    (a_docs && b_crate) || (b_docs && a_crate)
}

/// Overlay docs-mode lead + glossary onto an in-memory packet.
///
/// Does not drop `temporal_couplings`. Does not change `path_mode` / scores.
pub fn apply_docs_mode_presentation(packet: &mut ImpactPacket) {
    packet.actionable_lead = select_actionable_lead(packet);
    packet.glossary = Some(docs_mode_glossary());
}

/// Sorted, capped actionable punchlist (test-gap rows reserved, then couplings).
pub fn select_actionable_lead(packet: &ImpactPacket) -> Vec<ActionableLeadItem> {
    let mut gaps = test_gap_lead_items(packet);
    gaps.sort();
    let mut couplings = non_trivia_coupling_items(packet);
    couplings.sort();

    let mut lead = Vec::new();
    lead.extend(gaps);
    let remaining = ACTIONABLE_LEAD_CAP.saturating_sub(lead.len());
    lead.extend(couplings.into_iter().take(remaining));
    lead.sort();
    lead.truncate(ACTIONABLE_LEAD_CAP);
    lead
}

/// Glossary for `no_source_seeds` and mapped=0 (always both keys, sorted map).
pub fn docs_mode_glossary() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert(
        GLOSSARY_KEY_NO_SOURCE_SEEDS.to_string(),
        EXPLAIN_NO_SOURCE_SEEDS.to_string(),
    );
    map.insert(
        GLOSSARY_KEY_MAPPED_ZERO.to_string(),
        EXPLAIN_MAPPED_ZERO.to_string(),
    );
    map
}

/// Human Actionable section (title + up to 5 rows).
pub fn format_actionable_section(packet: &ImpactPacket) -> String {
    let mut lines = vec!["Actionable (≤5)".to_string()];
    if packet.actionable_lead.is_empty() {
        lines.push(
            "  (none — docs↔crate co-change trivia moved below; see glossary for test gaps)"
                .to_string(),
        );
    } else {
        for item in &packet.actionable_lead {
            lines.push(format!("  - {}", format_lead_item_line(item)));
        }
    }
    if let Some(ref glossary) = packet.glossary {
        if let Some(explain) = glossary.get(GLOSSARY_KEY_NO_SOURCE_SEEDS) {
            lines.push(format!("  {GLOSSARY_KEY_NO_SOURCE_SEEDS}: {explain}"));
        }
        if let Some(explain) = glossary.get(GLOSSARY_KEY_MAPPED_ZERO) {
            lines.push(format!("  {GLOSSARY_KEY_MAPPED_ZERO}: {explain}"));
        }
    }
    lines.join("\n")
}

/// Collapsed remainder line when `--full` is not set.
pub fn format_more_couplings_line(remaining: usize) -> String {
    format!("{remaining} more coupling(s) — pass --full to expand")
}

/// Couplings not shown in the lead (trivia + overflow).
pub fn remaining_coupling_count(packet: &ImpactPacket) -> usize {
    let shown = packet
        .actionable_lead
        .iter()
        .filter(|item| item.kind == "coupling")
        .count();
    packet.temporal_couplings.len().saturating_sub(shown)
}

fn format_lead_item_line(item: &ActionableLeadItem) -> String {
    match item.kind.as_str() {
        "coupling" => {
            let a = item.file_a.as_deref().unwrap_or("-");
            let b = item.file_b.as_deref().unwrap_or("-");
            let pct = item
                .score
                .map(|s| format!("{:.0}%", s * 100.0))
                .unwrap_or_else(|| "-".to_string());
            format!("coupling  {a} ↔ {b}  ({pct})")
        }
        "test_gap" => {
            let status = item.status.as_deref().unwrap_or("unknown");
            let mapped = item
                .mapped_count
                .map(|n| format!("mapped={n}"))
                .unwrap_or_default();
            let explain = item.explain.as_deref().unwrap_or("");
            format!("test_gap  {status}  {mapped} — {explain}")
        }
        other => other.to_string(),
    }
}

fn test_gap_lead_items(packet: &ImpactPacket) -> Vec<ActionableLeadItem> {
    let Some(ref gaps) = packet.test_gaps else {
        return Vec::new();
    };
    let mut items = Vec::new();
    if gaps.status == TestGapsStatus::NoSourceSeeds {
        items.push(ActionableLeadItem {
            kind: "test_gap".to_string(),
            file_a: None,
            file_b: None,
            score: None,
            status: Some(gaps.status.as_str().to_string()),
            mapped_count: Some(gaps.mapped_count),
            explain: Some(EXPLAIN_NO_SOURCE_SEEDS.to_string()),
        });
    } else if gaps.mapped_count == 0 {
        items.push(ActionableLeadItem {
            kind: "test_gap".to_string(),
            file_a: None,
            file_b: None,
            score: None,
            status: Some(gaps.status.as_str().to_string()),
            mapped_count: Some(0),
            explain: Some(EXPLAIN_MAPPED_ZERO.to_string()),
        });
    }
    if gaps.unmapped_count > 0 {
        items.push(ActionableLeadItem {
            kind: "test_gap".to_string(),
            file_a: None,
            file_b: None,
            score: None,
            status: Some("unmapped".to_string()),
            mapped_count: Some(gaps.mapped_count),
            explain: Some(format!(
                "{} changed source symbol(s) lack structural test mapping",
                gaps.unmapped_count
            )),
        });
    }
    items
}

fn non_trivia_coupling_items(packet: &ImpactPacket) -> Vec<ActionableLeadItem> {
    let mut items: Vec<ActionableLeadItem> = packet
        .temporal_couplings
        .iter()
        .filter(|tc| {
            let a = tc.file_a.to_string_lossy();
            let b = tc.file_b.to_string_lossy();
            !is_docs_crate_cochange_trivia(&a, &b, tc.score)
        })
        .map(|tc| {
            let mut file_a = normalize_path(&tc.file_a.to_string_lossy());
            let mut file_b = normalize_path(&tc.file_b.to_string_lossy());
            if file_a > file_b {
                std::mem::swap(&mut file_a, &mut file_b);
            }
            ActionableLeadItem {
                kind: "coupling".to_string(),
                file_a: Some(file_a),
                file_b: Some(file_b),
                score: Some(tc.score),
                status: None,
                mapped_count: None,
                explain: None,
            }
        })
        .collect();
    items.sort();
    items.dedup_by(|a, b| a.file_a == b.file_a && a.file_b == b.file_b);
    items
}

fn is_excluded_from_docs_shape(norm: &str) -> bool {
    if norm == ".github" || under_prefix(norm, ".github/") {
        return true;
    }
    if basename_eq(norm, "Cargo.toml") {
        return true;
    }
    if norm == "src" || under_prefix(norm, "src/") {
        return true;
    }
    has_source_extension(norm)
}

fn has_doc_extension(norm: &str) -> bool {
    matches!(extension_of(norm), Some("md" | "txt" | "rst"))
}

fn is_under_docs(norm: &str) -> bool {
    norm.eq_ignore_ascii_case("docs") || under_prefix(norm, "docs/")
}

fn is_conductor_process_doc(norm: &str) -> bool {
    for base in CONDUCTOR_PROCESS_BASENAMES {
        if basename_eq(norm, base) {
            return true;
        }
    }
    under_prefix(norm, "conductor/")
        || norm == "conductor"
        || under_prefix(norm, "coordinated/conductor/")
        || norm == "coordinated/conductor"
}

fn is_crate_source_path(path: &str) -> bool {
    let norm = normalize_path(path);
    if norm == "src" || under_prefix(&norm, "src/") {
        return true;
    }
    if basename_eq(&norm, "Cargo.toml") || basename_eq(&norm, "Cargo.lock") {
        return true;
    }
    if under_prefix(&norm, "crates/") {
        return true;
    }
    has_source_extension(&norm)
}

fn has_source_extension(norm: &str) -> bool {
    let Some(ext) = extension_of(norm) else {
        return false;
    };
    let ext_l = ext.to_ascii_lowercase();
    SOURCE_EXTENSIONS.contains(&ext_l.as_str())
}

fn under_prefix(norm: &str, prefix: &str) -> bool {
    norm.starts_with(prefix)
}

fn basename_eq(norm: &str, name: &str) -> bool {
    let base = norm.rsplit('/').next().unwrap_or(norm);
    base.eq_ignore_ascii_case(name)
}

fn extension_of(norm: &str) -> Option<&str> {
    let base = norm.rsplit('/').next().unwrap_or(norm);
    let dot = base.rfind('.')?;
    if dot == 0 || dot + 1 >= base.len() {
        return None;
    }
    Some(&base[dot + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::enrichment::test_gaps::TestGapsReport;
    use crate::impact::packet::TemporalCoupling;
    use std::path::PathBuf;

    #[test]
    fn is_documentation_shaped_matrix() {
        let docs: &[&str] = &[
            "docs/agent-output-contract.md",
            "docs/installation.md",
            "README.md",
            "notes.txt",
            "guide.rst",
            "conductor.md",
            "deferred.md",
            "conductor/0227/spec.md",
            "coordinated/conductor/deferred.md",
            r"docs\windows.md",
        ];
        for path in docs {
            assert!(
                is_documentation_shaped(path),
                "{path} should be documentation-shaped"
            );
        }

        let not_docs: &[&str] = &[
            "src/lib.rs",
            "src/README.md",
            "Cargo.toml",
            ".github/workflows/ci.yml",
            ".github/ISSUE_TEMPLATE.md",
            "docs/foo.rs",
            "tests/integration/foo.rs",
            "package.json",
        ];
        for path in not_docs {
            assert!(
                !is_documentation_shaped(path),
                "{path} must not be documentation-shaped"
            );
        }
    }

    #[test]
    fn auto_detect_docs_only_and_mixed_paths() {
        assert!(should_auto_detect_docs_mode([
            "docs/agent-output-contract.md"
        ]));
        assert!(should_auto_detect_docs_mode(["conductor.md"]));
        assert!(should_auto_detect_docs_mode([
            "docs/agent-output-contract.md",
            "conductor.md"
        ]));
        assert!(!should_auto_detect_docs_mode([
            "src/lib.rs",
            "docs/installation.md"
        ]));
        assert!(!should_auto_detect_docs_mode(["src/lib.rs"]));
        assert!(!should_auto_detect_docs_mode(Vec::<&str>::new()));
        assert!(!should_auto_detect_docs_mode([".github/workflows/ci.yml"]));
        assert!(!should_auto_detect_docs_mode(["Cargo.toml"]));
    }

    #[test]
    fn trivia_predicate_docs_crate_100_percent() {
        assert!(is_docs_crate_cochange_trivia(
            "conductor.md",
            "src/commands/scan.rs",
            1.0
        ));
        assert!(is_docs_crate_cochange_trivia(
            "docs/agent-output-contract.md",
            "src/cli/args/agent.rs",
            1.0
        ));
        assert!(is_docs_crate_cochange_trivia(
            "src/lib.rs",
            "docs/installation.md",
            0.95
        ));
        assert!(!is_docs_crate_cochange_trivia(
            "docs/a.md",
            "docs/b.md",
            1.0
        ));
        assert!(!is_docs_crate_cochange_trivia("src/a.rs", "src/b.rs", 1.0));
        assert!(!is_docs_crate_cochange_trivia(
            "conductor.md",
            "src/lib.rs",
            0.5
        ));
    }

    fn gaps_no_source_seeds() -> TestGapsReport {
        TestGapsReport {
            status: TestGapsStatus::NoSourceSeeds,
            source_seed_count: 0,
            mapped_count: 0,
            file_mapped_count: 0,
            unmapped_count: 0,
            unmapped_capped: false,
            unmapped_total: 0,
            unmapped: Vec::new(),
            mapped_sample: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn actionable_lead_excludes_docs_crate_trivia_and_caps_sorted() {
        let mut packet = ImpactPacket {
            temporal_couplings: vec![
                TemporalCoupling {
                    file_a: PathBuf::from("conductor.md"),
                    file_b: PathBuf::from("src/commands/scan.rs"),
                    score: 1.0,
                },
                TemporalCoupling {
                    file_a: PathBuf::from("docs/a.md"),
                    file_b: PathBuf::from("docs/b.md"),
                    score: 0.8,
                },
                TemporalCoupling {
                    file_a: PathBuf::from("README.md"),
                    file_b: PathBuf::from("CHANGELOG.md"),
                    score: 0.9,
                },
            ],
            test_gaps: Some(gaps_no_source_seeds()),
            ..ImpactPacket::default()
        };
        apply_docs_mode_presentation(&mut packet);

        assert!(
            packet
                .actionable_lead
                .iter()
                .all(|item| item.kind != "coupling"
                    || !is_docs_crate_cochange_trivia(
                        item.file_a.as_deref().unwrap_or(""),
                        item.file_b.as_deref().unwrap_or(""),
                        item.score.unwrap_or(0.0)
                    )),
            "trivia must not appear in actionableLead: {:?}",
            packet.actionable_lead
        );
        assert!(
            packet
                .actionable_lead
                .iter()
                .any(|i| i.status.as_deref() == Some("no_source_seeds")),
            "no_source_seeds must be in the lead"
        );
        let mut sorted = packet.actionable_lead.clone();
        sorted.sort();
        assert_eq!(
            packet.actionable_lead, sorted,
            "actionableLead must be sorted"
        );
        assert!(packet.actionable_lead.len() <= ACTIONABLE_LEAD_CAP);

        let glossary = packet.glossary.expect("glossary");
        assert!(glossary.contains_key(GLOSSARY_KEY_NO_SOURCE_SEEDS));
        assert!(glossary.contains_key(GLOSSARY_KEY_MAPPED_ZERO));
        assert_eq!(
            packet.temporal_couplings.len(),
            3,
            "full coupling list must remain"
        );
    }

    #[test]
    fn mixed_paths_do_not_enter_docs_mode_via_auto_detect() {
        assert!(!docs_mode_active(
            false,
            ["src/lib.rs", "docs/installation.md"]
        ));
        assert!(docs_mode_active(
            true,
            ["src/lib.rs", "docs/installation.md"]
        ));
    }

    #[test]
    fn format_actionable_mentions_glossary_keys() {
        let mut packet = ImpactPacket {
            test_gaps: Some(gaps_no_source_seeds()),
            ..ImpactPacket::default()
        };
        apply_docs_mode_presentation(&mut packet);
        let text = format_actionable_section(&packet);
        assert!(text.contains("Actionable (≤5)"), "{text}");
        assert!(text.contains("no_source_seeds"), "{text}");
        assert!(text.contains("mapped=0"), "{text}");
    }
}
