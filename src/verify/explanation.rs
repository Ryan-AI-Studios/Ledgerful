use crate::config::model::LocalModelConfig;
use crate::local_model::client::{ChatMessage, CompletionOptions, complete};
use crate::verify::ci_predictor::CIJobOutcome;
use crate::verify::semantic_predictor::{TestOutcome, TestStatus};
use miette::Result;
use tracing::info;

pub struct ExplanationEngine {
    config: LocalModelConfig,
}

impl ExplanationEngine {
    pub fn new(config: LocalModelConfig) -> Self {
        Self { config }
    }

    pub fn explain_test_failure(
        &self,
        test_file: &str,
        diff_summary: &str,
        historical_outcomes: &[(TestOutcome, f32)],
    ) -> Result<String> {
        if self.config.base_url.is_empty() {
            return Ok("Local model not configured; cannot provide explanation.".to_string());
        }

        let mut fail_context = String::new();
        for (outcome, sim) in historical_outcomes {
            if outcome.test_file == test_file && outcome.status == TestStatus::Failed {
                fail_context.push_str(&format!(
                    "- Similarity: {:.2}, Commit: {}, Diff: {}\n",
                    sim, outcome.commit_hash, outcome.diff_summary
                ));
            }
        }

        let prompt = format!(
            "Explain why the test file '{test_file}' is predicted to fail based on the following changes:\n\
            \n\
            Current Changes Summary:\n\
            {diff_summary}\n\
            \n\
            Historical Context (Similar changes that caused failures in this test):\n\
            {fail_context}\n\
            \n\
            Provide a concise, technical explanation (max 3 sentences) of the likely failure reason. \
            Be specific about how the current changes relate to past failure patterns."
        );

        self.generate_explanation(prompt)
    }

    pub fn explain_ci_failure(
        &self,
        job_name: &str,
        platform: &str,
        diff_summary: &str,
        historical_outcomes: &[(CIJobOutcome, f32)],
    ) -> Result<String> {
        if self.config.base_url.is_empty() {
            return Ok("Local model not configured; cannot provide explanation.".to_string());
        }

        let mut fail_context = String::new();
        for (outcome, sim) in historical_outcomes {
            if outcome.job_name == job_name && outcome.status == TestStatus::Failed {
                fail_context.push_str(&format!(
                    "- Similarity: {:.2}, Commit: {}\n",
                    sim, outcome.commit_hash
                ));
            }
        }

        let prompt = format!(
            "Explain why the CI job '{job_name}' on platform '{platform}' is predicted to fail based on the following changes:\n\
            \n\
            Current Changes Summary:\n\
            {diff_summary}\n\
            \n\
            Historical Context (Similar changes that caused failures in this CI job):\n\
            {fail_context}\n\
            \n\
            Provide a concise, technical explanation (max 3 sentences) of the likely failure reason. \
            Focus on the correlation between the change patterns and the CI gate risk."
        );

        self.generate_explanation(prompt)
    }

    fn generate_explanation(&self, prompt: String) -> Result<String> {
        info!("Generating failure explanation using local model...");

        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "You are an expert software engineer specializing in CI/CD and verification. Provide concise technical rationales.".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: prompt,
            },
        ];

        let options = CompletionOptions {
            max_tokens: 200,
            temperature: 0.1,
        };

        let response =
            complete(&self.config, &messages, &options, None).map_err(|e| miette::miette!(e))?;

        Ok(response.trim().to_string())
    }
}
