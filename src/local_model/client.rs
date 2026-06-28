mod cloud;
mod gemini;
mod ollama;
mod openai;
mod types;
mod util;

pub use cloud::has_ollama_cloud_fallback;
pub use gemini::gemini_complete;
pub use types::{ChatMessage, CompletionOptions, EndpointKind, EndpointTarget};
pub use util::{
    check_base_url_warnings, completion_target, detect_endpoint_kind, transport_is_timeout,
};

use crate::config::model::LocalModelConfig;
use std::time::Duration;

/// Reads a cloud-fallback credential/setting from the real process environment first,
/// falling back to a `.env` file in the current directory — matching the resolution
/// pattern already used for `OLLAMA_CLOUD_API_KEY` elsewhere in this module.
fn cloud_fallback_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| crate::config::model::read_env_key(key))
}

pub fn has_cloud_fallback(config: &LocalModelConfig) -> bool {
    has_ollama_cloud_fallback(config)
        || cloud_fallback_env("OPENROUTER_API_KEY").is_some()
        || cloud_fallback_env("GEMINI_API_KEY").is_some()
}

pub fn is_configured(config: &LocalModelConfig) -> bool {
    !config.base_url.is_empty() || config.generation_url.is_some() || has_cloud_fallback(config)
}

use cloud::ollama_cloud_endpoint;
use ollama::ollama_native_num_predict;
use types::CompletionEndpoint;

pub fn ping_completions(config: &LocalModelConfig) -> Result<String, String> {
    if config.base_url.is_empty() && config.generation_url.is_none() {
        return Err("not configured".to_string());
    }

    let check_url = config.generation_url.as_deref().unwrap_or(&config.base_url);
    // CR3: Increased from 150ms to 500ms to prevent false negatives on WSL/container hosts.
    if !crate::util::network::is_url_reachable(check_url, Duration::from_millis(500)) {
        return Err(format!(
            "Local model server at {} is unreachable",
            check_url
        ));
    }

    let url = if let Some(gen_url) = &config.generation_url {
        format!("{}/v1/chat/completions", gen_url)
    } else {
        format!("{}/v1/chat/completions", config.base_url)
    };
    tracing::debug!("Using completion URL: {}", url);

    let body = serde_json::json!({
        "model": config.generation_model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
        "stream": false,
    });

    // Use config timeout: lazy-loading servers need time to load the model before responding.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(std::cmp::min(config.timeout_secs, 5)))
        .timeout_read(Duration::from_secs(config.timeout_secs))
        .timeout_write(Duration::from_secs(30))
        .build();

    let response = match agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            return Err(format!(
                "{} server error ({})",
                code,
                body.chars().take(100).collect::<String>()
            ));
        }
        Err(ureq::Error::Transport(inner)) => {
            if format!("{:?}", inner).to_lowercase().contains("timeout") {
                return Err(format!("timed out after {}s", config.timeout_secs));
            }
            return Err(format!("unreachable ({})", inner));
        }
    };

    // Best-effort model name: read from response, fall back to configured model
    let model_name = response
        .into_json::<serde_json::Value>()
        .ok()
        .and_then(|v| {
            v.get("model")
                .and_then(|m| m.as_str().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| config.generation_model.clone());

    Ok(model_name)
}

pub fn complete(
    config: &LocalModelConfig,
    messages: &[ChatMessage],
    options: &CompletionOptions,
    timeout_secs_override: Option<u64>,
) -> Result<String, String> {
    if config.base_url.is_empty() && config.generation_url.is_none() && !has_cloud_fallback(config)
    {
        return Err(
            "Local model server is not configured. Start llama-server, configure Ollama Cloud, OpenRouter, or Gemini fallback."
                .to_string(),
        );
    }

    let local_base_url = config.generation_url.as_deref().unwrap_or(&config.base_url);
    if !local_base_url.is_empty() {
        // CR3: Fast network probe to prevent 20s TCP hangs when model server is down.
        if crate::util::network::is_url_reachable(local_base_url, Duration::from_millis(500)) {
            let endpoint = CompletionEndpoint {
                label: "Local model server",
                base_url: local_base_url,
                model: &config.generation_model,
                authorization: None,
            };
            let effective_timeout = timeout_secs_override.unwrap_or(config.timeout_secs);
            match complete_with_endpoint(&endpoint, effective_timeout, messages, options) {
                Ok(response) => return Ok(response),
                Err(error) if has_cloud_fallback(config) => {
                    tracing::debug!("Local completion failed ({error}); trying cloud fallback");
                }
                Err(error) => return Err(error),
            }
        } else if !has_cloud_fallback(config) {
            return Err(format!(
                "Local model server at {} is unreachable. Start llama-server, OpenRouter, or Gemini.",
                local_base_url
            ));
        } else {
            tracing::debug!(
                "Local model server at {} is unreachable; trying cloud fallback",
                local_base_url
            );
        }
    }

    let effective_timeout = timeout_secs_override.unwrap_or(config.timeout_secs);
    let mut last_error = String::new();

    if let Some(endpoint) = ollama_cloud_endpoint(config) {
        match complete_with_endpoint(&endpoint, effective_timeout, messages, options) {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_error = e.clone();
                tracing::debug!("Ollama cloud fallback failed: {}", e);
            }
        }
    }

    if let Some(api_key) = cloud_fallback_env("OPENROUTER_API_KEY") {
        let model = cloud_fallback_env("OPENROUTER_MODEL")
            .unwrap_or_else(|| "google/gemini-2.5-flash".to_string());
        let endpoint = CompletionEndpoint {
            label: "OpenRouter fallback",
            base_url: "https://openrouter.ai/api/v1",
            model: &model,
            authorization: Some(format!("Bearer {api_key}")),
        };
        match complete_with_endpoint(&endpoint, effective_timeout, messages, options) {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_error = e.clone();
                tracing::debug!("OpenRouter fallback failed: {}", e);
            }
        }
    }

    if let Some(api_key) = cloud_fallback_env("GEMINI_API_KEY") {
        let default_gemini = crate::config::model::GeminiConfig {
            api_key: Some(api_key),
            ..Default::default()
        };
        match gemini_complete(&default_gemini, messages, options) {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_error = e.clone();
                tracing::debug!("Gemini fallback failed: {}", e);
            }
        }
    }

    if !last_error.is_empty() {
        Err(format!(
            "Cloud fallback exhausted. Last error: {}",
            last_error
        ))
    } else {
        Err(format!(
            "Local model server at {} is unreachable. Start llama-server, configure OpenRouter or Gemini fallback.",
            local_base_url
        ))
    }
}

/// Hard-deadline wrapper for `complete` (Track TA15).
///
/// Spawns the HTTP call in a thread and uses `recv_timeout` to enforce a
/// hard deadline that covers the ENTIRE request lifecycle (DNS, connect,
/// TLS handshake, read). The inner ureq timeouts (`timeout_connect(5)` +
/// `timeout_read(timeout_secs)`) fire first when possible, giving a more
/// specific error. The `+5` buffer gives ureq a chance to fire before the
/// hard deadline.
///
/// Known limitation: if ureq hangs at the DNS resolution level, the spawned
/// thread cannot be forcefully killed in Rust. The thread leaks until the
/// DNS query times out at the OS level (typically 15-30s). This is acceptable
/// for CLI invocations because the process exits after `ask` returns. For
/// daemon mode, a future track should migrate to async `reqwest`.
pub fn complete_with_hard_deadline(
    config: &LocalModelConfig,
    messages: &[ChatMessage],
    options: &CompletionOptions,
    timeout_secs: Option<u64>,
) -> Result<String, String> {
    let effective_timeout = timeout_secs.unwrap_or(config.timeout_secs);
    let deadline = Duration::from_secs(effective_timeout + 5);

    let (tx, rx) = std::sync::mpsc::channel();
    let config_clone = config.clone();
    let messages_clone: Vec<ChatMessage> = messages.to_vec();
    let options_clone = options.clone();

    std::thread::spawn(move || {
        let result = complete(
            &config_clone,
            &messages_clone,
            &options_clone,
            Some(effective_timeout),
        );
        let _ = tx.send(result);
    });

    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "Hard timeout: request did not complete within {}s",
            effective_timeout
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(format!(
            "Provider thread panicked during request (timeout: {}s)",
            effective_timeout
        )),
    }
}

fn complete_with_endpoint(
    endpoint: &CompletionEndpoint<'_>,
    timeout_secs: u64,
    messages: &[ChatMessage],
    options: &CompletionOptions,
) -> Result<String, String> {
    let target = completion_target(endpoint.base_url);

    // Check for known problematic base URL shapes
    if let Some(warning) = check_base_url_warnings(endpoint.base_url, target.kind) {
        return Err(warning);
    }

    let body = match target.kind {
        EndpointKind::OllamaNative => {
            serde_json::json!({
                "model": endpoint.model,
                "messages": messages,
                "stream": false,
                "options": {
                    "num_predict": ollama_native_num_predict(options.max_tokens),
                    "temperature": options.temperature,
                },
            })
        }
        EndpointKind::OpenAICompatible => {
            serde_json::json!({
                "model": endpoint.model,
                "messages": messages,
                "max_tokens": options.max_tokens,
                "temperature": options.temperature,
                "stream": false,
            })
        }
    };

    tracing::debug!(
        "Using completion URL: {} (kind={:?})",
        target.url,
        target.kind
    );

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(timeout_secs))
        .timeout_write(Duration::from_secs(30))
        .build();

    let mut retry = false;

    let response = loop {
        let mut request = agent
            .post(&target.url)
            .set("Content-Type", "application/json");
        if let Some(value) = &endpoint.authorization {
            request = request.set("Authorization", value);
        }
        let result = request.send_json(&body);

        break match result {
            Ok(resp) => resp,
            Err(ureq::Error::Status(503, _response)) if !retry => {
                std::thread::sleep(Duration::from_secs(2));
                retry = true;
                continue;
            }
            Err(ureq::Error::Status(503, response)) => {
                let body_text = response.into_string().unwrap_or_default();
                return Err(format!(
                    "{} returned 503: {}",
                    endpoint.label,
                    body_text.chars().take(200).collect::<String>()
                ));
            }
            Err(ureq::Error::Status(429, _)) => {
                return Err(format!(
                    "{} rate limited. Wait a moment or check your quota/credits.",
                    endpoint.label
                ));
            }
            Err(ureq::Error::Status(code, response)) => {
                let body_text = response.into_string().unwrap_or_default();
                return Err(format!(
                    "{} returned {code}: {}",
                    endpoint.label,
                    body_text.chars().take(200).collect::<String>()
                ));
            }
            Err(ureq::Error::Transport(inner)) => {
                if transport_is_timeout(&inner) {
                    return Err(format!(
                        "{} timed out after {}s",
                        endpoint.label, timeout_secs
                    ));
                }
                return Err(format!(
                    "{} not reachable at {} \u{2014} {}",
                    endpoint.label, endpoint.base_url, inner
                ));
            }
        };
    };

    match target.kind {
        EndpointKind::OllamaNative => {
            let parsed: ollama::OllamaChatResponse = response
                .into_json()
                .map_err(|e| format!("Failed to parse Ollama native response: {e}"))?;
            if parsed.message.content.is_empty() {
                if let Some(ref thinking) = parsed.message.thinking
                    && !thinking.is_empty()
                {
                    return Err(format!(
                        "{} returned empty content (reasoning only: {} chars)",
                        endpoint.label,
                        thinking.len()
                    ));
                }
                return Err(format!("{} returned empty message content", endpoint.label));
            }
            Ok(parsed.message.content)
        }
        EndpointKind::OpenAICompatible => {
            let parsed: openai::CompletionResponse = response
                .into_json()
                .map_err(|e| format!("Failed to parse completion response: {e}"))?;
            let choice = parsed
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| "No completion choices returned".to_string())?;
            if choice.message.content.is_empty() {
                if let Some(reasoning) = choice.message.reasoning
                    && !reasoning.is_empty()
                {
                    tracing::warn!(
                        "Model returned thinking-only response ({} chars), using reasoning as content",
                        reasoning.len()
                    );
                    return Ok(reasoning);
                }
                return Err(format!("{} returned empty message content", endpoint.label));
            }
            Ok(choice.message.content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::LocalModelConfig;
    use httpmock::prelude::*;

    #[test]
    fn test_cloud_fallback_env_blank() {
        let key = "NONEXISTENT_BLANK_KEY_TEST";
        unsafe {
            std::env::set_var(key, "   ");
        }
        assert!(cloud_fallback_env(key).is_none());
        unsafe {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_cloud_fallback_env_missing() {
        assert!(cloud_fallback_env("DEFINITELY_MISSING_KEY").is_none());
    }

    fn test_config(base_url: &str) -> LocalModelConfig {
        // Isolate from this repo's real `.env` (which may have real OpenRouter/Gemini
        // keys for manual use) so cloud_fallback_env() can't make these tests flaky.
        if let Ok(tmp) = std::env::temp_dir().canonicalize() {
            let _ = std::env::set_current_dir(tmp);
        }
        LocalModelConfig {
            base_url: base_url.to_string(),
            embedding_url: None,
            generation_url: None,
            generation_model: "test-model".to_string(),
            timeout_secs: 30,
            ..LocalModelConfig::default()
        }
    }

    fn test_messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: "system".to_string(),
                content: "You are a helpful assistant.".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Hello!".to_string(),
            },
        ]
    }

    #[test]
    fn complete_success() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "choices": [
                        {
                            "message": {
                                "content": "Hello! How can I help you today?"
                            }
                        }
                    ]
                }));
        });

        let config = test_config(&server.base_url());
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        )
        .unwrap();
        assert_eq!(result, "Hello! How can I help you today?");
    }

    #[test]
    fn complete_503_retry() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(503).body("Service Unavailable");
        });

        let config = test_config(&server.base_url());
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("503"));
        // Verify retry happened: 2 calls total
        assert_eq!(mock.hits(), 2);
    }

    #[test]
    fn complete_429_rate_limited() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(429).body("Too Many Requests");
        });

        let config = test_config(&server.base_url());
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("rate limited"));
    }

    #[test]
    fn complete_other_status_error() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(500).body("Internal Server Error");
        });

        let config = test_config(&server.base_url());
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("500"));
        assert!(err.contains("Internal Server Error"));
    }

    #[test]
    fn complete_connection_refused() {
        let config = test_config("http://127.0.0.1:1");
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("is unreachable"));
    }

    #[test]
    fn complete_empty_choices() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "choices": []
                }));
        });

        let config = test_config(&server.base_url());
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No completion choices"));
    }

    #[test]
    fn complete_empty_url() {
        let config = test_config("");
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("is not configured"));
    }

    #[test]
    fn completions_ping_success() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "choices": [{"message": {"content": "hi"}}]
                }));
        });
        let config = test_config(&server.base_url());
        let result = ping_completions(&config);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), "test-model");
    }

    #[test]
    fn completions_ping_transport_failure() {
        let config = test_config("http://127.0.0.1:1");
        let result = ping_completions(&config);
        assert!(result.is_err());
        assert!(!result.unwrap_err().is_empty(), "error should not be empty");
    }

    #[test]
    fn completions_ping_non_200() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(503).body("Service Unavailable");
        });
        let config = test_config(&server.base_url());
        let result = ping_completions(&config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("503"), "expected '503' in: {err}");
    }

    #[test]
    fn transport_error_includes_cause() {
        // Use a port that nothing is listening on
        let config = test_config("http://127.0.0.1:1");
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("is unreachable"),
            "expected 'is unreachable' in: {err}"
        );
    }

    /// U22.1 (red): proves the timeout override is honored. The mock delays
    /// 5 seconds; with a 1-second override the call must abort with a
    /// "timed out" error and return well before the mock would have responded.
    #[test]
    fn complete_timeout_override_fires() {
        use std::time::Instant;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .delay(std::time::Duration::from_secs(5))
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "choices": [{"message": {"content": "too late"}}]
                }));
        });

        let config = test_config(&server.base_url());
        let start = Instant::now();
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            Some(1),
        );
        let elapsed = start.elapsed();

        assert!(result.is_err(), "expected timeout error, got: {result:?}");
        let err = result.unwrap_err();
        assert!(
            err.contains("timed out"),
            "expected 'timed out' in error, got: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "expected <3s, got {elapsed:?}"
        );
    }

    /// U22.1 (red): when the override is None the call should still succeed
    /// (and fall back to the config-provided timeout_secs, which is 30s here
    /// — long enough to outlast the mock's 100ms response).
    #[test]
    fn complete_timeout_override_none_falls_back_to_config() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "choices": [{"message": {"content": "fast"}}]
                }));
        });

        let config = test_config(&server.base_url());
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert_eq!(result.unwrap(), "fast");
    }

    #[test]
    fn complete_falls_back_to_ollama_cloud_with_auth() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions")
                .header("Authorization", "Bearer test-token")
                .json_body_partial(r#"{"model":"minimax-m3:cloud"}"#);
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "choices": [
                        {
                            "message": {
                                "content": "cloud response"
                            }
                        }
                    ]
                }));
        });

        let config = LocalModelConfig {
            base_url: "http://127.0.0.1:1".to_string(),
            ollama_cloud_url: Some(server.base_url()),
            ollama_cloud_api_key: Some("test-token".to_string()),
            ollama_cloud_model: Some("minimax-m3:cloud".to_string()),
            ..test_config("")
        };

        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        )
        .unwrap();
        assert_eq!(result, "cloud response");
        assert_eq!(mock.hits(), 1);
    }

    #[test]
    fn test_detect_endpoint_kind_openai() {
        assert_eq!(
            detect_endpoint_kind("https://ollama.com"),
            EndpointKind::OpenAICompatible
        );
        assert_eq!(
            detect_endpoint_kind("https://ollama.com/"),
            EndpointKind::OpenAICompatible
        );
        assert_eq!(
            detect_endpoint_kind("http://localhost:11434/v1"),
            EndpointKind::OpenAICompatible
        );
    }

    #[test]
    fn test_detect_endpoint_kind_native() {
        assert_eq!(
            detect_endpoint_kind("https://ollama.com/api"),
            EndpointKind::OllamaNative
        );
        assert_eq!(
            detect_endpoint_kind("https://ollama.com/api/"),
            EndpointKind::OllamaNative
        );
        assert_eq!(
            detect_endpoint_kind("http://localhost:11434/api"),
            EndpointKind::OllamaNative
        );
        assert_eq!(
            detect_endpoint_kind("https://api.ollama.com"),
            EndpointKind::OllamaNative
        );
    }

    #[test]
    fn test_api_dot_ollama_com_uses_native_api_chat() {
        let target = completion_target("https://api.ollama.com");
        assert_eq!(target.kind, EndpointKind::OllamaNative);
        assert_eq!(target.url, "https://api.ollama.com/api/chat");
    }

    #[test]
    fn test_check_base_url_warning_malformed_api_v1() {
        let warning =
            check_base_url_warnings("https://ollama.com/api/v1", EndpointKind::OllamaNative);
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("Unsupported Ollama URL shape"));
    }

    #[test]
    fn test_check_base_url_no_warning_for_valid() {
        assert!(
            check_base_url_warnings("https://ollama.com", EndpointKind::OpenAICompatible).is_none()
        );
        assert!(
            check_base_url_warnings("https://ollama.com/api", EndpointKind::OllamaNative).is_none()
        );
        assert!(
            check_base_url_warnings("http://localhost:11434", EndpointKind::OpenAICompatible)
                .is_none()
        );
    }

    #[test]
    fn test_ollama_native_endpoint_success() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/chat")
                .json_body_partial(r#"{"model":"test-model"}"#);
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "message": {
                        "content": "Ollama native response"
                    }
                }));
        });

        // Use a base URL ending in /api to trigger native mode
        let native_url = format!("{}/api", server.base_url().trim_end_matches('/'));
        let config = LocalModelConfig {
            base_url: String::new(),
            generation_url: None,
            ollama_cloud_url: Some(native_url),
            ollama_cloud_api_key: Some("test-token".to_string()),
            ollama_cloud_model: Some("test-model".to_string()),
            ..test_config("http://127.0.0.1:1")
        };

        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        )
        .unwrap();
        assert_eq!(result, "Ollama native response");
    }

    #[test]
    fn test_ollama_native_empty_content_reasoning() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/chat");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "message": {
                        "content": "",
                        "thinking": "I am thinking deeply about this..."
                    }
                }));
        });

        let native_url = format!("{}/api", server.base_url().trim_end_matches('/'));
        let config = LocalModelConfig {
            base_url: String::new(),
            generation_url: None,
            ollama_cloud_url: Some(native_url),
            ollama_cloud_api_key: Some("test-token".to_string()),
            ollama_cloud_model: Some("test-model".to_string()),
            ..test_config("http://127.0.0.1:1")
        };

        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reasoning only"),
            "expected reasoning-only error, got: {err}"
        );
    }

    #[test]
    fn test_api_dot_ollama_com_native_endpoint_success() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/chat")
                .header("Authorization", "Bearer test-token")
                .json_body_partial(r#"{"model":"test-model"}"#);
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "message": {
                        "content": "api dot ollama native response"
                    }
                }));
        });

        let base = format!("{}/api", server.base_url());
        let config = LocalModelConfig {
            base_url: String::new(),
            generation_url: None,
            ollama_cloud_url: Some(base),
            ollama_cloud_api_key: Some("test-token".to_string()),
            ollama_cloud_model: Some("test-model".to_string()),
            ..test_config("http://127.0.0.1:1")
        };

        let response = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        )
        .unwrap();
        assert_eq!(response, "api dot ollama native response");
        assert_eq!(mock.hits(), 1);
    }

    #[test]
    fn test_openai_compatible_empty_content_reasoning() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "choices": [
                        {
                            "message": {
                                "content": "",
                                "reasoning": "internal chain"
                            }
                        }
                    ]
                }));
        });

        let config = test_config(&server.base_url());
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(
            result.is_ok(),
            "expected reasoning content as Ok, got: {:?}",
            result
        );
        let content = result.unwrap();
        assert_eq!(content, "internal chain");
    }

    #[test]
    fn test_openai_compatible_reasoning_content_alias() {
        // Verify reasoning_content field name (llama.cpp standard) maps to 'reasoning' in Rust
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "choices": [
                        {
                            "message": {
                                "content": "",
                                "reasoning_content": "llama.cpp thinking chain here"
                            }
                        }
                    ]
                }));
        });

        let config = test_config(&server.base_url());
        let result = complete(
            &config,
            &test_messages(),
            &CompletionOptions::default(),
            None,
        );
        assert!(
            result.is_ok(),
            "expected reasoning content from reasoning_content alias, got: {:?}",
            result
        );
        let content = result.unwrap();
        assert_eq!(content, "llama.cpp thinking chain here");
    }

    #[test]
    fn test_ollama_key_alias_in_config() {
        // Verify that 'ollama_key' serde alias works for LocalModelConfig
        let toml_str = r#"
        base_url = ""
        ollama_key = "test-key-value"
        ollama_cloud_model = "minimax-m3:cloud"
        "#;
        let config: LocalModelConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ollama_cloud_api_key.as_deref(),
            Some("test-key-value")
        );
    }
}
