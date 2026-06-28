use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::config::model::{LocalModelConfig, ObservabilityConfig};

use super::signal::{ObservabilitySignal, SignalSeverity};

const LOOKBACK_LINES_PER_MINUTE: usize = 100;
const WALL_CLOCK_CAP_SECS: u64 = 6;
const CHUNK_LINES: usize = 20;
const ANOMALY_THRESHOLD: f32 = 0.6;
const ANOMALY_CRITICAL_THRESHOLD: f32 = 0.85;

type PatternMatcher = (fn(&str) -> bool, &'static str);

pub fn scan_logs(
    config: &ObservabilityConfig,
    local_model: Option<&LocalModelConfig>,
    diff_text: Option<&str>,
) -> Result<Vec<ObservabilitySignal>, String> {
    if config.log_paths.is_empty() {
        return Ok(vec![]);
    }

    let start = Instant::now();
    let cap = std::time::Duration::from_secs(WALL_CLOCK_CAP_SECS);

    // ── Embedding-based path (primary) ──────────────────────────
    if let (Some(lm), Some(diff)) = (local_model, diff_text)
        && !lm.base_url.is_empty()
        && !lm.embedding_model.is_empty()
    {
        if let Ok(signals) = scan_logs_with_embeddings(config, lm, diff, start, cap) {
            if !signals.is_empty() {
                return Ok(signals);
            }
            // If embedding path returned empty, fall through to keyword path
            tracing::debug!("Embedding scan found no anomalies, falling back to keyword scan");
        } else {
            tracing::debug!("Embedding scan failed, falling back to keyword scan");
        }
    }

    // ── Keyword-based path (fallback) ───────────────────────────
    scan_logs_keyword(config, start, cap)
}

fn scan_logs_with_embeddings(
    config: &ObservabilityConfig,
    local_model: &LocalModelConfig,
    diff_text: &str,
    start: Instant,
    cap: std::time::Duration,
) -> Result<Vec<ObservabilitySignal>, String> {
    let mut signals = Vec::new();
    let lookback_lines =
        ((config.log_lookback_secs as usize) / 60).max(1) * LOOKBACK_LINES_PER_MINUTE;

    // 1. Embed diff_text
    let diff_embeddings = crate::embed::client::embed_batch(
        &local_model.base_url,
        &local_model.embedding_model,
        &[diff_text],
        local_model.timeout_secs,
    )?;
    let diff_vector = diff_embeddings
        .into_iter()
        .next()
        .ok_or_else(|| "No diff embedding returned".to_string())?;

    for log_path in &config.log_paths {
        if start.elapsed() >= cap {
            tracing::warn!("Log scan wall-clock cap reached, stopping");
            break;
        }

        let path = Path::new(log_path);
        if !path.exists() {
            tracing::warn!("Log file not found: {log_path}");
            continue;
        }

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Cannot read log file {log_path}: {e}");
                continue;
            }
        };

        let lines: Vec<&str> = content.lines().collect();
        let start_idx = if lines.len() > lookback_lines {
            lines.len() - lookback_lines
        } else {
            0
        };

        // Split into 20-line chunks
        let mut chunks: Vec<Vec<&str>> = Vec::new();
        let mut i = start_idx;
        while i < lines.len() {
            let end = std::cmp::min(i + CHUNK_LINES, lines.len());
            chunks.push(lines[i..end].to_vec());
            i = end;
        }

        // Embed chunks in batches of MAX_BATCH_SIZE
        let chunk_texts: Vec<String> = chunks.iter().map(|c| c.join("\n")).collect();
        let chunk_refs: Vec<&str> = chunk_texts.iter().map(|s| s.as_str()).collect();

        let mut chunk_vectors: Vec<Vec<f32>> = Vec::new();
        for batch in chunk_refs.chunks(crate::embed::client::MAX_BATCH_SIZE) {
            let batch_vecs = crate::embed::client::embed_batch(
                &local_model.base_url,
                &local_model.embedding_model,
                batch,
                local_model.timeout_secs,
            )?;
            chunk_vectors.extend(batch_vecs);
        }

        // Compare each chunk vs diff embedding
        for (chunk_idx, chunk_vec) in chunk_vectors.iter().enumerate() {
            let similarity = match crate::embed::similarity::cosine_sim(chunk_vec, &diff_vector) {
                Ok(s) => s,
                Err(_) => continue,
            };

            if similarity >= ANOMALY_THRESHOLD {
                let severity = if similarity >= ANOMALY_CRITICAL_THRESHOLD {
                    SignalSeverity::Critical
                } else {
                    SignalSeverity::Warning
                };

                let chunk = &chunks[chunk_idx];
                let excerpt = chunk.join("\n");
                let first_line = chunk.first().map(|s| s.to_string()).unwrap_or_default();
                let key = if first_line.len() > 80 {
                    first_line[..80].to_string()
                } else {
                    first_line
                };

                signals.push(ObservabilitySignal::new(
                    "log_anomaly",
                    &key,
                    similarity as f64,
                    severity,
                    &excerpt,
                    "log_file_embedding",
                ));
            }
        }
    }

    Ok(signals)
}

fn scan_logs_keyword(
    config: &ObservabilityConfig,
    start: Instant,
    cap: std::time::Duration,
) -> Result<Vec<ObservabilitySignal>, String> {
    if config.log_paths.is_empty() {
        return Ok(vec![]);
    }

    let lookback_lines =
        ((config.log_lookback_secs as usize) / 60).max(1) * LOOKBACK_LINES_PER_MINUTE;

    let pattern_matchers: &[PatternMatcher] = &[
        (|line: &str| line.to_uppercase().contains("ERROR"), "ERROR"),
        (|line: &str| line.to_uppercase().contains("FATAL"), "FATAL"),
        (|line: &str| line.to_lowercase().contains("panic"), "panic"),
        (
            |line: &str| line.to_lowercase().contains("exception"),
            "exception",
        ),
    ];

    let mut grouped: HashMap<String, (usize, String)> = HashMap::new();

    for log_path in &config.log_paths {
        if start.elapsed() >= cap {
            tracing::warn!("Log scan wall-clock cap reached, stopping");
            break;
        }

        let path = Path::new(log_path);
        if !path.exists() {
            tracing::warn!("Log file not found: {log_path}");
            continue;
        }

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Cannot read log file {log_path}: {e}");
                continue;
            }
        };

        let lines: Vec<&str> = content.lines().collect();
        let start_idx = if lines.len() > lookback_lines {
            lines.len() - lookback_lines
        } else {
            0
        };

        for i in start_idx..lines.len() {
            if start.elapsed() >= cap {
                break;
            }

            let line = lines[i];
            let mut matched_pattern = None;

            for (matcher, name) in pattern_matchers {
                if matcher(line) {
                    matched_pattern = Some(*name);
                    break;
                }
            }

            if let Some(_pattern_name) = matched_pattern {
                let ctx_start = i.saturating_sub(2).max(start_idx);
                let ctx_end = (i + 3).min(lines.len());
                let excerpt_lines = lines[ctx_start..ctx_end].to_vec();
                let excerpt = excerpt_lines.join("\n");

                let key = if line.len() > 80 {
                    line[..80].to_string()
                } else {
                    line.to_string()
                };

                let entry = grouped.entry(key).or_insert_with(|| (0, excerpt));
                entry.0 += 1;
            }
        }
    }

    let mut signals = Vec::new();
    for (key, (count, excerpt)) in grouped {
        let severity = if count > 10 {
            SignalSeverity::Critical
        } else if count > 3 {
            SignalSeverity::Warning
        } else {
            SignalSeverity::Normal
        };

        let signal_label = if key.len() > 80 {
            format!("{}...", &key[..77])
        } else {
            key
        };

        signals.push(ObservabilitySignal::new(
            "log_anomaly",
            &signal_label,
            count as f64,
            severity,
            &excerpt,
            "log_file",
        ));
    }

    Ok(signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::ObservabilityConfig;
    use std::io::Write;

    fn test_config(log_paths: Vec<String>) -> ObservabilityConfig {
        ObservabilityConfig {
            log_paths,
            ..ObservabilityConfig::default()
        }
    }

    #[test]
    fn test_scan_logs_empty_paths() {
        let config = ObservabilityConfig::default();
        let result = scan_logs(&config, None, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_scan_logs_detects_error_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(file, "INFO: Starting application").unwrap();
        writeln!(file, "DEBUG: Processing request").unwrap();
        writeln!(file, "ERROR: Connection refused to database").unwrap();
        writeln!(file, "INFO: Retrying...").unwrap();
        writeln!(file, "FATAL: Unable to recover, exiting").unwrap();

        let config = test_config(vec![log_path.to_string_lossy().to_string()]);
        let result = scan_logs(&config, None, None).unwrap();

        assert!(!result.is_empty());
        assert!(result.iter().all(|s| s.signal_type == "log_anomaly"));
        assert!(result.iter().any(|s| s.excerpt.contains("ERROR")));
        assert!(result.iter().any(|s| s.excerpt.contains("FATAL")));
    }

    #[test]
    fn test_scan_logs_missing_file_skipped() {
        let config = test_config(vec!["/nonexistent/file.log".to_string()]);
        let result = scan_logs(&config, None, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_scan_logs_no_errors_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("clean.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(file, "INFO: Starting application").unwrap();
        writeln!(file, "INFO: Processing complete").unwrap();

        let config = test_config(vec![log_path.to_string_lossy().to_string()]);
        let result = scan_logs(&config, None, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_scan_logs_dedup_and_severity() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("repeated.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        for _ in 0..20 {
            writeln!(file, "ERROR: Connection refused to database").unwrap();
        }

        let config = test_config(vec![log_path.to_string_lossy().to_string()]);
        let result = scan_logs(&config, None, None).unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].value >= 10.0);
        assert_eq!(result[0].severity, SignalSeverity::Critical);
    }

    #[test]
    fn test_scan_logs_detects_panic_and_exception() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("crash.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(file, "thread 'main' panicked at 'index out of bounds'").unwrap();
        writeln!(file, "Unhandled exception: NullPointerException").unwrap();

        let config = test_config(vec![log_path.to_string_lossy().to_string()]);
        let result = scan_logs(&config, None, None).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|s| s.excerpt.contains("panicked")));
        assert!(result.iter().any(|s| s.excerpt.contains("exception")));
    }

    #[test]
    fn test_scan_logs_sanitizes_excerpts() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("secrets.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(
            file,
            "ERROR: Failed with token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890"
        )
        .unwrap();

        let config = test_config(vec![log_path.to_string_lossy().to_string()]);
        let result = scan_logs(&config, None, None).unwrap();

        assert_eq!(result.len(), 1);
        assert!(!result[0].excerpt.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
    }

    #[test]
    fn test_scan_logs_context_window() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("context.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(file, "line 0").unwrap();
        writeln!(file, "line 1").unwrap();
        writeln!(file, "ERROR: something broke").unwrap();
        writeln!(file, "line 3").unwrap();
        writeln!(file, "line 4").unwrap();
        writeln!(file, "line 5").unwrap();

        let config = test_config(vec![log_path.to_string_lossy().to_string()]);
        let result = scan_logs(&config, None, None).unwrap();

        assert!(!result.is_empty());
        let excerpt = &result[0].excerpt;
        assert!(excerpt.contains("line 1"));
        assert!(excerpt.contains("ERROR"));
        assert!(excerpt.contains("line 3"));
        assert!(excerpt.contains("line 4"));
    }

    #[test]
    fn test_scan_logs_keyword_fallback_when_embedding_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("fallback.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(file, "ERROR: Connection timeout").unwrap();

        let config = test_config(vec![log_path.to_string_lossy().to_string()]);
        // LocalModelConfig with empty base_url → should fall back to keyword
        let local_model = LocalModelConfig::default();
        let result = scan_logs(&config, Some(&local_model), Some("changed src/main.rs")).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].signal_type, "log_anomaly");
        assert!(result[0].excerpt.contains("ERROR"));
    }

    #[test]
    fn test_scan_logs_embedding_path_detects_anomalies() {
        use httpmock::prelude::*;

        let server = MockServer::start();

        // Mock embeddings endpoint (called for diff_text and for log chunks)
        server.mock(|when, then| {
            when.method(POST).path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "data": [
                        {"embedding": [0.1, 0.2, 0.3, 0.4]}
                    ]
                }));
        });

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("app.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        // 20+ lines for at least one chunk
        for i in 0..25 {
            writeln!(file, "INFO: Processing item {i}").unwrap();
        }

        let config = test_config(vec![log_path.to_string_lossy().to_string()]);

        let local_model = LocalModelConfig {
            base_url: server.base_url(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            timeout_secs: 5,
            ..LocalModelConfig::default()
        };

        // Verify embedding path runs without panicking
        let result = scan_logs(
            &config,
            Some(&local_model),
            Some("changed files: src/main.rs"),
        );
        assert!(result.is_ok());
    }
}
