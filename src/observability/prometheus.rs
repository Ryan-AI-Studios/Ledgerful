use std::time::Duration;

use crate::config::model::ObservabilityConfig;

use super::signal::{ObservabilitySignal, SignalSeverity};

pub fn query_prometheus(
    config: &ObservabilityConfig,
    query: &str,
) -> Result<Vec<ObservabilitySignal>, String> {
    if config.prometheus_url.is_empty() {
        return Ok(vec![]);
    }

    let url = format!("{}/api/v1/query", config.prometheus_url);

    let agent = ureq::AgentBuilder::new()
        .timeout_read(Duration::from_secs(6))
        .timeout_write(Duration::from_secs(6))
        .build();

    let response = match agent.get(&url).query("query", query).call() {
        Ok(r) => r,
        Err(ureq::Error::Transport(e)) => {
            tracing::warn!("Prometheus unreachable: {e}");
            return Ok(vec![]);
        }
        Err(ureq::Error::Status(code, _)) => {
            tracing::warn!("Prometheus returned status {code}");
            return Ok(vec![]);
        }
    };

    let body: serde_json::Value = match response.into_json() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse Prometheus response: {e}");
            return Ok(vec![]);
        }
    };

    let results = match body["data"]["result"].as_array() {
        Some(arr) => arr,
        None => return Ok(vec![]),
    };

    let signal_type = if query.contains("histogram_quantile") {
        "latency_p99"
    } else if query.contains("http_requests_total") {
        "error_rate"
    } else {
        "custom"
    };

    let mut signals = Vec::new();
    for result in results {
        let value = match result["value"].as_array() {
            Some(arr) if arr.len() >= 2 => {
                match arr[1].as_str().and_then(|s| s.parse::<f64>().ok()) {
                    Some(v) => v,
                    None => continue,
                }
            }
            _ => continue,
        };

        let metric = &result["metric"];
        let label = if let Some(obj) = metric.as_object() {
            if obj.is_empty() {
                "unknown".to_string()
            } else {
                let mut parts: Vec<String> = obj
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                    .collect();
                parts.sort();
                parts.join(", ")
            }
        } else {
            "unknown".to_string()
        };

        let severity = match signal_type {
            "error_rate" => {
                if value > 0.1 {
                    SignalSeverity::Critical
                } else if value > config.error_rate_threshold as f64 {
                    SignalSeverity::Warning
                } else {
                    SignalSeverity::Normal
                }
            }
            "latency_p99" => {
                if value > 1.0 {
                    SignalSeverity::Critical
                } else if value > 0.5 {
                    SignalSeverity::Warning
                } else {
                    SignalSeverity::Normal
                }
            }
            _ => {
                if value > 100.0 {
                    SignalSeverity::Critical
                } else if value > 50.0 {
                    SignalSeverity::Warning
                } else {
                    SignalSeverity::Normal
                }
            }
        };

        let excerpt = format!("{signal_type}: {label} = {value:.4}");

        signals.push(ObservabilitySignal::new(
            signal_type,
            &label,
            value,
            severity,
            &excerpt,
            "prometheus",
        ));
    }

    Ok(signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::ObservabilityConfig;
    use httpmock::prelude::*;

    fn test_config(prometheus_url: String) -> ObservabilityConfig {
        ObservabilityConfig {
            prometheus_url,
            ..ObservabilityConfig::default()
        }
    }

    #[test]
    fn test_query_prometheus_empty_url_returns_empty() {
        let config = ObservabilityConfig::default();
        let result = query_prometheus(&config, "up").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_query_prometheus_parses_valid_response() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(GET).path("/api/v1/query");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "status": "success",
                    "data": {
                        "resultType": "vector",
                        "result": [
                            {
                                "metric": {"job": "api", "instance": "localhost:9090"},
                                "value": [1680000000.0, "0.032"]
                            }
                        ]
                    }
                }));
        });

        let config = test_config(server.base_url());
        let query = "rate(http_requests_total{status=~\"5..\"}[5m])";
        let result = query_prometheus(&config, query).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].signal_type, "error_rate");
        assert!((result[0].value - 0.032).abs() < 1e-10);
        assert_eq!(result[0].source, "prometheus");
        assert_eq!(result[0].signal_label, "instance=localhost:9090, job=api");
    }

    #[test]
    fn test_query_prometheus_latency_query() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(GET).path("/api/v1/query");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "status": "success",
                    "data": {
                        "resultType": "vector",
                        "result": [
                            {
                                "metric": {"job": "api"},
                                "value": [1680000000.0, "0.75"]
                            }
                        ]
                    }
                }));
        });

        let config = test_config(server.base_url());
        let query = "histogram_quantile(0.99, rate(http_request_duration_seconds_bucket[5m]))";
        let result = query_prometheus(&config, query).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].signal_type, "latency_p99");
        assert!((result[0].value - 0.75).abs() < 1e-10);
        assert_eq!(result[0].severity, SignalSeverity::Warning);
    }

    #[test]
    fn test_query_prometheus_unreachable_returns_empty() {
        let config = ObservabilityConfig {
            prometheus_url: "http://127.0.0.1:1".to_string(),
            ..ObservabilityConfig::default()
        };
        let result = query_prometheus(&config, "up").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_query_prometheus_server_error_returns_empty() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(GET).path("/api/v1/query");
            then.status(503).body("Service Unavailable");
        });

        let config = test_config(server.base_url());
        let result = query_prometheus(&config, "up").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_query_prometheus_malformed_json_returns_empty() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(GET).path("/api/v1/query");
            then.status(200)
                .header("Content-Type", "application/json")
                .body("not valid json {{{");
        });

        let config = test_config(server.base_url());
        let result = query_prometheus(&config, "up").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_query_prometheus_no_results_returns_empty() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(GET).path("/api/v1/query");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "status": "success",
                    "data": {
                        "resultType": "vector",
                        "result": []
                    }
                }));
        });

        let config = test_config(server.base_url());
        let result = query_prometheus(&config, "up").unwrap();
        assert!(result.is_empty());
    }
}
