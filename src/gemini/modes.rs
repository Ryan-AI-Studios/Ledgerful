use crate::impact::packet::ImpactPacket;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Default)]
pub enum GeminiMode {
    #[default]
    Analyze,
    Suggest,
    ReviewPatch,
    Narrative,
}

impl fmt::Display for GeminiMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GeminiMode::Analyze => write!(f, "analyze"),
            GeminiMode::Suggest => write!(f, "suggest"),
            GeminiMode::ReviewPatch => write!(f, "review-patch"),
            GeminiMode::Narrative => write!(f, "narrative"),
        }
    }
}

pub fn build_system_prompt(mode: GeminiMode) -> String {
    match mode {
        GeminiMode::Analyze => r#"You are Ledgerful, an expert software engineering assistant.
Your goal is to help developers understand the impact and risk of their changes.
You have access to "Impact Packets" which describe repository state, changed files, and extracted symbols.
Provide concise, technical, and actionable insights. Focus on potential regressions, architectural shifts, and verification needs."#
            .to_string(),
        GeminiMode::Suggest => r#"You are Ledgerful, an expert software engineering assistant focused on verification strategy.
Given an Impact Packet describing changes, provide targeted verification recommendations.
For each risk area, specify: what to run, what to check, and why it matters.
Prioritize actionable steps over general advice. Be specific about commands, test suites, or manual checks."#
            .to_string(),
        GeminiMode::ReviewPatch => r#"You are Ledgerful, an expert code reviewer.
Given a diff/patch and the Impact Packet context, perform a focused code review.
Identify: potential bugs, style issues, missing error handling, security concerns, and test coverage gaps.
Be specific — reference line ranges and code patterns. Prioritize real issues over style nits."#
            .to_string(),
        GeminiMode::Narrative => r#"You are Ledgerful, a Senior Software Architect providing a narrative risk assessment.
Analyze the multi-dimensional data (Structural, Temporal, Complexity) provided.
Explain the "Butterfly Effect" — what is likely to break far away from the changes?
Speak to the architectural health and stability of the project.
Provide a high-level executive summary followed by deep technical dives into the riskiest areas."#
            .to_string(),
    }
}

pub fn build_user_prompt(
    mode: GeminiMode,
    packet: &ImpactPacket,
    query: &str,
    diff: Option<&str>,
) -> String {
    let packet_json = serde_json::to_string_pretty(packet).unwrap_or_else(|e| {
        tracing::warn!("Packet serialization failed: {e}");
        format!("{{\"error\": \"serialization failed: {e}\"}}")
    });

    let diff_section = match (mode, diff) {
        (GeminiMode::ReviewPatch, Some(d)) => format!("Diff:\n{}\n---\n", d),
        (GeminiMode::ReviewPatch, None) => {
            "No diff available (working tree is clean). Falling back to general analysis.\n---\n"
                .to_string()
        }
        _ => String::new(),
    };

    let instruction = match mode {
        GeminiMode::Analyze => "Please analyze the provided context and answer the question above.",
        GeminiMode::Suggest => {
            "Please suggest specific verification steps for the changes described above."
        }
        GeminiMode::ReviewPatch => "Please review the diff above and provide focused feedback.",
        GeminiMode::Narrative => {
            "Please provide a high-level narrative risk assessment based on the multi-dimensional intelligence provided above. Focus on architectural health and potential side effects."
        }
    };

    format!(
        r#"Context:
---
Impact Packet:
{}
---
{diff_section}
Question:
{}

{instruction}"#,
        packet_json, query
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::ImpactPacket;

    #[test]
    fn test_mode_display() {
        assert_eq!(GeminiMode::Analyze.to_string(), "analyze");
        assert_eq!(GeminiMode::Suggest.to_string(), "suggest");
        assert_eq!(GeminiMode::ReviewPatch.to_string(), "review-patch");
    }

    #[test]
    fn test_analyze_system_prompt() {
        let prompt = build_system_prompt(GeminiMode::Analyze);
        assert!(prompt.contains("impact and risk"));
    }

    #[test]
    fn test_suggest_system_prompt() {
        let prompt = build_system_prompt(GeminiMode::Suggest);
        assert!(prompt.contains("verification"));
    }

    #[test]
    fn test_review_patch_system_prompt() {
        let prompt = build_system_prompt(GeminiMode::ReviewPatch);
        assert!(prompt.contains("code reviewer"));
    }

    #[test]
    fn test_prompts_are_distinct() {
        let analyze = build_system_prompt(GeminiMode::Analyze);
        let suggest = build_system_prompt(GeminiMode::Suggest);
        let review = build_system_prompt(GeminiMode::ReviewPatch);
        assert_ne!(analyze, suggest);
        assert_ne!(suggest, review);
        assert_ne!(analyze, review);
    }

    #[test]
    fn test_review_patch_user_prompt_with_diff() {
        let packet = ImpactPacket::default();
        let prompt = build_user_prompt(
            GeminiMode::ReviewPatch,
            &packet,
            "review this",
            Some("diff --git a/file.rs\n+new line"),
        );
        assert!(prompt.contains("Diff:"));
        assert!(prompt.contains("new line"));
        assert!(prompt.contains("review the diff"));
    }

    #[test]
    fn test_review_patch_user_prompt_without_diff() {
        let packet = ImpactPacket::default();
        let prompt = build_user_prompt(GeminiMode::ReviewPatch, &packet, "review this", None);
        assert!(prompt.contains("No diff available"));
    }

    #[test]
    fn test_analyze_user_prompt() {
        let packet = ImpactPacket::default();
        let prompt = build_user_prompt(GeminiMode::Analyze, &packet, "What is the risk?", None);
        assert!(prompt.contains("Impact Packet:"));
        assert!(prompt.contains("What is the risk?"));
        assert!(!prompt.contains("Diff:"));
    }
}
