use crate::config::model::GeminiConfig;
use crate::local_model::client::types::{ChatMessage, CompletionOptions};
use std::time::Duration;

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const GEMINI_FAST_MODEL: &str = "gemini-3.1-flash-lite";

/// Lightweight Gemini completion that returns the response text.
/// Used by the semantic extractor's `--fast` mode to bypass the local model.
pub fn gemini_complete(
    config: &GeminiConfig,
    messages: &[ChatMessage],
    options: &CompletionOptions,
) -> Result<String, String> {
    let api_key = config
        .api_key
        .clone()
        .filter(|k| !k.is_empty())
        .or_else(|| {
            std::env::var("GEMINI_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
        })
        .ok_or_else(|| {
            "Gemini API key not configured. Set gemini.api_key in config.toml or GEMINI_API_KEY env var.".to_string()
        })?;

    let model = config
        .fast_model
        .as_deref()
        .filter(|m| !m.is_empty())
        .or_else(|| config.model.as_deref().filter(|m| !m.is_empty()))
        .unwrap_or(GEMINI_FAST_MODEL);

    let url = format!("{GEMINI_API_BASE}/{model}:generateContent");

    // Build Gemini request body from messages
    let system_parts: Vec<serde_json::Value> = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| serde_json::json!({ "text": m.content }))
        .collect();

    let contents: Vec<serde_json::Value> = messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "parts": [{ "text": m.content }]
            })
        })
        .collect();

    let mut body = serde_json::json!({
        "contents": contents,
        "generationConfig": {
            "maxOutputTokens": options.max_tokens,
            "temperature": options.temperature,
        }
    });

    if let Some(system) = system_parts.first() {
        body["system_instruction"] = serde_json::json!({ "parts": [system] });
    }

    let timeout = config.timeout_secs.unwrap_or(120).max(300);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(timeout))
        .timeout_write(Duration::from_secs(30))
        .build();

    let mut last_error = String::new();
    let mut delay = 2u64;

    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(delay));
            delay = delay.saturating_mul(2).min(30);
        }

        match agent
            .post(&url)
            .set("x-goog-api-key", &api_key)
            .send_json(&body)
        {
            Ok(response) => {
                let value: serde_json::Value = response
                    .into_json()
                    .map_err(|e| format!("Failed to parse Gemini response: {e}"))?;

                if let Some(error) = value.get("error") {
                    let code = error.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
                    let msg = error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown Gemini error");
                    return Err(format!("Gemini API error {code}: {msg}"));
                }

                return value["candidates"][0]["content"]["parts"][0]["text"]
                    .as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| format!("No text in Gemini response: {value}"));
            }
            Err(ureq::Error::Status(503, _)) => {
                last_error = "Gemini 503 (model overloaded)".to_string();
                continue;
            }
            Err(ureq::Error::Status(429, response)) => {
                let msg = response.into_string().unwrap_or_default();
                return Err(format!(
                    "Gemini API quota exhausted (429). Check your API key limits.\n{}",
                    msg.chars().take(200).collect::<String>()
                ));
            }
            Err(ureq::Error::Status(code, response)) => {
                let msg = response.into_string().unwrap_or_default();
                return Err(format!(
                    "Gemini API error {code}: {}",
                    msg.chars().take(200).collect::<String>()
                ));
            }
            Err(ureq::Error::Transport(e)) => {
                last_error = format!("Gemini HTTP error: {e}");
            }
        }
    }

    Err(format!(
        "Gemini request failed after 3 retries: {last_error}"
    ))
}
