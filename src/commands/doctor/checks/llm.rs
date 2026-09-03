use crate::commands::doctor::finding::{DoctorCategory, DoctorFinding};
use miette::Result;

/// Map embedding backend availability to an optional finding (0109).
pub(crate) fn embedding_finding(
    config: &crate::config::model::LocalModelConfig,
    avail: &BackendAvailabilityReport,
) -> Option<DoctorFinding> {
    use crate::embed::client::is_embedding_backend_configured;
    use crate::semantic::BackendStatus;

    match avail.status {
        BackendStatus::Ready => None,
        BackendStatus::NotConfigured => {
            // Partial: model name set without URL → warn; fully empty → info.
            let partial = !config.embedding_model.trim().is_empty()
                && !is_embedding_backend_configured(config);
            if partial {
                Some(DoctorFinding::warn(
                    "embed-partial-config",
                    DoctorCategory::Optional,
                    "Embedding model partially configured (model name set without URL) — not healthy Ready",
                ))
            } else {
                Some(DoctorFinding::info(
                    "embed-not-configured",
                    DoctorCategory::Optional,
                    "Embedding model not configured",
                ))
            }
        }
        BackendStatus::Unreachable => Some(DoctorFinding::warn(
            "embed-unreachable",
            DoctorCategory::Optional,
            "Embedding model unreachable",
        )),
    }
}

/// Result of a doctor availability probe for an optional/advertised backend.
///
/// **DoD-11 seam for 0095/0109:** SCIP and other optional toolchains reuse this
/// shape. `is_failure` means the backend is **not Ready** for display honesty
/// (0096 partial-config); severity lives on structured [`DoctorFinding`]s
/// (optional category — never blocks publish, never dashboard failures alone).
#[derive(Debug, Clone)]
pub struct BackendAvailabilityReport {
    /// Colored/human display string for the doctor line.
    pub display: String,
    /// Whether the backend is not Ready (display honesty; not soft-fail count).
    pub is_failure: bool,
    /// Orthogonal backend axis (mirrors semantic readiness).
    pub status: crate::semantic::BackendStatus,
    /// Full error detail for debug logging (not shown to user).
    pub debug_detail: Option<String>,
}

/// Format embedding-backend availability for doctor (DoD-6, DoD-11).
///
/// Gates on URL emptiness via `is_embedding_backend_configured` — **not**
/// on `embedding_model.is_empty()` alone — so partial config (model name
/// set, no URL) reports "Not configured" rather than a healthy
/// `(0 dims) @ `.
pub(crate) fn format_embedding_backend_availability(
    display_config: &crate::config::model::LocalModelConfig,
    probe_config: &crate::config::model::LocalModelConfig,
) -> BackendAvailabilityReport {
    use crate::embed::client::is_embedding_backend_configured;
    use crate::semantic::BackendStatus;
    use owo_colors::{OwoColorize, Stream};

    if !is_embedding_backend_configured(display_config) {
        return BackendAvailabilityReport {
            display: "Not configured"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
            is_failure: true,
            status: BackendStatus::NotConfigured,
            debug_detail: None,
        };
    }

    let endpoint = display_config
        .embedding_url
        .as_deref()
        .unwrap_or(&display_config.base_url);

    // Clone into 'static probe closure (abandoned-thread hard deadline, 0143).
    let probe_config = probe_config.clone();
    match probe_with_retry(move || crate::embed::client::check_local_model(&probe_config)) {
        ProbeResult::Healthy(dims) if dims.active => BackendAvailabilityReport {
            display: format!(
                "{} ({} dims) @ {}",
                if display_config.embedding_model.is_empty() {
                    dims.model_name.as_str()
                } else {
                    display_config.embedding_model.as_str()
                },
                dims.dimensions,
                endpoint
            ),
            is_failure: false,
            status: BackendStatus::Ready,
            debug_detail: None,
        },
        ProbeResult::Healthy(_dims) => {
            // URL set but probe returned inactive (0 dims) — treat as not ready.
            BackendAvailabilityReport {
                display: "Not configured"
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                is_failure: true,
                status: BackendStatus::NotConfigured,
                debug_detail: Some(
                    "Probe returned inactive dimensions despite URL being set".to_string(),
                ),
            }
        }
        ProbeResult::ReachableAfterRetry { val: dims, retries } if dims.active => {
            BackendAvailabilityReport {
                display: format!(
                    "{} ({} dims) @ {} (reachable after retry: flaky/transient - {})",
                    if display_config.embedding_model.is_empty() {
                        dims.model_name.as_str()
                    } else {
                        display_config.embedding_model.as_str()
                    },
                    dims.dimensions,
                    endpoint,
                    format!(
                        "{} {}",
                        retries,
                        if retries == 1 { "retry" } else { "retries" }
                    )
                    .if_supports_color(Stream::Stdout, |s| s.green())
                ),
                is_failure: false,
                status: BackendStatus::Ready,
                debug_detail: None,
            }
        }
        ProbeResult::ReachableAfterRetry { .. } => BackendAvailabilityReport {
            display: "Not configured"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
            is_failure: true,
            status: BackendStatus::NotConfigured,
            debug_detail: None,
        },
        ProbeResult::Unreachable { err, retries } => {
            let retry_suffix = if retries > 0 {
                format!(" after {} retries", retries)
            } else {
                String::new()
            };
            let truncated: String = err.chars().take(80).collect();
            let detail_hint = if err.chars().count() > 80 {
                " [set RUST_LOG=debug for details]"
            } else {
                ""
            };
            BackendAvailabilityReport {
                display: format!(
                    "unreachable ({}{}){}",
                    truncated.if_supports_color(Stream::Stdout, |s| s.yellow()),
                    retry_suffix,
                    detail_hint
                ),
                is_failure: true,
                status: BackendStatus::Unreachable,
                debug_detail: Some(err),
            }
        }
    }
}
#[derive(Debug)]
pub(crate) enum ProbeResult<T> {
    Healthy(T),
    ReachableAfterRetry { val: T, retries: u32 },
    Unreachable { err: String, retries: u32 },
}

pub(crate) fn is_transient_error(err: &str) -> bool {
    let err_lower = err.to_lowercase();
    if err_lower.contains("unreachable")
        || err_lower.contains("timed out")
        || err_lower.contains("timeout")
    {
        return true;
    }
    if err_lower.contains("502") || err_lower.contains("503") || err_lower.contains("504") {
        return true;
    }
    false
}

/// Doctor honesty: a listening local router (TCP open) is not "unreachable"
/// when the 2s ping budget expires (cold load). `ask` uses the full timeout.
pub(crate) fn completion_probe_failure_kind(
    tcp_ok: bool,
) -> (&'static str, &'static str, &'static str) {
    if tcp_ok {
        (
            "completion-not-ready",
            "listening; ping failed",
            "Completion model listening but ping failed in doctor budget",
        )
    } else {
        (
            "completion-unreachable",
            "unreachable",
            "Completion model unreachable",
        )
    }
}

/// Classification of a failed doctor completion ping. Pure: no network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionPingClass {
    /// Connect succeeded or HTTP answered; ping missed the doctor budget.
    Timeout,
    /// Transport-level connect failure (refused, DNS, reset).
    ConnectionFailed,
    /// Generation URL missing/empty.
    EmptyUrl,
}

impl CompletionPingClass {
    pub(crate) fn tcp_ok(self) -> bool {
        matches!(self, Self::Timeout)
    }
}

/// Classify a ping failure from ureq / `io::ErrorKind` — not OS English strings.
///
/// Unit tests must not open TCP or call `is_url_reachable`.
pub(crate) fn classify_completion_ping_error(
    url_empty: bool,
    ureq_kind: Option<ureq::ErrorKind>,
    io_kind: Option<std::io::ErrorKind>,
) -> CompletionPingClass {
    if url_empty {
        return CompletionPingClass::EmptyUrl;
    }
    if io_kind == Some(std::io::ErrorKind::TimedOut)
        || io_kind == Some(std::io::ErrorKind::WouldBlock)
    {
        return CompletionPingClass::Timeout;
    }
    match ureq_kind {
        Some(ureq::ErrorKind::ConnectionFailed) | Some(ureq::ErrorKind::Dns) => {
            CompletionPingClass::ConnectionFailed
        }
        Some(ureq::ErrorKind::HTTP) => CompletionPingClass::Timeout,
        Some(ureq::ErrorKind::Io) => CompletionPingClass::ConnectionFailed,
        None => CompletionPingClass::Timeout,
        _ => CompletionPingClass::ConnectionFailed,
    }
}

/// Human/JSON completion finding text: `sanitize_cause` then truncate.
pub(crate) fn completion_finding_message(finding_lead: &str, err: &str, retries: u32) -> String {
    let sanitized = crate::local_model::client::sanitize_cause(err);
    let truncated: String = sanitized.chars().take(80).collect();
    let retry_suffix = if retries > 0 {
        format!(" after {retries} retries")
    } else {
        String::new()
    };
    let detail_hint = if sanitized.chars().count() > 80 {
        " [set RUST_LOG=debug for details]"
    } else {
        ""
    };
    format!("{finding_lead} ({truncated}{retry_suffix}){detail_hint}")
}

/// Status-line twin of [`completion_finding_message`] (same sanitized truncate).
pub(crate) fn completion_status_detail(err: &str, retries: u32) -> (String, String, &'static str) {
    let sanitized = crate::local_model::client::sanitize_cause(err);
    let truncated: String = sanitized.chars().take(80).collect();
    let retry_suffix = if retries > 0 {
        format!(" after {retries} retries")
    } else {
        String::new()
    };
    let detail_hint = if sanitized.chars().count() > 80 {
        " [set RUST_LOG=debug for details]"
    } else {
        ""
    };
    (truncated, retry_suffix, detail_hint)
}

/// Doctor completion ping with retry; last failure classified without a second TCP.
pub(crate) fn probe_completion_classified(
    config: crate::config::model::LocalModelConfig,
) -> (ProbeResult<String>, CompletionPingClass) {
    let last_class = std::sync::Arc::new(std::sync::Mutex::new(CompletionPingClass::Timeout));
    let slot = std::sync::Arc::clone(&last_class);
    let result = probe_with_retry(move || {
        crate::local_model::client::ping_completions_detailed(&config).map_err(|failure| {
            let class = classify_completion_ping_error(
                failure.url_empty,
                failure.ureq_kind,
                failure.io_kind,
            );
            if let Ok(mut guard) = slot.lock() {
                *guard = class;
            }
            failure.message
        })
    });
    let class = match last_class.lock() {
        Ok(guard) => *guard,
        Err(_) => CompletionPingClass::Timeout,
    };
    (result, class)
}

/// Wall-clock cap on sleep time between retries (secondary bound).
/// Primary session-start bound for 0143 is [`PROBE_MAX_RETRIES`]: production
/// allows at most one retry (two attempts). `RETRY_BUDGET` still caps total
/// sleep so a long hang path cannot keep retrying forever if max_retries is
/// raised in tests.
///
/// This budget bounds only the *sleep* time between retries, not the
/// per-attempt network timeout (`model_config.timeout_secs`).
const RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);

/// Production max retries after the first attempt (0143 Fix A).
/// `1` → at most two attempts total: one try + one flap recovery retry.
/// Kills multi-second retry tax on fast-fail unreachable while still
/// recovering a single transient blip.
const PROBE_MAX_RETRIES: u32 = 1;

/// Delay between retry attempts. Kept short relative to `RETRY_BUDGET` so
/// a single production retry still fits inside the wall budget.
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// Per-attempt hard deadline for production doctor probes (0143).
///
/// Formula: `timeout_secs * 1000 + 250` ms with doctor `timeout_secs = 2`
/// → **2250 ms**. Covers the full request lifecycle including DNS; the
/// inner ureq connect/read timeouts fire first when possible.
const PROBE_PER_ATTEMPT_DEADLINE: std::time::Duration =
    std::time::Duration::from_millis(2 * 1000 + 250);

pub(crate) fn probe_with_retry<T, F>(probe_fn: F) -> ProbeResult<T>
where
    T: Send + 'static,
    F: Fn() -> Result<T, String> + Send + Sync + 'static,
{
    probe_with_retry_budgeted(
        probe_fn,
        RETRY_BUDGET,
        RETRY_DELAY,
        PROBE_PER_ATTEMPT_DEADLINE,
        PROBE_MAX_RETRIES,
    )
}

/// Core retry loop, parameterized by retry budget, inter-retry delay,
/// per-attempt hard deadline, and max retries so tests can exercise the
/// deadline / multi-retry logic with tiny durations instead of waiting
/// through the real production budget.
///
/// Retries on transient errors (per `is_transient_error`) continue only while
/// `retries < max_retries` **and** the elapsed wall-clock time spent in this
/// call is still under `budget`; once either bound is hit, the probe returns
/// `Unreachable` immediately with however many retries were actually
/// attempted. Non-transient ("semantic") errors always fail immediately with
/// zero retries.
///
/// Each attempt is spawned on a detached worker thread and awaited via
/// `recv_timeout` — **not** `thread::scope` join-first (0143 B1). On
/// timeout the worker is abandoned (CLI exits soon; private doctor helper).
pub(crate) fn probe_with_retry_budgeted<T, F>(
    probe_fn: F,
    budget: std::time::Duration,
    delay: std::time::Duration,
    per_attempt_deadline: std::time::Duration,
    max_retries: u32,
) -> ProbeResult<T>
where
    T: Send + 'static,
    F: Fn() -> Result<T, String> + Send + Sync + 'static,
{
    let probe_fn = std::sync::Arc::new(probe_fn);
    let start = std::time::Instant::now();
    let mut retries = 0;

    loop {
        // Spawn then recv_timeout — do NOT join before the deadline check.
        // Abandon the thread on timeout so DNS/TCP hangs cannot stall doctor.
        let (tx, rx) = std::sync::mpsc::channel::<Result<T, String>>();
        let probe = std::sync::Arc::clone(&probe_fn);
        let _handle = std::thread::spawn(move || {
            let _ = tx.send(probe());
        });

        let probe_result = match rx.recv_timeout(per_attempt_deadline) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let secs = per_attempt_deadline.as_secs_f64();
                // Prefer whole seconds when exact; otherwise show millis for test deadlines.
                let msg = if per_attempt_deadline.subsec_millis() == 0
                    && per_attempt_deadline.as_secs() > 0
                {
                    format!("probe timed out after {}s", per_attempt_deadline.as_secs())
                } else if secs >= 1.0 {
                    format!("probe timed out after {secs:.2}s")
                } else {
                    format!(
                        "probe timed out after {}ms",
                        per_attempt_deadline.as_millis()
                    )
                };
                Err(msg)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("probe thread panicked".to_string())
            }
        };

        match probe_result {
            Ok(val) => {
                if retries > 0 {
                    return ProbeResult::ReachableAfterRetry { val, retries };
                } else {
                    return ProbeResult::Healthy(val);
                }
            }
            Err(err) => {
                let elapsed = start.elapsed();
                // Cap sleep to remaining budget so a 503 retry cannot add a
                // second 2s wait on top of the doctor ping window. Still retry
                // while `elapsed < budget` (do not drop the retry entirely).
                if is_transient_error(&err) && retries < max_retries && elapsed < budget {
                    retries += 1;
                    let remaining = budget.saturating_sub(elapsed);
                    let sleep_for = delay.min(remaining);
                    if !sleep_for.is_zero() {
                        std::thread::sleep(sleep_for);
                    }
                    continue;
                }
                return ProbeResult::Unreachable { err, retries };
            }
        }
    }
}

pub(crate) fn format_active_ask_backend(config: &crate::config::model::Config) -> String {
    format_active_ask_backend_with(config, &|name| std::env::var(name).ok(), &|name| {
        crate::config::model::read_env_key(name)
    })
}

pub(crate) fn format_active_ask_backend_with(
    config: &crate::config::model::Config,
    env_reader: &dyn Fn(&str) -> Option<String>,
    dotenv_reader: &dyn Fn(&str) -> Option<String>,
) -> String {
    // If user configured a provider priority list, show the full chain
    // with model names (TA14 R6). Uses resolve_provider_entries so env var
    // overrides (LEDGERFUL_ASK_MODEL_N) are reflected in the display.
    if !config.ask.providers.priority.is_empty()
        && let Ok(entries) = crate::commands::ask::resolve_provider_entries(config, None)
        && !entries.is_empty()
    {
        let names: Vec<String> = entries
            .iter()
            .map(|e| {
                let model = e.model.as_deref().unwrap_or("");
                if model.is_empty() {
                    e.backend.display_name().to_string()
                } else {
                    format!("{} ({})", e.backend.display_name(), model)
                }
            })
            .collect();
        return names.join(" → ");
    }

    // Legacy display when no provider priority list is configured.
    use crate::commands::ask::{Backend, resolve_backend_with};
    let resolved = resolve_backend_with(config, None, env_reader, dotenv_reader);
    match resolved {
        Backend::Gemini => "Gemini (Cloud)".to_string(),
        Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
            let base_url = config
                .local_model
                .generation_url
                .as_deref()
                .unwrap_or(&config.local_model.base_url);
            if base_url.is_empty() {
                "Local (127.0.0.1)".to_string()
            } else {
                let host = parse_url_host(base_url).unwrap_or_else(|| "127.0.0.1".to_string());
                format!("Local ({})", host)
            }
        }
    }
}
pub(crate) fn parse_url_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))?;
    let authority = without_scheme.split('/').next()?;
    let host = authority.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompletionPingClass, classify_completion_ping_error, completion_finding_message,
        completion_probe_failure_kind,
    };

    #[test]
    fn listening_local_router_is_not_unreachable() {
        let (code, status, lead) = completion_probe_failure_kind(true);
        assert_eq!(code, "completion-not-ready");
        assert!(status.contains("listening"));
        assert!(lead.contains("listening"));
        assert!(!lead.to_lowercase().contains("unreachable"));
    }

    #[test]
    fn tcp_down_stays_unreachable() {
        let (code, status, lead) = completion_probe_failure_kind(false);
        assert_eq!(code, "completion-unreachable");
        assert_eq!(status, "unreachable");
        assert!(lead.contains("unreachable"));
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    fn classify_completion_ping_error__timeout_kind__not_unreachable() {
        let class = classify_completion_ping_error(
            false,
            Some(ureq::ErrorKind::Io),
            Some(std::io::ErrorKind::TimedOut),
        );
        assert_eq!(class, CompletionPingClass::Timeout);
        assert!(class.tcp_ok());
        let (code, _, lead) = completion_probe_failure_kind(class.tcp_ok());
        assert_eq!(code, "completion-not-ready");
        assert!(!lead.to_lowercase().contains("unreachable"));
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    fn classify_completion_ping_error__connection_failed_kind__transport() {
        let class =
            classify_completion_ping_error(false, Some(ureq::ErrorKind::ConnectionFailed), None);
        assert_eq!(class, CompletionPingClass::ConnectionFailed);
        assert!(!class.tcp_ok());
        let (code, status, lead) = completion_probe_failure_kind(class.tcp_ok());
        assert_eq!(code, "completion-unreachable");
        assert_eq!(status, "unreachable");
        assert!(lead.contains("unreachable"));
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    fn classify_completion_ping_error__empty_url__unreachable() {
        let class = classify_completion_ping_error(true, None, None);
        assert_eq!(class, CompletionPingClass::EmptyUrl);
        assert!(!class.tcp_ok());
        let (code, _, _) = completion_probe_failure_kind(class.tcp_ok());
        assert_eq!(code, "completion-unreachable");
    }

    #[test]
    #[allow(non_snake_case)] // project test naming: feature__condition__expected
    fn completion_finding_message__url_shaped_cause__sanitize_cause_strips() {
        let raw = "POST http://127.0.0.1:11434/v1/chat/completions \
Authorization Bearer sk-secret-token-xyz api_key=supersecret failed";
        let msg = completion_finding_message("Completion model unreachable", raw, 0);
        assert!(
            !msg.contains("sk-secret-token-xyz"),
            "bearer token must not leak: {msg}"
        );
        assert!(
            !msg.contains("supersecret"),
            "api_key value must not leak: {msg}"
        );
        assert!(msg.contains("[REDACTED]"), "redaction marker: {msg}");
        assert!(
            msg.contains("http://127.0.0.1:11434"),
            "url-shaped cause still present after sanitize: {msg}"
        );
    }
}
