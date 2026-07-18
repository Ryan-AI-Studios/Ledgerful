use crate::ledger::types::{Category, LedgerEntry};
use miette::{Result, miette};
use regex::Regex;
use serde::{Deserialize, Serialize};

const SOC2_MAPPING_TOML: &str = include_str!("../../mappings/soc2.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct ControlMapping {
    pub meta: Meta,
    pub control: Vec<Control>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub framework: String,
    pub version: String,
    pub source: String,
    pub disclaimer: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Control {
    pub id: String,
    pub title: String,
    pub evidence: Vec<String>,
    pub provenance: String,
    pub limit: String,
}

impl ControlMapping {
    pub fn load_static() -> Result<Self> {
        let mapping: Self = toml::from_str(SOC2_MAPPING_TOML)
            .map_err(|e| miette!("Failed to parse embedded SOC2 control mapping: {e}"))?;
        mapping.validate()?;
        Ok(mapping)
    }

    fn validate(&self) -> Result<()> {
        if self.meta.framework != "soc2" {
            return Err(miette!(
                "SOC2 mapping meta.framework must be 'soc2', got '{}'",
                self.meta.framework
            ));
        }
        if self.meta.status.is_empty() {
            return Err(miette!("SOC2 mapping meta.status must be non-empty"));
        }

        let id_regex = Regex::new(r"^CC\d+\.\d+$")
            .map_err(|e| miette!("Failed to compile control ID regex: {e}"))?;
        let mut seen = std::collections::HashSet::new();

        for control in &self.control {
            if control.id.is_empty() {
                return Err(miette!("SOC2 mapping control id must be non-empty"));
            }
            if !id_regex.is_match(&control.id) {
                return Err(miette!(
                    "SOC2 mapping control id '{}' does not match CC#.# pattern",
                    control.id
                ));
            }
            if !seen.insert(control.id.clone()) {
                return Err(miette!(
                    "SOC2 mapping contains duplicate control id '{}'",
                    control.id
                ));
            }
            if control.title.is_empty() {
                return Err(miette!(
                    "SOC2 mapping control '{}' title must be non-empty",
                    control.id
                ));
            }
            if control.provenance.is_empty() {
                return Err(miette!(
                    "SOC2 mapping control '{}' provenance must be non-empty",
                    control.id
                ));
            }
            if control.limit.is_empty() {
                return Err(miette!(
                    "SOC2 mapping control '{}' limit must be non-empty",
                    control.id
                ));
            }
            for keyword in &control.evidence {
                if !is_known_keyword(keyword) {
                    return Err(miette!(
                        "SOC2 mapping control '{}' contains unknown evidence keyword '{}'",
                        control.id,
                        keyword
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ControlSelector {
    raw: Vec<String>,
}

impl ControlSelector {
    pub fn new(raw: Vec<String>) -> Self {
        Self { raw }
    }

    pub fn requested(&self) -> &[String] {
        &self.raw
    }

    pub fn canonical_requested(&self) -> Vec<String> {
        canonicalize_requested(&self.raw)
    }

    pub fn select<'m>(&self, mapping: &'m ControlMapping) -> Result<Vec<&'m Control>> {
        let mut selected: Vec<&'m Control> = Vec::new();
        for selector in self.canonical_requested() {
            let matches = mapping
                .control
                .iter()
                .filter(|c| control_matches_selector(&c.id, &selector))
                .collect::<Vec<_>>();
            if matches.is_empty() {
                return Err(miette!(
                    "unknown SOC2 control selector: {selector}; valid selectors are exact IDs (e.g. CC8.1) or family wildcards (e.g. CC7.*)"
                ));
            }
            for control in matches {
                if !selected.iter().any(|s| s.id == control.id) {
                    selected.push(control);
                }
            }
        }
        selected.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(selected)
    }
}

fn canonicalize_requested(requested: &[String]) -> Vec<String> {
    let mut out = requested.to_vec();
    out.sort();
    out.dedup();
    out
}

fn control_matches_selector(id: &str, selector: &str) -> bool {
    if let Some(prefix) = selector.strip_suffix(".*") {
        id.starts_with(prefix) && id.len() > prefix.len() && id.as_bytes()[prefix.len()] == b'.'
    } else {
        id == selector
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LensIndex {
    meta: LensMeta,
    controls: Vec<LensControl>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LensMeta {
    framework: String,
    version: String,
    source: String,
    disclaimer: String,
    status: String,
    requested_controls: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LensControl {
    id: String,
    title: String,
    evidence: Vec<String>,
    provenance: String,
    limit: String,
    matching_tx_ids: Vec<String>,
}

pub fn generate_control_lens_files(
    mapping: &ControlMapping,
    selected: &[&Control],
    entries: &[LedgerEntry],
    requested: &[String],
) -> Result<Vec<(String, Vec<u8>)>> {
    let requested = canonicalize_requested(requested);
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let cover = generate_cover_md(mapping, selected, entries, &requested)?;
    out.push(("control-lens/cover.md".to_string(), cover.into_bytes()));

    let index = generate_index_json(mapping, selected, entries, &requested)?;
    out.push(("control-lens/index.json".to_string(), index));

    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn generate_index_json(
    mapping: &ControlMapping,
    selected: &[&Control],
    entries: &[LedgerEntry],
    requested: &[String],
) -> Result<Vec<u8>> {
    let mut controls: Vec<LensControl> = Vec::new();
    for control in selected {
        let mut tx_ids: Vec<String> = Vec::new();
        for entry in entries {
            if control
                .evidence
                .iter()
                .filter(|k| is_per_entry_keyword(k))
                .any(|keyword| matches_evidence_keyword(entry, keyword))
            {
                tx_ids.push(entry.tx_id.clone());
            }
        }
        tx_ids.sort();
        tx_ids.dedup();

        controls.push(LensControl {
            id: control.id.clone(),
            title: control.title.clone(),
            evidence: control.evidence.clone(),
            provenance: control.provenance.clone(),
            limit: control.limit.clone(),
            matching_tx_ids: tx_ids,
        });
    }

    let index = LensIndex {
        meta: LensMeta {
            framework: mapping.meta.framework.clone(),
            version: mapping.meta.version.clone(),
            source: mapping.meta.source.clone(),
            disclaimer: mapping.meta.disclaimer.clone(),
            status: mapping.meta.status.clone(),
            requested_controls: requested.to_vec(),
        },
        controls,
    };

    serde_json::to_vec(&index)
        .map_err(|e| miette!("Failed to serialize control-lens/index.json: {e}"))
}

fn generate_cover_md(
    mapping: &ControlMapping,
    selected: &[&Control],
    entries: &[LedgerEntry],
    requested: &[String],
) -> Result<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "# Control Lens: {}",
        mapping.meta.framework.to_uppercase()
    ));
    lines.push(String::new());
    lines.push(format!("**Framework version:** {}", mapping.meta.version));
    lines.push(format!("**Source:** {}", mapping.meta.source));
    lines.push(format!("**Mapping status:** {}", mapping.meta.status));
    lines.push(String::new());
    lines.push("## Requested controls".to_string());
    for id in requested {
        let title = selected
            .iter()
            .find(|c| c.id == *id)
            .map(|c| c.title.as_str())
            .unwrap_or("(matched by wildcard)");
        lines.push(format!("- `{}` — {}", id, title));
    }
    lines.push(String::new());
    lines.push("## What this lens includes".to_string());
    lines.push(format!(
        "This export contains the **entire intact signed Ledgerful bundle** ({} ledger entries) plus the additive control-lens files in this directory.",
        entries.len()
    ));
    lines.push("Existing evidence payloads (ledger.csv, verification_history.csv, adr/*.md, and chain_head.json) are preserved byte-identical. The manifest.json and manifest.sig files are regenerated and re-signed so their cryptographic coverage includes the additive control-lens files.".to_string());
    lines.push(String::new());
    lines.push("## Mapped evidence by control".to_string());
    for control in selected {
        lines.push(format!("### {} — {}", control.id, control.title));
        lines.push(format!(
            "- **Evidence categories:** {}",
            control.evidence.join(", ")
        ));
        lines.push(format!("- **Provenance:** {}", control.provenance));
        lines.push(format!("- **Honest limit:** {}", control.limit));
        let per_entry_keywords: Vec<&String> = control
            .evidence
            .iter()
            .filter(|k| is_per_entry_keyword(k))
            .collect();
        let framework_keywords: Vec<&String> = control
            .evidence
            .iter()
            .filter(|k| !is_per_entry_keyword(k))
            .collect();
        if !per_entry_keywords.is_empty() {
            let matching_count = entries
                .iter()
                .filter(|e| {
                    control
                        .evidence
                        .iter()
                        .filter(|k| is_per_entry_keyword(k))
                        .any(|k| matches_evidence_keyword(e, k))
                })
                .count();
            lines.push(format!(
                "- **Per-entry matches:** {matching_count} ledger entries matched by {}",
                per_entry_keywords
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !framework_keywords.is_empty() {
            lines.push(format!(
                "- **Framework-wide evidence:** {} (applies to the entire change-control system, not individual entries)",
                framework_keywords.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        lines.push(String::new());
    }
    lines.push("## Disclaimer".to_string());
    lines.push(mapping.meta.disclaimer.clone());
    lines.push(String::new());
    lines.push("## Honesty ceiling".to_string());
    lines.push("This lens is a rendering aid. It does not prove compliance, certification, or an auditor's opinion. It identifies evidence Ledgerful already produced; the customer's auditor decides whether that evidence satisfies a control.".to_string());

    Ok(lines.join("\n"))
}

const PER_ENTRY_KEYWORDS: &[&str] = &[
    "signed_ledger_entry",
    "verification_result",
    "risk_impact_analysis",
    "risk_score",
    "blast_radius",
    "impact_analysis",
    "signature_verification",
    "continuous_verification_runs",
];

const FRAMEWORK_KEYWORDS: &[&str] = &[
    "tamper_evident_chain",
    "scan_impact",
    "config_diff",
    "security_surface_diff",
    "hotspots",
    "temporal_couplings",
    "drift_detection",
    "no_unsigned_entries_gate",
    "verify_command",
    "drift_reconciliation",
];

fn is_per_entry_keyword(keyword: &str) -> bool {
    PER_ENTRY_KEYWORDS.contains(&keyword)
}

fn is_framework_keyword(keyword: &str) -> bool {
    FRAMEWORK_KEYWORDS.contains(&keyword)
}

fn is_known_keyword(keyword: &str) -> bool {
    is_per_entry_keyword(keyword) || is_framework_keyword(keyword)
}

pub fn matches_evidence_keyword(entry: &LedgerEntry, keyword: &str) -> bool {
    match keyword {
        "signed_ledger_entry" | "signature_verification" => entry.signature.is_some(),
        "verification_result" | "continuous_verification_runs" => {
            entry.verification_status.is_some()
        }
        "risk_impact_analysis" => entry.risk.is_some(),
        "blast_radius" | "impact_analysis" => matches!(
            entry.category,
            Category::Feature | Category::Architecture | Category::Bugfix | Category::Refactor
        ),
        "risk_score" => entry.risk.is_some(),
        _ if is_framework_keyword(keyword) => false,
        _ => false,
    }
}

pub fn render_mapping_doc(mapping: &ControlMapping) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("# SOC 2 Control-Evidence Mapping".to_string());
    lines.push(String::new());
    lines.push(
        "Ledgerful can optionally scope its SOC2 evidence export to a control lens.".to_string(),
    );
    lines.push(
        "The mapping lives in `mappings/soc2.toml` and is embedded at compile time.".to_string(),
    );
    lines.push(format!(
        "{} This is a mapping aid, NOT a certification or compliance attestation.",
        mapping.meta.disclaimer
    ));
    lines.push(String::new());
    lines.push("## Using the control lens".to_string());
    lines.push(String::new());
    lines.push("```bash".to_string());
    lines.push("ledgerful export evidence --profile soc2 --control CC8.1 --control \"CC7.*\" --out soc2-cc8.zip".to_string());
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push("* `--control` is repeatable.".to_string());
    lines.push("* Family wildcards are supported: `CC7.*` matches every control whose ID starts with `CC7.`.".to_string());
    lines.push("* Unknown or empty selectors are rejected with a clear error.".to_string());
    lines.push("* The export always contains the full signed evidence bundle; the control lens only adds `control-lens/cover.md` and `control-lens/index.json`, and the manifest/sig are regenerated to cover them.".to_string());
    lines.push(String::new());
    lines.push("## Mapping file format".to_string());
    lines.push(String::new());
    lines.push("The TOML file has two top-level sections:".to_string());
    lines.push(String::new());
    lines.push("```toml".to_string());
    lines.push("[meta]".to_string());
    lines.push(format!("framework = \"{}\"", mapping.meta.framework));
    lines.push(format!("version = \"{}\"", mapping.meta.version));
    lines.push(format!("source = \"{}\"", mapping.meta.source));
    lines.push(format!("disclaimer = \"{}\"", mapping.meta.disclaimer));
    lines.push(format!("status = \"{}\"", mapping.meta.status));
    lines.push(String::new());
    lines.push("[[control]]".to_string());
    lines.push("id = \"CC8.1\"".to_string());
    lines.push("title = \"Change Management\"".to_string());
    lines.push("evidence = [\"signed_ledger_entry\", \"verification_result\", \"risk_impact_analysis\", \"tamper_evident_chain\"]".to_string());
    lines.push("provenance = \"...\"".to_string());
    lines.push("limit = \"...\"".to_string());
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push(format!("Status: {}.", mapping.meta.status));
    lines.push(String::new());
    lines.push("Each control entry declares which Ledgerful evidence keywords it wants, why that evidence supports the control (`provenance`), and what the evidence does **not** prove (`limit`).".to_string());
    lines.push(String::new());
    lines.push("## Default controls".to_string());
    lines.push(String::new());
    lines.push("| ID | Title | Evidence keywords |".to_string());
    lines.push("|---|---|---|".to_string());
    for control in &mapping.control {
        let keywords: Vec<String> = control.evidence.iter().map(|s| format!("`{s}`")).collect();
        lines.push(format!(
            "| {} | {} | {} |",
            control.id,
            control.title,
            keywords.join(", ")
        ));
    }
    lines.push(String::new());
    lines.push("## Evidence keyword semantics".to_string());
    lines.push(String::new());
    lines.push("The mapping engine matches each keyword against ledger entry fields:".to_string());
    lines.push(String::new());
    lines.push("* `signed_ledger_entry` — entry has a non-empty `signature`.".to_string());
    lines
        .push("* `verification_result` — entry has a non-empty `verification_status`.".to_string());
    lines.push("* `risk_score` — entry has a non-empty `risk` field.".to_string());
    lines.push("* `risk_impact_analysis` — entry has a non-empty `risk` field.".to_string());
    lines.push("* `tamper_evident_chain` — the tamper-evident chain covers all entries; the chain as a whole is the evidence, not individual entries. Every entry is included because removing any entry would break continuity.".to_string());
    lines.push("* `blast_radius` / `impact_analysis` / `scan_impact` — currently matched by change category (Feature, Architecture, Bugfix, Refactor) because per-entry risk/impact fields are not populated in the current schema; future releases will narrow these to dedicated fields when available.".to_string());
    lines.push("* `config_diff`, `security_surface_diff`, `hotspots`, `temporal_couplings`, `drift_detection`, `signature_verification`, `no_unsigned_entries_gate`, `verify_command`, `continuous_verification_runs`, `drift_reconciliation` — currently match entries with verification or risk metadata; future releases will narrow these to dedicated fields.".to_string());
    lines.push(String::new());
    lines.push("## Control provenance and honest limits".to_string());
    lines.push(String::new());
    for control in &mapping.control {
        lines.push(format!("### {} — {}", control.id, control.title));
        lines.push(String::new());
        lines.push(format!("* **Provenance:** {}", control.provenance));
        lines.push(format!("* **Honest limit:** {}", control.limit));
        lines.push(String::new());
    }
    lines.push("## Important limitations".to_string());
    lines.push(String::new());
    lines.push(format!(
        "* This mapping is a **default starting point** based on the {}.",
        mapping.meta.source
    ));
    lines.push("* Every customer environment and auditor interprets controls differently. Validate and customize `mappings/soc2.toml` before relying on it for an audit.".to_string());
    lines.push("* The tool produces evidence; the auditor renders the opinion.".to_string());
    lines.push("* Existing evidence payloads (ledger.csv, verification_history.csv, adr/*.md, chain_head.json) are preserved byte-identical. The manifest.json and manifest.sig files are regenerated so their signature covers the additive lens files.".to_string());

    lines.join("\n").to_string() + "\n"
}

pub fn banned_terms() -> &'static [&'static str] {
    &[
        "soc 2 compliant",
        "soc2 compliant",
        "hipaa compliant",
        "certified",
        "is audited",
        "are audited",
        "was audited",
        "been audited",
        "audited by",
        "tamper-proof",
        "you are compliant",
        "is a compliance attestation",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_static_mapping_parses_all_six_controls() {
        let mapping = ControlMapping::load_static().unwrap();
        assert_eq!(mapping.control.len(), 6);
        let ids: Vec<&str> = mapping.control.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"CC8.1"));
        assert!(ids.contains(&"CC3.4"));
        assert!(ids.contains(&"CC7.1"));
        assert!(ids.contains(&"CC7.2"));
        assert!(ids.contains(&"CC6.8"));
        assert!(ids.contains(&"CC4.1"));
    }

    #[test]
    fn selector_exact_match() {
        let mapping = ControlMapping::load_static().unwrap();
        let selector = ControlSelector::new(vec!["CC8.1".to_string()]);
        let selected = selector.select(&mapping).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "CC8.1");
    }

    #[test]
    fn selector_family_wildcard() {
        let mapping = ControlMapping::load_static().unwrap();
        let selector = ControlSelector::new(vec!["CC7.*".to_string()]);
        let selected = selector.select(&mapping).unwrap();
        let ids: Vec<&str> = selected.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["CC7.1", "CC7.2"]);
    }

    #[test]
    fn selector_unknown_rejected() {
        let mapping = ControlMapping::load_static().unwrap();
        let selector = ControlSelector::new(vec!["CC99.9".to_string()]);
        assert!(selector.select(&mapping).is_err());
    }
}
