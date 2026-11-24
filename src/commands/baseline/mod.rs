//! `ledgerful baseline create|list|show|compare` (Track 0601).
//!
//! One payload kind (`fileDependencyEdges`), refuse-overwrite storage, and a
//! three-valued regression verdict. Incomparable results carry no counts and
//! no items; `"none"` is never a boolean pass.

pub(crate) mod delta;
pub(crate) mod extract;
pub(crate) mod identity;

use crate::cli::args::BaselineCommands;
use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::output::json::emit;
use crate::output::requested_exit::request_exit;
use crate::state::layout::Layout;
use crate::state::migrations::get_migrations_count;
use crate::state::storage::StorageManager;
pub(crate) use crate::state::storage::baselines::BaselineError;
use crate::state::storage::baselines::{
    BaselineRow, compute_payload_digest, get_baseline, insert_baseline, list_baselines,
    payload_digest_verified, validate_baseline_name,
};
use chrono::Utc;
pub(crate) use delta::{
    Comparability, DeltaCounts, DeltaItem, EdgeSetDelta, IncomparableReason, Regression,
    compare_edge_sets, regression_for,
};
#[allow(
    unused_imports,
    reason = "Reserved for the separately armed architecture consumer"
)]
pub(crate) use delta::{DeltaClass, EdgeKey};
use delta::{compare_edge_sets_with, gate_reasons};
pub(crate) use extract::extract_file_edge_set;
pub(crate) use identity::{BaselineIdentity, CompletenessState, FileEdgeSet, InputCompleteness};
#[allow(
    unused_imports,
    reason = "Reserved for the separately armed architecture consumer"
)]
pub(crate) use identity::{BaselineScope, EdgeCoverage, FileEdge};
use identity::{StoredPayload, canonical_identity_json, canonical_payload_json};
use miette::{IntoDiagnostic, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

const CREATE_KIND: &str = "analysisBaselineCreate";
const LIST_KIND: &str = "analysisBaselineList";
const SHOW_KIND: &str = "analysisBaselineShow";
const COMPARE_KIND: &str = "analysisBaselineCompare";
const SCHEMA_VERSION: u32 = 1;

/// Entry point from the dispatcher.
pub(crate) fn execute_baseline(command: BaselineCommands) -> Result<()> {
    match command {
        BaselineCommands::Create { name, json } => execute_create(&name, json),
        BaselineCommands::List { json } => execute_list(json),
        BaselineCommands::Show { name, json } => execute_show(&name, json),
        BaselineCommands::Compare {
            name,
            json,
            all,
            fail_on_regression,
        } => execute_compare(&name, json, all, fail_on_regression),
    }
}

fn execute_create(name: &str, json: bool) -> Result<()> {
    validate_baseline_name(name)?;
    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;

    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Err(miette::miette!(
            "no index database exists at {} (run `ledgerful index` first); \
             refusing to create baseline `{name}` (INDEX_EMPTY)",
            db_path.as_str()
        ));
    }

    let probe = Connection::open_with_flags(
        db_path.as_std_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .into_diagnostic()?;
    verify_baseline_db_version(&probe)?;
    drop(probe);

    let storage = StorageManager::init_with_layout(&layout)?;
    let set = extract_file_edge_set(&storage, &config)?;
    let (payload_digest, created_at) = create_baseline(&storage, name, &set)?;

    let envelope = CreateEnvelope::from_set(name, &set, &payload_digest, &created_at);
    if json {
        emit(&envelope)?;
    } else {
        println!(
            "Created baseline {name}: {} edge(s), digest {payload_digest}, completeness COMPLETE",
            set.edges.len()
        );
    }
    Ok(())
}

/// Validate completeness, then insert one row. Returns `(payloadDigest,
/// createdAt)`. No storage is written when the set is not `COMPLETE`.
pub(crate) fn create_baseline(
    storage: &StorageManager,
    name: &str,
    set: &FileEdgeSet,
) -> Result<(String, String)> {
    if !set.identity.input_completeness.is_complete() {
        return Err(miette::miette!(
            "index is not COMPLETE; refusing to create baseline `{name}`: {}",
            incomplete_description(&set.identity.input_completeness, set.coverage.files_indexed)
        ));
    }

    let (identity_json, payload_json) = canonical_set_json(set)?;
    let payload_digest = compute_payload_digest(&identity_json, &payload_json);
    let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    insert_baseline(
        storage,
        &BaselineRow {
            name: name.to_string(),
            baseline_schema: set.identity.baseline_schema,
            payload_kind: set.identity.payload_kind.clone(),
            identity_json,
            payload_json,
            payload_digest: payload_digest.clone(),
            created_at: created_at.clone(),
        },
    )?;

    Ok((payload_digest, created_at))
}

pub(crate) fn canonical_set_json(set: &FileEdgeSet) -> Result<(String, String)> {
    let payload = StoredPayload {
        coverage: set.coverage.clone(),
        edges: set.edges.clone(),
    };
    Ok((
        canonical_identity_json(&set.identity)?,
        canonical_payload_json(&payload)?,
    ))
}

fn execute_list(json: bool) -> Result<()> {
    let layout = get_layout()?;
    let storage = open_baseline_read_only(&layout)?;
    let rows = list_baselines(&storage)?;
    let entries = rows
        .iter()
        .map(ListEntry::from_row)
        .collect::<Result<Vec<_>>>()?;
    let envelope = ListEnvelope {
        schema_version: SCHEMA_VERSION,
        kind: LIST_KIND,
        baselines: entries,
    };
    if json {
        emit(&envelope)?;
    } else if envelope.baselines.is_empty() {
        println!("No analysis baselines.");
    } else {
        for entry in &envelope.baselines {
            println!(
                "{}  {}  {} edge(s)  {}",
                entry.name, entry.created_at, entry.edge_count, entry.payload_digest
            );
        }
    }
    Ok(())
}

fn execute_show(name: &str, json: bool) -> Result<()> {
    let layout = get_layout()?;
    let storage = open_baseline_read_only(&layout)?;
    let raw = get_baseline(&storage, name)?;
    let envelope = show_envelope_for(&raw)?;
    if json {
        emit(&envelope)?;
    } else {
        println!(
            "Baseline {}: {} edge(s), digest {} (verified: {}), created {}, completeness {}",
            envelope.name,
            envelope.edges.len(),
            envelope.payload_digest,
            envelope.digest_verified,
            raw.created_at,
            completeness_token(envelope.identity.input_completeness.state)
        );
        println!(
            "  coverage: filesIndexed {}, filesParseFailed {}, edgesResolvedCrossFile {}, edgesResolvedIntraFile {}, edgesAmbiguous {}, edgesUnresolved {}, edgesCapped {}",
            envelope.coverage.files_indexed,
            envelope.coverage.files_parse_failed,
            envelope.coverage.edges_resolved_cross_file,
            envelope.coverage.edges_resolved_intra_file,
            envelope.coverage.edges_ambiguous,
            envelope.coverage.edges_unresolved,
            envelope.coverage.edges_capped
        );
    }
    Ok(())
}

fn completeness_token(state: CompletenessState) -> &'static str {
    match state {
        CompletenessState::Complete => "COMPLETE",
        CompletenessState::Incomplete => "INCOMPLETE",
    }
}

fn verify_baseline_db_version(conn: &Connection) -> Result<()> {
    let found: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .into_diagnostic()?;
    let supported = i64::try_from(get_migrations_count()).into_diagnostic()?;
    if found > supported {
        return Err(BaselineError::SchemaTooNew { found, supported }.into());
    }
    Ok(())
}

fn open_baseline_read_only(layout: &Layout) -> Result<StorageManager> {
    let storage = StorageManager::open_read_only_sqlite_only(layout)?;
    verify_baseline_db_version(storage.get_connection())?;
    Ok(storage)
}

fn incomplete_description(completeness: &InputCompleteness, files_indexed: u64) -> String {
    let reasons = completeness.reasons.join(", ");
    if files_indexed > 0
        && completeness
            .reasons
            .iter()
            .any(|reason| reason == "INDEX_EMPTY" || reason == "INDEX_INDETERMINATE")
    {
        format!("{reasons} (missing or invalid index evidence)")
    } else {
        reasons
    }
}

fn show_envelope_for(raw: &BaselineRow) -> Result<ShowEnvelope> {
    let identity = decode_required(raw, "identity", &raw.identity_json)?;
    let payload: StoredPayload = decode_required(raw, "payload", &raw.payload_json)?;
    Ok(ShowEnvelope {
        schema_version: SCHEMA_VERSION,
        kind: SHOW_KIND,
        name: raw.name.clone(),
        identity,
        coverage: payload.coverage,
        payload_digest: raw.payload_digest.clone(),
        digest_verified: payload_digest_verified(raw),
        edges: payload.edges,
    })
}

fn execute_compare(name: &str, json: bool, all: bool, fail_on_regression: bool) -> Result<()> {
    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;
    let storage = open_baseline_read_only(&layout)?;
    let loaded = load_baseline_checked(&storage, name)?;
    let current = extract_file_edge_set(&storage, &config)?;
    let delta = compare_loaded(&loaded, &current, all);

    let envelope = CompareEnvelope::from_delta(&loaded, &current, &delta);
    if json {
        emit(&envelope)?;
    } else {
        println!(
            "Baseline {}: digest {}, status {}, regression {}",
            envelope.baseline.name,
            envelope.baseline.payload_digest,
            envelope.status,
            envelope.regression.as_str()
        );
        if let Some(counts) = &envelope.counts {
            println!(
                "  added {}, removed {}, worsened {}, improved {}, unchanged {}",
                counts.added, counts.removed, counts.worsened, counts.improved, counts.unchanged
            );
        }
        if !envelope.incomparable_reasons.is_empty() {
            println!(
                "  incomparable: {}",
                envelope.incomparable_reasons.join(", ")
            );
            if !current.identity.input_completeness.is_complete() {
                println!(
                    "  live index: {}",
                    incomplete_description(
                        &current.identity.input_completeness,
                        current.coverage.files_indexed
                    )
                );
            }
        }
    }

    if fail_on_regression && envelope.regression != Regression::None {
        request_exit(1);
        return Err(miette::miette!(
            "baseline `{name}` regression is {} (--fail-on-regression)",
            envelope.regression.as_str()
        ));
    }
    Ok(())
}

/// Loaded baseline row with an independently parsed identity and payload.
#[derive(Debug, Clone)]
pub(crate) struct LoadedBaseline {
    pub name: String,
    pub payload_digest: String,
    pub identity: Option<BaselineIdentity>,
    pub set: Option<FileEdgeSet>,
    pub pre_gate: Vec<IncomparableReason>,
}

fn decode_required<T: serde::de::DeserializeOwned>(
    row: &BaselineRow,
    field: &str,
    encoded: &str,
) -> Result<T> {
    serde_json::from_str(encoded).map_err(|error| {
        BaselineError::Corrupt {
            name: row.name.clone(),
            message: format!("{field}: {error}"),
        }
        .into()
    })
}

fn supports_row(row: &BaselineRow, identity: &BaselineIdentity) -> bool {
    row.baseline_schema == identity::BASELINE_SCHEMA
        && row.payload_kind == identity::PAYLOAD_KIND
        && row.baseline_schema == identity.baseline_schema
        && row.payload_kind == identity.payload_kind
}

/// Read a verified baseline for the architecture consumer.
#[allow(
    dead_code,
    reason = "The separately armed architecture consumer depends on this seam"
)]
pub(crate) fn load_baseline(storage: &StorageManager, name: &str) -> Result<FileEdgeSet> {
    verify_baseline_db_version(storage.get_connection())?;
    let row = get_baseline(storage, name)?;
    decode_verified_baseline_row(&row)
}

/// Reuse the strict 0601 row decoder on a caller's schema-qualified snapshot.
pub(crate) fn decode_verified_baseline_row(row: &BaselineRow) -> Result<FileEdgeSet> {
    if !payload_digest_verified(row) {
        return Err(BaselineError::DigestMismatch {
            name: row.name.clone(),
        }
        .into());
    }
    let identity: BaselineIdentity = decode_required(row, "identity", &row.identity_json)?;
    if !supports_row(row, &identity) {
        return Err(BaselineError::RowSchemaMismatch {
            name: row.name.clone(),
        }
        .into());
    }
    let payload: StoredPayload = decode_required(row, "payload", &row.payload_json)?;
    Ok(FileEdgeSet {
        identity,
        coverage: payload.coverage,
        edges: payload.edges,
    })
}

/// Parse a stored row. Identity and payload parse independently so a
/// parseable identity is still emitted when the payload is not usable.
pub(crate) fn load_baseline_checked(
    storage: &StorageManager,
    name: &str,
) -> Result<LoadedBaseline> {
    let row = get_baseline(storage, name)?;
    let mut pre_gate = Vec::new();

    let identity: Option<BaselineIdentity> = match serde_json::from_str(&row.identity_json) {
        Ok(identity) => Some(identity),
        Err(_) => {
            pre_gate.push(IncomparableReason::SchemaMismatch);
            None
        }
    };
    let payload: Option<StoredPayload> = match serde_json::from_str(&row.payload_json) {
        Ok(payload) => Some(payload),
        Err(_) => {
            pre_gate.push(IncomparableReason::SchemaMismatch);
            None
        }
    };
    if !payload_digest_verified(&row) {
        pre_gate.push(IncomparableReason::BaselineDigestMismatch);
    }

    let set = match (identity.clone(), payload) {
        (Some(identity), Some(payload)) => Some(FileEdgeSet {
            identity,
            coverage: payload.coverage,
            edges: payload.edges,
        }),
        _ => None,
    };

    let set = set.filter(|s| {
        let ok = supports_row(&row, &s.identity);
        if !ok {
            pre_gate.push(IncomparableReason::SchemaMismatch);
        }
        ok
    });

    pre_gate.sort();
    pre_gate.dedup();

    Ok(LoadedBaseline {
        name: row.name,
        payload_digest: row.payload_digest,
        identity,
        set,
        pre_gate,
    })
}

/// Merge the stored side's pre-gate (digest / unparseable identity) with the
/// pure comparison so no counts or items leak out of an unusable baseline.
pub(crate) fn compare_loaded(
    loaded: &LoadedBaseline,
    current: &FileEdgeSet,
    include_unchanged: bool,
) -> EdgeSetDelta {
    if let Some(set) = &loaded.set
        && loaded.pre_gate.is_empty()
    {
        return if include_unchanged {
            compare_edge_sets_with(set, current, true)
        } else {
            compare_edge_sets(set, current)
        };
    }

    let mut reasons = loaded.pre_gate.clone();
    reasons.extend(gate_reasons(loaded.identity.as_ref(), &current.identity));
    reasons.sort();
    reasons.dedup();
    let notes = loaded
        .identity
        .as_ref()
        .filter(|identity| identity.ledgerful_version != current.identity.ledgerful_version)
        .map(|_| vec!["analyzerBuildDiffers".to_string()])
        .unwrap_or_default();

    EdgeSetDelta {
        status: Comparability::Incomparable(reasons),
        regression: Regression::Unknown,
        regression_reasons: Vec::new(),
        notes,
        counts: None,
        items: None,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateEnvelope {
    schema_version: u32,
    kind: &'static str,
    name: String,
    payload_kind: String,
    payload_digest: String,
    identity: BaselineIdentity,
    coverage: identity::EdgeCoverage,
    edge_count: usize,
    created_at: String,
}

impl CreateEnvelope {
    fn from_set(name: &str, set: &FileEdgeSet, digest: &str, created_at: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind: CREATE_KIND,
            name: name.to_string(),
            payload_kind: set.identity.payload_kind.clone(),
            payload_digest: digest.to_string(),
            identity: set.identity.clone(),
            coverage: set.coverage.clone(),
            edge_count: set.edges.len(),
            created_at: created_at.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListEnvelope {
    schema_version: u32,
    kind: &'static str,
    baselines: Vec<ListEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListEntry {
    name: String,
    created_at: String,
    payload_kind: String,
    baseline_schema: u32,
    payload_digest: String,
    edge_count: usize,
    input_completeness: ListInputCompleteness,
}

/// List rows expose only the completeness state (spec 3.5).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListInputCompleteness {
    state: CompletenessState,
}

impl ListEntry {
    fn from_row(row: &BaselineRow) -> Result<Self> {
        let identity: BaselineIdentity = decode_required(row, "identity", &row.identity_json)?;
        let payload: StoredPayload = decode_required(row, "payload", &row.payload_json)?;
        Ok(Self {
            name: row.name.clone(),
            created_at: row.created_at.clone(),
            payload_kind: row.payload_kind.clone(),
            baseline_schema: row.baseline_schema,
            payload_digest: row.payload_digest.clone(),
            edge_count: payload.edges.len(),
            input_completeness: ListInputCompleteness {
                state: identity.input_completeness.state,
            },
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShowEnvelope {
    schema_version: u32,
    kind: &'static str,
    name: String,
    identity: BaselineIdentity,
    coverage: identity::EdgeCoverage,
    payload_digest: String,
    digest_verified: bool,
    edges: Vec<identity::FileEdge>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineRef {
    name: String,
    payload_digest: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompareEnvelope {
    schema_version: u32,
    kind: &'static str,
    baseline: BaselineRef,
    status: &'static str,
    incomparable_reasons: Vec<String>,
    baseline_identity: Option<BaselineIdentity>,
    current_identity: BaselineIdentity,
    current_completeness: InputCompleteness,
    regression: Regression,
    regression_reasons: Vec<String>,
    notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<DeltaCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<Vec<DeltaItem>>,
}

impl CompareEnvelope {
    fn from_delta(loaded: &LoadedBaseline, current: &FileEdgeSet, delta: &EdgeSetDelta) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind: COMPARE_KIND,
            baseline: BaselineRef {
                name: loaded.name.clone(),
                payload_digest: loaded.payload_digest.clone(),
            },
            status: delta.status.as_str(),
            incomparable_reasons: delta.status.reasons(),
            baseline_identity: loaded.identity.clone(),
            current_identity: current.identity.clone(),
            current_completeness: current.identity.input_completeness.clone(),
            regression: delta.regression,
            regression_reasons: delta.regression_reasons.clone(),
            notes: delta.notes.clone(),
            counts: delta.counts.clone(),
            items: delta.items.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::baseline::identity::{
        BASELINE_SCHEMA, BaselineScope, EdgeCoverage, FileEdge, PAYLOAD_KIND,
    };
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn identity(complete: bool) -> BaselineIdentity {
        BaselineIdentity {
            baseline_schema: BASELINE_SCHEMA,
            payload_kind: PAYLOAD_KIND.to_string(),
            scope: BaselineScope::repo(),
            parser_version: "1".to_string(),
            edge_sources: vec!["native".to_string()],
            ledgerful_version: "0.2.18".to_string(),
            index_head_hash: Some("head".to_string()),
            input_digest: "blake3:same".to_string(),
            input_completeness: if complete {
                InputCompleteness::complete()
            } else {
                InputCompleteness::incomplete(vec!["CONTENT_DRIFT".to_string()])
            },
        }
    }

    fn edge_set(edges: &[(&str, &str, u32)], complete: bool) -> FileEdgeSet {
        FileEdgeSet {
            identity: identity(complete),
            coverage: EdgeCoverage::default(),
            edges: edges
                .iter()
                .map(|(s, d, w)| FileEdge {
                    src_path: (*s).to_string(),
                    dst_path: (*d).to_string(),
                    weight: *w,
                })
                .collect(),
        }
    }

    fn store(storage: &StorageManager, name: &str, set: &FileEdgeSet) -> String {
        let payload = StoredPayload {
            coverage: set.coverage.clone(),
            edges: set.edges.clone(),
        };
        let identity_json = canonical_identity_json(&set.identity).unwrap();
        let payload_json = canonical_payload_json(&payload).unwrap();
        let digest = compute_payload_digest(&identity_json, &payload_json);
        insert_baseline(
            storage,
            &BaselineRow {
                name: name.to_string(),
                baseline_schema: BASELINE_SCHEMA,
                payload_kind: PAYLOAD_KIND.to_string(),
                identity_json,
                payload_json,
                payload_digest: digest.clone(),
                created_at: "2026-11-21T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        digest
    }

    fn json_value<T: Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    #[test]
    fn baseline_envelope_create_shape() {
        let set = edge_set(&[("a", "b", 1)], true);
        let value = json_value(&CreateEnvelope::from_set("base1", &set, "blake3:x", "t"));
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys[0], "schemaVersion");
        assert_eq!(keys[1], "kind");
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["kind"], "analysisBaselineCreate");
        assert_eq!(value["edgeCount"], 1);
        assert_eq!(value["payloadKind"], "fileDependencyEdges");
        assert_eq!(value["identity"]["inputCompleteness"]["state"], "COMPLETE");
    }

    #[test]
    fn baseline_envelope_list_shape() {
        let set = edge_set(&[("a", "b", 1)], true);
        let storage = in_memory_storage();
        store(&storage, "base1", &set);
        let rows = list_baselines(&storage).unwrap();
        let entries = rows
            .iter()
            .map(ListEntry::from_row)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let value = json_value(&ListEnvelope {
            schema_version: SCHEMA_VERSION,
            kind: LIST_KIND,
            baselines: entries,
        });
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["schemaVersion", "kind", "baselines"]);
        assert_eq!(value["baselines"][0]["edgeCount"], 1);
        assert_eq!(
            value["baselines"][0]["inputCompleteness"]["state"],
            "COMPLETE"
        );
    }

    #[test]
    fn baseline_envelope_show_shape_and_digest_verified() {
        let storage = in_memory_storage();
        let set = edge_set(&[("a", "b", 1)], true);
        store(&storage, "base1", &set);
        let loaded = load_baseline_checked(&storage, "base1").unwrap();
        assert!(loaded.pre_gate.is_empty());
        let raw = get_baseline(&storage, "base1").unwrap();
        assert!(payload_digest_verified(&raw));
        let envelope = ShowEnvelope {
            schema_version: SCHEMA_VERSION,
            kind: SHOW_KIND,
            name: loaded.name.clone(),
            identity: loaded.set.clone().unwrap().identity,
            coverage: EdgeCoverage::default(),
            payload_digest: loaded.payload_digest,
            digest_verified: true,
            edges: loaded.set.unwrap().edges,
        };
        let value = json_value(&envelope);
        assert_eq!(value["kind"], "analysisBaselineShow");
        assert_eq!(value["digestVerified"], true);
        assert_eq!(value["edges"][0]["srcPath"], "a");
    }

    #[test]
    fn baseline_envelope_compare_comparable_shape() {
        let storage = in_memory_storage();
        let baseline = edge_set(&[("a", "b", 1)], true);
        store(&storage, "base1", &baseline);
        let loaded = load_baseline_checked(&storage, "base1").unwrap();
        let current = edge_set(&[("a", "b", 2)], true);
        let delta = compare_loaded(&loaded, &current, false);
        let value = json_value(&CompareEnvelope::from_delta(&loaded, &current, &delta));
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys[0], "schemaVersion");
        assert_eq!(keys[1], "kind");
        assert_eq!(value["status"], "comparable");
        assert_eq!(value["regression"], "present");
        assert_eq!(value["counts"]["worsened"], 1);
        assert_eq!(value["items"][0]["class"], "worsened");
        assert_eq!(value["items"][0]["baselineWeight"], 1);
        assert_eq!(value["items"][0]["currentWeight"], 2);
        assert_eq!(value["baselineIdentity"]["baselineSchema"], 1);
        assert_eq!(value["currentCompleteness"]["state"], "COMPLETE");
    }

    #[test]
    fn baseline_envelope_compare_incomparable_omits_counts_items() {
        let storage = in_memory_storage();
        let baseline = edge_set(&[("a", "b", 1)], true);
        store(&storage, "base1", &baseline);
        let loaded = load_baseline_checked(&storage, "base1").unwrap();
        let current = edge_set(&[("a", "b", 1)], false);
        let delta = compare_loaded(&loaded, &current, false);
        let value = json_value(&CompareEnvelope::from_delta(&loaded, &current, &delta));
        assert_eq!(value["status"], "incomparable");
        assert_eq!(value["regression"], "unknown");
        assert_eq!(value["incomparableReasons"][0], "CURRENT_INCOMPLETE");
        assert!(value.get("counts").is_none(), "counts must be absent");
        assert!(value.get("items").is_none(), "items must be absent");
        assert!(value["currentCompleteness"]["reasons"][0] == "CONTENT_DRIFT");
    }

    #[test]
    fn baseline_incomparable_schema_mismatch_parseable_identity_emitted() {
        let storage = in_memory_storage();
        let mut baseline = edge_set(&[("a", "b", 1)], true);
        baseline.identity.baseline_schema = 2;
        store(&storage, "base1", &baseline);
        let loaded = load_baseline_checked(&storage, "base1").unwrap();
        assert!(loaded.set.is_none());
        assert_eq!(loaded.identity.as_ref().unwrap().baseline_schema, 2);
        let current = edge_set(&[("a", "b", 1)], true);
        let delta = compare_loaded(&loaded, &current, false);
        let value = json_value(&CompareEnvelope::from_delta(&loaded, &current, &delta));
        assert_eq!(value["baselineIdentity"]["baselineSchema"], 2);
        assert!(value.get("counts").is_none());
    }

    #[test]
    fn baseline_incomparable_unparseable_identity_emits_null() {
        let storage = in_memory_storage();
        let set = edge_set(&[("a", "b", 1)], true);
        let payload = StoredPayload {
            coverage: EdgeCoverage::default(),
            edges: set.edges.clone(),
        };
        let payload_json = canonical_payload_json(&payload).unwrap();
        let identity_json = "{not json".to_string();
        let digest = compute_payload_digest(&identity_json, &payload_json);
        insert_baseline(
            &storage,
            &BaselineRow {
                name: "base1".to_string(),
                baseline_schema: BASELINE_SCHEMA,
                payload_kind: PAYLOAD_KIND.to_string(),
                identity_json,
                payload_json,
                payload_digest: digest,
                created_at: "2026-11-21T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        let loaded = load_baseline_checked(&storage, "base1").unwrap();
        assert!(loaded.identity.is_none());
        let current = edge_set(&[("a", "b", 1)], true);
        let delta = compare_loaded(&loaded, &current, false);
        let value = json_value(&CompareEnvelope::from_delta(&loaded, &current, &delta));
        assert_eq!(value["baselineIdentity"], serde_json::Value::Null);
        assert_eq!(value["incomparableReasons"][0], "SCHEMA_MISMATCH");
        assert!(value["currentIdentity"].is_object());
        assert!(value["currentCompleteness"].is_object());
    }

    #[test]
    fn baseline_envelope_has_no_boolean_verdict_keys() {
        let storage = in_memory_storage();
        let baseline = edge_set(&[("a", "b", 1)], true);
        store(&storage, "base1", &baseline);
        let loaded = load_baseline_checked(&storage, "base1").unwrap();
        let current = edge_set(&[("a", "b", 2)], true);
        let delta = compare_loaded(&loaded, &current, true);
        let value = json_value(&CompareEnvelope::from_delta(&loaded, &current, &delta));

        fn walk(value: &serde_json::Value, bad: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, inner) in map {
                        if matches!(
                            key.as_str(),
                            "regressed" | "ok" | "clean" | "passed" | "noRegression"
                        ) {
                            bad.push(key.clone());
                        }
                        walk(inner, bad);
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|i| walk(i, bad)),
                _ => {}
            }
        }
        let mut bad = Vec::new();
        walk(&value, &mut bad);
        assert!(bad.is_empty(), "forbidden verdict keys: {bad:?}");
        assert_eq!(value["items"][0]["class"], "worsened");
    }

    #[test]
    fn baseline_digest_mismatch_is_incomparable_even_when_identity_parses() {
        let storage = in_memory_storage();
        let baseline = edge_set(&[("a", "b", 1)], true);
        store(&storage, "base1", &baseline);
        storage
            .get_connection()
            .execute_batch("DROP TRIGGER analysis_baselines_no_update;")
            .unwrap();
        storage
            .get_connection()
            .execute(
                "UPDATE analysis_baselines SET payload_json = '{\"coverage\":{},\"edges\":[]}' WHERE name = 'base1'",
                [],
            )
            .unwrap();
        let loaded = load_baseline_checked(&storage, "base1").unwrap();
        assert!(
            loaded
                .pre_gate
                .contains(&IncomparableReason::BaselineDigestMismatch)
        );
        let current = edge_set(&[("a", "b", 1)], true);
        let delta = compare_loaded(&loaded, &current, false);
        assert!(delta.counts.is_none() && delta.items.is_none());
        assert_eq!(delta.regression, Regression::Unknown);
    }
}

#[cfg(test)]
mod create_refusal_tests {
    use super::*;
    use crate::commands::baseline::identity::{
        BASELINE_SCHEMA, BaselineScope, EdgeCoverage, InputCompleteness, PAYLOAD_KIND,
    };
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn incomplete_set(reason: &str) -> FileEdgeSet {
        FileEdgeSet {
            identity: BaselineIdentity {
                baseline_schema: BASELINE_SCHEMA,
                payload_kind: PAYLOAD_KIND.to_string(),
                scope: BaselineScope::repo(),
                parser_version: "1".to_string(),
                edge_sources: vec!["native".to_string()],
                ledgerful_version: "0.2.18".to_string(),
                index_head_hash: None,
                input_digest: "blake3:x".to_string(),
                input_completeness: InputCompleteness::incomplete(vec![reason.to_string()]),
            },
            coverage: EdgeCoverage::default(),
            edges: Vec::new(),
        }
    }

    #[test]
    fn baseline_create_refuses_incomplete_and_writes_no_row() {
        for reason in [
            "INDEX_STALE",
            "CONTENT_DRIFT",
            "INDEX_EMPTY",
            "INDEX_INDETERMINATE",
            "PARSE_FAILED_FILES",
            "CAPPED_EDGES",
            "SCAN_INCOMPLETE",
        ] {
            let storage = in_memory_storage();
            let err = create_baseline(&storage, "base1", &incomplete_set(reason))
                .expect_err("incomplete create must refuse");
            assert!(
                err.to_string().contains(reason),
                "refusal must name {reason}: {err}"
            );
            assert!(
                list_baselines(&storage).unwrap().is_empty(),
                "a refused create must write no row ({reason})"
            );
        }
    }
}

#[cfg(test)]
mod envelope_reason_tests {
    use super::*;
    use crate::commands::baseline::identity::{
        BASELINE_SCHEMA, BaselineScope, EdgeCoverage, FileEdge, InputCompleteness, PAYLOAD_KIND,
    };
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn set(parser: &str, complete: bool) -> FileEdgeSet {
        FileEdgeSet {
            identity: BaselineIdentity {
                baseline_schema: BASELINE_SCHEMA,
                payload_kind: PAYLOAD_KIND.to_string(),
                scope: BaselineScope::repo(),
                parser_version: parser.to_string(),
                edge_sources: vec!["native".to_string()],
                ledgerful_version: "0.2.18".to_string(),
                index_head_hash: None,
                input_digest: "blake3:x".to_string(),
                input_completeness: if complete {
                    InputCompleteness::complete()
                } else {
                    InputCompleteness::incomplete(vec!["CONTENT_DRIFT".to_string()])
                },
            },
            coverage: EdgeCoverage::default(),
            edges: vec![FileEdge {
                src_path: "a".to_string(),
                dst_path: "b".to_string(),
                weight: 1,
            }],
        }
    }

    fn store(storage: &StorageManager, name: &str, set: &FileEdgeSet) {
        let payload = StoredPayload {
            coverage: set.coverage.clone(),
            edges: set.edges.clone(),
        };
        let identity_json = canonical_identity_json(&set.identity).unwrap();
        let payload_json = canonical_payload_json(&payload).unwrap();
        insert_baseline(
            storage,
            &BaselineRow {
                name: name.to_string(),
                baseline_schema: BASELINE_SCHEMA,
                payload_kind: PAYLOAD_KIND.to_string(),
                identity_json: identity_json.clone(),
                payload_json: payload_json.clone(),
                payload_digest: compute_payload_digest(&identity_json, &payload_json),
                created_at: "2026-11-21T00:00:00Z".to_string(),
            },
        )
        .unwrap();
    }

    fn reason_of(value: &serde_json::Value) -> String {
        value["incomparableReasons"][0]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn baseline_incomparable_reasons_omit_counts_and_items_in_json() {
        let storage = in_memory_storage();
        let current = set("1", true);

        // PARSER_VERSION_MISMATCH
        let mut stored = set("2", true);
        store(&storage, "pv", &stored);
        let loaded = load_baseline_checked(&storage, "pv").unwrap();
        let value = serde_json::to_value(CompareEnvelope::from_delta(
            &loaded,
            &current,
            &compare_loaded(&loaded, &current, false),
        ))
        .unwrap();
        assert_eq!(reason_of(&value), "PARSER_VERSION_MISMATCH");
        assert!(value.get("counts").is_none());
        assert!(value.get("items").is_none());

        // EDGE_SOURCES_MISMATCH
        stored = set("1", true);
        stored.identity.edge_sources = vec!["native".to_string(), "scip".to_string()];
        store(&storage, "src", &stored);
        let loaded = load_baseline_checked(&storage, "src").unwrap();
        let value = serde_json::to_value(CompareEnvelope::from_delta(
            &loaded,
            &current,
            &compare_loaded(&loaded, &current, false),
        ))
        .unwrap();
        assert_eq!(reason_of(&value), "EDGE_SOURCES_MISMATCH");
        assert!(value.get("counts").is_none() && value.get("items").is_none());

        // SCOPE_MISMATCH
        stored = set("1", true);
        stored.identity.scope = BaselineScope {
            kind: "component".to_string(),
        };
        store(&storage, "scope", &stored);
        let loaded = load_baseline_checked(&storage, "scope").unwrap();
        let value = serde_json::to_value(CompareEnvelope::from_delta(
            &loaded,
            &current,
            &compare_loaded(&loaded, &current, false),
        ))
        .unwrap();
        assert_eq!(reason_of(&value), "SCOPE_MISMATCH");
        assert!(value.get("counts").is_none() && value.get("items").is_none());

        // BASELINE_INCOMPLETE
        stored = set("1", false);
        store(&storage, "incomplete", &stored);
        let loaded = load_baseline_checked(&storage, "incomplete").unwrap();
        let value = serde_json::to_value(CompareEnvelope::from_delta(
            &loaded,
            &current,
            &compare_loaded(&loaded, &current, false),
        ))
        .unwrap();
        assert_eq!(reason_of(&value), "BASELINE_INCOMPLETE");
        assert!(value.get("counts").is_none() && value.get("items").is_none());
    }

    #[test]
    fn baseline_show_reports_unverified_digest_after_out_of_band_edit() {
        let storage = in_memory_storage();
        let stored = set("1", true);
        store(&storage, "base1", &stored);
        storage
            .get_connection()
            .execute_batch("DROP TRIGGER analysis_baselines_no_update;")
            .unwrap();
        let tampered = StoredPayload {
            coverage: EdgeCoverage::default(),
            edges: vec![FileEdge {
                src_path: "x".to_string(),
                dst_path: "y".to_string(),
                weight: 1,
            }],
        };
        let tampered_json = canonical_payload_json(&tampered).unwrap();
        storage
            .get_connection()
            .execute(
                "UPDATE analysis_baselines SET payload_json = ?1 WHERE name = 'base1'",
                [tampered_json],
            )
            .unwrap();
        let raw = get_baseline(&storage, "base1").unwrap();
        let envelope = show_envelope_for(&raw).unwrap();
        assert!(
            !envelope.digest_verified,
            "an out-of-band payload edit must report digestVerified false"
        );
    }
    #[test]
    fn baseline_corruption_is_typed_and_retains_live_incomplete_reason() {
        let storage = in_memory_storage();
        let mut complete = set("1", true);
        complete.identity.input_completeness = InputCompleteness::complete();
        create_baseline(&storage, "broken", &complete).unwrap();
        storage
            .get_connection()
            .execute_batch(
                "DROP TRIGGER analysis_baselines_no_update;
             UPDATE analysis_baselines SET payload_json = '{' WHERE name = 'broken';",
            )
            .unwrap();
        let raw = get_baseline(&storage, "broken").unwrap();
        let list_err = ListEntry::from_row(&raw).unwrap_err();
        let show_err = show_envelope_for(&raw).unwrap_err();
        for error in [list_err, show_err] {
            assert!(matches!(error.downcast_ref::<BaselineError>(),
                Some(BaselineError::Corrupt { name, .. }) if name == "broken"));
        }
        let loaded = load_baseline_checked(&storage, "broken").unwrap();
        complete.identity.input_completeness =
            InputCompleteness::incomplete(vec!["SCAN_INCOMPLETE".into()]);
        let delta = compare_loaded(&loaded, &complete, false);
        let envelope =
            serde_json::to_value(CompareEnvelope::from_delta(&loaded, &complete, &delta)).unwrap();
        assert_eq!(
            envelope["incomparableReasons"],
            serde_json::json!([
                "BASELINE_DIGEST_MISMATCH",
                "CURRENT_INCOMPLETE",
                "SCHEMA_MISMATCH"
            ])
        );
        assert!(envelope["baselineIdentity"].is_object());
        assert!(envelope.get("items").is_none());
    }
}
