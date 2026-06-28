use crate::bridge::model::{BridgePayload, Privacy, calculate_hash, deserialize_record};
use crate::impact::packet::{AiInsight, Hotspot, RelevantDecision, VerificationResult};
use crate::state::layout::Layout;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BridgeState {
    last_inbound_hash: Option<String>,
    last_outbound_hash: Option<String>,
    privacy: Option<Privacy>,
}

pub fn execute_import(in_path: String) -> Result<()> {
    let current_dir = std::env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

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
                        imported_hotspots.push(Hotspot {
                            path: PathBuf::from(path),
                            score: *score as f32,
                            display_score: 0.0,
                            complexity: 0,
                            frequency: 1.0,
                            centrality: None,
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

            // Merge hotspots
            for hs in imported_hotspots {
                if let Some(existing) = packet.hotspots.iter_mut().find(|h| h.path == hs.path) {
                    existing.score = hs.score;
                } else {
                    packet.hotspots.push(hs);
                }
            }

            // Merge verifications
            packet.verification_results.extend(imported_verifications);

            // Merge decisions
            packet.relevant_decisions.extend(imported_decisions);

            packet.finalize();

            let updated_json = serde_json::to_string_pretty(&packet).into_diagnostic()?;
            fs::write(&impact_path, updated_json).into_diagnostic()?;

            println!(
                "Imported {} insights, {} hotspots, {} verifications, and {} decisions into latest impact report.",
                insights_len, hotspots_len, verifications_len, decisions_len
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

fn save_bridge_state(layout: &Layout, state: &BridgeState) -> Result<()> {
    let path = layout.bridge_state_file();
    let json = serde_json::to_string_pretty(state).into_diagnostic()?;
    fs::write(path, json).into_diagnostic()
}
