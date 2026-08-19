use super::cloud;
use super::cloud::has_ollama_cloud_fallback;
use super::completion_text;
use super::fallback_error;
use super::fallback_error::format_full_report;
use super::gemini::gemini_complete_unsanitized;
use super::ollama;
use super::openai;
use super::types::{self, ChatMessage, CompletionOptions, EndpointKind, EndpointTarget};
use super::util::{check_base_url_warnings, completion_target, transport_is_timeout};

use crate::config::model::LocalModelConfig;
use crate::local_model::cloud_policy::{CloudPolicy, cloud_policy_forbidden_error};
use std::time::Duration;

/// Cap for Local TCP precheck and connect budgets (B2b / B3).
/// Cold delayed-accept routers need more than 500ms; connection refused
/// on loopback still returns immediately from the OS.
pub const LOCAL_TCP_PRECHECK_CAP_SECS: u64 = 30;

/// Cloud-fallback arm budget when CLI `--timeout` is omitted (M2 / B3b).
/// Must not inherit the local load budget (300).
pub const DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS: u64 = 15;

/// Extra seconds beyond primary (+ cloud cascade when configured) for the hard-deadline
/// wrapper so ureq can fire a specific error first (0160 M4 / TA15).
pub const HARD_DEADLINE_BUFFER_SECS: u64 = 5;

/// Cloud endpoints keep a short connect budget.
const CLOUD_CONNECT_TIMEOUT_SECS: u64 = 5;

/// Local TCP precheck / connect budget: `min(cap, effective)`.
fn local_tcp_budget_secs(effective_timeout: u64) -> u64 {
    std::cmp::min(LOCAL_TCP_PRECHECK_CAP_SECS, effective_timeout)
}

fn connect_timeout_secs(is_local: bool, effective_timeout: u64) -> u64 {
    if is_local {
        local_tcp_budget_secs(effective_timeout)
    } else {
        CLOUD_CONNECT_TIMEOUT_SECS
    }
}

/// Reads a cloud-fallback credential/setting from the real process environment first,
/// falling back to a `.env` file in the current directory — matching the resolution
/// pattern already used for `OLLAMA_CLOUD_API_KEY` elsewhere in this module.
pub(crate) fn cloud_fallback_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| crate::config::model::read_env_key(key))
}

/// Whether cloud credentials/config are present (ignores [`CloudPolicy`]).
/// Prefer [`has_cloud_fallback`] at call sites that must honor Forbidden.
pub fn has_cloud_fallback_credentials(config: &LocalModelConfig) -> bool {
    has_ollama_cloud_fallback(config)
        || cloud_fallback_env("OPENROUTER_API_KEY").is_some()
        || cloud_fallback_env("GEMINI_API_KEY").is_some()
}

/// Cloud fallback is available only when credentials exist **and** policy allows.
/// Under `CloudPolicy::Forbidden`, always false (preflight honesty / zero cloud).
pub fn has_cloud_fallback(config: &LocalModelConfig) -> bool {
    if CloudPolicy::from_env().is_forbidden() {
        return false;
    }
    has_cloud_fallback_credentials(config)
}

/// Local or (when Allowed) cloud completion is configured.
/// Under Forbidden, cloud keys do not count as configured.
pub fn is_configured(config: &LocalModelConfig) -> bool {
    !config.base_url.is_empty() || config.generation_url.is_some() || has_cloud_fallback(config)
}

/// Count configured cloud fallback arms (credentials present; ignores policy).
/// Used for hard-deadline sizing (0160 M4).
pub fn configured_cloud_arm_count(config: &LocalModelConfig) -> u64 {
    let mut n = 0u64;
    if has_ollama_cloud_fallback(config) {
        n += 1;
    }
    if cloud_fallback_env("OPENROUTER_API_KEY").is_some() {
        n += 1;
    }
    if cloud_fallback_env("GEMINI_API_KEY").is_some() {
        n += 1;
    }
    n
}

/// Outer hard-deadline seconds for [`complete_with_hard_deadline`].
///
/// **Formula (0160 M4):**
/// - When cloud fallback is **actually available** ([`has_cloud_fallback`] — credentials
///   **and** policy allows; not mere credentials under `LEDGERFUL_CLOUD_POLICY=forbidden`):
///   `primary_timeout + (configured_cloud_arm_count * cloud_timeout) + HARD_DEADLINE_BUFFER_SECS`
///   so a full local+cloud cascade can finish under normal budgets and emit a multi-cause
///   report instead of opaque hard-timeout-only mid-cascade.
/// - No cloud fallback possible (no credentials or Forbidden):
///   `primary_timeout + HARD_DEADLINE_BUFFER_SECS` (legacy local-only).
///
/// `primary_timeout` / `cloud_timeout` match [`complete`]: `Some(n)` applies to all arms;
/// `None` uses config for local and [`DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS`] for cloud.
pub fn hard_deadline_secs(config: &LocalModelConfig, timeout_secs: Option<u64>) -> u64 {
    let primary_timeout = timeout_secs.unwrap_or(config.timeout_secs);
    let cloud_timeout = timeout_secs.unwrap_or(DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS);
    // Must match complete_with_options cloud_ok path (policy-aware), not credentials alone.
    if has_cloud_fallback(config) {
        let arms = configured_cloud_arm_count(config);
        primary_timeout
            .saturating_add(arms.saturating_mul(cloud_timeout))
            .saturating_add(HARD_DEADLINE_BUFFER_SECS)
    } else {
        primary_timeout.saturating_add(HARD_DEADLINE_BUFFER_SECS)
    }
}

use cloud::ollama_cloud_endpoint;
use ollama::ollama_native_num_predict;
use types::CompletionEndpoint;

pub fn ping_completions(config: &LocalModelConfig) -> Result<String, String> {
    if config.base_url.is_empty() && config.generation_url.is_none() {
        return Err("not configured".to_string());
    }

    let check_url = config.generation_url.as_deref().unwrap_or(&config.base_url);
    // Local TCP precheck uses min(30, effective) so delayed-accept cold routers
    // are not killed at 500ms (B2b). Connection refused on loopback stays fast.
    let precheck = Duration::from_secs(local_tcp_budget_secs(config.timeout_secs));
    if !crate::util::network::is_url_reachable(check_url, precheck) {
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

    // Local connect budget matches complete path (B3); read uses full config timeout.
    let connect_secs = connect_timeout_secs(true, config.timeout_secs);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(connect_secs))
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
    complete_with_options(config, messages, options, timeout_secs_override, None)
}

fn complete_with_options(
    config: &LocalModelConfig,
    messages: &[ChatMessage],
    options: &CompletionOptions,
    timeout_secs_override: Option<u64>,
    first_byte_secs: Option<u64>,
) -> Result<String, String> {
    let policy = CloudPolicy::from_env();
    let cloud_ok = has_cloud_fallback(config);

    // Local primary budget: explicit override or config (default 300).
    // Cloud fallback budget: explicit override or DEFAULT_CLOUD_FALLBACK (15)
    // so omitted CLI does not hang multi-minute on dead cloud keys (M2 / B3b).
    let local_timeout = timeout_secs_override.unwrap_or(config.timeout_secs);
    let cloud_timeout = timeout_secs_override.unwrap_or(DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS);

    if config.base_url.is_empty() && config.generation_url.is_none() && !cloud_ok {
        if policy.is_forbidden() {
            return Err(cloud_policy_forbidden_error(
                "no local model configured and cloud fallback denied",
            ));
        }
        return Err(
            "Local model server is not configured. Start llama-server, configure Ollama Cloud, OpenRouter, or Gemini fallback."
                .to_string(),
        );
    }

    let local_base_url = config.generation_url.as_deref().unwrap_or(&config.base_url);
    // 0160: retain local failure when cloud cascade runs (multi-cause report).
    let mut local_error: Option<String> = None;

    if !local_base_url.is_empty() {
        // Local TCP precheck: min(30, effective). Delayed-accept cold routers
        // survive; connection refused on loopback stays OS-immediate (B2b/D5).
        let precheck = Duration::from_secs(local_tcp_budget_secs(local_timeout));
        if crate::util::network::is_url_reachable(local_base_url, precheck) {
            let endpoint = CompletionEndpoint {
                label: "Local model server",
                base_url: local_base_url,
                model: &config.generation_model,
                authorization: None,
            };
            match complete_with_endpoint(
                &endpoint,
                local_timeout,
                messages,
                options,
                first_byte_secs,
                true, // is_local — explicit param, never label match (D7/L3)
            ) {
                Ok(response) => return Ok(response),
                Err(error) if cloud_ok => {
                    tracing::debug!("Local completion failed ({error}); trying cloud fallback");
                    local_error = Some(error);
                }
                Err(error) => {
                    if policy.is_forbidden() && has_cloud_fallback_credentials(config) {
                        return Err(cloud_policy_forbidden_error(&format!(
                            "local completion failed ({error}); cloud fallback denied"
                        )));
                    }
                    return Err(error);
                }
            }
        } else if !cloud_ok {
            if policy.is_forbidden() {
                return Err(cloud_policy_forbidden_error(&format!(
                    "local model at {local_base_url} unreachable; cloud fallback denied"
                )));
            }
            return Err(format!(
                "Local model server at {} is unreachable. Start llama-server, OpenRouter, or Gemini.",
                local_base_url
            ));
        } else {
            tracing::debug!(
                "Local model server at {} is unreachable; trying cloud fallback",
                local_base_url
            );
            local_error = Some(format!(
                "Local model server at {local_base_url} is unreachable"
            ));
        }
    }

    // Defense in depth: never enter cloud arms under Forbidden.
    if policy.is_forbidden() {
        return Err(cloud_policy_forbidden_error(
            "cloud completion path blocked",
        ));
    }

    // Single sanitize pass before any cloud network call (RT-A1).
    let sanitized_messages = sanitize_messages_for_egress(messages);

    // 0160: collect each cloud attempt (label, error) for multi-cause exhaust.
    let mut cloud_attempts: Vec<(String, String)> = Vec::new();

    if let Some(endpoint) = ollama_cloud_endpoint(config) {
        match complete_with_endpoint(
            &endpoint,
            cloud_timeout,
            &sanitized_messages,
            options,
            first_byte_secs,
            false, // cloud
        ) {
            Ok(response) => return Ok(response),
            Err(e) => {
                tracing::debug!("Ollama cloud fallback failed: {}", e);
                let is_cq = fallback_error::classify_error(&e)
                    == fallback_error::ErrorClass::ContentQuality;
                cloud_attempts.push((endpoint.label.to_string(), e));
                // B4 soft: short-circuit cascade after content-quality fail
                // (do not charge later providers for the same unusable pattern).
                // Transport failures still cascade.
                if is_cq {
                    return Err(format_full_report(local_error.as_deref(), &cloud_attempts));
                }
            }
        }
    }

    if let Some(api_key) = cloud_fallback_env("OPENROUTER_API_KEY") {
        let model = cloud_fallback_env("OPENROUTER_MODEL")
            .unwrap_or_else(|| "google/gemini-2.5-flash".to_string());
        let openrouter_base = cloud_fallback_env("OPENROUTER_BASE_URL")
            .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
        let endpoint = CompletionEndpoint {
            label: "OpenRouter fallback",
            base_url: &openrouter_base,
            model: &model,
            authorization: Some(format!("Bearer {api_key}")),
        };
        match complete_with_endpoint(
            &endpoint,
            cloud_timeout,
            &sanitized_messages,
            options,
            first_byte_secs,
            false, // cloud
        ) {
            Ok(response) => return Ok(response),
            Err(e) => {
                tracing::debug!("OpenRouter fallback failed: {}", e);
                let is_cq = fallback_error::classify_error(&e)
                    == fallback_error::ErrorClass::ContentQuality;
                cloud_attempts.push((endpoint.label.to_string(), e));
                if is_cq {
                    return Err(format_full_report(local_error.as_deref(), &cloud_attempts));
                }
            }
        }
    }

    if let Some(api_key) = cloud_fallback_env("GEMINI_API_KEY") {
        // Same M2 budget as OR/OC: omit CLI → 15; explicit N → N.
        // Do not use default GeminiConfig (None → 120) or the old .max(300) floor.
        let default_gemini = crate::config::model::GeminiConfig {
            api_key: Some(api_key),
            timeout_secs: Some(cloud_timeout),
            ..Default::default()
        };
        // Messages already sanitized above — pass through once (no double-mangle).
        match gemini_complete_unsanitized(&default_gemini, &sanitized_messages, options) {
            Ok(response) => return Ok(response),
            Err(e) => {
                tracing::debug!("Gemini fallback failed: {}", e);
                cloud_attempts.push(("Gemini fallback".to_string(), e));
                // Last arm — short-circuit not needed; exhaust below.
            }
        }
    }

    if !cloud_attempts.is_empty() {
        // M2: multi-cause report always contains greppable `Cloud fallback exhausted`.
        Err(format_full_report(local_error.as_deref(), &cloud_attempts))
    } else {
        Err(format!(
            "Local model server at {} is unreachable. Start llama-server, configure OpenRouter or Gemini fallback.",
            local_base_url
        ))
    }
}

/// First-byte timeout wrapper for `complete` (Track 0017).
///
/// Bounds only the time to receive response headers (first byte). Once the
/// server begins responding, the normal generous `complete` read timeout
/// covers the rest of the (potentially slow) body generation/parsing. This
/// prevents stalling for the full generation timeout when a server accepts
/// the TCP connection but never sends headers, while still allowing slow-but-
/// healthy completions to finish.
///
/// Defaults to 15 seconds for the first-byte budget.
pub fn complete_with_first_byte_timeout(
    config: &LocalModelConfig,
    messages: &[ChatMessage],
    options: &CompletionOptions,
    timeout_secs_override: Option<u64>,
    first_byte_secs: Option<u64>,
) -> Result<String, String> {
    complete_with_options(
        config,
        messages,
        options,
        timeout_secs_override,
        first_byte_secs.or(Some(15)),
    )
}

/// Returns true if the error string indicates a first-byte timeout.
pub fn is_first_byte_timeout_error(err: &str) -> bool {
    err.to_lowercase().contains("first byte timeout")
}

/// Hard-deadline wrapper for `complete` (Track TA15 / 0160 M4).
///
/// Spawns the HTTP call in a thread and uses `recv_timeout` to enforce a
/// hard deadline that covers the ENTIRE request lifecycle (DNS, connect,
/// TLS handshake, read). The inner ureq timeouts fire first when possible,
/// giving a more specific error.
///
/// **Deadline formula** (see [`hard_deadline_secs`]):
/// - Cloud credentials present:
///   `primary + (configured_cloud_arm_count * cloud_timeout) + HARD_DEADLINE_BUFFER_SECS`
/// - No cloud credentials: `primary + HARD_DEADLINE_BUFFER_SECS`
///
/// Sizing lets a normal local+cloud cascade finish and return a multi-cause
/// report instead of discarding in-flight work as opaque hard-timeout-only.
///
/// `timeout_secs` semantics match [`complete`]: `Some(n)` is an explicit
/// budget for all arms; `None` uses config for Local and
/// [`DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS`] for cloud fallback (M2).
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
    let primary_timeout = timeout_secs.unwrap_or(config.timeout_secs);
    let deadline_secs = hard_deadline_secs(config, timeout_secs);
    let deadline = Duration::from_secs(deadline_secs);
    // Policy-aware: Forbidden + credentials must not claim an unfinished cascade.
    let cloud_possible = has_cloud_fallback(config);

    let (tx, rx) = std::sync::mpsc::channel();
    let config_clone = config.clone();
    let messages_clone: Vec<ChatMessage> = messages.to_vec();
    let options_clone = options.clone();

    // Pass Option through so cloud fallback stays short when CLI omitted (M2).
    std::thread::spawn(move || {
        let result = complete(&config_clone, &messages_clone, &options_clone, timeout_secs);
        let _ = tx.send(result);
    });

    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            if cloud_possible {
                // M4: multi-cause-aware hard-timeout — never sole opaque hard-timeout
                // when a cascade was possible. Word local attempt only when local
                // was configured (M1: cloud-only must not claim a local try).
                let local_configured =
                    !config.base_url.is_empty() || config.generation_url.is_some();
                let cascade_clause = if local_configured {
                    "local attempt + cloud cascade unfinished"
                } else {
                    "cloud cascade unfinished (no local attempt configured)"
                };
                Err(format!(
                    "Hard timeout: request did not complete within {deadline_secs}s \
({cascade_clause}; primary budget {primary_timeout}s). \
Raise `--timeout` / `local_model.timeout_secs`, warm the model, or disable cloud fallback \
(clear ollama_cloud_* / OPENROUTER_API_KEY / GEMINI_API_KEY; or LEDGERFUL_CLOUD_POLICY=forbidden)."
                ))
            } else {
                Err(format!(
                    "Hard timeout: request did not complete within {primary_timeout}s"
                ))
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(format!(
            "Provider thread panicked during request (timeout: {primary_timeout}s)"
        )),
    }
}

/// Sanitize every chat message body for cloud egress (single pass).
fn sanitize_messages_for_egress(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| ChatMessage {
            role: m.role.clone(),
            content: crate::gemini::sanitize::sanitize_for_egress(&m.content).sanitized,
        })
        .collect()
}

fn complete_with_endpoint(
    endpoint: &CompletionEndpoint<'_>,
    timeout_secs: u64,
    messages: &[ChatMessage],
    options: &CompletionOptions,
    first_byte_secs: Option<u64>,
    is_local: bool,
) -> Result<String, String> {
    let target = completion_target(endpoint.base_url);

    // Check for known problematic base URL shapes
    if let Some(warning) = check_base_url_warnings(endpoint.base_url, target.kind) {
        return Err(warning);
    }

    let body = build_endpoint_body(endpoint, messages, options, &target);

    tracing::debug!(
        "Using completion URL: {} (kind={:?})",
        target.url,
        target.kind
    );

    if let Some(fb_secs) = first_byte_secs {
        return complete_endpoint_with_first_byte(
            endpoint,
            &target,
            &body,
            timeout_secs,
            fb_secs,
            is_local,
        );
    }

    let response = send_endpoint_request(endpoint, &target, &body, timeout_secs, is_local)?;
    parse_endpoint_response(response, endpoint, &target)
}

fn build_endpoint_body(
    endpoint: &CompletionEndpoint<'_>,
    messages: &[ChatMessage],
    options: &CompletionOptions,
    target: &EndpointTarget,
) -> serde_json::Value {
    match target.kind {
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
    }
}

fn send_endpoint_request(
    endpoint: &CompletionEndpoint<'_>,
    target: &EndpointTarget,
    body: &serde_json::Value,
    timeout_secs: u64,
    is_local: bool,
) -> Result<ureq::Response, String> {
    // Local: min(30, effective); cloud: 5s. Explicit is_local param — never
    // match on endpoint.label (D7/L3).
    let connect_secs = connect_timeout_secs(is_local, timeout_secs);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(connect_secs))
        .timeout_read(Duration::from_secs(timeout_secs))
        .timeout_write(Duration::from_secs(30))
        .build();

    let mut retry = false;

    loop {
        let mut request = agent
            .post(&target.url)
            .set("Content-Type", "application/json");
        if let Some(value) = &endpoint.authorization {
            request = request.set("Authorization", value);
        }

        match request.send_json(body) {
            Ok(resp) => return Ok(resp),
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
                if transport_is_timeout(&inner)
                    || inner
                        .to_string()
                        .to_lowercase()
                        .contains("first byte timeout")
                {
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
        }
    }
}

fn parse_endpoint_response(
    response: ureq::Response,
    endpoint: &CompletionEndpoint<'_>,
    target: &EndpointTarget,
) -> Result<String, String> {
    match target.kind {
        EndpointKind::OllamaNative => {
            let parsed: ollama::OllamaChatResponse = response
                .into_json()
                .map_err(|e| format!("Failed to parse Ollama native response: {e}"))?;
            apply_completion_text(
                endpoint.label,
                &parsed.message.content,
                parsed.message.thinking.as_deref(),
            )
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
            apply_completion_text(
                endpoint.label,
                &choice.message.content,
                choice.message.reasoning.as_deref(),
            )
        }
    }
}

/// Shared content/reasoning resolution for OpenAI-compatible and Ollama-native arms.
/// Strips think tags, extracts markers from reasoning, never promotes raw CoT (0159).
fn apply_completion_text(
    label: &str,
    content: &str,
    reasoning: Option<&str>,
) -> Result<String, String> {
    let reasoning_chars = reasoning.map(|r| r.chars().count()).unwrap_or(0);
    match completion_text::resolve_completion_text(content, reasoning) {
        Ok(text) => {
            if content.trim().is_empty() && reasoning_chars > 0 {
                tracing::debug!(
                    endpoint = %label,
                    reasoning_chars,
                    answer_chars = text.chars().count(),
                    "extracted product answer from reasoning after empty content"
                );
            }
            Ok(text)
        }
        Err(err) => {
            if let completion_text::CompletionTextError::ReasoningOnly { chars } = &err {
                tracing::warn!(
                    endpoint = %label,
                    reasoning_chars = *chars,
                    "completion returned thinking-only response (no extractable answer)"
                );
            }
            Err(completion_text::format_completion_text_error(label, err))
        }
    }
}

/// First-byte timeout wrapper for a single endpoint call.
///
/// The full HTTP call (headers + body read) runs in a dedicated worker thread
/// so that all ureq/agent state stays in one thread and the response stream is
/// never moved across thread boundaries. A short `recv_timeout` guards the
/// header phase; once headers arrive the caller waits for the body parse with
/// the normal read timeout. This keeps the first-byte deadline scoped to the
/// time until the server begins the HTTP response, while still allowing slow
/// body generation to finish under the generous read timeout.
fn complete_endpoint_with_first_byte(
    endpoint: &CompletionEndpoint<'_>,
    target: &EndpointTarget,
    body: &serde_json::Value,
    timeout_secs: u64,
    first_byte_secs: u64,
    is_local: bool,
) -> Result<String, String> {
    let (headers_tx, headers_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<String, String>>();

    // Capture owned data for the worker thread.
    let endpoint_owned = CompletionEndpointOwned {
        label: endpoint.label.to_string(),
        base_url: endpoint.base_url.to_string(),
        model: endpoint.model.to_string(),
        authorization: endpoint.authorization.clone(),
    };
    let target = target.clone();
    let body = body.clone();

    std::thread::spawn(move || {
        match send_endpoint_request_owned(&endpoint_owned, &target, &body, timeout_secs, is_local) {
            Ok(response) => {
                let headers_ok = headers_tx.send(Ok(())).is_ok();
                if headers_ok {
                    let parse_result =
                        parse_endpoint_response_owned(response, &endpoint_owned, &target);
                    let _ = result_tx.send(parse_result);
                }
            }
            Err(err) => {
                let _ = headers_tx.send(Err(err.clone()));
                let _ = result_tx.send(Err(err));
            }
        }
    });

    match headers_rx.recv_timeout(Duration::from_secs(first_byte_secs)) {
        Ok(Ok(())) => match result_rx.recv_timeout(Duration::from_secs(timeout_secs)) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "{} timed out after {}s",
                endpoint.label, timeout_secs
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(format!(
                "{} provider thread panicked during response parsing",
                endpoint.label
            )),
        },
        Ok(Err(err)) => Err(err),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "First byte timeout: model did not begin responding within {}s",
            first_byte_secs
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(format!(
            "{} provider thread panicked during request",
            endpoint.label
        )),
    }
}

#[derive(Clone)]
struct CompletionEndpointOwned {
    label: String,
    base_url: String,
    #[allow(dead_code)]
    model: String,
    authorization: Option<String>,
}

fn send_endpoint_request_owned(
    endpoint: &CompletionEndpointOwned,
    target: &EndpointTarget,
    body: &serde_json::Value,
    timeout_secs: u64,
    is_local: bool,
) -> Result<ureq::Response, String> {
    // Local: min(30, effective); cloud: 5s. Explicit is_local param (D7/L3).
    let connect_secs = connect_timeout_secs(is_local, timeout_secs);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(connect_secs))
        .timeout_read(Duration::from_secs(timeout_secs))
        .timeout_write(Duration::from_secs(30))
        .build();

    let mut retry = false;

    loop {
        let mut request = agent
            .post(&target.url)
            .set("Content-Type", "application/json");
        if let Some(value) = &endpoint.authorization {
            request = request.set("Authorization", value);
        }

        match request.send_json(body) {
            Ok(resp) => return Ok(resp),
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
                if transport_is_timeout(&inner)
                    || inner
                        .to_string()
                        .to_lowercase()
                        .contains("first byte timeout")
                {
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
        }
    }
}

fn parse_endpoint_response_owned(
    response: ureq::Response,
    endpoint: &CompletionEndpointOwned,
    target: &EndpointTarget,
) -> Result<String, String> {
    match target.kind {
        EndpointKind::OllamaNative => {
            let parsed: ollama::OllamaChatResponse = response
                .into_json()
                .map_err(|e| format!("Failed to parse Ollama native response: {e}"))?;
            apply_completion_text(
                &endpoint.label,
                &parsed.message.content,
                parsed.message.thinking.as_deref(),
            )
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
            apply_completion_text(
                &endpoint.label,
                &choice.message.content,
                choice.message.reasoning.as_deref(),
            )
        }
    }
}
