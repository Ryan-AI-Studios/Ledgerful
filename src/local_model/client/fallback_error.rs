//! Multi-cause cloud-fallback error assembly (track 0160).
//!
//! When local completion fails and cloud arms exhaust (or cloud-only fails),
//! operators get a primary class, retained local cause, greppable 0159 tokens,
//! literal `Cloud fallback exhausted`, sanitized causes, and actionable Next steps
//! — not a last-error-only string that erases the local trigger.

/// Error class for primary selection (M5 / M8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    ContentQuality,
    Auth,
    RateLimit,
    Transport,
    Other,
}

impl ErrorClass {
    /// Stable token used in Primary / compact lines.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::ContentQuality => "content-quality",
            ErrorClass::Auth => "auth",
            ErrorClass::RateLimit => "rate-limit",
            ErrorClass::Transport => "transport",
            ErrorClass::Other => "other",
        }
    }
}

/// Classify a single attempt error string (local or cloud).
pub fn classify_error(err: &str) -> ErrorClass {
    let lower = err.to_lowercase();

    // Content quality (0159 greppable tokens) — check before other classes.
    if lower.contains("reasoning only")
        || lower.contains("empty content")
        || lower.contains("empty message content")
    {
        return ErrorClass::ContentQuality;
    }

    // Auth: HTTP 401/403 (token match; do not use bare "forbidden" — policy code).
    if lower.contains("401") || lower.contains("403") {
        return ErrorClass::Auth;
    }

    // Rate limit (M8).
    if lower.contains("rate limited") || lower.contains("429") {
        return ErrorClass::RateLimit;
    }

    // Transport / timeout / unreachable.
    if lower.contains("timed out")
        || lower.contains("hard timeout")
        || lower.contains("first byte timeout")
        || lower.contains("unreachable")
        || lower.contains("connection refused")
        || lower.contains("not reachable")
        || (lower.contains("timeout") && !lower.contains("gateway timeout"))
        || lower.contains("os error")
    {
        return ErrorClass::Transport;
    }

    ErrorClass::Other
}

/// Sanitize a single cause for embed (M3): strip bearer tokens and `api_key=` values.
/// Behavior mirrors `commands/ask/backend.rs::sanitize_error_for_logging` (duplicated
/// to keep `local_model` free of ask-layer imports).
pub fn sanitize_cause(err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    let mut sanitized = err.to_string();

    if let Some(idx) = lower.find("bearer ") {
        let start = idx;
        let rest = &sanitized[start + 7..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ']')
            .unwrap_or(rest.len());
        sanitized = format!(
            "{}bearer [REDACTED]{}",
            &sanitized[..start],
            &sanitized[start + 7 + end..]
        );
    }

    let lower2 = sanitized.to_ascii_lowercase();
    if let Some(idx) = lower2.find("api_key=") {
        let start = idx;
        let rest = &sanitized[start + 8..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ']' || c == '&')
            .unwrap_or(rest.len());
        sanitized = format!(
            "{}api_key=[REDACTED]{}",
            &sanitized[..start],
            &sanitized[start + 8 + end..]
        );
    }

    sanitized
}

/// True when `err` looks like a 0160 multi-cause fallback report.
pub fn is_multi_cause_fallback_error(err: &str) -> bool {
    err.contains("Cloud fallback exhausted")
}

/// Collapse a multi-cause full report to a single line for degrade / next-provider /
/// miette paths (M6 / M7). Non-multi-cause strings pass through (newlines collapsed
/// only when present so single-line templates stay safe).
pub fn compact_completion_error(err: &str) -> String {
    if is_multi_cause_fallback_error(err) {
        if !err.contains('\n') {
            return err.to_string();
        }
        // Re-derive compact from section headers when we have a full report.
        let mut primary_detail: Option<String> = None;
        let mut primary_class: Option<String> = None;
        let mut local: Option<String> = None;
        let mut clouds: Vec<String> = Vec::new();
        for line in err.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("Primary:") {
                let rest = rest.trim();
                // "content-quality (detail)" or free-form
                if let Some((cls, detail)) = rest.split_once(" — ") {
                    primary_class = Some(cls.trim().to_string());
                    primary_detail = Some(detail.trim().to_string());
                } else if let Some((cls, detail)) = rest.split_once(" (") {
                    primary_class = Some(cls.trim().to_string());
                    let d = detail.trim_end_matches(')').trim();
                    if !d.is_empty() {
                        primary_detail = Some(d.to_string());
                    }
                } else {
                    primary_detail = Some(rest.to_string());
                }
            } else if let Some(rest) = t.strip_prefix("Local:") {
                local = Some(rest.trim().to_string());
            } else if let Some(rest) = t.strip_prefix("Cloud:") {
                clouds.push(rest.trim().to_string());
            }
        }
        let class = primary_class.unwrap_or_else(|| "other".to_string());
        let detail = primary_detail.unwrap_or_else(|| "unknown".to_string());
        return format_compact_line(local.as_deref(), &clouds, &class, &detail);
    }

    if err.contains('\n') {
        err.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" | ")
    } else {
        err.to_string()
    }
}

/// Build the full multi-line multi-cause report (terminal path).
///
/// - **M1:** when `local_error` is `None`, omit "after local attempt" / `Local:`.
/// - **M2:** always contains literal `Cloud fallback exhausted`.
/// - **M3:** each cause sanitized before embed.
/// - **M5/M8:** primary class over local + all cloud attempts.
pub fn format_full_report(
    local_error: Option<&str>,
    cloud_attempts: &[(impl AsRef<str>, impl AsRef<str>)],
) -> String {
    let local_sanitized = local_error.map(sanitize_cause);
    let clouds: Vec<(String, String)> = cloud_attempts
        .iter()
        .map(|(label, err)| (label.as_ref().to_string(), sanitize_cause(err.as_ref())))
        .collect();

    let (primary_class, primary_detail) = select_primary(local_sanitized.as_deref(), &clouds);

    let header = if local_sanitized.is_some() {
        "Cloud fallback exhausted after local attempt."
    } else {
        "Cloud fallback exhausted."
    };

    let mut out = String::new();
    out.push_str(header);
    out.push('\n');
    out.push('\n');
    out.push_str(&format!(
        "Primary: {} — {}\n",
        primary_class.as_str(),
        primary_detail
    ));

    if let Some(ref local) = local_sanitized {
        out.push_str(&format!("Local: {local}\n"));
    }

    if clouds.is_empty() {
        out.push_str("Cloud: (no cloud attempts recorded)\n");
    } else {
        for (label, err) in &clouds {
            // Prefer labeled cloud line; error often already includes label.
            if err.starts_with(label.as_str()) {
                out.push_str(&format!("Cloud: {err}\n"));
            } else {
                out.push_str(&format!("Cloud: {label}: {err}\n"));
            }
        }
    }

    out.push('\n');
    out.push_str("Next:\n");
    for step in remediation_lines(local_sanitized.as_deref(), &clouds, primary_class) {
        out.push_str(&format!("- {step}\n"));
    }

    out
}

/// Compact single-line form (M6): no embedded newlines.
pub fn format_compact_report(
    local_error: Option<&str>,
    cloud_attempts: &[(impl AsRef<str>, impl AsRef<str>)],
) -> String {
    let local_sanitized = local_error.map(sanitize_cause);
    let clouds: Vec<(String, String)> = cloud_attempts
        .iter()
        .map(|(label, err)| (label.as_ref().to_string(), sanitize_cause(err.as_ref())))
        .collect();

    let (primary_class, primary_detail) = select_primary(local_sanitized.as_deref(), &clouds);

    let cloud_summaries: Vec<String> = clouds
        .iter()
        .map(|(label, err)| {
            if err.starts_with(label.as_str()) {
                err.clone()
            } else {
                format!("{label}: {err}")
            }
        })
        .collect();

    format_compact_line(
        local_sanitized.as_deref(),
        &cloud_summaries,
        primary_class.as_str(),
        &primary_detail,
    )
}

fn format_compact_line(
    local: Option<&str>,
    clouds: &[String],
    primary_class: &str,
    primary_detail: &str,
) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "Cloud fallback exhausted: primary {primary_class} ({primary_detail})"
    ));
    if let Some(l) = local {
        parts.push(format!("local: {l}"));
    }
    if clouds.is_empty() {
        parts.push("cloud: (none)".to_string());
    } else {
        parts.push(format!("cloud: {}", clouds.join(" | ")));
    }
    let line = parts.join("; ");
    // Defensive: strip any accidental newlines from nested causes.
    line.replace(['\n', '\r'], " ")
}

/// Primary class selection (M5 order over local + cloud):
/// 1. Content quality if ANY (prefer **last** CQ detail)
/// 2. Auth if any 401/403
/// 3. RateLimit if any 429 / rate limited (M8)
/// 4. Transport (local or cloud)
/// 5. Else last cloud error; if no cloud, local-only detail
fn select_primary(local: Option<&str>, clouds: &[(String, String)]) -> (ErrorClass, String) {
    let mut ordered: Vec<(ErrorClass, String)> = Vec::new();
    if let Some(l) = local {
        ordered.push((classify_error(l), l.to_string()));
    }
    for (_label, err) in clouds {
        ordered.push((classify_error(err), err.clone()));
    }

    if let Some((_, detail)) = ordered
        .iter()
        .rfind(|(c, _)| *c == ErrorClass::ContentQuality)
    {
        return (ErrorClass::ContentQuality, detail.clone());
    }
    if let Some((_, detail)) = ordered.iter().find(|(c, _)| *c == ErrorClass::Auth) {
        return (ErrorClass::Auth, detail.clone());
    }
    if let Some((_, detail)) = ordered.iter().find(|(c, _)| *c == ErrorClass::RateLimit) {
        return (ErrorClass::RateLimit, detail.clone());
    }
    if let Some((_, detail)) = ordered.iter().find(|(c, _)| *c == ErrorClass::Transport) {
        return (ErrorClass::Transport, detail.clone());
    }

    // Else last cloud; if no cloud, local.
    if let Some((_label, err)) = clouds.last() {
        return (classify_error(err), err.clone());
    }
    if let Some(l) = local {
        return (classify_error(l), l.to_string());
    }
    (ErrorClass::Other, "unknown failure".to_string())
}

/// Context-sensitive remediation lines for the Next: block (B3).
fn remediation_lines(
    local: Option<&str>,
    clouds: &[(String, String)],
    primary: ErrorClass,
) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();

    let local_class = local.map(classify_error);
    let any_cq = primary == ErrorClass::ContentQuality
        || local_class == Some(ErrorClass::ContentQuality)
        || clouds
            .iter()
            .any(|(_, e)| classify_error(e) == ErrorClass::ContentQuality);
    let any_rate = primary == ErrorClass::RateLimit
        || clouds
            .iter()
            .any(|(_, e)| classify_error(e) == ErrorClass::RateLimit)
        || local_class == Some(ErrorClass::RateLimit);

    if let Some(l) = local {
        let lc = classify_error(l);
        let lower = l.to_lowercase();
        if lc == ErrorClass::Transport {
            if lower.contains("timed out")
                || lower.contains("hard timeout")
                || lower.contains("first byte timeout")
                || (lower.contains("timeout") && !lower.contains("gateway timeout"))
            {
                steps.push(
                    "Retry/warm local: raise `--timeout` or `local_model.timeout_secs`; warm/preload the model; retry"
                        .to_string(),
                );
            } else {
                steps.push(
                    "Start the local router / check `local_model.base_url` (or generation_url); ensure the server accepts completions"
                        .to_string(),
                );
            }
        }
    }

    if any_cq {
        steps.push(
            "Model returned no product answer (empty / reasoning-only). Try a different model or backend; do not treat this as a local transport failure only"
                .to_string(),
        );
    }

    if any_rate {
        steps.push("Rate limited (429): wait a moment or check your quota/credits".to_string());
    }

    // Always offer cloud disable + alternate when cloud was in play.
    if !clouds.is_empty() || local.is_some() {
        steps.push(
            "Disable cloud fallback: clear `local_model.ollama_cloud_url` / `ollama_cloud_api_key` / `ollama_cloud_model` and/or unset `OPENROUTER_API_KEY` / `GEMINI_API_KEY`; or set `LEDGERFUL_CLOUD_POLICY=forbidden`"
                .to_string(),
        );
        steps.push("Try alternate: `ledgerful ask --backend gemini \"…\"`".to_string());
        steps.push(
            "Offline: structural locate / CLI metadata short-circuits need no LLM".to_string(),
        );
    }

    // Deduplicate while preserving order (deterministic).
    let mut seen = std::collections::BTreeSet::new();
    steps.retain(|s| seen.insert(s.clone()));
    steps
}

/// True when the multi-cause report's **local** cause is a timeout (for execute hints).
pub fn local_cause_is_timeout(err: &str) -> bool {
    for line in err.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Local:") {
            return classify_error(rest) == ErrorClass::Transport && {
                let lower = rest.to_lowercase();
                lower.contains("timed out")
                    || lower.contains("hard timeout")
                    || lower.contains("first byte timeout")
                    || (lower.contains("timeout") && !lower.contains("gateway timeout"))
            };
        }
    }
    // Compact form: "local: …;"
    if let Some(idx) = err.to_ascii_lowercase().find("local:") {
        let rest = &err[idx + "local:".len()..];
        let segment = rest.split(';').next().unwrap_or(rest);
        let lower = segment.to_lowercase();
        return lower.contains("timed out")
            || lower.contains("hard timeout")
            || lower.contains("first byte timeout")
            || (lower.contains("timeout") && !lower.contains("gateway timeout"));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_timeout_plus_cloud_reasoning_only_multi_cause() {
        let local = "Local model server timed out after 30s";
        let clouds = [(
            "Ollama Cloud fallback",
            "Ollama Cloud fallback returned empty content (reasoning only: 1996 chars)",
        )];
        let full = format_full_report(Some(local), &clouds);
        assert!(
            full.contains("Cloud fallback exhausted"),
            "M2 greppable exhausted: {full}"
        );
        assert!(
            full.contains("after local attempt"),
            "local present header: {full}"
        );
        assert!(full.contains("Local:"), "Local section: {full}");
        assert!(full.contains("timed out"), "local timeout retained: {full}");
        assert!(full.contains("reasoning only"), "0159 token: {full}");
        assert!(
            full.contains("Primary: content-quality"),
            "primary CQ: {full}"
        );
        assert!(full.contains("Next:"), "remediation block: {full}");
        assert!(
            full.to_lowercase().contains("timeout")
                || full.contains("gemini")
                || full.contains("LEDGERFUL_CLOUD_POLICY"),
            "actionable next step: {full}"
        );
    }

    #[test]
    fn local_unreachable_plus_cloud_empty_multi_cause() {
        let local = "Local model server at http://127.0.0.1:1 is unreachable";
        let clouds = [(
            "Ollama Cloud fallback",
            "Ollama Cloud fallback returned empty message content",
        )];
        let full = format_full_report(Some(local), &clouds);
        assert!(full.contains("unreachable"), "local retained: {full}");
        assert!(
            full.contains("empty message content") || full.contains("empty content"),
            "empty token: {full}"
        );
        assert!(
            full.contains("Primary: content-quality"),
            "primary CQ: {full}"
        );
        assert!(full.contains("Cloud fallback exhausted"));
    }

    #[test]
    fn cloud_only_reasoning_only_no_local_section() {
        let clouds = [(
            "Ollama Cloud fallback",
            "Ollama Cloud fallback returned empty content (reasoning only: 42 chars)",
        )];
        let full = format_full_report(None, &clouds);
        assert!(full.contains("Cloud fallback exhausted"), "M2: {full}");
        assert!(
            !full.contains("after local attempt"),
            "M1 no false local header: {full}"
        );
        assert!(
            !full.lines().any(|l| l.trim().starts_with("Local:")),
            "M1 no Local: section: {full}"
        );
        assert!(full.contains("reasoning only"), "CQ token: {full}");
        assert!(full.contains("Primary: content-quality"));
    }

    #[test]
    fn local_reasoning_only_plus_cloud_transport_primary_cq() {
        let local = "Local model server returned empty content (reasoning only: 10 chars)";
        let clouds = [(
            "OpenRouter fallback",
            "OpenRouter fallback not reachable at https://openrouter.ai — connection refused",
        )];
        let full = format_full_report(Some(local), &clouds);
        assert!(
            full.contains("Primary: content-quality"),
            "M5 local CQ wins over cloud transport: {full}"
        );
        assert!(full.contains("reasoning only"));
        assert!(full.contains("not reachable") || full.contains("connection refused"));
    }

    #[test]
    fn rate_limit_primary_and_quota_remediation() {
        let local = "Local model server at http://127.0.0.1:1 is unreachable";
        let clouds = [(
            "OpenRouter fallback",
            "OpenRouter fallback rate limited. Wait a moment or check your quota/credits.",
        )];
        let full = format_full_report(Some(local), &clouds);
        assert!(
            full.contains("Primary: rate-limit"),
            "M8 rate-limit primary: {full}"
        );
        assert!(
            !full.contains("Primary: content-quality"),
            "no false CQ: {full}"
        );
        assert!(!full.contains("Primary: auth"), "no false auth: {full}");
        assert!(
            full.to_lowercase().contains("quota")
                || full.to_lowercase().contains("credits")
                || full.to_lowercase().contains("wait"),
            "quota/wait remediation: {full}"
        );
    }

    #[test]
    fn compact_form_has_no_newlines_full_has_sections() {
        let local = "Local model server timed out after 5s";
        let clouds = [(
            "Ollama Cloud fallback",
            "Ollama Cloud fallback returned empty content (reasoning only: 3 chars)",
        )];
        let full = format_full_report(Some(local), &clouds);
        let compact = format_compact_report(Some(local), &clouds);
        assert!(
            !compact.contains('\n') && !compact.contains('\r'),
            "M6 compact no newlines: {compact:?}"
        );
        assert!(compact.contains("Cloud fallback exhausted"));
        assert!(compact.contains("content-quality") || compact.contains("primary"));
        assert!(full.contains("Primary:"));
        assert!(full.contains("Local:"));
        assert!(full.contains("Cloud:"));
        assert!(full.contains("Next:"));
    }

    #[test]
    fn synthetic_secrets_redacted() {
        let local = "Local model server timed out after 5s";
        let clouds = [(
            "OpenRouter fallback",
            "OpenRouter returned 401: Authorization Bearer sk-secret-token-xyz failed api_key=supersecret",
        )];
        let full = format_full_report(Some(local), &clouds);
        let compact = format_compact_report(Some(local), &clouds);
        assert!(
            !full.contains("sk-secret-token-xyz"),
            "bearer secret must not leak: {full}"
        );
        assert!(
            !full.contains("supersecret"),
            "api_key secret must not leak: {full}"
        );
        assert!(full.contains("[REDACTED]"), "redaction marker: {full}");
        assert!(!compact.contains("sk-secret-token-xyz"));
        assert!(!compact.contains("supersecret"));
    }

    #[test]
    fn multi_cloud_oc_or_listed_cq_primary_wins() {
        let local = "Local model server at http://127.0.0.1:1 is unreachable";
        let clouds = [
            (
                "Ollama Cloud fallback",
                "Ollama Cloud fallback returned empty content (reasoning only: 50 chars)",
            ),
            (
                "OpenRouter fallback",
                "OpenRouter fallback timed out after 15s",
            ),
        ];
        let full = format_full_report(Some(local), &clouds);
        assert!(full.contains("Ollama Cloud") || full.contains("reasoning only"));
        assert!(full.contains("OpenRouter") || full.contains("timed out"));
        assert!(
            full.contains("Primary: content-quality"),
            "CQ wins over later transport: {full}"
        );
        // Both cloud lines present
        let cloud_lines: Vec<_> = full
            .lines()
            .filter(|l| l.trim().starts_with("Cloud:"))
            .collect();
        assert!(cloud_lines.len() >= 2, "both cloud attempts listed: {full}");
    }

    #[test]
    fn compact_completion_error_collapses_full_report() {
        let full = format_full_report(
            Some("Local model server at http://127.0.0.1:1 is unreachable"),
            &[(
                "Ollama Cloud fallback",
                "Ollama Cloud fallback returned empty content (reasoning only: 9 chars)",
            )],
        );
        let compact = compact_completion_error(&full);
        assert!(!compact.contains('\n'), "no newlines: {compact:?}");
        assert!(compact.contains("Cloud fallback exhausted"));
        assert!(is_multi_cause_fallback_error(&full));
        assert!(is_multi_cause_fallback_error(&compact));
    }

    #[test]
    fn local_cause_is_timeout_detects_local_section() {
        let full = format_full_report(
            Some("Local model server timed out after 30s"),
            &[(
                "Ollama Cloud fallback",
                "Ollama Cloud fallback returned empty content (reasoning only: 1 chars)",
            )],
        );
        assert!(local_cause_is_timeout(&full));
        let unreachable = format_full_report(
            Some("Local model server at http://127.0.0.1:1 is unreachable"),
            &[(
                "Ollama Cloud fallback",
                "Ollama Cloud fallback returned empty content (reasoning only: 1 chars)",
            )],
        );
        assert!(!local_cause_is_timeout(&unreachable));
    }

    #[test]
    fn classify_error_matrix() {
        assert_eq!(
            classify_error("returned empty content (reasoning only: 3 chars)"),
            ErrorClass::ContentQuality
        );
        assert_eq!(
            classify_error("returned empty message content"),
            ErrorClass::ContentQuality
        );
        assert_eq!(
            classify_error("Ollama Cloud returned 401: unauthorized"),
            ErrorClass::Auth
        );
        assert_eq!(
            classify_error("OpenRouter rate limited. Wait a moment or check your quota/credits."),
            ErrorClass::RateLimit
        );
        assert_eq!(
            classify_error("Local model server timed out after 5s"),
            ErrorClass::Transport
        );
        assert_eq!(
            classify_error("Local model server at x is unreachable"),
            ErrorClass::Transport
        );
        assert_eq!(
            classify_error("Failed to parse completion response"),
            ErrorClass::Other
        );
    }

    #[test]
    fn remediation_mentions_cloud_policy_forbidden() {
        let full = format_full_report(
            Some("unreachable"),
            &[(
                "Ollama Cloud fallback",
                "Ollama Cloud fallback returned empty content (reasoning only: 2 chars)",
            )],
        );
        assert!(
            full.contains("LEDGERFUL_CLOUD_POLICY=forbidden"),
            "policy remediation: {full}"
        );
    }
}
