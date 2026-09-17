//! Explicit `bridge query` result states (0366).
//!
//! `query_unified` stays fail-open for ask. This module owns CLI status,
//! `--json`, and human/exit agreement.

use super::client_cli::{self, CliRun};
use super::is_bridge_enabled;
use crate::bridge::ipc::IpcClient;
use crate::bridge::model::{BridgeDirection, BridgePayload, BridgeRecord, deserialize_record};
use crate::state::layout::Layout;
use crate::util::query::sanitize_fts5_query;
use miette::{Result, miette};
use serde::Serialize;
use std::time::Duration;

pub const BRIDGE_ENABLE_HINT: &str =
    "Bridge is disabled. Enable with `bridge.enabled = true` in config or set LEDGERFUL_BRIDGE=1.";

const NEXT_UNAVAILABLE: &str =
    "Start the configured provider or install the binary named by bridge.provider_command";
const NEXT_ALLOWLIST: &str = "bridge.provider_command must be `ai-brains`";
const NEXT_EMPTY: &str = "If a provider is installed, run the provider's sync/daemon command (e.g. `provider sync query` or `provider daemon start` to enable IPC).";
const NEXT_MALFORMED: &str = "Provider output was not valid BridgeRecord NDJSON.";

const PRODUCTION_CLI_TIMEOUT: Duration = Duration::from_millis(2000);
const PRODUCTION_IPC_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryStatus {
    Disabled,
    Unavailable,
    Failed,
    Empty,
    Populated,
}

impl QueryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
            Self::Empty => "empty",
            Self::Populated => "populated",
        }
    }

    pub fn ok(self) -> bool {
        matches!(self, Self::Disabled | Self::Empty | Self::Populated)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuerySource {
    Ipc,
    Cli,
}

impl QuerySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ipc => "ipc",
            Self::Cli => "cli",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InsightHit {
    pub memory_id: String,
    pub relevance: f64,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct QueryOutcome {
    pub status: QueryStatus,
    pub source: Option<QuerySource>,
    pub provider_command: Option<String>,
    pub insights: Vec<InsightHit>,
    pub skipped_lines: u64,
    pub message: Option<String>,
    pub next: Option<String>,
    insight_records: Vec<BridgeRecord>,
}

impl QueryOutcome {
    fn disabled() -> Self {
        Self {
            status: QueryStatus::Disabled,
            source: None,
            provider_command: None,
            insights: Vec::new(),
            skipped_lines: 0,
            message: None,
            next: Some(BRIDGE_ENABLE_HINT.to_string()),
            insight_records: Vec::new(),
        }
    }

    pub fn insight_records(&self) -> Vec<BridgeRecord> {
        self.insight_records.clone()
    }
}

#[derive(Debug, Clone)]
pub enum IpcOverride {
    Miss,
    Records(Vec<BridgeRecord>),
}

#[derive(Debug, Clone)]
pub struct QueryTransport {
    pub try_ipc: bool,
    pub cli_timeout: Duration,
    pub ipc: Option<IpcOverride>,
    pub(crate) cli: Option<CliRun>,
}

impl QueryTransport {
    pub fn production() -> Self {
        Self {
            try_ipc: true,
            cli_timeout: PRODUCTION_CLI_TIMEOUT,
            ipc: None,
            cli: None,
        }
    }
}

pub fn query_status(query: &str, layout: &Layout, transport: &QueryTransport) -> QueryOutcome {
    query_status_inner(query, layout, transport, is_bridge_enabled(layout))
}

pub(crate) fn query_status_inner(
    query: &str,
    layout: &Layout,
    transport: &QueryTransport,
    enabled: bool,
) -> QueryOutcome {
    if !enabled {
        return QueryOutcome::disabled();
    }

    let command = provider_command_for(layout);
    let sanitized = sanitize_fts5_query(query);

    if transport.try_ipc
        && let Some(records) = probe_ipc(layout, &sanitized, transport)
        && !records.is_empty()
    {
        return classify_records(QuerySource::Ipc, None, records, 0);
    }

    let run = match &transport.cli {
        Some(r) => r.clone(),
        None => client_cli::query_external_cli(&sanitized, transport.cli_timeout, &command),
    };
    cli_run_to_outcome(run)
}

fn probe_ipc(
    layout: &Layout,
    sanitized: &str,
    transport: &QueryTransport,
) -> Option<Vec<BridgeRecord>> {
    match &transport.ipc {
        Some(IpcOverride::Miss) => None,
        Some(IpcOverride::Records(records)) => Some(records.clone()),
        None => try_real_ipc(layout, sanitized),
    }
}

fn try_real_ipc(layout: &Layout, sanitized: &str) -> Option<Vec<BridgeRecord>> {
    let mut client = IpcClient::connect_with_timeout(PRODUCTION_IPC_TIMEOUT).ok()?;
    let payload = BridgePayload::Query {
        text: sanitized.to_string(),
    };
    let req = BridgeRecord::new(
        BridgeDirection::Inbound,
        layout.get_project_id(),
        "query",
        payload,
    );
    if client.send_record(&req).is_err() {
        return None;
    }
    client.receive_records().ok().filter(|r| !r.is_empty())
}

fn provider_command_for(layout: &Layout) -> String {
    crate::config::load::load_config(layout)
        .map(|c| c.bridge.provider_command)
        .unwrap_or_else(|_| "ai-brains".to_string())
}

fn cli_run_to_outcome(run: CliRun) -> QueryOutcome {
    match run {
        CliRun::AllowlistDenied { command } => QueryOutcome {
            status: QueryStatus::Failed,
            source: Some(QuerySource::Cli),
            provider_command: Some(command),
            insights: Vec::new(),
            skipped_lines: 0,
            message: Some(NEXT_ALLOWLIST.to_string()),
            next: Some(NEXT_ALLOWLIST.to_string()),
            insight_records: Vec::new(),
        },
        CliRun::SpawnErr { command } => QueryOutcome {
            status: QueryStatus::Unavailable,
            source: Some(QuerySource::Cli),
            provider_command: Some(command),
            insights: Vec::new(),
            skipped_lines: 0,
            message: None,
            next: Some(NEXT_UNAVAILABLE.to_string()),
            insight_records: Vec::new(),
        },
        CliRun::Timeout { command } => QueryOutcome {
            status: QueryStatus::Failed,
            source: Some(QuerySource::Cli),
            provider_command: Some(command),
            insights: Vec::new(),
            skipped_lines: 0,
            message: Some("Provider query timed out.".to_string()),
            next: None,
            insight_records: Vec::new(),
        },
        CliRun::NonZero { command, message } => QueryOutcome {
            status: QueryStatus::Failed,
            source: Some(QuerySource::Cli),
            provider_command: Some(command),
            insights: Vec::new(),
            skipped_lines: 0,
            message: Some(truncate_message(&message)),
            next: None,
            insight_records: Vec::new(),
        },
        CliRun::WaitErr { command, message } => QueryOutcome {
            status: QueryStatus::Failed,
            source: Some(QuerySource::Cli),
            provider_command: Some(command),
            insights: Vec::new(),
            skipped_lines: 0,
            message: Some(truncate_message(&message)),
            next: None,
            insight_records: Vec::new(),
        },
        CliRun::Stdout { command, stdout } => parse_cli_stdout(command, &stdout),
    }
}

fn parse_cli_stdout(command: String, stdout: &str) -> QueryOutcome {
    let mut records = Vec::new();
    let mut unparseable: u64 = 0;
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match deserialize_record(line) {
            Ok(record) => records.push(record),
            Err(_) => unparseable += 1,
        }
    }
    if records.is_empty() && unparseable > 0 {
        return QueryOutcome {
            status: QueryStatus::Failed,
            source: Some(QuerySource::Cli),
            provider_command: Some(command),
            insights: Vec::new(),
            skipped_lines: unparseable,
            message: Some(NEXT_MALFORMED.to_string()),
            next: Some(NEXT_MALFORMED.to_string()),
            insight_records: Vec::new(),
        };
    }
    classify_records(QuerySource::Cli, Some(command), records, unparseable)
}

fn classify_records(
    source: QuerySource,
    provider_command: Option<String>,
    records: Vec<BridgeRecord>,
    unparseable: u64,
) -> QueryOutcome {
    let mut insights = Vec::new();
    let mut insight_records = Vec::new();
    let mut skipped = unparseable;
    for record in records {
        match &record.payload {
            BridgePayload::Insight {
                memory_id,
                relevance,
                content,
            } if relevance.is_finite() => {
                insights.push(InsightHit {
                    memory_id: memory_id.clone(),
                    relevance: *relevance,
                    content: content.clone(),
                });
                insight_records.push(record);
            }
            _ => skipped += 1,
        }
    }
    insights.sort_by(|a, b| {
        a.memory_id
            .cmp(&b.memory_id)
            .then(a.relevance.total_cmp(&b.relevance))
    });
    insight_records.sort_by(|a, b| match (&a.payload, &b.payload) {
        (
            BridgePayload::Insight {
                memory_id: id_a,
                relevance: rel_a,
                ..
            },
            BridgePayload::Insight {
                memory_id: id_b,
                relevance: rel_b,
                ..
            },
        ) => id_a.cmp(id_b).then(rel_a.total_cmp(rel_b)),
        _ => std::cmp::Ordering::Equal,
    });

    if insights.is_empty() {
        QueryOutcome {
            status: QueryStatus::Empty,
            source: Some(source),
            provider_command,
            insights,
            skipped_lines: skipped,
            message: None,
            next: Some(NEXT_EMPTY.to_string()),
            insight_records,
        }
    } else {
        QueryOutcome {
            status: QueryStatus::Populated,
            source: Some(source),
            provider_command,
            insights,
            skipped_lines: skipped,
            message: None,
            next: None,
            insight_records,
        }
    }
}

fn truncate_message(raw: &str) -> String {
    let trimmed = raw.trim();
    let one_line = trimmed.lines().next().unwrap_or(trimmed).trim();
    const MAX: usize = 240;
    if one_line.chars().count() <= MAX {
        one_line.to_string()
    } else {
        one_line.chars().take(MAX).collect()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InsightJson {
    memory_id: String,
    relevance: f64,
    content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeQueryEnvelope {
    schema_version: u32,
    kind: &'static str,
    ok: bool,
    status: &'static str,
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    results: Option<Vec<InsightJson>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_lines: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

fn envelope(query: &str, outcome: &QueryOutcome) -> BridgeQueryEnvelope {
    let populated = outcome.status == QueryStatus::Populated;
    BridgeQueryEnvelope {
        schema_version: 1,
        kind: "bridgeQuery",
        ok: outcome.status.ok(),
        status: outcome.status.as_str(),
        query: query.to_string(),
        source: outcome.source.map(QuerySource::as_str),
        provider_command: outcome.provider_command.clone(),
        result_count: populated.then_some(outcome.insights.len()),
        results: populated.then(|| {
            outcome
                .insights
                .iter()
                .map(|h| InsightJson {
                    memory_id: h.memory_id.clone(),
                    relevance: h.relevance,
                    content: h.content.clone(),
                })
                .collect()
        }),
        skipped_lines: (outcome.skipped_lines > 0).then_some(outcome.skipped_lines),
        message: outcome.message.clone(),
        next: outcome.next.clone(),
    }
}

pub fn execute_query(query: String, json: bool) -> Result<()> {
    let layout = crate::state::layout::get_layout_or_cwd_if_not_git()?;
    let outcome = query_status(&query, &layout, &QueryTransport::production());
    finish(json, &query, outcome)
}

fn finish(json: bool, query: &str, outcome: QueryOutcome) -> Result<()> {
    if json {
        crate::output::json::emit(&envelope(query, &outcome))?;
        if !outcome.status.ok() {
            crate::output::requested_exit::request_exit(1);
            return Err(miette!("{}", outcome.status.as_str()));
        }
        return Ok(());
    }

    match outcome.status {
        QueryStatus::Disabled => {
            eprintln!("{}", BRIDGE_ENABLE_HINT);
            println!("Status: disabled");
            println!("Next: set bridge.enabled = true or LEDGERFUL_BRIDGE=1");
            Ok(())
        }
        QueryStatus::Empty => {
            println!("Status: empty");
            if let Some(next) = &outcome.next {
                println!("{next}");
            }
            Ok(())
        }
        QueryStatus::Populated => {
            println!(
                "Recalled {} memories from external provider:",
                outcome.insights.len()
            );
            for hit in &outcome.insights {
                println!("- [{:.2}] {}", hit.relevance, hit.content);
            }
            Ok(())
        }
        QueryStatus::Unavailable | QueryStatus::Failed => {
            println!("Status: {}", outcome.status.as_str());
            if let Some(msg) = &outcome.message {
                println!("{msg}");
            }
            if let Some(next) = &outcome.next
                && outcome.message.as_deref() != Some(next.as_str())
            {
                println!("{next}");
            }
            crate::output::requested_exit::request_exit(1);
            Err(miette!("{}", outcome.status.as_str()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::client::client_cli::CliRun;
    use crate::state::layout::Layout;
    use camino::Utf8Path;
    use tempfile::tempdir;

    fn layout() -> (tempfile::TempDir, Layout) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        (tmp, layout)
    }

    fn insight(id: &str, relevance: f64, content: &str) -> BridgeRecord {
        BridgeRecord::new(
            BridgeDirection::Inbound,
            "p".to_string(),
            "insight",
            BridgePayload::Insight {
                memory_id: id.to_string(),
                relevance,
                content: content.to_string(),
            },
        )
    }

    fn hotspot() -> BridgeRecord {
        BridgeRecord::new(
            BridgeDirection::Inbound,
            "p".to_string(),
            "hotspot",
            BridgePayload::Hotspot {
                path: "src/lib.rs".to_string(),
                score: 0.5,
                reason: "x".to_string(),
                temporal_coupling: 0.0,
                failure_risk_probability: 0.0,
            },
        )
    }

    fn transport_cli(run: CliRun) -> QueryTransport {
        QueryTransport {
            try_ipc: false,
            cli_timeout: Duration::from_millis(50),
            ipc: None,
            cli: Some(run),
        }
    }

    #[test]
    fn disabled_status() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::SpawnErr {
                command: "ai-brains".into(),
            }),
            false,
        );
        assert_eq!(out.status, QueryStatus::Disabled);
        assert!(out.status.ok());
        assert!(out.source.is_none());
        assert!(out.provider_command.is_none());
        assert!(
            out.next
                .as_deref()
                .is_some_and(|n| n.contains("LEDGERFUL_BRIDGE"))
        );
    }

    #[test]
    fn unavailable_spawn_err_emits_cli_source() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::SpawnErr {
                command: "ai-brains".into(),
            }),
            true,
        );
        assert_eq!(out.status, QueryStatus::Unavailable);
        assert!(!out.status.ok());
        assert_eq!(out.source, Some(QuerySource::Cli));
        assert_eq!(out.provider_command.as_deref(), Some("ai-brains"));
        assert_eq!(out.next.as_deref(), Some(NEXT_UNAVAILABLE));
    }

    #[test]
    fn allowlist_deny_is_failed_cli() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::AllowlistDenied {
                command: "powershell".into(),
            }),
            true,
        );
        assert_eq!(out.status, QueryStatus::Failed);
        assert_eq!(out.source, Some(QuerySource::Cli));
        assert_eq!(out.provider_command.as_deref(), Some("powershell"));
        assert_eq!(out.next.as_deref(), Some(NEXT_ALLOWLIST));
    }

    #[test]
    fn timeout_is_failed() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::Timeout {
                command: "ai-brains".into(),
            }),
            true,
        );
        assert_eq!(out.status, QueryStatus::Failed);
        assert!(
            out.message
                .as_deref()
                .is_some_and(|m| m.contains("timed out"))
        );
    }

    #[test]
    fn nonzero_is_failed() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::NonZero {
                command: "ai-brains".into(),
                message: "boom\nsecret".into(),
            }),
            true,
        );
        assert_eq!(out.status, QueryStatus::Failed);
        assert_eq!(out.message.as_deref(), Some("boom"));
    }

    #[test]
    fn malformed_stdout_is_failed() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::Stdout {
                command: "ai-brains".into(),
                stdout: "not-json\n".into(),
            }),
            true,
        );
        assert_eq!(out.status, QueryStatus::Failed);
        assert_eq!(out.skipped_lines, 1);
        assert!(out.insights.is_empty());
    }

    #[test]
    fn empty_stdout_is_empty() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::Stdout {
                command: "ai-brains".into(),
                stdout: String::new(),
            }),
            true,
        );
        assert_eq!(out.status, QueryStatus::Empty);
        assert!(out.status.ok());
        assert_eq!(out.skipped_lines, 0);
        assert_eq!(out.source, Some(QuerySource::Cli));
    }

    #[test]
    fn valid_non_insight_is_empty_with_skipped() {
        let rec = crate::bridge::model::serialize_record(&hotspot()).unwrap();
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::Stdout {
                command: "ai-brains".into(),
                stdout: format!("{rec}\n"),
            }),
            true,
        );
        assert_eq!(out.status, QueryStatus::Empty);
        assert_eq!(out.skipped_lines, 1);
    }

    #[test]
    fn populated_sorts_and_skips_nan() {
        let (_tmp, layout) = layout();
        let transport = QueryTransport {
            try_ipc: true,
            cli_timeout: Duration::from_millis(50),
            ipc: Some(IpcOverride::Records(vec![
                insight("b", 0.1, "second"),
                insight("z", f64::NAN, "skip"),
                insight("a", 0.9, "first"),
            ])),
            cli: Some(CliRun::SpawnErr {
                command: "ai-brains".into(),
            }),
        };
        let out = query_status_inner("q", &layout, &transport, true);
        assert_eq!(out.status, QueryStatus::Populated);
        assert_eq!(out.insights.len(), 2);
        assert_eq!(out.insights[0].memory_id, "a");
        assert_eq!(out.insights[1].memory_id, "b");
        assert_eq!(out.skipped_lines, 1);
        let env = envelope("q", &out);
        assert_eq!(env.result_count, Some(2));
        assert!(env.results.is_some());
    }

    #[test]
    fn ipc_records_short_circuit_cli() {
        let (_tmp, layout) = layout();
        let transport = QueryTransport {
            try_ipc: true,
            cli_timeout: Duration::from_millis(50),
            ipc: Some(IpcOverride::Records(vec![insight("m1", 0.5, "from-ipc")])),
            cli: Some(CliRun::SpawnErr {
                command: "ai-brains".into(),
            }),
        };
        let out = query_status_inner("q", &layout, &transport, true);
        assert_eq!(out.status, QueryStatus::Populated);
        assert_eq!(out.source, Some(QuerySource::Ipc));
        assert!(out.provider_command.is_none());
        assert_eq!(out.insights[0].content, "from-ipc");
    }

    #[test]
    fn empty_envelope_omits_results() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "q",
            &layout,
            &transport_cli(CliRun::Stdout {
                command: "ai-brains".into(),
                stdout: String::new(),
            }),
            true,
        );
        let env = envelope("q", &out);
        let v = serde_json::to_value(&env).unwrap();
        assert!(v.get("results").is_none());
        assert!(v.get("resultCount").is_none());
        assert_eq!(v["status"], "empty");
        assert_eq!(v["kind"], "bridgeQuery");
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn disabled_envelope_omits_source() {
        let (_tmp, layout) = layout();
        let out = query_status_inner(
            "hello",
            &layout,
            &transport_cli(CliRun::SpawnErr {
                command: "x".into(),
            }),
            false,
        );
        let env = envelope("hello", &out);
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["status"], "disabled");
        assert!(v.get("source").is_none());
        assert!(v.get("providerCommand").is_none());
        assert_eq!(v["query"], "hello");
        assert_eq!(v["ok"], true);
    }
}
