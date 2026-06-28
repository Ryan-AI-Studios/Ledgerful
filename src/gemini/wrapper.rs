use crate::config::model::GeminiConfig;
use crate::gemini::modes::GeminiMode;
use crate::impact::packet::{ImpactPacket, RiskLevel};
use indicatif::{ProgressBar, ProgressStyle};
use miette::Result;
use owo_colors::OwoColorize;
use std::path::Path;
use std::thread;
use std::time::Duration;

const DEFAULT_GEMINI_TIMEOUT_SECS: u64 = 120;
const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const MAX_RETRIES: u32 = 3;

pub fn run_query(
    system_prompt: &str,
    user_prompt: &str,
    timeout_secs: Option<u64>,
    model: &str,
    api_key: Option<&str>,
) -> Result<()> {
    let key = resolve_api_key(api_key)?;
    let timeout = timeout_secs.unwrap_or(DEFAULT_GEMINI_TIMEOUT_SECS);

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb.set_message(format!("Consulting Gemini ({model})..."));
    pb.enable_steady_tick(Duration::from_millis(100));

    let response =
        call_with_hard_deadline(system_prompt, user_prompt, Some(timeout), model, Some(&key))?;

    pb.finish_and_clear();
    println!("\n{}", "Gemini Response:".bold().green());
    println!("{response}");

    Ok(())
}

pub fn select_gemini_model(
    config: &GeminiConfig,
    mode: GeminiMode,
    packet: &ImpactPacket,
) -> String {
    if let Some(m) = &config.model
        && !m.is_empty()
    {
        return m.clone();
    }

    if matches!(mode, GeminiMode::ReviewPatch | GeminiMode::Narrative)
        || packet.risk_level == RiskLevel::High
    {
        if let Ok(env_deep) = std::env::var("GEMINI_DEEP_MODEL") {
            return env_deep;
        }
        config
            .deep_model
            .clone()
            .unwrap_or_else(|| "gemini-3.1-pro".to_string())
    } else {
        if let Ok(env_fast) = std::env::var("GEMINI_FAST_MODEL") {
            return env_fast;
        }
        config
            .fast_model
            .clone()
            .unwrap_or_else(|| "gemini-3.1-flash-lite".to_string())
    }
}

fn call_with_retry(
    system_prompt: &str,
    user_prompt: &str,
    model: &str,
    api_key: &str,
    timeout_secs: u64,
    pb: &ProgressBar,
) -> Result<String> {
    let url = format!("{GEMINI_API_BASE}/{model}:generateContent");

    let body = serde_json::json!({
        "system_instruction": {
            "parts": [{ "text": system_prompt }]
        },
        "contents": [{
            "role": "user",
            "parts": [{ "text": user_prompt }]
        }]
    });

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(timeout_secs))
        .timeout_write(Duration::from_secs(30))
        .build();

    let mut delay = Duration::from_secs(2);

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            pb.set_message(format!(
                "Gemini overloaded, retrying in {}s… (attempt {}/{})",
                delay.as_secs(),
                attempt + 1,
                MAX_RETRIES
            ));
            thread::sleep(delay);
            delay *= 2;
        }

        match agent
            .post(&url)
            .set("x-goog-api-key", api_key)
            .send_json(&body)
        {
            Ok(response) => {
                let value: serde_json::Value = response
                    .into_json()
                    .map_err(|e| miette::miette!("Failed to parse Gemini response: {e}"))?;
                return extract_text(&value);
            }
            Err(ureq::Error::Status(503, response)) => {
                if attempt + 1 < MAX_RETRIES {
                    let body_hint = response.into_string().unwrap_or_default();
                    tracing::warn!(
                        "Gemini 503 on attempt {} — model overloaded: {}",
                        attempt + 1,
                        body_hint.chars().take(120).collect::<String>()
                    );
                    continue;
                }
                return Err(miette::miette!(
                    "Gemini service unavailable after {MAX_RETRIES} retries (503)"
                ));
            }
            Err(ureq::Error::Status(429, response)) => {
                let msg = response.into_string().unwrap_or_default();
                return Err(miette::miette!(
                    "Gemini API quota exhausted (429). Check your API key limits.\n{}",
                    msg.chars().take(200).collect::<String>()
                ));
            }
            Err(ureq::Error::Status(code, response)) => {
                let msg = response.into_string().unwrap_or_default();
                return Err(miette::miette!(
                    "Gemini API error {code}: {}",
                    msg.chars().take(200).collect::<String>()
                ));
            }
            Err(e) => {
                return Err(miette::miette!("Gemini HTTP error: {e}"));
            }
        }
    }

    Err(miette::miette!(
        "Gemini service unavailable after {MAX_RETRIES} retries"
    ))
}

/// Hard-deadline wrapper for Gemini calls (Track TA15).
///
/// Same pattern as `complete_with_hard_deadline` in `local_model/client.rs`:
/// spawns the HTTP call in a thread and uses `recv_timeout` to enforce a hard
/// deadline covering DNS, connect, TLS, and read.
pub fn call_with_hard_deadline(
    system_prompt: &str,
    user_prompt: &str,
    timeout_secs: Option<u64>,
    model: &str,
    api_key: Option<&str>,
) -> Result<String> {
    let timeout = timeout_secs.unwrap_or(DEFAULT_GEMINI_TIMEOUT_SECS);
    let deadline = Duration::from_secs(timeout + 5);

    let (tx, rx) = std::sync::mpsc::channel();
    let system_clone = system_prompt.to_string();
    let user_clone = user_prompt.to_string();
    let model_clone = model.to_string();
    let key_clone = api_key.map(|k| k.to_string());

    std::thread::spawn(move || {
        let pb = ProgressBar::hidden();
        let result = call_with_retry(
            &system_clone,
            &user_clone,
            &model_clone,
            key_clone.as_deref().unwrap_or(""),
            timeout,
            &pb,
        );
        let _ = tx.send(result);
    });

    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(miette::miette!(
            "Hard timeout: Gemini request did not complete within {}s",
            timeout
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(miette::miette!(
            "Gemini provider thread panicked during request (timeout: {}s)",
            timeout
        )),
    }
}

fn extract_text(value: &serde_json::Value) -> Result<String> {
    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
        let msg = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown Gemini error");
        return Err(miette::miette!("Gemini API error {code}: {msg}"));
    }

    value["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| miette::miette!("No text in Gemini response: {value}"))
}

fn resolve_api_key(configured_key: Option<&str>) -> Result<String> {
    if let Some(key) = configured_key.and_then(non_empty) {
        return Ok(key.to_string());
    }

    if let Ok(key) = std::env::var("GEMINI_API_KEY")
        && !key.trim().is_empty()
    {
        return Ok(key.trim().to_string());
    }

    if let Some(key) = read_env_key(Path::new(".env")) {
        return Ok(key);
    }

    Err(miette::miette!(
        "No Gemini API key found. Set GEMINI_API_KEY environment variable, \
         add it to .env, or set [gemini] api_key in .ledgerful/config.toml"
    ))
}

fn read_env_key(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim().strip_prefix("export ").unwrap_or(key.trim());
        if key != "GEMINI_API_KEY" {
            continue;
        }

        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }

    None
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_text, read_env_key};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn extract_text_returns_candidate_text() {
        let json = serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "Hello world" }] } }]
        });
        assert_eq!(extract_text(&json).unwrap(), "Hello world");
    }

    #[test]
    fn extract_text_surfaces_api_error_object() {
        let json = serde_json::json!({
            "error": { "code": 429, "message": "Quota exceeded" }
        });
        let err = extract_text(&json).unwrap_err();
        assert!(format!("{err:?}").contains("429"));
        assert!(format!("{err:?}").contains("Quota exceeded"));
    }

    #[test]
    fn extract_text_fails_gracefully_on_missing_parts() {
        let json = serde_json::json!({ "candidates": [] });
        assert!(extract_text(&json).is_err());
    }

    #[test]
    fn reads_gemini_key_from_env_file() {
        let tmp = tempdir().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "\n# local secret\nIGNORED\nexport GEMINI_API_KEY=\"test-key-value\"\nOTHER=value\n",
        )
        .unwrap();

        assert_eq!(read_env_key(&env_path), Some("test-key-value".to_string()));
    }

    #[test]
    fn ignores_missing_or_empty_env_key() {
        let tmp = tempdir().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(&env_path, "GEMINI_API_KEY=\n").unwrap();

        assert_eq!(read_env_key(&env_path), None);
    }
}
