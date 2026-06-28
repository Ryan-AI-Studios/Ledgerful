use crate::config::model::LocalModelConfig;
use crate::local_model::client::{ChatMessage, CompletionOptions, complete};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct IntentDraft {
    pub what: String,
    pub why: String,
    pub risk: String,
    pub related: Vec<String>,
    pub confidence: f64,
}

pub fn draft_intent(config: &LocalModelConfig, repo_root: &Path) -> Result<IntentDraft, String> {
    // 1. Gather git context
    let staged_diff = get_staged_diff(repo_root).unwrap_or_default();
    let commit_msg = get_commit_msg(repo_root).unwrap_or_default();
    let branch_name = get_branch_name(repo_root).unwrap_or_default();
    let recent_commits = get_recent_commits(repo_root).unwrap_or_default();

    // 2. Truncate diff to avoid token overflow (~2000 tokens / 8000 chars roughly)
    let truncated_diff = if staged_diff.len() > 8000 {
        format!("{}... [truncated]", &staged_diff[..8000])
    } else {
        staged_diff
    };

    // 3. Assemble prompt messages
    let system_prompt = "\
You are an expert software architect. Analyze the provided git diff, commit message, branch name, and recent commit history.
Your goal is to extract the change intent and categorize the risk.
You MUST output a single JSON object in the exact format shown below, with NO markdown wrapper, no backticks, and no other text:
{
  \"what\": \"A concise 1-line summary of what changed (max 80 chars).\",
  \"why\": \"A brief paragraph explaining the architectural decision, reasoning, or rationale for this change.\",
  \"risk\": \"TRIVIAL\" | \"LOW\" | \"MEDIUM\" | \"HIGH\" | \"CRITICAL\",
  \"related\": [\"TICKET-123\", \"ADR-45\"],
  \"confidence\": 0.0 to 1.0 (representing your confidence in this classification)
}";

    let user_content = format!(
        "Branch: {}\n\nCOMMIT_EDITMSG:\n{}\n\nRecent Commits:\n{}\n\nStaged Diff:\n{}",
        branch_name, commit_msg, recent_commits, truncated_diff
    );

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: system_prompt.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_content,
        },
    ];

    // 4. Force 2-second timeout for the completions endpoint
    let mut custom_config = config.clone();
    custom_config.timeout_secs = 2;

    let options = CompletionOptions {
        max_tokens: 1024,
        temperature: 0.2, // low temperature for structured output
    };

    let raw_response = complete(&custom_config, &messages, &options, None)?;

    // 5. Parse JSON response
    parse_intent_json(&raw_response)
}

fn parse_intent_json(raw: &str) -> Result<IntentDraft, String> {
    // Strip markdown blocks if the model ignored system prompts and put backticks
    let mut cleaned = raw.trim();
    if cleaned.starts_with("```")
        && let Some(start_idx) = cleaned.find('{')
        && let Some(end_idx) = cleaned.rfind('}')
    {
        cleaned = &cleaned[start_idx..=end_idx];
    }

    serde_json::from_str(cleaned)
        .map_err(|e| format!("Failed to parse intent JSON: {}, raw: {}", e, cleaned))
}

fn get_staged_diff(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["diff", "--staged"])
        .current_dir(repo_root)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

fn get_commit_msg(repo_root: &Path) -> Option<String> {
    let path = repo_root.join(".git/COMMIT_EDITMSG");
    if path.exists() {
        fs::read_to_string(&path).ok()
    } else {
        None
    }
}

fn get_branch_name(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn get_recent_commits(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["log", "-n", "10", "--oneline"])
        .current_dir(repo_root)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clean_json() {
        let raw = r#"{
            "what": "Fix auth error",
            "why": "Added error handling to auth endpoint to prevent panic.",
            "risk": "LOW",
            "related": ["ENG-101"],
            "confidence": 0.95
        }"#;

        let parsed = parse_intent_json(raw).unwrap();
        assert_eq!(parsed.what, "Fix auth error");
        assert_eq!(parsed.risk, "LOW");
        assert_eq!(parsed.confidence, 0.95);
        assert_eq!(parsed.related, vec!["ENG-101"]);
    }

    #[test]
    fn test_parse_json_with_markdown() {
        let raw = r#"```json
        {
            "what": "Fix auth error",
            "why": "Added error handling to auth endpoint.",
            "risk": "LOW",
            "related": [],
            "confidence": 0.9
        }
        ```"#;

        let parsed = parse_intent_json(raw).unwrap();
        assert_eq!(parsed.what, "Fix auth error");
    }
}
