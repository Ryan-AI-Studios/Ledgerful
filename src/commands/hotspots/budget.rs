use crate::commands::hotspots::list::{latest_hotspot_history_timestamp, snapshot_age_secs_from};
use crate::state::storage::StorageManager;
use chrono::{DateTime, Utc};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use serde::Serialize;

pub(super) const DEFAULT_HOTSPOTS_BUDGET_THRESHOLD: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum BudgetStatus {
    Ok,
    Violation,
    NoData,
    NotConfigured,
}

impl BudgetStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Violation => "VIOLATION",
            Self::NoData => "NO_DATA",
            Self::NotConfigured => "NOT_CONFIGURED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ThresholdSource {
    Cli,
    Config,
    Default,
}

impl ThresholdSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Config => "config",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BudgetViolation {
    pub path: String,
    pub score: f64,
    pub threshold: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BudgetReport {
    pub status: BudgetStatus,
    pub score_unit: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_source: Option<ThresholdSource>,
    pub evaluated: u64,
    pub violations: Vec<BudgetViolation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_age_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub legacy_score_count: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub skipped_non_finite: u64,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

pub(super) fn evaluate_hotspot_budget(
    storage: &StorageManager,
    cli_threshold: Option<f64>,
    config_threshold: Option<f64>,
    fail: bool,
    head: Option<String>,
    now: DateTime<Utc>,
) -> Result<BudgetReport> {
    let snapshot_at = latest_hotspot_history_timestamp(storage);
    let snapshot_age_secs = snapshot_at
        .as_deref()
        .and_then(|ts| snapshot_age_secs_from(ts, now));

    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT file_path, score FROM hotspot_history \
         WHERE timestamp = (SELECT MAX(timestamp) FROM hotspot_history) \
         ORDER BY file_path ASC, score DESC",
        )
        .into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .into_diagnostic()?;

    let mut evaluated = 0_u64;
    let mut legacy_score_count = 0_u64;
    let mut skipped_non_finite = 0_u64;
    let mut finite_scores: Vec<(String, f64)> = Vec::new();

    for row in rows {
        let (path, score) = row.into_diagnostic()?;
        if !score.is_finite() {
            skipped_non_finite += 1;
            continue;
        }
        evaluated += 1;
        if score > 1.0 {
            legacy_score_count += 1;
        }
        finite_scores.push((path, score));
    }

    let resolved = resolve_threshold(cli_threshold, config_threshold, fail);
    let (status, threshold, threshold_source, violations) = match resolved {
        ThresholdResolution::NotConfigured => (BudgetStatus::NotConfigured, None, None, Vec::new()),
        ThresholdResolution::Ready { threshold, source } => {
            if evaluated == 0 {
                (
                    BudgetStatus::NoData,
                    Some(threshold),
                    Some(source),
                    Vec::new(),
                )
            } else {
                let mut violations: Vec<BudgetViolation> = finite_scores
                    .into_iter()
                    .filter(|(_, score)| *score > threshold)
                    .map(|(path, score)| BudgetViolation {
                        path,
                        score,
                        threshold,
                    })
                    .collect();
                violations.sort_by(|a, b| {
                    a.path.cmp(&b.path).then_with(|| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                });
                let status = if violations.is_empty() {
                    BudgetStatus::Ok
                } else {
                    BudgetStatus::Violation
                };
                (status, Some(threshold), Some(source), violations)
            }
        }
    };

    Ok(BudgetReport {
        status,
        score_unit: "score",
        threshold,
        threshold_source,
        evaluated,
        violations,
        snapshot_at,
        snapshot_age_secs,
        head,
        legacy_score_count,
        skipped_non_finite,
    })
}

enum ThresholdResolution {
    NotConfigured,
    Ready {
        threshold: f64,
        source: ThresholdSource,
    },
}

fn resolve_threshold(
    cli_threshold: Option<f64>,
    config_threshold: Option<f64>,
    fail: bool,
) -> ThresholdResolution {
    if let Some(threshold) = cli_threshold {
        return ThresholdResolution::Ready {
            threshold,
            source: ThresholdSource::Cli,
        };
    }
    if let Some(threshold) = config_threshold {
        return ThresholdResolution::Ready {
            threshold,
            source: ThresholdSource::Config,
        };
    }
    if fail {
        return ThresholdResolution::NotConfigured;
    }
    ThresholdResolution::Ready {
        threshold: DEFAULT_HOTSPOTS_BUDGET_THRESHOLD,
        source: ThresholdSource::Default,
    }
}

pub(super) fn format_budget_human(report: &BudgetReport) -> String {
    let mut lines = vec!["Hotspot Budget Check".to_string()];
    lines.push(format!("  Status: {}", report.status.as_str()));
    lines.push("  Unit: score (0-1)".to_string());
    if let (Some(threshold), Some(source)) = (report.threshold, report.threshold_source) {
        lines.push(format!(
            "  Threshold: {:.2} ({})",
            threshold,
            source.as_str()
        ));
    }
    lines.push(format!(
        "  Evaluated: {}{}",
        report.evaluated,
        evaluated_snapshot_suffix(report)
    ));
    match report.status {
        BudgetStatus::Ok => {
            lines.push("  All evaluated hotspots are within budget.".to_string());
        }
        BudgetStatus::Violation => {
            for v in &report.violations {
                lines.push(format!(
                    "  ! {} exceeds budget: {:.2} > {:.2}",
                    v.path, v.score, v.threshold
                ));
            }
        }
        BudgetStatus::NoData => {
            lines.push(
                "  No hotspot_history snapshot. Run `ledgerful hotspots --snapshot`.".to_string(),
            );
        }
        BudgetStatus::NotConfigured => {
            lines.push("  --fail requires --threshold or [hotspots] budget_threshold.".to_string());
        }
    }
    if report.legacy_score_count > 0 {
        lines.push(format!(
            "  Note: {} row(s) have score > 1 (compared as stored; not converted).",
            report.legacy_score_count
        ));
    }
    lines.join("\n")
}

fn evaluated_snapshot_suffix(report: &BudgetReport) -> String {
    match (&report.snapshot_at, report.snapshot_age_secs) {
        (Some(ts), Some(age)) if report.status != BudgetStatus::NoData => {
            format!(" (snapshot {ts} age {age}s)")
        }
        _ => String::new(),
    }
}

pub(super) fn execute_hotspots_budget(
    storage: &StorageManager,
    repo: &gix::Repository,
    config: &crate::config::model::Config,
    json: bool,
    threshold: Option<f64>,
    fail: bool,
) -> Result<()> {
    let head = repo.head_commit().ok().map(|c| c.id().to_string());
    let report = evaluate_hotspot_budget(
        storage,
        threshold,
        config.hotspots.budget_threshold,
        fail,
        head,
        Utc::now(),
    )?;

    if json {
        crate::output::json::emit(&report)
            .map_err(|e| miette::miette!("Failed to serialize budget check: {}", e))?;
    } else {
        let rendered = format_budget_human(&report);
        for (i, line) in rendered.lines().enumerate() {
            if i == 0 {
                println!(
                    "{}",
                    line.if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
            } else if line.contains("Status: VIOLATION") {
                println!(
                    "  Status: {}",
                    "VIOLATION"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
                );
            } else if line.contains("Status: OK") {
                println!(
                    "  Status: {}",
                    "OK".if_supports_color(Stream::Stdout, |s| s.green())
                );
            } else if let Some(rest) = line.strip_prefix("  ! ") {
                let bang = "!".if_supports_color(Stream::Stdout, |s| s.yellow());
                println!("  {bang} {rest}");
            } else {
                println!("{line}");
            }
        }
    }

    request_fail_exit(&report, fail)
}

pub(super) fn request_fail_exit(report: &BudgetReport, fail: bool) -> Result<()> {
    if fail && report.status != BudgetStatus::Ok {
        crate::output::requested_exit::request_exit(1);
        return Err(miette::miette!(
            "hotspots budget failed: {}",
            report.status.as_str()
        ));
    }
    Ok(())
}
