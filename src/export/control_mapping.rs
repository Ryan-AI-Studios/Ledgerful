use crate::ledger::types::{Category, LedgerEntry};
use miette::{Result, miette};
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
        toml::from_str(SOC2_MAPPING_TOML)
            .map_err(|e| miette!("Failed to parse embedded SOC2 control mapping: {e}"))
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

    pub fn select<'m>(&self, mapping: &'m ControlMapping) -> Result<Vec<&'m Control>> {
        let mut selected: Vec<&'m Control> = Vec::new();
        for selector in &self.raw {
            let matches = mapping
                .control
                .iter()
                .filter(|c| control_matches_selector(&c.id, selector))
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
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let cover = generate_cover_md(mapping, selected, entries, requested)?;
    out.push(("control-lens/cover.md".to_string(), cover.into_bytes()));

    let index = generate_index_json(mapping, selected, entries, requested)?;
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
    let mut requested_sorted = requested.to_vec();
    requested_sorted.sort();

    let mut controls: Vec<LensControl> = Vec::new();
    for control in selected {
        let mut tx_ids: Vec<String> = Vec::new();
        for entry in entries {
            if control
                .evidence
                .iter()
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
            requested_controls: requested_sorted,
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
    lines.push("The bundle itself is not truncated: ledger.csv, verification_history.csv, manifest.json, manifest.sig, manifest.pub, and any chain_head.json or adr files are preserved unchanged.".to_string());
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
                "- **Framework-wide evidence:** {} (report/command-level evidence, not a per-entry match)",
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

fn is_per_entry_keyword(keyword: &str) -> bool {
    matches!(
        keyword,
        "signed_ledger_entry"
            | "verification_result"
            | "risk_impact_analysis"
            | "tamper_evident_chain"
            | "risk_score"
            | "blast_radius"
            | "impact_analysis"
            | "signature_verification"
            | "no_unsigned_entries_gate"
            | "verify_command"
            | "continuous_verification_runs"
            | "drift_reconciliation"
            | "scan_impact"
            | "config_diff"
            | "security_surface_diff"
            | "hotspots"
            | "temporal_couplings"
            | "drift_detection"
    )
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
        "tamper_evident_chain"
        | "scan_impact"
        | "config_diff"
        | "security_surface_diff"
        | "hotspots"
        | "temporal_couplings"
        | "drift_detection"
        | "no_unsigned_entries_gate"
        | "verify_command"
        | "drift_reconciliation" => true,
        _ => false,
    }
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
