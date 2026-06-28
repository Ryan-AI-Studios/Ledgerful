use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{CIPrediction, ImpactPacket};
use crate::verify::ci_predictor::{compute_ci_failure_scores, query_similar_ci_outcomes};
use crate::verify::semantic_predictor::build_diff_text;
use miette::Result;

pub struct CIPredictorProvider;

impl EnrichmentProvider for CIPredictorProvider {
    fn name(&self) -> &'static str {
        "CIPredictorProvider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if context.config.verify.semantic_weight == 0.0 {
            return Ok(());
        }

        let diff_text = build_diff_text(packet);
        if diff_text.is_empty() {
            return Ok(());
        }

        let conn = context.storage.get_connection();

        let similar_outcomes = match query_similar_ci_outcomes(
            conn,
            &context.config.local_model,
            &diff_text,
            20, // top_k
        ) {
            Ok(outcomes) => outcomes,
            Err(e) => {
                context.add_warning(format!("CI prediction query failed: {}", e));
                return Ok(());
            }
        };

        if similar_outcomes.is_empty() {
            return Ok(());
        }

        let failure_scores = compute_ci_failure_scores(&similar_outcomes);

        for (job_name, score) in failure_scores {
            // Find platform for this job from outcomes
            let platform = similar_outcomes
                .iter()
                .find(|(o, _)| o.job_name == job_name)
                .map(|(o, _)| o.platform.clone())
                .unwrap_or_else(|| "unknown".to_string());

            packet.ci_predictions.push(CIPrediction {
                job_name,
                platform,
                failure_probability: score as f32,
                explanation: None, // Explanations generated on-demand via --explain
            });
        }

        Ok(())
    }
}
