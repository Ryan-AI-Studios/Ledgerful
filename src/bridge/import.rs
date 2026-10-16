use crate::bridge::model::{BridgePayload, Privacy, calculate_hash, deserialize_record};
use crate::impact::hotspots::normalize_score;
use crate::impact::packet::{AiInsight, Hotspot, RelevantDecision, VerificationResult};
use crate::impact::path_class::normalize_path;
use crate::state::layout::Layout;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

/// One `BridgePayload::Hotspot` line. Not a packet row: complexity and
/// frequency are not on the wire, so this must never be pushed into the packet.
struct ImportedHotspot {
    path: String,
    score: f64,
}

struct HotspotMergeCounts {
    updated: usize,
    skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BridgeState {
    last_inbound_hash: Option<String>,
    last_outbound_hash: Option<String>,
    privacy: Option<Privacy>,
}

pub fn execute_import(in_path: String) -> Result<()> {
    let layout = crate::state::layout::get_layout_or_cwd_if_not_git()?;

    if !camino::Utf8Path::new(&in_path).exists() {
        return Err(miette::miette!("Input file does not exist: {}", in_path));
    }

    let mut state = load_bridge_state(&layout)?;

    let file = File::open(&in_path).into_diagnostic()?;
    let reader = BufReader::new(file);

    let mut imported_insights = Vec::new();
    let mut imported_hotspots = Vec::new();
    let mut imported_verifications = Vec::new();
    let mut imported_decisions = Vec::new();
    let mut other_records_count = 0;
    let mut rejected_lineage_count = 0;

    for line in reader.lines() {
        let line = line.into_diagnostic()?;
        if line.trim().is_empty() {
            continue;
        }

        match deserialize_record(&line) {
            Ok(record) => {
                // Validate parent_hash if present in the record
                if let Some(actual_parent) = &record.parent_hash {
                    if let Some(expected_parent) = &state.last_inbound_hash {
                        if actual_parent != expected_parent {
                            rejected_lineage_count += 1;
                            tracing::warn!(
                                "Bridge record rejected: parent_hash mismatch. Expected {}, found {}",
                                expected_parent,
                                actual_parent
                            );
                            continue;
                        }
                    } else {
                        rejected_lineage_count += 1;
                        tracing::warn!(
                            "Bridge record rejected: non-null parent_hash {} but state has no previous inbound hash",
                            actual_parent
                        );
                        continue;
                    }
                }

                // Combine privacy (strictest wins)
                state.privacy = match state.privacy {
                    Some(current) => Some(current.combine(record.privacy)),
                    None => Some(record.privacy),
                };

                match &record.payload {
                    BridgePayload::Insight {
                        memory_id,
                        relevance,
                        content,
                    } => {
                        imported_insights.push(AiInsight {
                            memory_id: memory_id.clone(),
                            relevance: *relevance,
                            content: content.clone(),
                        });
                    }
                    BridgePayload::Hotspot { path, score, .. } => {
                        imported_hotspots.push(ImportedHotspot {
                            path: path.clone(),
                            score: *score,
                        });
                    }
                    BridgePayload::VerifyOutcome(outcome) => {
                        imported_verifications.push(VerificationResult {
                            name: format!("Bridge Verify: {}", outcome.command),
                            command: outcome.command.clone(),
                            exit_code: if outcome.success { 0 } else { 1 },
                            stdout: String::new(),
                            stderr: outcome.error_snippet.clone().unwrap_or_default(),
                            duration_ms: 0,
                            truncated: false,
                        });
                    }
                    BridgePayload::LedgerDelta {
                        tx_id,
                        intent,
                        files_changed,
                    } => {
                        imported_decisions.push(RelevantDecision {
                            file_path: PathBuf::from(format!("tx/{}", tx_id)),
                            heading: Some(format!("Ledger Delta: {}", intent)),
                            excerpt: format!(
                                "Transaction {} changed {} files.",
                                tx_id, files_changed
                            ),
                            similarity: 1.0,
                            rerank_score: None,
                            staleness_days: None,
                            staleness_tier: None,
                        });
                    }
                    BridgePayload::Query { .. } => {
                        other_records_count += 1;
                    }
                    BridgePayload::Madr { .. } => {
                        other_records_count += 1;
                    }
                    BridgePayload::RiskAlert { .. } => {
                        other_records_count += 1;
                    }
                    BridgePayload::Snapshot(_) => {
                        other_records_count += 1;
                    }
                }

                // Update state with this record's hash for next line (or next import)
                state.last_inbound_hash = Some(calculate_hash(&record));
            }
            Err(e) => {
                tracing::warn!("Failed to deserialize bridge record: {}", e);
            }
        }
    }

    let total_imported = imported_insights.len()
        + imported_hotspots.len()
        + imported_verifications.len()
        + imported_decisions.len();

    if total_imported == 0 && other_records_count == 0 {
        if rejected_lineage_count > 0 {
            return Err(miette::miette!(
                "No records imported. {} records rejected due to invalid lineage.",
                rejected_lineage_count
            ));
        }
        println!("No valid bridge records found in input.");
        return Ok(());
    }

    let insights_len = imported_insights.len();
    let hotspots_len = imported_hotspots.len();
    let verifications_len = imported_verifications.len();
    let decisions_len = imported_decisions.len();

    // Update latest-impact.json with imported insights and other record types
    match crate::state::reports::read_latest_impact_report(&layout)? {
        Some(crate::state::reports::LatestImpactReport::Packet(mut packet)) => {
            let impact_path = layout.reports_dir().join("latest-impact.json");

            // Merge insights
            packet.ai_insights.extend(imported_insights);

            let hotspot_counts = merge_imported_hotspots(&mut packet.hotspots, &imported_hotspots);

            // Merge verifications
            packet.verification_results.extend(imported_verifications);

            // Merge decisions
            packet.relevant_decisions.extend(imported_decisions);

            packet.finalize();

            let updated_json = serde_json::to_string_pretty(&packet).into_diagnostic()?;
            fs::write(&impact_path, updated_json).into_diagnostic()?;

            println!(
                "{}",
                format_packet_import_summary(
                    insights_len,
                    hotspot_counts.updated,
                    hotspot_counts.skipped,
                    verifications_len,
                    decisions_len
                )
            );
        }
        Some(crate::state::reports::LatestImpactReport::CleanTree(_)) => {
            println!(
                "Note: latest-impact.json is a clean-tree tombstone (no pending changes); \
                 {} insights, {} hotspots, {} verifications, and {} decisions were imported \
                 but not merged into it. Run `ledgerful scan --impact` after the next change \
                 to apply them.",
                insights_len, hotspots_len, verifications_len, decisions_len
            );
        }
        None => {
            println!(
                "Warning: No latest-impact.json found. Records imported but not applied to any report."
            );
        }
    }

    if other_records_count > 0 {
        println!(
            "Note: Skipped {} non-enrichment records (not yet supported for active enrichment).",
            other_records_count
        );
    }

    if rejected_lineage_count > 0 {
        println!(
            "Warning: Rejected {} records due to invalid parent_hash lineage.",
            rejected_lineage_count
        );
    }

    save_bridge_state(&layout, &state)?;

    Ok(())
}

fn load_bridge_state(layout: &Layout) -> Result<BridgeState> {
    let path = layout.bridge_state_file();
    if path.exists() {
        let content = fs::read_to_string(path).into_diagnostic()?;
        serde_json::from_str(&content).into_diagnostic()
    } else {
        Ok(BridgeState::default())
    }
}

/// Update `score` and `display_score` on rows whose normalized path matches.
/// Unknown paths are counted and not appended. `hotspot_provenance` is not
/// read or written by this function.
fn merge_imported_hotspots(
    hotspots: &mut [Hotspot],
    imported: &[ImportedHotspot],
) -> HotspotMergeCounts {
    let mut updated_rows = BTreeSet::new();
    let mut skipped = 0usize;
    for row in imported {
        let key = normalize_path(&row.path);
        if key.is_empty() {
            skipped += 1;
            continue;
        }
        let mut matched = false;
        for (index, existing) in hotspots.iter_mut().enumerate() {
            let existing_key = normalize_path(&existing.path.to_string_lossy());
            if existing_key != key {
                continue;
            }
            let score = row.score as f32;
            existing.score = score;
            existing.display_score = normalize_score(f64::from(score)) as f32;
            updated_rows.insert(index);
            matched = true;
        }
        if !matched {
            skipped += 1;
        }
    }
    HotspotMergeCounts {
        updated: updated_rows.len(),
        skipped,
    }
}

fn format_packet_import_summary(
    insights: usize,
    updated: usize,
    skipped: usize,
    verifications: usize,
    decisions: usize,
) -> String {
    format!(
        "Imported {insights} insights, {updated} hotspot scores ({skipped} skipped outside the persisted hotspot list), {verifications} verifications, and {decisions} decisions into latest impact report."
    )
}

fn save_bridge_state(layout: &Layout, state: &BridgeState) -> Result<()> {
    let path = layout.bridge_state_file();
    let json = serde_json::to_string_pretty(state).into_diagnostic()?;
    fs::write(path, json).into_diagnostic()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, score: f32, complexity: i32) -> Hotspot {
        Hotspot {
            path: PathBuf::from(path),
            score,
            display_score: 1.25,
            complexity,
            frequency: 1.5,
            centrality: Some(4),
        }
    }

    fn imported(path: &str, score: f64) -> ImportedHotspot {
        ImportedHotspot {
            path: path.to_string(),
            score,
        }
    }

    #[test]
    fn merge_skips_path_absent_from_packet() {
        let mut hotspots = vec![row("src/kept.rs", 0.2, 7)];
        let counts = merge_imported_hotspots(&mut hotspots, &[imported("src/other.rs", 0.9)]);
        assert_eq!(counts.updated, 0);
        assert_eq!(counts.skipped, 1);
        assert_eq!(hotspots.len(), 1);
        assert_eq!(hotspots[0].path, PathBuf::from("src/kept.rs"));
        assert_eq!(hotspots[0].score, 0.2);
        assert_eq!(hotspots[0].complexity, 7);
    }

    #[test]
    fn merge_updates_score_and_display_inside_persisted_list() {
        let mut hotspots = vec![row("src/kept.rs", 0.2, 7)];
        let imported_score = 0.55_f64;
        let counts =
            merge_imported_hotspots(&mut hotspots, &[imported("src/kept.rs", imported_score)]);
        let stored = imported_score as f32;
        assert_eq!(counts.updated, 1);
        assert_eq!(counts.skipped, 0);
        assert_eq!(hotspots[0].score, stored);
        assert_eq!(
            hotspots[0].display_score,
            normalize_score(f64::from(stored)) as f32
        );
        assert_eq!(hotspots[0].complexity, 7);
        assert_eq!(hotspots[0].frequency, 1.5);
        assert_eq!(hotspots[0].centrality, Some(4));
        assert_eq!(hotspots[0].path, PathBuf::from("src/kept.rs"));
    }

    #[test]
    fn merge_matches_backslash_and_dot_slash_without_adding_a_row() {
        let mut hotspots = vec![row("src/bridge/import.rs", 0.2, 3)];
        let counts = merge_imported_hotspots(
            &mut hotspots,
            &[
                imported(r"src\bridge\import.rs", 0.4),
                imported("./src/bridge/import.rs", 0.7),
            ],
        );
        let stored = 0.7_f64 as f32;
        assert_eq!(counts.updated, 1);
        assert_eq!(counts.skipped, 0);
        assert_eq!(hotspots.len(), 1);
        assert_eq!(hotspots[0].path, PathBuf::from("src/bridge/import.rs"));
        assert_eq!(hotspots[0].score, stored);
        assert_eq!(hotspots[0].complexity, 3);
    }

    #[test]
    fn merge_counts_one_row_when_two_lines_match_and_skips_empty() {
        let mut hotspots = vec![row("src/kept.rs", 0.2, 1)];
        let counts = merge_imported_hotspots(
            &mut hotspots,
            &[
                imported("src/kept.rs", 0.3),
                imported("src/kept.rs", 0.8),
                imported("", 0.1),
                imported("./", 0.1),
            ],
        );
        assert_eq!(counts.updated, 1);
        assert_eq!(counts.skipped, 2);
        assert_eq!(hotspots.len(), 1);
        assert_eq!(hotspots[0].score, 0.8_f64 as f32);
    }

    #[test]
    fn merge_updates_every_stored_row_with_the_same_normalized_path() {
        let mut hotspots = vec![row("src/kept.rs", 0.2, 1), row(r"src\kept.rs", 0.2, 2)];
        let counts = merge_imported_hotspots(&mut hotspots, &[imported("src/kept.rs", 0.5)]);
        assert_eq!(counts.updated, 2);
        assert_eq!(counts.skipped, 0);
        assert_eq!(hotspots.len(), 2);
        assert_eq!(hotspots[0].score, 0.5);
        assert_eq!(hotspots[1].score, 0.5);
        assert_eq!(hotspots[0].complexity, 1);
        assert_eq!(hotspots[1].complexity, 2);
        assert_eq!(hotspots[0].path, PathBuf::from("src/kept.rs"));
        assert_eq!(hotspots[1].path, PathBuf::from(r"src\kept.rs"));
    }

    #[test]
    fn packet_import_summary_names_updated_and_skipped() {
        let line = format_packet_import_summary(1, 1, 2, 0, 0);
        assert_eq!(
            line,
            "Imported 1 insights, 1 hotspot scores (2 skipped outside the persisted hotspot list), 0 verifications, and 0 decisions into latest impact report."
        );
    }
}
