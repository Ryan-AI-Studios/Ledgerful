//! Bounded counterfactual over one local, unqualified observation.

#[cfg(test)]
mod projection_tests;
mod source;

use std::path::Path;
use std::time::{Duration, Instant};

use gix::ObjectId;
use serde::{Deserialize, Deserializer, Serialize};

use crate::change_evidence::git::exact_oid;
use crate::commands::baseline::{FileEdge, FileEdgeSet};

use super::classify::{CycleClass, observation_reasons, projected_cycle_change};
use super::components::{Assignment, ComponentMap, RuleDecision, decide_rule, project_cycles};
use super::config::{config_digest, is_valid_component_name, parse_bounded_architecture_toml};

const MAX_PATH_BYTES: usize = 256;
const MAX_PAIRS: usize = 5_000;
const MAX_REPLY_BYTES: usize = 32 * 1024;
const MAX_TIME: Duration = Duration::from_secs(3);
const FILE_CALL: &str = "fileCall";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProposalRequest {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) kind: String,
    pub(crate) head: String,
    #[serde(default, deserialize_with = "present_context")]
    pub(crate) context: Option<String>,
}

fn present_context<'de, D: Deserializer<'de>>(value: D) -> Result<Option<String>, D::Error> {
    String::deserialize(value).map(Some)
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("ARCHITECTURE_PROPOSAL_INVALID_REQUEST")]
pub(crate) struct InvalidProposal;

impl ProposalRequest {
    fn validate(&self) -> Result<ObjectId, InvalidProposal> {
        if !valid_path(&self.from)
            || !valid_path(&self.to)
            || self.from == self.to
            || self.kind.is_empty()
            || self.kind.len() > 64
            || !self
                .kind
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || self.context.as_deref().is_some_and(|context| {
                !context
                    .strip_prefix("blake3:")
                    .is_some_and(|digest| lower_hex(digest, 64))
            })
        {
            return Err(InvalidProposal);
        }
        exact_oid(self.head.as_bytes()).map_err(|_| InvalidProposal)
    }
}

fn lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_path(path: &str) -> bool {
    path.len() <= MAX_PATH_BYTES
        && crate::change_evidence::tree_index::is_evidence_path(path)
        && path
            .split('/')
            .all(|part| !part.ends_with('.') && !windows_device_name(part))
}

fn windows_device_name(segment: &str) -> bool {
    let name = segment.split('.').next().unwrap_or_default();
    ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|device| name.eq_ignore_ascii_case(device))
        || matches!(name.as_bytes(), [prefix @ .., b'0'..=b'9']
            if prefix.eq_ignore_ascii_case(b"COM") || prefix.eq_ignore_ascii_case(b"LPT"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProposalPacket {
    kind: &'static str,
    schema_version: u32,
    status: &'static str,
    reason_codes: Vec<String>,
    permission: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_context: Option<ObservedContext>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    projection: Option<Projection>,
}

impl ProposalPacket {
    fn unknown(reason: &str) -> Self {
        Self::with_reasons(vec![reason.to_owned()])
    }

    fn with_reasons(mut reason_codes: Vec<String>) -> Self {
        reason_codes.sort_unstable();
        reason_codes.dedup();
        Self {
            kind: "architectureProposal",
            schema_version: 1,
            status: "unknown",
            reason_codes,
            permission: "unknown",
            observed_context: None,
            projection: None,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ObservedContext {
    head: String,
    index_head: String,
    input_digest: String,
    config_digest: String,
    token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Projection {
    source_component: String,
    target_component: String,
    evidence_qualification: &'static str,
    rule_decision: &'static str,
    rule_id: Option<String>,
    cycle_effect: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    members: Option<Vec<String>>,
}

pub(crate) fn evaluate(
    request: ProposalRequest,
    effective_cwd: &Path,
) -> Result<ProposalPacket, InvalidProposal> {
    let deadline = Instant::now() + MAX_TIME;
    let head = request.validate()?;
    if request.kind != FILE_CALL {
        return Ok(ProposalPacket::unknown("UNSUPPORTED_KIND"));
    }
    let mut check = || {
        (Instant::now() < deadline)
            .then_some(())
            .ok_or("LIMIT_EXCEEDED")
    };
    let packet = match source::observe(&request, effective_cwd, head, deadline) {
        Ok(snapshot) => evaluate_snapshot(request, snapshot, &mut check),
        Err(reason) => ProposalPacket::unknown(reason),
    };
    // Check pretty serialization too: CLI and MCP have different framing.
    if Instant::now() >= deadline
        || serde_json::to_vec_pretty(&packet).map_or(true, |raw| raw.len() > MAX_REPLY_BYTES)
        || Instant::now() >= deadline
    {
        return Ok(ProposalPacket::unknown("LIMIT_EXCEEDED"));
    }
    Ok(packet)
}

struct Snapshot {
    raw_config: Vec<u8>,
    index_config_digest: blake3::Hash,
    scope: [u8; 32],
    edges: FileEdgeSet,
}

fn evaluate_snapshot(
    request: ProposalRequest,
    snapshot: Snapshot,
    check: &mut impl FnMut() -> Result<(), &'static str>,
) -> ProposalPacket {
    evaluate_snapshot_checked(request, snapshot, check).unwrap_or_else(ProposalPacket::unknown)
}

fn evaluate_snapshot_checked(
    request: ProposalRequest,
    snapshot: Snapshot,
    check: &mut impl FnMut() -> Result<(), &'static str>,
) -> Result<ProposalPacket, &'static str> {
    check()?;
    let baseline = &snapshot.edges;
    if !baseline.identity.input_completeness.is_complete() {
        return Ok(ProposalPacket::with_reasons(
            baseline.identity.input_completeness.reasons.clone(),
        ));
    }
    let config = parse_bounded_architecture_toml(&snapshot.raw_config)?;
    check()?;
    let map = ComponentMap::build(&config).map_err(|_| "INVALID_CONTEXT_CONFIG")?;
    check()?;
    let (Assignment::Assigned(from), Assignment::Assigned(to)) = (
        map.assign_checked(&request.from, check)?,
        map.assign_checked(&request.to, check)?,
    ) else {
        return Err("UNSUPPORTED_ENDPOINT_MAPPING");
    };
    if from == to {
        return Err("UNSUPPORTED_ENDPOINT_MAPPING");
    }
    let (rule_decision, rule_id) = match decide_rule(&config, &from, &to) {
        RuleDecision::Allowed { rule_id } => ("allow", Some(rule_id)),
        RuleDecision::Forbidden { rule_id } => ("forbid", Some(rule_id)),
        RuleDecision::Unconstrained => ("unconstrained", None),
    };
    check()?;
    if rule_id.is_some_and(|id| !is_valid_component_name(id)) {
        return Err("UNREPRESENTABLE_IDENTIFIER");
    }
    let observed = context_for(&request, &snapshot, check)?;
    match request.context.as_ref() {
        None => {
            let mut packet = ProposalPacket::unknown("CONTEXT_REQUIRED");
            packet.observed_context = Some(observed);
            return Ok(packet);
        }
        Some(context) if context != &observed.token => {
            return Err("CONTEXT_MISMATCH");
        }
        Some(_) => {}
    }
    let current = add_pair(&baseline.edges, request.from, request.to, check)?;
    let prior = project_cycles(&baseline.edges, &map, check)?;
    let projected = project_cycles(&current, &map, check)?;
    let affected = projected_cycle_change(&prior.groups, &projected.groups, [&from, &to], check)?;
    let mut reasons = observation_reasons(
        prior.cross_component_edges,
        prior.ambiguous_mapping_edges,
        &baseline.coverage,
    );
    if prior.unmapped_edges != 0 {
        reasons.push("UNMAPPED_COMPONENTS".to_owned());
    }
    let (cycle_effect, members) = match affected {
        Some((class, members)) => {
            let effect = if class == CycleClass::Introduced {
                "introduced"
            } else {
                "grown"
            };
            (effect, Some(members.to_vec()))
        }
        None if reasons.is_empty() => ("noChangeObserved", None),
        None => ("unknown", None),
    };
    reasons.push("UNQUALIFIED_EVIDENCE".to_owned());
    let mut packet = ProposalPacket::with_reasons(reasons);
    packet.status = "evaluated";
    packet.observed_context = Some(observed);
    packet.projection = Some(Projection {
        source_component: from,
        target_component: to,
        evidence_qualification: "unqualified",
        rule_decision,
        rule_id: rule_id.map(str::to_owned),
        cycle_effect,
        members,
    });
    check()?;
    Ok(packet)
}

fn add_pair(
    baseline: &[FileEdge],
    from: String,
    to: String,
    check: &mut impl FnMut() -> Result<(), &'static str>,
) -> Result<Vec<FileEdge>, &'static str> {
    check()?;
    let slot =
        baseline.binary_search_by(|edge| (&edge.src_path, &edge.dst_path).cmp(&(&from, &to)));
    let size = baseline.len() + usize::from(slot.is_err());
    if size > MAX_PAIRS {
        return Err("LIMIT_EXCEEDED");
    }
    let mut current = Vec::with_capacity(size);
    for edge in baseline {
        check()?;
        current.push(edge.clone());
    }
    match slot {
        Ok(index) => {
            let weight = &mut current[index].weight;
            *weight = weight.checked_add(1).ok_or("LIMIT_EXCEEDED")?;
        }
        Err(index) => current.insert(
            index,
            FileEdge {
                src_path: from,
                dst_path: to,
                weight: 1,
            },
        ),
    }
    check()?;
    Ok(current)
}

fn context_for(
    request: &ProposalRequest,
    snapshot: &Snapshot,
    check: &mut impl FnMut() -> Result<(), &'static str>,
) -> Result<ObservedContext, &'static str> {
    let baseline = &snapshot.edges;
    let config_digest = config_digest(&snapshot.raw_config);
    let index_head = baseline
        .identity
        .index_head_hash
        .as_deref()
        .unwrap_or_default();
    let identity = &baseline.identity;
    let coverage = &baseline.coverage;
    let mut hash = blake3::Hasher::new();
    hash.update(b"ledgerful:architecture-proposal:v1\0");
    let mut frame = |bytes: &[u8]| {
        hash.update(&(bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    };
    for bytes in [
        snapshot.scope.as_slice(),
        request.head.as_bytes(),
        index_head.as_bytes(),
        identity.input_digest.as_bytes(),
        snapshot.index_config_digest.as_bytes(),
        config_digest.as_bytes(),
        identity.parser_version.as_bytes(),
        identity.ledgerful_version.as_bytes(),
        request.from.as_bytes(),
        request.to.as_bytes(),
        request.kind.as_bytes(),
    ] {
        frame(bytes);
    }
    for value in [
        coverage.files_indexed,
        coverage.files_parse_failed,
        coverage.edges_resolved_cross_file,
        coverage.edges_resolved_intra_file,
        coverage.edges_ambiguous,
        coverage.edges_unresolved,
        coverage.edges_capped,
        identity.edge_sources.len() as u64,
    ] {
        frame(&value.to_be_bytes());
    }
    for producer in &identity.edge_sources {
        frame(producer.as_bytes());
    }
    frame(b"COMPLETE");
    frame(&(baseline.edges.len() as u64).to_be_bytes());
    for edge in &baseline.edges {
        check()?;
        frame(edge.src_path.as_bytes());
        frame(edge.dst_path.as_bytes());
        frame(&edge.weight.to_be_bytes());
    }
    check()?;
    Ok(ObservedContext {
        head: request.head.clone(),
        index_head: index_head.to_owned(),
        input_digest: identity.input_digest.clone(),
        config_digest,
        token: format!("blake3:{}", hash.finalize().to_hex()),
    })
}
