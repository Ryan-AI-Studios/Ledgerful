//! Outer response and tracing policy for the protected change evidence route.

use crate::change_evidence::{
    buffer::MemoryBudget,
    http_query::{ChangeEvidenceQuery, parse_change_evidence_query},
    http_verification::VerificationSection,
    http_worker::{DetachedResponse, WorkerContext, WorkerError},
    pack_store::{
        Observation,
        passive_db::{PassiveReadError, PassiveSelection, ScopedExactPair, prove_live_checkout},
    },
};
use crate::commands::web::{error::WebError, state::AppState};
use crate::state::layout::{Layout, StateDirOrigin};
use crate::state::storage::change_evidence::{
    http::{BaselineCandidate, BaselineVerdict, SelectionMetadata},
    writes::prepared::{
        ComparableComparison, GitPartialReason, IncomparableComparison, PreparationRefusal,
        PreparedComparison, PreparedFile, PreparedGit, PreparedIndexMarker, PreparedRecheck,
        PreparedRecord,
    },
};
use axum::extract::{Extension, OriginalUri, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::Instant;

const PATH: &str = "/api/change-evidence";
const REQUEST_BUDGET: Duration = Duration::from_millis(3500);
// One retained scope observation brackets both SQL snapshots. Budget the
// native anchor reacquisitions and both discovery passes on that same meter.
const RETAINED_PROOF_WORK_UNITS: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(super) struct EvidenceDeadline(pub(super) Instant);

pub(super) fn is_evidence_request(request: &Request) -> bool {
    request
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| request.uri(), |uri| &uri.0)
        .path()
        == PATH
}

/// Run outside Host, peer, CORS, rate, and token policy so denial responses
/// receive the same cache directive. Those policies keep their existing order.
pub(super) async fn response_boundary(mut request: Request, next: Next) -> Response {
    let evidence = is_evidence_request(&request);
    if evidence {
        request
            .extensions_mut()
            .insert(EvidenceDeadline(Instant::now() + REQUEST_BUDGET));
    }

    let mut response = next.run(request).await;
    if evidence {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[derive(Clone, Copy)]
enum Problem {
    InvalidQuery,
    PairNotFound,
    Disabled,
    Uninitialized,
    SchemaIncompatible,
    ScopeUnavailable,
    StorageBusy,
    WorkerSaturated,
    RequestBudget,
    Internal,
}

impl Problem {
    fn response(self) -> Response {
        let (status, body) = match self {
            Self::InvalidQuery => (
                StatusCode::BAD_REQUEST,
                r#"{"type":"about:blank","title":"Bad Request","status":400,"reasonCode":"invalid-query"}"#,
            ),
            Self::PairNotFound => (
                StatusCode::NOT_FOUND,
                r#"{"type":"about:blank","title":"Not Found","status":404,"reasonCode":"pair-not-found"}"#,
            ),
            Self::Disabled => (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"type":"about:blank","title":"Service Unavailable","status":503,"reasonCode":"disabled"}"#,
            ),
            Self::Uninitialized => (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"type":"about:blank","title":"Service Unavailable","status":503,"reasonCode":"uninitialized"}"#,
            ),
            Self::SchemaIncompatible => (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"type":"about:blank","title":"Service Unavailable","status":503,"reasonCode":"schema-incompatible"}"#,
            ),
            Self::ScopeUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"type":"about:blank","title":"Service Unavailable","status":503,"reasonCode":"scope-unavailable"}"#,
            ),
            Self::StorageBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"type":"about:blank","title":"Service Unavailable","status":503,"reasonCode":"storage-busy"}"#,
            ),
            Self::WorkerSaturated => (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"type":"about:blank","title":"Service Unavailable","status":503,"reasonCode":"worker-saturated"}"#,
            ),
            Self::RequestBudget => (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"type":"about:blank","title":"Service Unavailable","status":503,"reasonCode":"request-budget"}"#,
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"type":"about:blank","title":"Internal Server Error","status":500,"reasonCode":"evidence-internal"}"#,
            ),
        };
        (
            status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            body,
        )
            .into_response()
    }
}

impl From<WorkerError> for Problem {
    fn from(error: WorkerError) -> Self {
        match error {
            WorkerError::Saturated => Self::WorkerSaturated,
            WorkerError::Deadline | WorkerError::Cancelled | WorkerError::TooLarge => {
                Self::RequestBudget
            }
            WorkerError::Join | WorkerError::Registration | WorkerError::Storage => Self::Internal,
            WorkerError::PassiveRead(error) => match error {
                PassiveReadError::PairNotFound => Self::PairNotFound,
                PassiveReadError::Uninitialized => Self::Uninitialized,
                PassiveReadError::SchemaIncompatible => Self::SchemaIncompatible,
                PassiveReadError::ScopeUnavailable => Self::ScopeUnavailable,
                PassiveReadError::StorageBusy => Self::StorageBusy,
                PassiveReadError::RequestBudget => Self::RequestBudget,
                PassiveReadError::Internal => Self::Internal,
            },
        }
    }
}

#[derive(utoipa::ToSchema)]
#[allow(dead_code, reason = "Schema of the fixed problem responses")]
#[serde(rename_all = "camelCase")]
struct EvidenceProblemSchema {
    #[serde(rename = "type")]
    kind: String,
    title: String,
    status: u16,
    reason_code: String,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct EvidenceResponse {
    schema_version: u8,
    repo_scope: String,
    display_name: String,
    head: String,
    base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_digest: Option<String>,
    git: GitSection,
    baseline_candidates: BaselineCandidates,
    comparison: Comparison,
    verification: VerificationSection,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(
    tag = "status",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
enum GitSection {
    Ready {
        files: Vec<PreparedFile>,
        total: usize,
        omitted: usize,
    },
    Partial {
        files: Vec<PreparedFile>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        omitted: Option<usize>,
        #[serde(rename = "reasonCode", skip_serializing_if = "Option::is_none")]
        reason_code: Option<GitPartialReason>,
    },
    Unavailable {
        #[serde(rename = "reasonCode")]
        reason_code: GitReason,
    },
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
enum GitReason {
    NotPrepared,
    GitFilesUnavailable,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(
    tag = "status",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
enum BaselineCandidates {
    Ready {
        items: Vec<BaselineCandidate>,
        complete: bool,
    },
    Unavailable {
        #[serde(rename = "reasonCode")]
        reason_code: CandidateReason,
    },
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
enum CandidateReason {
    CandidatesUnavailable,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(
    tag = "status",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
enum Comparison {
    SelectionRequired,
    Comparable {
        #[schema(value_type = String, format = DateTime)]
        #[serde(rename = "capturedAt")]
        captured_at: DateTime<Utc>,
        #[schema(value_type = String, format = DateTime)]
        #[serde(rename = "liveCheckedAt")]
        live_checked_at: DateTime<Utc>,
        #[serde(flatten)]
        evidence: Box<ComparableComparison>,
    },
    Incomparable {
        #[serde(flatten)]
        evidence: Box<IncomparableComparison>,
    },
    Unavailable {
        #[serde(rename = "reasonCode")]
        reason_code: ComparisonReason,
    },
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(untagged)]
enum ComparisonReason {
    Baseline(BaselineVerdict),
    Preparation(PreparationRefusal),
}

impl Comparison {
    fn unavailable(reason: BaselineVerdict) -> Self {
        Self::Unavailable {
            reason_code: ComparisonReason::Baseline(reason),
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/change-evidence",
    operation_id = "getChangeEvidence",
    tag = "changes",
    security(("changeEvidenceBearer" = [])),
    params(
        ("repoScope" = String, Query, description = "Opaque serving repository scope UUID"),
        ("head" = String, Query, description = "Full lowercase SHA-1 commit ID"),
        ("base" = String, Query, description = "Full lowercase SHA-1 only-parent commit ID"),
        ("baselineName" = Option<String>, Query, description = "Exact attested baseline name; requires baselineDigest"),
        ("baselineDigest" = Option<String>, Query, description = "Exact blake3 payload digest; requires baselineName"),
    ),
    responses(
        (status = 200, description = "Validated exact-pair evidence", body = EvidenceResponse),
        (status = 400, description = "Invalid evidence query", content_type = "application/problem+json", body = EvidenceProblemSchema),
        (status = 403, description = "Authorization or policy denied", content_type = "application/problem+json", body = crate::commands::web::error::ProblemDetail),
        (status = 404, description = "Exact pair unavailable", content_type = "application/problem+json", body = EvidenceProblemSchema),
        (status = 429, description = "Rate limited", content_type = "application/problem+json", body = crate::commands::web::error::ProblemDetail),
        (status = 500, description = "Inconsistent evidence infrastructure", content_type = "application/problem+json", body = EvidenceProblemSchema),
        (status = 503, description = "Evidence serving unavailable or request budget exhausted", content_type = "application/problem+json", body = EvidenceProblemSchema),
    )
)]
pub(crate) async fn get_change_evidence(
    State(state): State<Arc<AppState>>,
    Extension(EvidenceDeadline(deadline)): Extension<EvidenceDeadline>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if headers
        .get_all(header::ORIGIN)
        .iter()
        .any(|origin| !super::middleware::origin_is_loopback(origin))
    {
        return WebError::Forbidden.into_response();
    }
    let Ok(query) = parse_change_evidence_query(uri.query().unwrap_or_default().as_bytes()) else {
        return Problem::InvalidQuery.response();
    };
    if state.change_evidence_disabled {
        return Problem::Disabled.response();
    }
    let Some(origin) = state.change_evidence_origin else {
        return Problem::ScopeUnavailable.response();
    };
    let layout = state.layout.clone();
    let worker = match state
        .change_evidence_workers
        .try_start(deadline, move |worker| {
            read_response(query, &layout, origin, &worker)
        }) {
        Ok(worker) => worker,
        Err(error) => return Problem::from(error).response(),
    };
    match worker.wait().await {
        Ok(body) => (
            [(header::CONTENT_TYPE, "application/json")],
            body.into_bytes().into_vec(),
        )
            .into_response(),
        Err(error) => Problem::from(error).response(),
    }
}

fn read_response(
    query: ChangeEvidenceQuery,
    layout: &Layout,
    origin: StateDirOrigin,
    worker: &WorkerContext,
) -> Result<DetachedResponse, WorkerError> {
    let check = || {
        if worker.interrupted().load(Ordering::Acquire)
            || std::time::Instant::now() >= worker.deadline()
        {
            return Err(WorkerError::Deadline);
        }
        Ok(())
    };
    let memory = MemoryBudget::new(32 * 1024 * 1024);
    let read_bytes = std::sync::atomic::AtomicUsize::new(64 * 1024 * 1024);
    let mut observation = Observation::open_scoped_passive(
        layout,
        origin,
        worker.deadline(),
        worker.interrupted(),
        &read_bytes,
        RETAINED_PROOF_WORK_UNITS,
        &memory,
    )
    .map_err(|_| WorkerError::PassiveRead(PassiveReadError::ScopeUnavailable))?;
    observation
        .observe_local_git_config()
        .map_err(|_| WorkerError::PassiveRead(PassiveReadError::ScopeUnavailable))?;
    let pair = match observation.observe_scoped_commit_pair(layout, origin, query.head, query.base)
    {
        Ok(pair) => pair,
        Err(PassiveReadError::PairNotFound) => {
            // Preserve store-unavailable precedence for an invalid pair. A valid
            // pair takes its initial scope and evidence from one read snapshot.
            observation
                .read_passive_evidence(layout, origin, worker, None)
                .map_err(WorkerError::PassiveRead)?;
            return Err(WorkerError::PassiveRead(PassiveReadError::PairNotFound));
        }
        Err(error) => return Err(WorkerError::PassiveRead(error)),
    };
    let read = observation
        .read_passive_evidence(
            layout,
            origin,
            worker,
            Some(PassiveSelection {
                scope: query.repo_scope,
                proof: &pair,
                baseline: query
                    .baseline_name
                    .as_deref()
                    .zip(query.baseline_digest.as_deref()),
                earlier: None,
            }),
        )
        .map_err(WorkerError::PassiveRead)?;
    observation
        .confirm_pack_inventory()
        .map_err(|_| WorkerError::PassiveRead(PassiveReadError::ScopeUnavailable))?;
    observation
        .confirm_local_git_config()
        .map_err(|_| WorkerError::PassiveRead(PassiveReadError::ScopeUnavailable))?;
    check()?;
    let history = match read.history {
        Some(Ok(history)) => Some(history),
        Some(Err(PassiveReadError::RequestBudget)) => return Err(WorkerError::Deadline),
        Some(Err(_)) | None => None,
    };
    let candidates = read.candidates.and_then(Result::ok);
    let eligible = candidates
        .as_ref()
        .is_some_and(|page| !page.items.is_empty());
    let complete_empty = candidates
        .as_ref()
        .is_some_and(|page| page.complete && page.items.is_empty());
    let baseline_candidates = match candidates {
        Some(page) => BaselineCandidates::Ready {
            items: page.items,
            complete: page.complete,
        },
        None => BaselineCandidates::Unavailable {
            reason_code: CandidateReason::CandidatesUnavailable,
        },
    };
    let now = Utc::now();
    let mut git = read.pair_git;
    let comparison = match (read.selection, read.prepared) {
        (Some(Ok(SelectionMetadata::Eligible)), Ok(Some(record))) => {
            let proof = ProofRequest {
                query: &query,
                pair: &pair,
                layout,
                origin,
                worker,
                memory: &memory,
                read_bytes: &read_bytes,
            }
            .prove(&mut observation, &record, read.prepared_recheck, now);
            git = Ok(Some(record.evidence.git));
            match (proof, record.evidence.comparison) {
                (Err(reason), _) => Comparison::unavailable(reason),
                (Ok(()), PreparedComparison::Comparable(evidence)) => {
                    let captured_at = record.captured_at.ok_or(WorkerError::Storage)?;
                    Comparison::Comparable {
                        captured_at,
                        live_checked_at: Utc::now(),
                        evidence,
                    }
                }
                (Ok(()), PreparedComparison::Incomparable(evidence)) => {
                    Comparison::Incomparable { evidence }
                }
                (Ok(()), PreparedComparison::Unavailable { reason_code }) => {
                    Comparison::Unavailable {
                        reason_code: ComparisonReason::Preparation(reason_code),
                    }
                }
            }
        }
        (selection, stored) => {
            let prepared_readable = stored.is_ok();
            if let Ok(Some(record)) = stored {
                git = Ok(Some(record.evidence.git));
            }
            match selection {
                Some(Ok(SelectionMetadata::Unavailable(reason))) => Comparison::unavailable(reason),
                Some(Ok(SelectionMetadata::Eligible)) if prepared_readable => {
                    Comparison::unavailable(BaselineVerdict::NotPrepared)
                }
                None if eligible => Comparison::SelectionRequired,
                None if complete_empty => Comparison::unavailable(BaselineVerdict::NotPrepared),
                _ => Comparison::unavailable(BaselineVerdict::LiveProofUnavailable),
            }
        }
    };
    check()?;
    let git = match git {
        Ok(Some(PreparedGit::Ready {
            files,
            total,
            omitted,
        })) => GitSection::Ready {
            files,
            total,
            omitted,
        },
        Ok(Some(PreparedGit::Partial {
            files,
            total,
            omitted,
            reason_code,
        })) => GitSection::Partial {
            files,
            total,
            omitted,
            reason_code,
        },
        Ok(None) => GitSection::Unavailable {
            reason_code: GitReason::NotPrepared,
        },
        _ => GitSection::Unavailable {
            reason_code: GitReason::GitFilesUnavailable,
        },
    };
    let display_name =
        super::handlers::safe_discovery_label(Some(layout.root.file_name().unwrap_or_default()));
    let mut response = EvidenceResponse {
        schema_version: 1,
        repo_scope: read.scope.to_string(),
        display_name,
        head: pair.commits().head.to_string(),
        base: pair.commits().parent.to_string(),
        baseline_name: query.baseline_name,
        baseline_digest: query.baseline_digest,
        git,
        baseline_candidates,
        comparison,
        verification: VerificationSection::from_history(history, now, TimeDelta::days(7)),
    };
    loop {
        match DetachedResponse::serialize(&response) {
            Err(WorkerError::TooLarge) if response.trim_sample() => check()?,
            result => return result,
        }
    }
}

fn current_guard(
    observation: &mut Observation<'_>,
    pair: &ScopedExactPair,
    layout: &Layout,
    origin: StateDirOrigin,
    revision_changed: BaselineVerdict,
) -> Result<(), BaselineVerdict> {
    match observation.confirm_scoped_guard(layout, origin, pair.commits().head) {
        Ok(guard) if &guard == pair.guard() => Ok(()),
        Err("head-mismatch") => Err(revision_changed),
        Err("refs-changed") => Err(BaselineVerdict::HeadMoved),
        _ => Err(BaselineVerdict::LiveProofUnavailable),
    }
}

fn proof_clock(
    captured_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), BaselineVerdict> {
    let captured_at = captured_at.ok_or(BaselineVerdict::ProofTimeInvalid)?;
    let age = now - captured_at;
    if age < TimeDelta::zero() {
        Err(BaselineVerdict::ProofTimeFuture)
    } else if age > TimeDelta::days(7) {
        Err(BaselineVerdict::ProofStale)
    } else {
        Ok(())
    }
}

fn require_index_age(
    indexed_at: &str,
    threshold_days: u64,
    now: DateTime<Utc>,
) -> Result<(), BaselineVerdict> {
    use crate::index::staleness::{IndexFreshnessState, age_with_index_timestamp};
    let indexed_at = DateTime::parse_from_rfc3339(indexed_at)
        .map_err(|_| BaselineVerdict::IndexStale)?
        .with_timezone(&Utc);
    // Population does not change the landed age boundary. Comparable input
    // already carries independently validated coverage.
    match age_with_index_timestamp(true, indexed_at, now, threshold_days) {
        Some((IndexFreshnessState::FreshPopulated, _)) => Ok(()),
        _ => Err(BaselineVerdict::IndexStale),
    }
}

fn recheck_verdict(
    check: Option<Result<PreparedRecheck, PassiveReadError>>,
) -> Result<(), BaselineVerdict> {
    match check {
        Some(Ok(PreparedRecheck::Current)) => Ok(()),
        Some(Ok(PreparedRecheck::IndexChanged)) => Err(BaselineVerdict::IndexStale),
        _ => Err(BaselineVerdict::LiveProofUnavailable),
    }
}

struct ProofRequest<'a> {
    query: &'a ChangeEvidenceQuery,
    pair: &'a ScopedExactPair,
    layout: &'a Layout,
    origin: StateDirOrigin,
    worker: &'a WorkerContext,
    memory: &'a MemoryBudget,
    read_bytes: &'a std::sync::atomic::AtomicUsize,
}

impl ProofRequest<'_> {
    fn prove(
        &self,
        observation: &mut Observation<'_>,
        record: &PreparedRecord,
        first_recheck: Option<Result<PreparedRecheck, PassiveReadError>>,
        now: DateTime<Utc>,
    ) -> Result<(), BaselineVerdict> {
        current_guard(
            observation,
            self.pair,
            self.layout,
            self.origin,
            BaselineVerdict::HistoricalHead,
        )?;
        proof_clock(record.captured_at, now)?;
        recheck_verdict(first_recheck)?;
        let marker = record.evidence.index_marker.as_ref();
        let comparable = matches!(
            &record.evidence.comparison,
            PreparedComparison::Comparable(_)
        );
        prove_live_checkout(
            self.layout,
            self.origin,
            self.worker,
            self.memory,
            self.read_bytes,
            self.pair,
        )?;
        if let Some(marker) = marker {
            self.confirm_policy(observation, marker, comparable.then_some(now))?;
        }
        let repeat = observation
            .read_passive_evidence(
                self.layout,
                self.origin,
                self.worker,
                Some(PassiveSelection {
                    scope: self.query.repo_scope,
                    proof: self.pair,
                    baseline: self
                        .query
                        .baseline_name
                        .as_deref()
                        .zip(self.query.baseline_digest.as_deref()),
                    earlier: Some(record),
                }),
            )
            .map_err(|_| BaselineVerdict::LiveProofUnavailable)?;
        recheck_verdict(repeat.prepared_recheck)?;
        current_guard(
            observation,
            self.pair,
            self.layout,
            self.origin,
            BaselineVerdict::HeadMoved,
        )?;
        if let Some(marker) = marker {
            self.confirm_policy(observation, marker, comparable.then(Utc::now))?;
        }
        Ok(())
    }

    fn confirm_policy(
        &self,
        observation: &mut Observation<'_>,
        marker: &PreparedIndexMarker,
        age_at: Option<DateTime<Utc>>,
    ) -> Result<(), BaselineVerdict> {
        let days = confirm_config(observation, &marker.config_fingerprint)?;
        if let Some(now) = age_at {
            let indexed_at = marker
                .last_indexed_at
                .as_deref()
                .ok_or(BaselineVerdict::IndexStale)?;
            require_index_age(indexed_at, days, now)?;
        }
        confirm_discovery(observation, self.layout, &marker.discovery_fingerprint)
    }
}

fn confirm_discovery(
    observation: &mut Observation<'_>,
    layout: &Layout,
    expected: &str,
) -> Result<(), BaselineVerdict> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        match observation.evidence_discovery_fingerprint(layout) {
            Ok(observed) if observed == expected => Ok(()),
            Ok(_) => Err(BaselineVerdict::ConfigChanged),
            Err(_) => Err(BaselineVerdict::LiveProofUnavailable),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (observation, layout, expected);
        Err(BaselineVerdict::LiveProofUnavailable)
    }
}

fn confirm_config(
    observation: &mut Observation<'_>,
    expected: &str,
) -> Result<u64, BaselineVerdict> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        match observation.evidence_index_config() {
            Ok((observed, days)) if observed == expected => Ok(days),
            Ok(_) => Err(BaselineVerdict::ConfigChanged),
            Err(_) => Err(BaselineVerdict::LiveProofUnavailable),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (observation, expected);
        Err(BaselineVerdict::LiveProofUnavailable)
    }
}

impl EvidenceResponse {
    /// Preserve exact counts and signal when an oversized combined packet
    /// needs shorter deterministic changed-file or changed-edge samples.
    fn trim_sample(&mut self) -> bool {
        if let Comparison::Comparable { evidence, .. } = &mut self.comparison {
            let len = evidence.items.len();
            if len > 0 {
                let kept = len / 2;
                evidence.items.truncate(kept);
                evidence.sample.complete = false;
                evidence.sample.omitted += len - kept;
                return true;
            }
        }
        match &mut self.git {
            GitSection::Ready { files, total, .. } if !files.is_empty() => {
                let mut files = std::mem::take(files);
                files.truncate(files.len() / 2);
                self.git = GitSection::Partial {
                    omitted: Some(*total - files.len()),
                    total: Some(*total),
                    files,
                    reason_code: None,
                };
                true
            }
            GitSection::Partial { files, omitted, .. } if !files.is_empty() => {
                let len = files.len();
                files.truncate(len / 2);
                if let Some(omitted) = omitted {
                    *omitted += len - files.len();
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::middleware;
    use axum::routing::get;
    use tower::ServiceExt;

    const QUERY: &str = "repoScope=4826be00-5419-4a5a-9b51-27c0383dad03&head=0123456789abcdef0123456789abcdef01234567&base=abcdef0123456789abcdef0123456789abcdef01";
    const TOKEN: &str = "evidence-router-test";

    fn app(disabled: bool, restricted: bool) -> Router {
        let mut state = AppState::new(
            Layout::new("."),
            TOKEN.to_owned(),
            None,
            restricted.then(std::collections::HashSet::new),
            None,
        );
        state.change_evidence_disabled = disabled;
        super::super::router::router(Arc::new(state))
    }

    fn request(query: &str, host: &str, auth: bool, origin: Option<&str>) -> Request {
        let mut request = Request::builder()
            .uri(format!("{PATH}?{query}"))
            .header(header::HOST, host);
        if auth {
            request = request.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        request.body(Body::empty()).unwrap()
    }

    async fn problem(response: Response, status: StatusCode, reason: Option<&str>) {
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        assert!(response.headers().get(header::LOCATION).is_none());
        let bytes = axum::body::to_bytes(response.into_body(), 512)
            .await
            .unwrap();
        let message: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(message["status"], status.as_u16());
        if let Some(reason) = reason {
            assert_eq!(message["reasonCode"], reason);
        }
        let raw = std::str::from_utf8(&bytes).unwrap();
        for fragment in [
            "4826be00",
            "0123456789abcdef",
            "private-query",
            "repoScope",
            "verification",
        ] {
            assert!(!raw.contains(fragment), "response echoed {fragment}");
        }
    }

    #[test]
    fn a_prepared_row_can_outlive_its_retained_index_age_threshold() {
        let indexed = "2026-11-21T10:00:00Z";
        let prepared = DateTime::parse_from_rfc3339("2026-11-22T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let boundary = prepared + TimeDelta::days(1);
        assert!(proof_clock(Some(prepared), boundary).is_ok());
        assert!(require_index_age(indexed, 1, boundary - TimeDelta::nanoseconds(1)).is_ok());
        assert!(matches!(
            require_index_age(indexed, 1, boundary),
            Err(BaselineVerdict::IndexStale)
        ));
        assert!(require_index_age(indexed, 2, boundary).is_ok());
    }

    #[test]
    fn proof_age_uses_a_strict_seven_day_cutoff() {
        let now = DateTime::parse_from_rfc3339("2026-11-23T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(proof_clock(Some(now - TimeDelta::days(7)), now).is_ok());
        assert!(matches!(
            proof_clock(
                Some(now - TimeDelta::days(7) - TimeDelta::nanoseconds(1)),
                now
            ),
            Err(BaselineVerdict::ProofStale)
        ));
        assert!(matches!(
            proof_clock(Some(now + TimeDelta::nanoseconds(1)), now),
            Err(BaselineVerdict::ProofTimeFuture)
        ));
        assert!(matches!(
            proof_clock(None, now),
            Err(BaselineVerdict::ProofTimeInvalid)
        ));
    }

    #[tokio::test]
    async fn real_router_keeps_evidence_behind_shared_policies() {
        let router = app(true, false);
        for denied in [
            request(QUERY, "localhost", false, None),
            request(QUERY, "foreign.invalid", true, None),
            request(QUERY, "localhost", true, Some("https://foreign.invalid")),
        ] {
            problem(
                router.clone().oneshot(denied).await.unwrap(),
                StatusCode::FORBIDDEN,
                None,
            )
            .await;
        }
        let peer = app(true, true)
            .oneshot(request(QUERY, "localhost", true, None))
            .await
            .unwrap();
        problem(peer, StatusCode::FORBIDDEN, None).await;
        let malformed = router
            .clone()
            .oneshot(request("private-query=hidden", "localhost", true, None))
            .await
            .unwrap();
        problem(malformed, StatusCode::BAD_REQUEST, Some("invalid-query")).await;
        let disabled = router
            .oneshot(request(
                QUERY,
                "localhost",
                true,
                Some("http://localhost:3210"),
            ))
            .await
            .unwrap();
        problem(disabled, StatusCode::SERVICE_UNAVAILABLE, Some("disabled")).await;
    }

    #[tokio::test]
    async fn real_router_rate_limits_evidence_and_preserves_scope_error_body() {
        let router = app(false, false);
        for _ in 0..60 {
            let response = router
                .clone()
                .oneshot(request(QUERY, "localhost", true, None))
                .await
                .unwrap();
            problem(
                response,
                StatusCode::SERVICE_UNAVAILABLE,
                Some("scope-unavailable"),
            )
            .await;
        }
        let rate_limited = router
            .oneshot(request(QUERY, "localhost", true, None))
            .await
            .unwrap();
        problem(rate_limited, StatusCode::TOO_MANY_REQUESTS, None).await;
    }

    #[tokio::test]
    async fn boundary_marks_even_denied_evidence_and_leaves_legacy_cache_policy() {
        let deny = Router::new()
            .route(
                "/change-evidence",
                get(
                    |Extension(deadline): Extension<EvidenceDeadline>| async move {
                        assert!(deadline.0 > Instant::now());
                        StatusCode::FORBIDDEN
                    },
                ),
            )
            .route("/session", get(|| async { StatusCode::FORBIDDEN }));
        let router = Router::new()
            .nest("/api", deny)
            .layer(middleware::from_fn(response_boundary));

        let request = |uri| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let denied = router
            .clone()
            .oneshot(request("/api/change-evidence?repoScope=private"))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            denied.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );

        let legacy = router
            .oneshot(request("/api/session?legacy=1"))
            .await
            .unwrap();
        assert_eq!(legacy.status(), StatusCode::FORBIDDEN);
        assert!(legacy.headers().get(header::CACHE_CONTROL).is_none());
    }
}
