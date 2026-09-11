//! Non-loading generation readiness probe: `GET {origin}/health` only.
//!
//! Never GET `/v1/health` — C:\llm's fallback proxy loads the model.

use crate::config::model::LocalModelConfig;
use serde_json::Value;
use std::time::Duration;

/// Strip trailing `/` and a trailing `/v1` so a configured OpenAI-style
/// base (`http://127.0.0.1:8081/v1`) still hits origin `/health`.
pub(crate) fn health_origin(url: &str) -> String {
    let mut base = url.trim().trim_end_matches('/').to_string();
    if let Some(stripped) = base.strip_suffix("/v1") {
        base = stripped.trim_end_matches('/').to_string();
    }
    base
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealthProbeResult {
    Ready {
        display: String,
    },
    Cold {
        detail: String,
    },
    Loading {
        detail: String,
    },
    Busy {
        detail: String,
    },
    Unreachable {
        detail: String,
    },
    /// No `/health` or unknown body — caller may POST ping.
    Unknown,
}

/// Probe `{origin}/health` (and `/models` only when `status=ok` lacks `model_loaded`).
pub(crate) fn probe_generation_health(config: &LocalModelConfig) -> HealthProbeResult {
    let check_url = config.generation_url.as_deref().unwrap_or(&config.base_url);
    if check_url.trim().is_empty() {
        return HealthProbeResult::Unknown;
    }
    let origin = health_origin(check_url);
    if origin.is_empty() {
        return HealthProbeResult::Unknown;
    }

    let budget = config.timeout_secs.max(1);
    let connect = Duration::from_secs(budget.min(30));
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(connect)
        .timeout_read(Duration::from_secs(budget))
        .timeout_write(Duration::from_secs(budget.min(30)))
        .build();

    let health_url = format!("{origin}/health");
    match agent.get(&health_url).call() {
        Ok(resp) => classify_health_ok(config, &agent, &origin, resp.status(), read_body(resp)),
        Err(ureq::Error::Status(code, resp)) => {
            classify_health_status(config, &agent, &origin, code, read_body(resp))
        }
        Err(ureq::Error::Transport(inner)) => {
            if crate::local_model::client::util::transport_is_timeout(&inner) {
                HealthProbeResult::Unknown
            } else if matches!(
                inner.kind(),
                ureq::ErrorKind::ConnectionFailed | ureq::ErrorKind::Dns
            ) {
                HealthProbeResult::Unreachable {
                    detail: format!("Local model server at {check_url} is unreachable"),
                }
            } else {
                HealthProbeResult::Unknown
            }
        }
    }
}

fn read_body(resp: ureq::Response) -> String {
    resp.into_string().unwrap_or_default()
}

fn classify_health_status(
    config: &LocalModelConfig,
    agent: &ureq::Agent,
    origin: &str,
    code: u16,
    body: String,
) -> HealthProbeResult {
    match code {
        409 => HealthProbeResult::Busy {
            detail: "VRAM conflict".to_string(),
        },
        503 => HealthProbeResult::Loading {
            detail: loading_detail(&body),
        },
        200 => classify_health_ok(config, agent, origin, 200, body),
        _ => HealthProbeResult::Unknown,
    }
}

fn classify_health_ok(
    config: &LocalModelConfig,
    agent: &ureq::Agent,
    origin: &str,
    status: u16,
    body: String,
) -> HealthProbeResult {
    if status != 200 {
        return HealthProbeResult::Unknown;
    }
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return HealthProbeResult::Unknown,
    };

    if parsed.get("proxy").and_then(Value::as_bool) == Some(true) {
        return match parsed.get("model_loaded").and_then(Value::as_bool) {
            Some(true) => HealthProbeResult::Ready {
                display: ready_display(config),
            },
            _ => HealthProbeResult::Cold {
                detail: "idle".to_string(),
            },
        };
    }

    if let Some(loaded) = parsed.get("model_loaded").and_then(Value::as_bool) {
        return if loaded {
            HealthProbeResult::Ready {
                display: ready_display(config),
            }
        } else {
            HealthProbeResult::Cold {
                detail: "idle".to_string(),
            }
        };
    }

    if parsed.get("status").and_then(Value::as_str) == Some("ok") {
        return classify_models_listing(config, agent, origin);
    }

    HealthProbeResult::Unknown
}

fn classify_models_listing(
    config: &LocalModelConfig,
    agent: &ureq::Agent,
    origin: &str,
) -> HealthProbeResult {
    let models_url = format!("{origin}/models");
    let body = match agent.get(&models_url).call() {
        Ok(resp) => read_body(resp),
        Err(ureq::Error::Status(200, resp)) => read_body(resp),
        Err(_) => {
            return HealthProbeResult::Cold {
                detail: "model not loaded".to_string(),
            };
        }
    };
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return HealthProbeResult::Cold {
                detail: "model not loaded".to_string(),
            };
        }
    };
    let Some(rows) = parsed.get("data").and_then(Value::as_array) else {
        return HealthProbeResult::Cold {
            detail: "model not loaded".to_string(),
        };
    };
    let mut any_loading = false;
    for row in rows {
        let value = row
            .get("status")
            .and_then(|s| s.get("value").or(Some(s)))
            .and_then(Value::as_str)
            .unwrap_or("");
        if value.eq_ignore_ascii_case("loaded") {
            return HealthProbeResult::Ready {
                display: ready_display(config),
            };
        }
        if value.eq_ignore_ascii_case("loading") {
            any_loading = true;
        }
    }
    if any_loading {
        HealthProbeResult::Loading {
            detail: "loading model".to_string(),
        }
    } else {
        HealthProbeResult::Cold {
            detail: "model not loaded".to_string(),
        }
    }
}

fn loading_detail(body: &str) -> String {
    if body.to_ascii_lowercase().contains("load") {
        "loading model".to_string()
    } else {
        "loading".to_string()
    }
}

fn ready_display(config: &LocalModelConfig) -> String {
    if config.generation_model.trim().is_empty() {
        "ready".to_string()
    } else {
        config.generation_model.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn cfg(base: &str) -> LocalModelConfig {
        LocalModelConfig {
            base_url: base.to_string(),
            generation_url: None,
            generation_model: "gemma".to_string(),
            timeout_secs: 2,
            ..LocalModelConfig::default()
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn health_origin__strips_trailing_v1() {
        assert_eq!(
            health_origin("http://127.0.0.1:8081/v1"),
            "http://127.0.0.1:8081"
        );
        assert_eq!(
            health_origin("http://127.0.0.1:8081/v1/"),
            "http://127.0.0.1:8081"
        );
        assert_eq!(
            health_origin("http://127.0.0.1:8081/"),
            "http://127.0.0.1:8081"
        );
    }

    #[test]
    fn health_model_loaded_false_is_cold() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"model_loaded":false,"proxy":true}"#);
        });
        let completions = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).body("{}");
        });
        let v1_health = server.mock(|when, then| {
            when.method(GET).path("/v1/health");
            then.status(200).body("{}");
        });
        let models = server.mock(|when, then| {
            when.method(GET).path("/models");
            then.status(200).body("{}");
        });

        let result = probe_generation_health(&cfg(&server.base_url()));
        assert_eq!(
            result,
            HealthProbeResult::Cold {
                detail: "idle".to_string()
            }
        );
        health.assert();
        completions.assert_calls(0);
        v1_health.assert_calls(0);
        models.assert_calls(0);
    }

    #[test]
    fn health_model_loaded_true_is_ready() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"model_loaded":true}"#);
        });
        let completions = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).body("{}");
        });

        let result = probe_generation_health(&cfg(&server.base_url()));
        assert_eq!(
            result,
            HealthProbeResult::Ready {
                display: "gemma".to_string()
            }
        );
        health.assert();
        completions.assert_calls(0);
    }

    #[test]
    fn health_llamacpp_503_is_loading() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(503)
                .header("content-type", "application/json")
                .body(r#"{"error":{"message":"Loading model"}}"#);
        });
        let completions = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).body("{}");
        });

        let result = probe_generation_health(&cfg(&server.base_url()));
        assert_eq!(
            result,
            HealthProbeResult::Loading {
                detail: "loading model".to_string()
            }
        );
        health.assert();
        completions.assert_calls(0);
    }

    #[test]
    fn health_llamacpp_200_status_ok_without_models_is_cold() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"status":"ok"}"#);
        });
        let models = server.mock(|when, then| {
            when.method(GET).path("/models");
            then.status(404).body("missing");
        });
        let completions = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).body("{}");
        });

        let result = probe_generation_health(&cfg(&server.base_url()));
        assert_eq!(
            result,
            HealthProbeResult::Cold {
                detail: "model not loaded".to_string()
            }
        );
        health.assert();
        models.assert();
        completions.assert_calls(0);
    }

    #[test]
    fn health_llamacpp_models_loaded_is_ready() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"status":"ok"}"#);
        });
        let models = server.mock(|when, then| {
            when.method(GET).path("/models");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"data":[{"id":"gemma","status":{"value":"loaded"}}]}"#);
        });
        let completions = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).body("{}");
        });

        let result = probe_generation_health(&cfg(&server.base_url()));
        assert_eq!(
            result,
            HealthProbeResult::Ready {
                display: "gemma".to_string()
            }
        );
        health.assert();
        models.assert();
        completions.assert_calls(0);
    }

    #[test]
    fn health_409_is_busy() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(409).body(r#"{"error":"vram_conflict"}"#);
        });
        let completions = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).body("{}");
        });

        let result = probe_generation_health(&cfg(&server.base_url()));
        assert_eq!(
            result,
            HealthProbeResult::Busy {
                detail: "VRAM conflict".to_string()
            }
        );
        health.assert();
        completions.assert_calls(0);
    }

    #[test]
    fn health_v1_base_hits_origin_health() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"model_loaded":false}"#);
        });
        let v1_health = server.mock(|when, then| {
            when.method(GET).path("/v1/health");
            then.status(200).body(r#"{"model_loaded":true}"#);
        });

        let mut config = cfg(&format!("{}/v1", server.base_url()));
        config.generation_url = Some(format!("{}/v1", server.base_url()));
        let result = probe_generation_health(&config);
        assert_eq!(
            result,
            HealthProbeResult::Cold {
                detail: "idle".to_string()
            }
        );
        health.assert();
        v1_health.assert_calls(0);
    }

    #[test]
    fn no_health_is_unknown() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(404).body("no");
        });
        let result = probe_generation_health(&cfg(&server.base_url()));
        assert_eq!(result, HealthProbeResult::Unknown);
        health.assert();
    }

    #[test]
    fn health_proxy_true_without_model_loaded_is_cold() {
        let server = MockServer::start();
        let health = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"proxy":true}"#);
        });
        let models = server.mock(|when, then| {
            when.method(GET).path("/models");
            then.status(200).body("{}");
        });
        let completions = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).body("{}");
        });
        let result = probe_generation_health(&cfg(&server.base_url()));
        assert_eq!(
            result,
            HealthProbeResult::Cold {
                detail: "idle".to_string()
            }
        );
        health.assert();
        models.assert_calls(0);
        completions.assert_calls(0);
    }

    #[test]
    fn empty_url_is_unknown() {
        let config = LocalModelConfig {
            base_url: String::new(),
            generation_url: Some(String::new()),
            generation_model: "gemma".to_string(),
            timeout_secs: 2,
            ..LocalModelConfig::default()
        };
        assert_eq!(probe_generation_health(&config), HealthProbeResult::Unknown);
    }
}
