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
                if is_transient_error(&err) && retries < max_retries && elapsed + delay <= budget {
                    retries += 1;
                    std::thread::sleep(delay);
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
