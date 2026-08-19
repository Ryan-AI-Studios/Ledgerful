use super::*;
use crate::config::model::LocalModelConfig;
use crate::local_model::cloud_policy::CloudPolicy;
use httpmock::prelude::*;
use std::time::Duration;

#[test]
fn test_cloud_fallback_env_blank() {
    let key = "NONEXISTENT_BLANK_KEY_TEST";
    // Legitimate: test-only env mutation (edition-2024 set_var is unsafe).
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        std::env::set_var(key, "   ");
    }
    assert!(cloud_fallback_env(key).is_none());
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        std::env::remove_var(key);
    }
}

#[test]
fn test_cloud_fallback_env_missing() {
    assert!(cloud_fallback_env("DEFINITELY_MISSING_KEY").is_none());
}

mod env_guard {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/integration/common/env_guard.rs"
    ));
}

fn test_config(base_url: &str) -> LocalModelConfig {
    // Isolate from this repo's real `.env` (which may have real OpenRouter/Gemini
    // keys for manual use) so cloud_fallback_env() can't make these tests flaky.
    // Legitimate: chdir to OS temp so tests ignore the repo's real .env.
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
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

/// Clear process cloud credentials/policy so local-only assertions are not
/// masked by OpenRouter/Gemini fallback or Forbidden policy races.
/// Hold the returned guards for the duration of the test.
fn isolate_cloud_env() -> Vec<env_guard::TempEnv> {
    vec![
        env_guard::TempEnv::remove("GEMINI_API_KEY"),
        env_guard::TempEnv::remove("OPENROUTER_API_KEY"),
        env_guard::TempEnv::remove("OLLAMA_CLOUD_API_KEY"),
        env_guard::TempEnv::remove(crate::local_model::cloud_policy::CLOUD_POLICY_ENV),
    ]
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

/// 0158 DoD-3: Local mock delay >5s and < effective must succeed
/// (no 5s probe / 500ms precheck kill).
#[test]
#[serial_test::serial(env)]
fn complete_local_survives_delay_over_five_seconds() {
    use std::time::Instant;

    let _iso = isolate_cloud_env();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(200)
            .delay(Duration::from_secs(8))
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{"message": {"content": "cold-ok"}}]
            }));
    });

    let mut config = test_config(&server.base_url());
    config.timeout_secs = 15;
    let start = Instant::now();
    let result = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(15),
    );
    let elapsed = start.elapsed();
    assert!(
        result.is_ok(),
        "delay 8s under 15s budget must succeed: {result:?}"
    );
    assert_eq!(result.unwrap().trim(), "cold-ok");
    assert!(
        elapsed >= Duration::from_secs(7),
        "expected ~8s delay, got {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(14),
        "expected under effective, got {elapsed:?}"
    );
}

/// 0158 DoD-4: closed-port Local fails fast (no ~300s wait).
#[test]
#[serial_test::serial(env)]
fn complete_closed_port_local_fails_fast() {
    use std::time::Instant;

    let _iso = isolate_cloud_env();
    let mut config = test_config("http://127.0.0.1:1");
    config.timeout_secs = 300;
    let start = Instant::now();
    let result = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        None, // omitted CLI → would be 300 primary, but refused is immediate
    );
    let elapsed = start.elapsed();
    assert!(result.is_err(), "expected unreachable, got: {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("unreachable")
            || err.to_lowercase().contains("refused")
            || err.to_lowercase().contains("not reachable"),
        "expected unreachable/refused, got: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "closed port must not wait local budget, got {elapsed:?}"
    );
}

/// 0158 M2: when CLI override is None, cloud fallback uses ≤15-class budget,
/// not local 300.
#[test]
#[serial_test::serial(env)]
fn complete_cloud_fallback_uses_short_budget_when_override_none() {
    use env_guard::TempEnv;
    use std::time::Instant;

    let openrouter = MockServer::start();
    openrouter.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(200)
            .delay(Duration::from_secs(20))
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{"message": {"content": "too-slow"}}]
            }));
    });

    let _gem = TempEnv::remove("GEMINI_API_KEY");
    let _or = TempEnv::set("OPENROUTER_API_KEY", "sk-or-test-not-real");
    let _orm = TempEnv::set("OPENROUTER_MODEL", "test/model");
    let _orb = TempEnv::set("OPENROUTER_BASE_URL", &openrouter.base_url());

    let mut config = test_config("http://127.0.0.1:1"); // local unreachable
    config.timeout_secs = 300;

    let start = Instant::now();
    let result = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        None, // omitted → cloud fallback 15s class
    );
    let elapsed = start.elapsed();
    assert!(
        result.is_err(),
        "expected cloud timeout/fail, got: {result:?}"
    );
    // Must finish well under local 300s; cloud arm is 15 + connect buffer.
    assert!(
        elapsed < Duration::from_secs(25),
        "cloud fallback must not inherit 300s local budget, got {elapsed:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(10),
        "expected cloud arm to run ~15s class, got {elapsed:?}"
    );
}

/// 0158 H1: Gemini cloud-fallback arm uses short M2 budget when override is None
/// (not default GeminiConfig / not .max(300) floor).
#[test]
fn gemini_fallback_budget_is_short_when_override_none() {
    use super::gemini::gemini_read_timeout_secs;
    use crate::config::model::GeminiConfig;

    // Mirrors complete_with_options Gemini arm wiring when CLI override is None.
    let cloud_timeout = DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS;
    let fallback = GeminiConfig {
        api_key: Some("test-key-not-real".to_string()),
        timeout_secs: Some(cloud_timeout),
        ..Default::default()
    };
    assert_eq!(cloud_timeout, 15);
    assert_eq!(gemini_read_timeout_secs(&fallback), 15);
    // Explicit override N must also win (same as OR/OC).
    let explicit = GeminiConfig {
        api_key: Some("test-key-not-real".to_string()),
        timeout_secs: Some(3),
        ..Default::default()
    };
    assert_eq!(gemini_read_timeout_secs(&explicit), 3);
    // Unset config (primary-class) stays 120, not 300.
    assert_eq!(gemini_read_timeout_secs(&GeminiConfig::default()), 120);
}

#[test]
#[serial_test::serial(env)]
fn complete_success() {
    let _iso = isolate_cloud_env();
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
#[serial_test::serial(env)]
fn complete_503_retry() {
    let _iso = isolate_cloud_env();
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
    assert_eq!(mock.calls(), 2);
}

#[test]
#[serial_test::serial(env)]
fn complete_429_rate_limited() {
    let _iso = isolate_cloud_env();
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
#[serial_test::serial(env)]
fn complete_other_status_error() {
    let _iso = isolate_cloud_env();
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
#[serial_test::serial(env)]
fn complete_connection_refused() {
    let _iso = isolate_cloud_env();
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
#[serial_test::serial(env)]
fn complete_empty_choices() {
    let _iso = isolate_cloud_env();
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
#[serial_test::serial(env)]
fn complete_empty_url() {
    let _iso = isolate_cloud_env();
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
#[serial_test::serial(env)]
fn transport_error_includes_cause() {
    let _iso = isolate_cloud_env();
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
#[serial_test::serial(env)]
fn complete_timeout_override_fires() {
    use std::time::Instant;

    let _iso = isolate_cloud_env();
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

/// U17.3: a fast full response must complete when the first-byte wrapper is
/// active. This exercises the success path of
/// `complete_with_first_byte_timeout` end-to-end with a real HTTP server.
#[test]
#[serial_test::serial(env)]
fn complete_first_byte_timeout_fast_response_succeeds() {
    let _iso = isolate_cloud_env();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{"message": {"content": "fast but timed"}}]
            }));
    });

    let config = test_config(&server.base_url());
    let result = complete_with_first_byte_timeout(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        None,
        None,
    );
    assert!(result.is_ok(), "expected Ok, got: {result:?}");
    assert_eq!(result.unwrap(), "fast but timed");
}

/// U22.1 (red): when the override is None the call should still succeed
/// (and fall back to the config-provided timeout_secs, which is 30s here
/// — long enough to outlast the mock's 100ms response).
#[test]
#[serial_test::serial(env)]
fn complete_timeout_override_none_falls_back_to_config() {
    let _iso = isolate_cloud_env();
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
#[serial_test::serial(env)]
fn complete_falls_back_to_ollama_cloud_with_auth() {
    // Clear Forbidden policy so Ollama Cloud fallback is allowed.
    let _iso = isolate_cloud_env();
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions")
            .header("Authorization", "Bearer test-token")
            .json_body_includes(r#"{"model":"minimax-m3:cloud"}"#);
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
    assert_eq!(mock.calls(), 1);
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
    let warning = check_base_url_warnings("https://ollama.com/api/v1", EndpointKind::OllamaNative);
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
        check_base_url_warnings("http://localhost:11434", EndpointKind::OpenAICompatible).is_none()
    );
}

#[test]
#[serial_test::serial(env)]
fn test_ollama_native_endpoint_success() {
    // Clear Forbidden policy so Ollama Cloud native path is allowed.
    let _iso = isolate_cloud_env();
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/api/chat")
            .json_body_includes(r#"{"model":"test-model"}"#);
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
#[serial_test::serial(env)]
fn test_ollama_native_empty_content_reasoning() {
    // Isolate cloud keys so OpenRouter/Gemini cannot mask the reasoning-only Err.
    let _iso = isolate_cloud_env();

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
#[serial_test::serial(env)]
fn test_api_dot_ollama_com_native_endpoint_success() {
    // Clear Forbidden policy so Ollama Cloud native path is allowed.
    let _iso = isolate_cloud_env();
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/api/chat")
            .header("Authorization", "Bearer test-token")
            .json_body_includes(r#"{"model":"test-model"}"#);
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
    assert_eq!(mock.calls(), 1);
}

#[test]
#[serial_test::serial(env)]
fn test_openai_compatible_empty_content_reasoning() {
    // 0159: thinking-only must fail closed — never promote reasoning as content.
    let _iso = isolate_cloud_env();

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
        result.is_err(),
        "expected reasoning-only Err, got: {:?}",
        result
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("reasoning only"),
        "expected greppable reasoning only, got: {err}"
    );
}

#[test]
#[serial_test::serial(env)]
fn test_openai_compatible_reasoning_content_alias() {
    // reasoning_content alias still deserializes; pure CoT must not be promoted (0159).
    let _iso = isolate_cloud_env();

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
        result.is_err(),
        "expected reasoning-only Err from reasoning_content alias, got: {:?}",
        result
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("reasoning only"),
        "expected greppable reasoning only, got: {err}"
    );
}

#[test]
#[serial_test::serial(env)]
fn test_openai_compatible_content_think_tags_stripped() {
    let _iso = isolate_cloud_env();

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
                            "content": "<think>scratch</think> Clean answer"
                        }
                    }
                ]
            }));
    });

    let config = test_config(&server.base_url());
    let response = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        None,
    )
    .unwrap();
    assert_eq!(response, "Clean answer");
}

#[test]
#[serial_test::serial(env)]
fn test_ollama_native_content_think_tags_stripped() {
    let _iso = isolate_cloud_env();

    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/api/chat");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "message": {
                    "content": "<think>inner monologue</think> Ollama answer"
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

    let response = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        None,
    )
    .unwrap();
    assert_eq!(response, "Ollama answer");
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

/// U17.1: an accept-then-hang server (TCP accepts but never sends headers)
/// must fail fast via the first-byte timeout, not wait for the full read
/// timeout.
#[test]
#[serial_test::serial(env)]
fn complete_first_byte_timeout_accept_then_hang() {
    use std::time::Instant;

    let _iso = isolate_cloud_env();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("get local addr");

    std::thread::spawn(move || {
        // Accept connections and hold them open without reading/writing,
        // simulating an overloaded or hung model server.
        while let Ok((stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
            // Leak this thread until the test process exits; the
            // first-byte wrapper returns long before then.
            std::thread::sleep(Duration::from_secs(60));
        }
    });

    // Give the listener thread a moment to start accepting.
    std::thread::sleep(Duration::from_millis(100));

    let mut config = test_config(&format!("http://{}", addr));
    // Keep the inner ureq read timeout short so the spawned worker thread
    // finishes quickly after the first-byte timeout fires.
    config.timeout_secs = 5;

    let start = Instant::now();
    let result = complete_with_first_byte_timeout(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(3),
        Some(2),
    );
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "expected first-byte timeout error, got: {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        is_first_byte_timeout_error(&err),
        "expected first-byte timeout error, got: {err}"
    );
    // Under load CI may add ~1s; still well under the 5s read timeout.
    assert!(
        elapsed < Duration::from_secs(5),
        "expected <5s first-byte fail-fast, got {elapsed:?}"
    );
}

/// U17.2: connection-refused must fail fast without waiting for the first
/// byte or the read timeout.
#[test]
#[serial_test::serial(env)]
fn complete_first_byte_timeout_connection_refused() {
    use std::time::Instant;

    // Isolate cloud keys so fallback cannot mask the local refused path.
    let _iso = isolate_cloud_env();

    let config = test_config("http://127.0.0.1:1");
    let start = Instant::now();
    let result = complete_with_first_byte_timeout(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        None,
        Some(2),
    );
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "expected unreachable error, got: {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("is unreachable"),
        "expected 'is unreachable' in error, got: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "expected fast fail <5s, got {elapsed:?}"
    );
}

#[test]
fn is_first_byte_timeout_error_classifies() {
    assert!(is_first_byte_timeout_error(
        "First byte timeout: model did not begin responding within 2s"
    ));
    assert!(!is_first_byte_timeout_error("Some other transport error"));
    assert!(!is_first_byte_timeout_error(""));
}

// --- Track 0073: CloudPolicy Forbidden network-assertion matrix ---

use crate::local_model::cloud_policy::{
    CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_CODE, CLOUD_POLICY_FORBIDDEN_VALUE,
    MCP_ALLOW_CLOUD_EGRESS_ENV,
};
use env_guard::TempEnv;

/// Isolate process env + chdir so repo `.env` / ambient keys cannot enable cloud.
fn forbidden_cloud_isolation() -> Vec<TempEnv> {
    // Legitimate: chdir to OS temp so tests ignore the repo's real .env.
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    if let Ok(tmp) = std::env::temp_dir().canonicalize() {
        let _ = std::env::set_current_dir(tmp);
    }
    vec![
        TempEnv::set(CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE),
        TempEnv::remove(MCP_ALLOW_CLOUD_EGRESS_ENV),
        TempEnv::set("GEMINI_API_KEY", "test-gemini-key-not-real"),
        TempEnv::set("OPENROUTER_API_KEY", "sk-or-v1-test-not-real"),
        TempEnv::remove("OPENROUTER_BASE_URL"),
    ]
}

#[test]
#[serial_test::serial(env)]
fn has_cloud_fallback_false_under_forbidden_even_with_keys() {
    let _guards = forbidden_cloud_isolation();
    let mut config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        ollama_cloud_url: Some("https://api.ollama.com".to_string()),
        ollama_cloud_api_key: Some("ollama-key".to_string()),
        ollama_cloud_model: Some("model:cloud".to_string()),
        ..LocalModelConfig::default()
    };
    config.generation_model = "test".to_string();
    assert!(
        has_cloud_fallback_credentials(&config),
        "credentials must be visible so the test is meaningful"
    );
    assert!(
        !has_cloud_fallback(&config),
        "has_cloud_fallback must ignore cloud under Forbidden"
    );
    // Local URL present → still configured; cloud-only would not be.
    assert!(is_configured(&config));
    config.base_url.clear();
    assert!(
        !is_configured(&config),
        "is_configured must ignore cloud keys under Forbidden"
    );
}

#[test]
#[serial_test::serial(env)]
fn complete_forbidden_zero_http_to_ollama_cloud_mock() {
    let cloud = MockServer::start();
    let mock = cloud.mock(|when, then| {
        when.any_request();
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{ "message": { "content": "should never see this" } }]
            }));
    });

    let _guards = forbidden_cloud_isolation();
    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        generation_model: "test-model".to_string(),
        timeout_secs: 5,
        ollama_cloud_url: Some(cloud.base_url()),
        ollama_cloud_api_key: Some("ollama-key".to_string()),
        ollama_cloud_model: Some("model:cloud".to_string()),
        ..LocalModelConfig::default()
    };

    let err = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(3),
    )
    .expect_err("Forbidden + local-down must error without cloud");
    assert!(
        err.contains(CLOUD_POLICY_FORBIDDEN_CODE),
        "error must name cloud_policy_forbidden, got: {err}"
    );
    assert!(
        err.contains(MCP_ALLOW_CLOUD_EGRESS_ENV),
        "error must name opt-in env, got: {err}"
    );
    mock.assert_calls(0);
}

/// F-002: compose MCP spawn-env helper + complete under the inherited
/// Forbidden marker (keystone path without spawning a real binary).
#[test]
#[serial_test::serial(env)]
fn mcp_spawn_env_inherited_forbidden_zero_http_to_cloud_mock() {
    use crate::local_model::cloud_policy::{
        CLOUD_POLICY_ENV, MCP_ALLOW_CLOUD_EGRESS_ENV, mcp_tool_spawn_env,
    };

    let cloud = MockServer::start();
    let mock = cloud.mock(|when, then| {
        when.any_request();
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{ "message": { "content": "should never see this" } }]
            }));
    });

    // Parent MCP host: no allow-cloud opt-in → spawn env includes Forbidden.
    let _allow = TempEnv::remove(MCP_ALLOW_CLOUD_EGRESS_ENV);
    let spawn_env = mcp_tool_spawn_env();
    assert!(
        spawn_env
            .iter()
            .any(|(k, v)| k == CLOUD_POLICY_ENV && v == "forbidden"),
        "MCP spawn must set Forbidden marker"
    );
    assert!(
        spawn_env
            .iter()
            .any(|(k, v)| k == "LEDGERFUL_NON_INTERACTIVE" && v == "1")
    );

    // Child inherits spawn env (apply the same pairs the parent would set).
    let mut env_guards: Vec<TempEnv> = Vec::new();
    env_guards.push(TempEnv::remove("OPENROUTER_API_KEY"));
    env_guards.push(TempEnv::remove("GEMINI_API_KEY"));
    // Legitimate: chdir to OS temp so repo .env is ignored.
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    if let Ok(tmp) = std::env::temp_dir().canonicalize() {
        let _ = std::env::set_current_dir(tmp);
    }
    for (k, v) in &spawn_env {
        env_guards.push(TempEnv::set(k, v));
    }
    assert!(CloudPolicy::from_env().is_forbidden());

    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        generation_model: "test-model".to_string(),
        timeout_secs: 5,
        ollama_cloud_url: Some(cloud.base_url()),
        ollama_cloud_api_key: Some("ollama-key".to_string()),
        ollama_cloud_model: Some("model:cloud".to_string()),
        ..LocalModelConfig::default()
    };

    let err = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(3),
    )
    .expect_err("MCP-inherited Forbidden + local-down must not hit cloud");
    assert!(
        err.contains(CLOUD_POLICY_FORBIDDEN_CODE),
        "error must name cloud_policy_forbidden, got: {err}"
    );
    assert!(
        err.contains(MCP_ALLOW_CLOUD_EGRESS_ENV),
        "error must name opt-in env, got: {err}"
    );
    mock.assert_calls(0);
    drop(env_guards);
}

#[test]
#[serial_test::serial(env)]
fn complete_forbidden_zero_http_to_openrouter_mock() {
    let cloud = MockServer::start();
    let mock = cloud.mock(|when, then| {
        when.any_request();
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{ "message": { "content": "should never see this" } }]
            }));
    });

    let _guards = forbidden_cloud_isolation();
    // Point OPENROUTER_BASE_URL at the mock; Forbidden must still zero hits.
    let _or_url = TempEnv::set("OPENROUTER_BASE_URL", &cloud.base_url());
    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        generation_model: "test-model".to_string(),
        timeout_secs: 5,
        ..LocalModelConfig::default()
    };

    let err = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(3),
    )
    .expect_err("Forbidden + local-down must error without OpenRouter");
    assert!(err.contains(CLOUD_POLICY_FORBIDDEN_CODE), "got: {err}");
    assert!(err.contains(MCP_ALLOW_CLOUD_EGRESS_ENV), "got: {err}");
    mock.assert_calls(0);
}

#[test]
#[serial_test::serial(env)]
fn complete_forbidden_with_config_ollama_and_env_gemini_openrouter() {
    // Config-sourced Ollama Cloud + env-sourced Gemini/OpenRouter keys;
    // local closed; Forbidden → zero cloud HTTP + structured error.
    let cloud = MockServer::start();
    let mock = cloud.mock(|when, then| {
        when.any_request();
        then.status(200).body("nope");
    });

    let _guards = forbidden_cloud_isolation();
    let _or_url = TempEnv::set("OPENROUTER_BASE_URL", &cloud.base_url());
    let config = LocalModelConfig {
        base_url: String::new(),
        generation_url: None,
        generation_model: "test-model".to_string(),
        timeout_secs: 5,
        ollama_cloud_url: Some(cloud.base_url()),
        ollama_cloud_api_key: Some("from-config".to_string()),
        ollama_cloud_model: Some("cloud-model".to_string()),
        ..LocalModelConfig::default()
    };

    let err = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(3),
    )
    .expect_err("must fail closed");
    assert!(err.contains(CLOUD_POLICY_FORBIDDEN_CODE), "got: {err}");
    mock.assert_calls(0);
}

/// 0073 Codex R1: priority chain listing cloud backends must resolve
/// Local-only under Forbidden, and complete must emit zero cloud HTTP.
#[test]
#[serial_test::serial(env)]
fn priority_chain_forbidden_zero_http_to_cloud_mock() {
    use crate::commands::ask::{Backend, resolve_provider_entries};
    use crate::config::model::{Config, Provider, ProviderEntry, ProvidersConfig};

    let cloud = MockServer::start();
    let mock = cloud.mock(|when, then| {
        when.any_request();
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{ "message": { "content": "should never see this" } }]
            }));
    });

    let _guards = forbidden_cloud_isolation();
    let _or_url = TempEnv::set("OPENROUTER_BASE_URL", &cloud.base_url());
    let _or_key = TempEnv::set("OPENROUTER_API_KEY", "sk-or-v1-test-not-real");

    let mut config = Config::default();
    config.local_model.base_url = "http://127.0.0.1:1".to_string();
    config.local_model.generation_model = "test-model".to_string();
    config.local_model.timeout_secs = 5;
    config.local_model.ollama_cloud_url = Some(cloud.base_url());
    config.local_model.ollama_cloud_api_key = Some("ollama-key".to_string());
    config.local_model.ollama_cloud_model = Some("model:cloud".to_string());
    config.ask.providers = ProvidersConfig {
        priority: vec![
            ProviderEntry {
                backend: Provider::OpenRouter,
                model: Some("or-model".to_string()),
                timeout_secs: Some(5),
                api_key_env: Some("OPENROUTER_API_KEY".to_string()),
                base_url: Some(cloud.base_url()),
            },
            ProviderEntry {
                backend: Provider::OllamaCloud,
                model: Some("model:cloud".to_string()),
                timeout_secs: Some(5),
                api_key_env: None,
                base_url: Some(cloud.base_url()),
            },
            ProviderEntry {
                backend: Provider::Gemini,
                model: None,
                timeout_secs: Some(5),
                api_key_env: None,
                base_url: None,
            },
            ProviderEntry {
                backend: Provider::Local,
                model: Some("test-model".to_string()),
                timeout_secs: Some(5),
                api_key_env: None,
                base_url: Some("http://127.0.0.1:1".to_string()),
            },
        ],
    };

    let entries =
        resolve_provider_entries(&config, Some(Backend::Local)).expect("resolve must succeed");
    assert!(
        entries.iter().all(|e| e.backend == Provider::Local),
        "Forbidden priority chain must be pure Local-only, got: {entries:?}"
    );
    assert!(!entries.is_empty());

    // complete path that would be used after priority truncation
    let err = complete(
        &config.local_model,
        &test_messages(),
        &CompletionOptions::default(),
        Some(3),
    )
    .expect_err("Forbidden + local-down must not hit cloud via priority chain");
    assert!(
        err.contains(CLOUD_POLICY_FORBIDDEN_CODE),
        "error must name cloud_policy_forbidden, got: {err}"
    );
    assert!(
        err.contains(MCP_ALLOW_CLOUD_EGRESS_ENV),
        "error must name opt-in env, got: {err}"
    );
    mock.assert_calls(0);
}

#[test]
#[serial_test::serial(env)]
fn gemini_complete_blocked_under_forbidden() {
    let _guards = forbidden_cloud_isolation();
    let gemini = crate::config::model::GeminiConfig {
        api_key: Some("fake".to_string()),
        ..Default::default()
    };
    let err = gemini_complete(&gemini, &test_messages(), &CompletionOptions::default())
        .expect_err("gemini_complete must hard-fail under Forbidden");
    assert!(err.contains(CLOUD_POLICY_FORBIDDEN_CODE), "got: {err}");
    assert!(err.contains(MCP_ALLOW_CLOUD_EGRESS_ENV), "got: {err}");
}

#[test]
#[serial_test::serial(env)]
fn openrouter_sanitize_redacts_api_key_shaped_strings() {
    // Allowed policy: OpenRouter arm receives sanitized bodies.
    // A body_includes(secret) mock must see zero hits; the general mock gets the call.
    let server = MockServer::start();
    let secret = "AKIAIOSFODNN7EXAMPLE";
    // Register the leak detector first so it is evaluated when matching.
    let secret_leak_mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions")
            .body_includes(secret);
        then.status(500).body("secret leaked into request body");
    });
    let ok_mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            }));
    });

    let _pol = TempEnv::remove(CLOUD_POLICY_ENV);
    let _key = TempEnv::set("OPENROUTER_API_KEY", "sk-or-v1-test-key-not-real");
    let _url = TempEnv::set("OPENROUTER_BASE_URL", &server.base_url());
    // Legitimate: chdir to OS temp so repo .env is ignored.
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    if let Ok(tmp) = std::env::temp_dir().canonicalize() {
        let _ = std::env::set_current_dir(tmp);
    }

    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        generation_model: "test".to_string(),
        timeout_secs: 10,
        ..LocalModelConfig::default()
    };
    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: format!("api_key = \"{secret}\""),
    }];
    let result = complete(&config, &messages, &CompletionOptions::default(), Some(5));
    assert!(
        result.is_ok(),
        "OpenRouter mock should succeed under Allowed with sanitized body; got: {result:?}"
    );
    assert_eq!(
        secret_leak_mock.calls(),
        0,
        "OpenRouter request body must not contain the raw API-key-shaped secret"
    );
    assert!(
        ok_mock.calls() >= 1,
        "expected at least one sanitized OpenRouter call"
    );
}

// --- 0160 multi-cause cloud fallback honesty ---

/// DoD-1: local unreachable + OC thinking-only → multi-cause with local + reasoning only + remediation.
#[test]
#[serial_test::serial(env)]
fn multi_cause_local_unreachable_plus_oc_reasoning_only() {
    let _iso = isolate_cloud_env();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/api/chat");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "message": {
                    "content": "",
                    "thinking": "I am thinking deeply about the dogfood case..."
                }
            }));
    });

    let native_url = format!("{}/api", server.base_url().trim_end_matches('/'));
    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        generation_url: None,
        generation_model: "test-model".to_string(),
        timeout_secs: 10,
        ollama_cloud_url: Some(native_url),
        ollama_cloud_api_key: Some("test-token".to_string()),
        ollama_cloud_model: Some("test-model".to_string()),
        ..LocalModelConfig::default()
    };

    let err = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(5),
    )
    .expect_err("expected multi-cause exhaust");
    assert!(
        err.contains("Cloud fallback exhausted"),
        "M2 greppable exhausted: {err}"
    );
    assert!(
        err.to_lowercase().contains("unreachable") || err.to_lowercase().contains("not reachable"),
        "local cause retained: {err}"
    );
    assert!(
        err.contains("reasoning only"),
        "0159 greppable reasoning only: {err}"
    );
    assert!(
        err.contains("Next:")
            || err.contains("LEDGERFUL_CLOUD_POLICY")
            || err.to_lowercase().contains("gemini")
            || err.to_lowercase().contains("timeout"),
        "actionable remediation: {err}"
    );
    assert!(
        err.contains("Primary: content-quality") || err.contains("content-quality"),
        "primary CQ: {err}"
    );
}

/// DoD-3 / M1: cloud-only OC reasoning-only — no false local claim.
#[test]
#[serial_test::serial(env)]
fn multi_cause_cloud_only_reasoning_only_no_false_local() {
    let _iso = isolate_cloud_env();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "",
                        "reasoning": "cloud-only chain of thought"
                    }
                }]
            }));
    });

    let config = LocalModelConfig {
        base_url: String::new(),
        generation_url: None,
        generation_model: "test-model".to_string(),
        timeout_secs: 10,
        ollama_cloud_url: Some(server.base_url()),
        ollama_cloud_api_key: Some("test-token".to_string()),
        ollama_cloud_model: Some("test-model".to_string()),
        ..LocalModelConfig::default()
    };

    let err = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(5),
    )
    .expect_err("expected cloud-only CQ fail");
    assert!(err.contains("Cloud fallback exhausted"), "M2: {err}");
    assert!(
        !err.contains("after local attempt"),
        "M1 no after local attempt: {err}"
    );
    assert!(
        !err.lines().any(|l| l.trim().starts_with("Local:")),
        "M1 no Local: section: {err}"
    );
    assert!(err.contains("reasoning only"), "reasoning only: {err}");
}

/// M4: hard-deadline sizing includes cloud arms; cascade report not erased.
#[test]
#[serial_test::serial(env)]
fn hard_deadline_sizing_and_multi_cause_not_opaque_only() {
    let _iso = isolate_cloud_env();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(200)
            .delay(Duration::from_millis(400))
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "",
                        "reasoning": "delayed reasoning only"
                    }
                }]
            }));
    });

    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        generation_model: "test-model".to_string(),
        timeout_secs: 30,
        ollama_cloud_url: Some(server.base_url()),
        ollama_cloud_api_key: Some("test-token".to_string()),
        ollama_cloud_model: Some("test-model".to_string()),
        ..LocalModelConfig::default()
    };

    // Explicit short CLI timeout: primary=3, cloud=3, arms=1 → deadline = 3+3+5 = 11.
    let override_secs = Some(3u64);
    let deadline = hard_deadline_secs(&config, override_secs);
    assert!(
        deadline >= 3 + 3 + HARD_DEADLINE_BUFFER_SECS,
        "M4 formula primary + arms*cloud + buffer, got {deadline}"
    );
    // Without cloud, would be primary+5 only.
    let mut no_cloud = config.clone();
    no_cloud.ollama_cloud_url = None;
    no_cloud.ollama_cloud_api_key = None;
    no_cloud.ollama_cloud_model = None;
    assert_eq!(
        hard_deadline_secs(&no_cloud, override_secs),
        3 + HARD_DEADLINE_BUFFER_SECS
    );

    let err = complete_with_hard_deadline(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        override_secs,
    )
    .expect_err("expected multi-cause or cascade error");
    // Must not be opaque hard-timeout-only when cascade can produce a report.
    let is_hard_only = err.starts_with("Hard timeout:")
        && !err.contains("cascade")
        && !err.contains("reasoning only");
    assert!(
        !is_hard_only,
        "M4 must not erase to opaque hard-timeout-only, got: {err}"
    );
    assert!(
        err.contains("Cloud fallback exhausted") || err.contains("reasoning only"),
        "expected multi-cause or CQ tokens, got: {err}"
    );
}

#[test]
#[serial_test::serial(env)]
fn hard_deadline_message_mentions_cascade_when_cloud_possible() {
    // Pure formula unit (no HTTP): with credentials, deadline expands.
    let _iso = isolate_cloud_env();
    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        ollama_cloud_url: Some("https://example.invalid".to_string()),
        ollama_cloud_api_key: Some("k".to_string()),
        ollama_cloud_model: Some("m".to_string()),
        timeout_secs: 10,
        ..LocalModelConfig::default()
    };
    assert_eq!(configured_cloud_arm_count(&config), 1);
    // arms=1 → primary + 1*cloud + buffer
    assert_eq!(
        hard_deadline_secs(&config, Some(10)),
        10 + 10 + HARD_DEADLINE_BUFFER_SECS
    );
    assert_eq!(
        hard_deadline_secs(&config, None),
        10 + DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS + HARD_DEADLINE_BUFFER_SECS
    );
}

/// B4: content-quality on OC short-circuits — OR mock must not be hit.
#[test]
#[serial_test::serial(env)]
fn content_quality_short_circuits_further_cloud_arms() {
    use env_guard::TempEnv;

    let oc = MockServer::start();
    oc.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "",
                        "reasoning": "stop here"
                    }
                }]
            }));
    });
    let or = MockServer::start();
    let or_mock = or.mock(|when, then| {
        when.any_request();
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{ "message": { "content": "should not be called" } }]
            }));
    });

    let _gem = TempEnv::remove("GEMINI_API_KEY");
    let _pol = TempEnv::remove(crate::local_model::cloud_policy::CLOUD_POLICY_ENV);
    let _or = TempEnv::set("OPENROUTER_API_KEY", "sk-or-test-not-real");
    let _orm = TempEnv::set("OPENROUTER_MODEL", "test/model");
    let _orb = TempEnv::set("OPENROUTER_BASE_URL", &or.base_url());
    // Isolate repo .env (Gemini keys etc.) without leaking process cwd.
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    let _cwd = if let Ok(tmp) = std::env::temp_dir().canonicalize() {
        Some(crate::tests::DirGuard::new(&tmp))
    } else {
        None
    };

    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        generation_model: "test-model".to_string(),
        timeout_secs: 10,
        ollama_cloud_url: Some(oc.base_url()),
        ollama_cloud_api_key: Some("test-token".to_string()),
        ollama_cloud_model: Some("test-model".to_string()),
        ..LocalModelConfig::default()
    };

    let err = complete(
        &config,
        &test_messages(),
        &CompletionOptions::default(),
        Some(5),
    )
    .expect_err("CQ exhaust");
    assert!(err.contains("reasoning only") || err.contains("content-quality"));
    assert_eq!(
        or_mock.calls(),
        0,
        "B4: OpenRouter must not be called after OC content-quality"
    );
}

/// Codex P2: Forbidden policy + credentials must not expand hard-deadline for
/// nonexistent cloud arms (has_cloud_fallback, not credentials alone).
#[test]
#[serial_test::serial(env)]
fn hard_deadline_forbidden_policy_does_not_expand_for_creds() {
    use env_guard::TempEnv;
    let _iso = isolate_cloud_env();
    let _pol = TempEnv::set(
        crate::local_model::cloud_policy::CLOUD_POLICY_ENV,
        "forbidden",
    );
    let config = LocalModelConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        ollama_cloud_url: Some("https://example.invalid".to_string()),
        ollama_cloud_api_key: Some("k".to_string()),
        ollama_cloud_model: Some("m".to_string()),
        timeout_secs: 10,
        ..LocalModelConfig::default()
    };
    assert!(
        has_cloud_fallback_credentials(&config),
        "creds present for fixture"
    );
    assert!(
        !has_cloud_fallback(&config),
        "Forbidden must deny cloud fallback"
    );
    assert_eq!(
        hard_deadline_secs(&config, Some(10)),
        10 + HARD_DEADLINE_BUFFER_SECS,
        "must not expand for cloud arms when policy forbids cascade"
    );
}
