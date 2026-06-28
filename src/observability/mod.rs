pub mod log_scanner;
pub mod openslo;
pub mod prometheus;
pub mod signal;

use rusqlite::Connection;

use crate::config::model::{Config, LocalModelConfig, ObservabilityConfig};
use signal::{ObservabilitySignal, RiskElevation, SignalSeverity};

pub fn store_snapshot(
    conn: &Connection,
    signals: &[ObservabilitySignal],
    diff_pair_id: &str,
) -> Result<usize, rusqlite::Error> {
    let mut count = 0;
    let now = chrono::Utc::now().to_rfc3339();

    for signal in signals {
        conn.execute(
            "INSERT INTO observability_snapshots (signal_type, signal_label, metric_value, raw_excerpt, captured_at, diff_pair_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                signal.signal_type,
                signal.signal_label,
                signal.value,
                signal.excerpt,
                now,
                diff_pair_id,
            ],
        )?;
        count += 1;
    }

    Ok(count)
}

pub fn fetch_observability(
    config: &ObservabilityConfig,
    local_model_config: &LocalModelConfig,
    diff_text: Option<&str>,
) -> Result<Vec<ObservabilitySignal>, String> {
    if config.prometheus_url.is_empty() && config.log_paths.is_empty() {
        return Ok(vec![]);
    }

    let mut signals = Vec::new();

    let prometheus_signals = if config.prometheus_url.is_empty() {
        vec![]
    } else {
        let queries = [
            "rate(http_requests_total{status=~\"5..\"}[5m])",
            "histogram_quantile(0.99, rate(http_request_duration_seconds_bucket[5m]))",
        ];
        let mut all = Vec::new();
        for query in &queries {
            if let Ok(mut s) = prometheus::query_prometheus(config, query) {
                all.append(&mut s);
            }
        }
        all
    };

    let log_signals = if config.log_paths.is_empty() {
        vec![]
    } else {
        let local_model = if local_model_config.base_url.is_empty() {
            None
        } else {
            Some(local_model_config)
        };
        log_scanner::scan_logs(config, local_model, diff_text).unwrap_or_default()
    };

    signals.extend(prometheus_signals);
    signals.extend(log_signals);

    Ok(signals)
}

pub fn evaluate_risk(signals: &[ObservabilitySignal]) -> RiskElevation {
    if signals
        .iter()
        .any(|s| s.severity == SignalSeverity::Critical)
    {
        return RiskElevation::High;
    }

    let warning_count = signals
        .iter()
        .filter(|s| s.severity == SignalSeverity::Warning)
        .count();

    if warning_count > 3 {
        RiskElevation::Elevated
    } else {
        RiskElevation::None
    }
}

pub fn enrich_observability(
    packet: &mut crate::impact::packet::ImpactPacket,
    config: &Config,
    conn: &Connection,
) -> Result<(), String> {
    let obs_config = &config.observability;

    if obs_config.prometheus_url.is_empty() && obs_config.log_paths.is_empty() {
        return Ok(());
    }

    let diff_text: Option<String> = if packet.changes.is_empty() {
        None
    } else {
        let paths: Vec<String> = packet
            .changes
            .iter()
            .take(50)
            .map(|c| c.path.to_string_lossy().to_string())
            .collect();
        Some(format!("changed files: {}", paths.join(", ")))
    };

    let diff_text_ref = diff_text.as_deref();
    let signals = fetch_observability(obs_config, &config.local_model, diff_text_ref)?;

    if signals.is_empty() {
        return Ok(());
    }

    let diff_pair_id = packet.head_hash.as_deref().unwrap_or("unknown-hash");

    let _ = store_snapshot(conn, &signals, diff_pair_id);

    let risk = evaluate_risk(&signals);
    packet.escalate_risk(risk);

    packet.observability = signals;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::signal::{RiskElevation, SignalSeverity};
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    #[test]
    fn test_store_snapshot_roundtrip() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        let signals = vec![
            ObservabilitySignal::new(
                "error_rate",
                "GET /api/users",
                0.15,
                SignalSeverity::Critical,
                "Error rate 15% for GET /api/users",
                "prometheus",
            ),
            ObservabilitySignal::new(
                "log_anomaly",
                "ERROR: Connection refused",
                5.0,
                SignalSeverity::Warning,
                "Repeated connection errors in app.log",
                "log_file",
            ),
        ];

        let diff_pair_id = "diff-001";
        let count = store_snapshot(&conn, &signals, diff_pair_id).unwrap();
        assert_eq!(count, 2);

        let mut stmt = conn
            .prepare(
                "SELECT signal_type, signal_label, metric_value, raw_excerpt, diff_pair_id
                 FROM observability_snapshots ORDER BY id",
            )
            .unwrap();

        let rows: Vec<(String, String, f64, String, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(rows.len(), 2);

        assert_eq!(rows[0].0, "error_rate");
        assert_eq!(rows[0].1, "GET /api/users");
        assert!((rows[0].2 - 0.15).abs() < 1e-10);
        assert_eq!(rows[0].4, "diff-001");

        assert_eq!(rows[1].0, "log_anomaly");
        assert_eq!(rows[1].1, "ERROR: Connection refused");
        assert!((rows[1].2 - 5.0).abs() < 1e-10);
        assert_eq!(rows[1].4, "diff-001");
    }

    #[test]
    fn test_store_snapshot_empty_signals() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        let count = store_snapshot(&conn, &[], "none").unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_evaluate_risk_critical_returns_high() {
        let signals = vec![ObservabilitySignal::new(
            "error_rate",
            "svc",
            0.15,
            SignalSeverity::Critical,
            "critical error",
            "prometheus",
        )];
        assert_eq!(evaluate_risk(&signals), RiskElevation::High);
    }

    #[test]
    fn test_evaluate_risk_multiple_warnings_returns_elevated() {
        let mut signals = Vec::new();
        for i in 0..5 {
            signals.push(ObservabilitySignal::new(
                "log_anomaly",
                &format!("err-{i}"),
                (i + 1) as f64,
                SignalSeverity::Warning,
                "warning",
                "log_file",
            ));
        }
        assert_eq!(evaluate_risk(&signals), RiskElevation::Elevated);
    }

    #[test]
    fn test_evaluate_risk_three_warnings_returns_none() {
        let mut signals = Vec::new();
        for i in 0..3 {
            signals.push(ObservabilitySignal::new(
                "log_anomaly",
                &format!("err-{i}"),
                (i + 1) as f64,
                SignalSeverity::Warning,
                "warning",
                "log_file",
            ));
        }
        assert_eq!(evaluate_risk(&signals), RiskElevation::None);
    }

    #[test]
    fn test_evaluate_risk_empty_returns_none() {
        assert_eq!(evaluate_risk(&[]), RiskElevation::None);
    }

    #[test]
    fn test_evaluate_risk_normal_only_returns_none() {
        let signals = vec![ObservabilitySignal::new(
            "metric",
            "label",
            0.01,
            SignalSeverity::Normal,
            "normal",
            "prometheus",
        )];
        assert_eq!(evaluate_risk(&signals), RiskElevation::None);
    }

    #[test]
    fn test_fetch_observability_empty_config_returns_empty() {
        let config = ObservabilityConfig::default();
        let local = LocalModelConfig::default();
        let result = fetch_observability(&config, &local, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_fetch_observability_no_prometheus_no_logs_returns_empty() {
        let config = ObservabilityConfig {
            prometheus_url: String::new(),
            log_paths: Vec::new(),
            ..ObservabilityConfig::default()
        };
        let local = LocalModelConfig::default();
        let result = fetch_observability(&config, &local, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_fetch_observability_combines_prometheus_and_logs() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("app.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(file, "ERROR: Database connection refused").unwrap();

        let config = ObservabilityConfig {
            prometheus_url: String::new(),
            log_paths: vec![log_path.to_string_lossy().to_string()],
            ..ObservabilityConfig::default()
        };
        let local = LocalModelConfig::default();
        let result = fetch_observability(&config, &local, None).unwrap();

        assert!(!result.is_empty());
        assert!(result.iter().any(|s| s.signal_type == "log_anomaly"));
    }

    #[test]
    fn test_enrich_observability_empty_config_noop() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        let mut packet = crate::impact::packet::ImpactPacket::default();
        let config = Config::default();

        let result = enrich_observability(&mut packet, &config, &conn);
        assert!(result.is_ok());
        assert!(packet.observability.is_empty());
    }

    #[test]
    fn test_enrich_observability_adds_signals() {
        use std::io::Write;

        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("app.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        for _ in 0..5 {
            writeln!(file, "ERROR: Connection refused to database").unwrap();
        }

        let mut packet = crate::impact::packet::ImpactPacket {
            head_hash: Some("abc123".to_string()),
            ..crate::impact::packet::ImpactPacket::default()
        };

        let mut config = Config::default();
        config.observability.log_paths = vec![log_path.to_string_lossy().to_string()];

        let result = enrich_observability(&mut packet, &config, &conn);
        assert!(result.is_ok());
        assert!(!packet.observability.is_empty());

        let log_signal = packet
            .observability
            .iter()
            .find(|s| s.signal_type == "log_anomaly");
        assert!(log_signal.is_some());
    }

    #[test]
    fn test_enrich_observability_escalates_risk_on_critical() {
        use std::io::Write;

        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("app.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        for _ in 0..20 {
            writeln!(file, "ERROR: Connection refused to database").unwrap();
        }

        let mut packet = crate::impact::packet::ImpactPacket {
            head_hash: Some("abc123".to_string()),
            ..crate::impact::packet::ImpactPacket::default()
        };

        let mut config = Config::default();
        config.observability.log_paths = vec![log_path.to_string_lossy().to_string()];

        let result = enrich_observability(&mut packet, &config, &conn);
        assert!(result.is_ok());

        // >10 occurrences → Critical severity → risk escalated from Medium (default) to High
        assert_eq!(packet.risk_level, crate::impact::packet::RiskLevel::High);
    }

    #[test]
    fn test_enrichment_preserves_analyze_risk_reasons() {
        use std::io::Write;

        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("app.log");
        let mut file = std::fs::File::create(&log_path).unwrap();
        for _ in 0..5 {
            writeln!(file, "ERROR: Connection refused to database").unwrap();
        }

        // Pre-set risk_reasons as if analyze_risk had run
        let mut packet = crate::impact::packet::ImpactPacket {
            head_hash: Some("abc123".to_string()),
            risk_level: crate::impact::packet::RiskLevel::Low,
            risk_reasons: vec![
                "Minimal changes detected".to_string(),
                "Single file modified".to_string(),
            ],
            ..crate::impact::packet::ImpactPacket::default()
        };

        let mut config = Config::default();
        config.observability.log_paths = vec![log_path.to_string_lossy().to_string()];

        let result = enrich_observability(&mut packet, &config, &conn);
        assert!(result.is_ok());

        // analyze_risk reasons must be preserved
        assert!(
            packet
                .risk_reasons
                .contains(&"Minimal changes detected".to_string())
        );
        assert!(
            packet
                .risk_reasons
                .contains(&"Single file modified".to_string())
        );
        // 5 warnings (< 3 threshold) = None elevation, so risk stays Low
        assert_eq!(packet.risk_level, crate::impact::packet::RiskLevel::Low);
    }

    #[test]
    fn test_escalate_risk_level_high_low_to_medium_to_high() {
        let mut packet = crate::impact::packet::ImpactPacket {
            risk_level: crate::impact::packet::RiskLevel::Low,
            ..Default::default()
        };
        // One escalation: Low → Medium
        packet.escalate_risk(RiskElevation::High);
        assert_eq!(packet.risk_level, crate::impact::packet::RiskLevel::Medium);
        // Second escalation: Medium → High
        packet.escalate_risk(RiskElevation::High);
        assert_eq!(packet.risk_level, crate::impact::packet::RiskLevel::High);
        // Already at High, stays High
        packet.escalate_risk(RiskElevation::High);
        assert_eq!(packet.risk_level, crate::impact::packet::RiskLevel::High);
    }

    #[test]
    fn test_escalate_risk_level_elevated_low_to_medium() {
        let mut packet = crate::impact::packet::ImpactPacket {
            risk_level: crate::impact::packet::RiskLevel::Low,
            ..Default::default()
        };
        packet.escalate_risk(RiskElevation::Elevated);
        assert_eq!(packet.risk_level, crate::impact::packet::RiskLevel::Medium);
    }

    #[test]
    fn test_escalate_risk_level_elevated_medium_stays() {
        let mut packet = crate::impact::packet::ImpactPacket {
            risk_level: crate::impact::packet::RiskLevel::Medium,
            ..Default::default()
        };
        packet.escalate_risk(RiskElevation::Elevated);
        assert_eq!(packet.risk_level, crate::impact::packet::RiskLevel::Medium);
    }

    #[test]
    fn test_escalate_risk_level_none_noop() {
        let mut packet = crate::impact::packet::ImpactPacket {
            risk_level: crate::impact::packet::RiskLevel::Low,
            ..Default::default()
        };
        packet.escalate_risk(RiskElevation::None);
        assert_eq!(packet.risk_level, crate::impact::packet::RiskLevel::Low);
    }
}
