//! Change-set affected HTTP flows over indexed `api_routes`.
//!
//! Probe-first statuses distinguish missing table / empty map / no change seeds /
//! unavailable from a genuine available scan. Match kinds prefer direct handler
//! binds over registration-file hits; blast mediation walks **`blast.edges` only**
//! (never bare `must_touch_*` name sets). `confidenceClass` is set only for
//! blast-mediated hits from the justifying edge (0117 vocabulary).

use crate::impact::enrichment::blast::normalize_path;
use crate::impact::enrichment::edge_confidence::{ConfidenceClass, confidence_class};
use crate::impact::packet::{BlastEdge, BlastRadius, ChangedFile};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Cap on `flows` entries in a report.
pub const FLOWS_CAP: usize = 20;

/// Structural honesty note (always emitted when a report is built).
pub const HONESTY_NOTE: &str = "Registered HTTP routes only (api_routes); not distributed traces or CRG-style call-chain flows.";

/// Status vocabulary for change-set affected flows (no bare `"empty"`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AffectedFlowsStatus {
    Available,
    EmptyMap,
    MissingTable,
    NoChangeSeeds,
    Unavailable,
}

impl AffectedFlowsStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::EmptyMap => "empty_map",
            Self::MissingTable => "missing_table",
            Self::NoChangeSeeds => "no_change_seeds",
            Self::Unavailable => "unavailable",
        }
    }
}

/// How a route was matched to the change set (priority order = enum order).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    HandlerSymbol,
    HandlerImplFile,
    RouteFile,
    BlastSymbol,
    BlastFile,
}

impl MatchKind {
    /// Priority number (1 = strongest). Lower is better for sort/dedupe.
    pub fn priority(self) -> u8 {
        match self {
            Self::HandlerSymbol => 1,
            Self::HandlerImplFile => 2,
            Self::RouteFile => 3,
            Self::BlastSymbol => 4,
            Self::BlastFile => 5,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::HandlerSymbol => "handler_symbol",
            Self::HandlerImplFile => "handler_impl_file",
            Self::RouteFile => "route_file",
            Self::BlastSymbol => "blast_symbol",
            Self::BlastFile => "blast_file",
        }
    }
}

/// One affected HTTP flow (route registration touched by the change set).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AffectedFlowEntry {
    pub method: String,
    pub path_pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_symbol_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_file: Option<String>,
    pub framework: String,
    pub match_kind: MatchKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_confidence: Option<f64>,
    /// SCREAMING_SNAKE; only set for blast-mediated matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_class: Option<ConfidenceClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl Eq for AffectedFlowEntry {}

/// Deterministic, budgeted affected-flows report for a change set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AffectedFlowsReport {
    pub status: AffectedFlowsStatus,
    pub flow_count: usize,
    pub flow_capped: bool,
    pub flow_total: usize,
    pub flows: Vec<AffectedFlowEntry>,
    pub notes: Vec<String>,
}

/// Optional inputs for flow computation (staleness, etc.).
#[derive(Debug, Clone, Default)]
pub struct AffectedFlowsOpts {
    /// Packet/report HEAD hash; when set and index metadata differs, a
    /// staleness note is appended (read-only; no auto-refresh).
    pub head_hash: Option<String>,
}

impl AffectedFlowsReport {
    /// Empty unavailable report (no DB / soft-open failed).
    pub fn unavailable() -> Self {
        Self::with_status(AffectedFlowsStatus::Unavailable, Vec::new())
    }

    fn with_status(status: AffectedFlowsStatus, extra_notes: Vec<String>) -> Self {
        let mut notes = default_notes();
        notes.extend(extra_notes);
        notes.sort();
        notes.dedup();
        Self {
            status,
            flow_count: 0,
            flow_capped: false,
            flow_total: 0,
            flows: Vec::new(),
            notes,
        }
    }
}

fn default_notes() -> Vec<String> {
    vec![HONESTY_NOTE.to_string()]
}

/// Sort flows: matchKind priority asc, then method, pathPattern, framework, handler.
pub fn sort_affected_flows(flows: &mut [AffectedFlowEntry]) {
    flows.sort_by(|a, b| {
        a.match_kind
            .priority()
            .cmp(&b.match_kind.priority())
            .then_with(|| a.method.cmp(&b.method))
            .then_with(|| a.path_pattern.cmp(&b.path_pattern))
            .then_with(|| a.framework.cmp(&b.framework))
            .then_with(|| {
                a.handler_symbol_name
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.handler_symbol_name.as_deref().unwrap_or(""))
            })
    });
}

/// Probe `sqlite_master` for the `api_routes` table.
fn table_exists(conn: &Connection) -> Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='api_routes'",
        [],
        |row| row.get::<_, i64>(0).map(|c| c > 0),
    )
}

/// COUNT(*) from `api_routes`. Err on query failure (not silent 0).
fn route_row_count(conn: &Connection) -> Result<i64, rusqlite::Error> {
    conn.query_row("SELECT COUNT(*) FROM api_routes", [], |row| row.get(0))
}

/// Optional staleness note when index HEAD differs from the provided head.
fn staleness_note(conn: &Connection, head_hash: Option<&str>) -> Option<String> {
    let packet_head = head_hash.filter(|h| !h.is_empty())?;
    let indexed_head: Option<String> = conn
        .query_row(
            "SELECT value FROM index_metadata WHERE key = 'head_hash'",
            [],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    match indexed_head.as_deref() {
        Some(indexed) if indexed != packet_head => Some(format!(
            "api_routes may be stale: index head_hash ({indexed}) ≠ change head ({packet_head})"
        )),
        None => Some("api_routes staleness unknown: index_metadata.head_hash missing".to_string()),
        _ => None,
    }
}

fn collect_extra_notes(conn: &Connection, opts: &AffectedFlowsOpts) -> Vec<String> {
    let mut extra = Vec::new();
    if let Some(note) = staleness_note(conn, opts.head_hash.as_deref()) {
        extra.push(note);
    }
    extra
}

/// Shared probe prefix: missing_table / empty_map / unavailable-on-query-err,
/// else `None` (continue).
fn probe_table(conn: &Connection, opts: &AffectedFlowsOpts) -> Option<AffectedFlowsReport> {
    match table_exists(conn) {
        Err(e) => Some(AffectedFlowsReport::with_status(
            AffectedFlowsStatus::Unavailable,
            vec![format!(
                "api_routes probe failed (sqlite_master): {e}; not treating as missing_table"
            )],
        )),
        Ok(false) => Some(AffectedFlowsReport::with_status(
            AffectedFlowsStatus::MissingTable,
            collect_extra_notes(conn, opts),
        )),
        Ok(true) => match route_row_count(conn) {
            Err(e) => Some(AffectedFlowsReport::with_status(
                AffectedFlowsStatus::Unavailable,
                vec![format!(
                    "api_routes COUNT(*) failed: {e}; not treating as empty_map"
                )],
            )),
            Ok(0) => Some(AffectedFlowsReport::with_status(
                AffectedFlowsStatus::EmptyMap,
                collect_extra_notes(conn, opts),
            )),
            Ok(_) => None,
        },
    }
}

/// Indexed route row with resolved registration + optional impl file.
#[derive(Debug, Clone)]
struct RouteRow {
    method: String,
    path_pattern: String,
    framework: String,
    handler_symbol_id: Option<i64>,
    handler_symbol_name: Option<String>,
    registration_file: String,
    /// Implementation file of `handler_symbol_id` via `project_symbols` (not registration).
    impl_file: Option<String>,
    route_confidence: Option<f64>,
    evidence: Option<String>,
}

/// Change-set seeds used for direct match kinds.
#[derive(Debug, Default)]
struct ChangeSeeds {
    files: BTreeSet<String>,
    symbol_ids: BTreeSet<i64>,
    /// (symbol_name, impl_file) pairs resolved from changed symbols.
    name_files: BTreeSet<(String, String)>,
    /// Bare symbol names from changed files (for null-id unique name+impl match).
    names: BTreeSet<String>,
}

fn load_routes(conn: &Connection) -> Result<Vec<RouteRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT ar.method, ar.path_pattern, ar.framework,
                ar.handler_symbol_id, ar.handler_symbol_name,
                pf.file_path, ar.route_confidence, ar.evidence,
                impl_pf.file_path
         FROM api_routes ar
         JOIN project_files pf ON ar.handler_file_id = pf.id
         LEFT JOIN project_symbols ps ON ar.handler_symbol_id = ps.id
         LEFT JOIN project_files impl_pf ON ps.file_id = impl_pf.id",
    )?;
    let rows = stmt.query_map([], |row| {
        let reg: String = row.get(5)?;
        let conf: Option<f64> = row.get(6)?;
        let evidence: Option<String> = row.get(7)?;
        let impl_path: Option<String> = row.get(8)?;
        Ok(RouteRow {
            method: row.get(0)?,
            path_pattern: row.get(1)?,
            framework: row.get(2)?,
            handler_symbol_id: row.get(3)?,
            handler_symbol_name: row.get(4)?,
            registration_file: normalize_path(&reg),
            impl_file: impl_path.map(|p| normalize_path(&p)),
            route_confidence: conf,
            evidence,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    // Deterministic load order for stable downstream processing.
    out.sort_by(|a, b| {
        a.method
            .cmp(&b.method)
            .then_with(|| a.path_pattern.cmp(&b.path_pattern))
            .then_with(|| a.framework.cmp(&b.framework))
            .then_with(|| {
                a.handler_symbol_name
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.handler_symbol_name.as_deref().unwrap_or(""))
            })
    });
    Ok(out)
}

/// Resolve change seeds from packet changes (file paths + bound symbol ids/names).
fn collect_change_seeds(
    conn: &Connection,
    changes: &[ChangedFile],
) -> Result<ChangeSeeds, rusqlite::Error> {
    let mut seeds = ChangeSeeds::default();

    for change in changes {
        let file_path = normalize_path(&change.path.to_string_lossy());
        if !file_path.is_empty() {
            seeds.files.insert(file_path.clone());
        }

        let Some(symbols) = change.symbols.as_ref() else {
            continue;
        };

        for symbol in symbols {
            seeds.names.insert(symbol.name.clone());
            seeds
                .name_files
                .insert((symbol.name.clone(), file_path.clone()));

            // Prefer qualified_name when present (same order as resolve_seeds).
            if let Some(ref qn) = symbol.qualified_name
                && !qn.is_empty()
            {
                let mut stmt = conn
                    .prepare("SELECT ps.id FROM project_symbols ps WHERE ps.qualified_name = ?1")?;
                let ids = stmt.query_map([qn.as_str()], |row| row.get::<_, i64>(0))?;
                let mut matched = false;
                for id in ids {
                    seeds.symbol_ids.insert(id?);
                    matched = true;
                }
                if matched {
                    continue;
                }
            }

            // file_path + symbol_name
            let mut stmt = conn.prepare(
                "SELECT ps.id
                 FROM project_symbols ps
                 JOIN project_files pf ON ps.file_id = pf.id
                 WHERE pf.file_path = ?1 AND ps.symbol_name = ?2",
            )?;
            let ids = stmt.query_map([file_path.as_str(), symbol.name.as_str()], |row| {
                row.get::<_, i64>(0)
            })?;
            for id in ids {
                seeds.symbol_ids.insert(id?);
            }
        }
    }

    Ok(seeds)
}

/// Count project_symbols with given name (for uniqueness checks).
fn count_symbols_named(conn: &Connection, name: &str) -> Result<usize, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM project_symbols WHERE symbol_name = ?1",
        [name],
        |row| row.get::<_, i64>(0).map(|c| c as usize),
    )
}

/// True when `(name, file)` is a unique project_symbols bind (exactly one row).
fn unique_name_file(conn: &Connection, name: &str, file: &str) -> Result<bool, rusqlite::Error> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM project_symbols ps
         JOIN project_files pf ON ps.file_id = pf.id
         WHERE ps.symbol_name = ?1 AND pf.file_path = ?2",
        [name, file],
        |row| row.get(0),
    )?;
    Ok(n == 1)
}

/// Count handlers that share a given symbol name (for blast bare-name uniqueness).
fn handlers_with_name(routes: &[RouteRow], name: &str) -> usize {
    routes
        .iter()
        .filter(|r| r.handler_symbol_name.as_deref() == Some(name))
        .count()
}

/// Count handlers whose impl or registration file equals `file`.
fn handlers_in_file<'a>(routes: &'a [RouteRow], file: &str) -> Vec<&'a RouteRow> {
    routes
        .iter()
        .filter(|r| r.registration_file == file || r.impl_file.as_deref() == Some(file))
        .collect()
}

/// Candidate match with optional blast class/evidence.
#[derive(Debug, Clone)]
struct MatchCandidate {
    kind: MatchKind,
    confidence_class: Option<ConfidenceClass>,
    evidence: Option<String>,
    /// For blast ties: prefer higher collapse_priority edge.
    collapse_priority: u8,
}

impl MatchCandidate {
    fn direct(kind: MatchKind) -> Self {
        Self {
            kind,
            confidence_class: None,
            evidence: None,
            collapse_priority: 0,
        }
    }

    fn blast(kind: MatchKind, edge: &BlastEdge) -> Self {
        let class = if edge.confidence_class != ConfidenceClass::Unknown {
            edge.confidence_class
        } else {
            confidence_class(&edge.resolution_status, &edge.evidence)
        };
        let evidence = if edge.evidence.is_empty() {
            None
        } else {
            // Keep short; cap evidence length for packet size.
            let short = if edge.evidence.len() > 120 {
                format!("{}…", &edge.evidence[..119])
            } else {
                edge.evidence.clone()
            };
            Some(short)
        };
        Self {
            kind,
            confidence_class: Some(class),
            evidence,
            collapse_priority: class.collapse_priority(),
        }
    }

    /// True if `self` is a better match than `other` for dedupe.
    fn better_than(&self, other: &Self, self_conf: Option<f64>, other_conf: Option<f64>) -> bool {
        let sp = self.kind.priority();
        let op = other.kind.priority();
        if sp != op {
            return sp < op;
        }
        // Same match kind: prefer higher blast collapse_priority.
        if self.collapse_priority != other.collapse_priority {
            return self.collapse_priority > other.collapse_priority;
        }
        // Higher route_confidence wins.
        let sc = self_conf.unwrap_or(0.0);
        let oc = other_conf.unwrap_or(0.0);
        if (sc - oc).abs() > f64::EPSILON {
            return sc > oc;
        }
        false
    }
}

fn handler_files(route: &RouteRow) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    files.insert(route.registration_file.clone());
    if let Some(ref impl_f) = route.impl_file {
        files.insert(impl_f.clone());
    }
    files
}

fn edge_symbol_matches_handler(edge: &BlastEdge, handler_name: &str) -> bool {
    edge.from_symbol == handler_name || edge.to_symbol == handler_name
}

fn edge_file_touches_handler(edge: &BlastEdge, route: &RouteRow) -> bool {
    let files = handler_files(route);
    files.contains(&normalize_path(&edge.from_file))
        || files.contains(&normalize_path(&edge.to_file))
}

/// Try blast_symbol: symbol on edge matches handler + file pairing constraint.
fn try_blast_symbol(
    route: &RouteRow,
    edges: &[BlastEdge],
    all_routes: &[RouteRow],
) -> Option<MatchCandidate> {
    let handler_name = route.handler_symbol_name.as_deref()?;
    let mut best: Option<MatchCandidate> = None;

    for edge in edges {
        if !edge_symbol_matches_handler(edge, handler_name) {
            continue;
        }

        let file_ok = edge_file_touches_handler(edge, route);
        if !file_ok {
            // Bare name alone: only if unique among handlers for that edge's file pair.
            let from_f = normalize_path(&edge.from_file);
            let to_f = normalize_path(&edge.to_file);
            let unique_for_pair = handlers_with_name(all_routes, handler_name) == 1
                && (handlers_in_file(all_routes, &from_f)
                    .iter()
                    .any(|r| r.handler_symbol_name.as_deref() == Some(handler_name))
                    || handlers_in_file(all_routes, &to_f)
                        .iter()
                        .any(|r| r.handler_symbol_name.as_deref() == Some(handler_name)));
            // Reject common bare-name FP: symbol name on unrelated edge files.
            if !unique_for_pair {
                continue;
            }
        }

        let cand = MatchCandidate::blast(MatchKind::BlastSymbol, edge);
        let take = match &best {
            None => true,
            Some(prev) => cand.collapse_priority > prev.collapse_priority,
        };
        if take {
            best = Some(cand);
        }
    }

    best
}

/// Try blast_file: edge file matches handler file + paired symbol is handler
/// (or unique handler in that file).
fn try_blast_file(
    route: &RouteRow,
    edges: &[BlastEdge],
    all_routes: &[RouteRow],
) -> Option<MatchCandidate> {
    let handler_name = route.handler_symbol_name.as_deref();
    let route_files = handler_files(route);
    let mut best: Option<MatchCandidate> = None;

    for edge in edges {
        let from_f = normalize_path(&edge.from_file);
        let to_f = normalize_path(&edge.to_file);
        let from_hit = route_files.contains(&from_f);
        let to_hit = route_files.contains(&to_f);
        if !from_hit && !to_hit {
            continue;
        }

        // Paired symbol is the handler, or unique handler in that file.
        let symbol_ok = match handler_name {
            Some(name) => {
                let from_sym = edge.from_symbol == name;
                let to_sym = edge.to_symbol == name;
                if from_sym || to_sym {
                    true
                } else {
                    // Unique handler in the matched file(s).
                    let mut ok = false;
                    if from_hit {
                        let hs = handlers_in_file(all_routes, &from_f);
                        ok = hs.len() == 1
                            && hs[0].method == route.method
                            && hs[0].path_pattern == route.path_pattern
                            && hs[0].framework == route.framework;
                    }
                    if !ok && to_hit {
                        let hs = handlers_in_file(all_routes, &to_f);
                        ok = hs.len() == 1
                            && hs[0].method == route.method
                            && hs[0].path_pattern == route.path_pattern
                            && hs[0].framework == route.framework;
                    }
                    ok
                }
            }
            None => {
                // No handler name: only unique handler in the matched file.
                let mut ok = false;
                if from_hit {
                    let hs = handlers_in_file(all_routes, &from_f);
                    ok = hs.len() == 1
                        && hs[0].method == route.method
                        && hs[0].path_pattern == route.path_pattern
                        && hs[0].framework == route.framework;
                }
                if !ok && to_hit {
                    let hs = handlers_in_file(all_routes, &to_f);
                    ok = hs.len() == 1
                        && hs[0].method == route.method
                        && hs[0].path_pattern == route.path_pattern
                        && hs[0].framework == route.framework;
                }
                ok
            }
        };

        if !symbol_ok {
            continue;
        }

        let cand = MatchCandidate::blast(MatchKind::BlastFile, edge);
        let take = match &best {
            None => true,
            Some(prev) => cand.collapse_priority > prev.collapse_priority,
        };
        if take {
            best = Some(cand);
        }
    }

    best
}

fn match_route(
    conn: &Connection,
    route: &RouteRow,
    seeds: &ChangeSeeds,
    blast: Option<&BlastRadius>,
    all_routes: &[RouteRow],
) -> Result<Option<MatchCandidate>, rusqlite::Error> {
    // Priority 1: handler_symbol
    if let Some(hid) = route.handler_symbol_id {
        if seeds.symbol_ids.contains(&hid) {
            return Ok(Some(MatchCandidate::direct(MatchKind::HandlerSymbol)));
        }
    } else if let Some(ref name) = route.handler_symbol_name {
        // Null id: name + impl file unique match via project_symbols only
        // (never registration file / handler_file_id).
        for (seed_name, seed_file) in &seeds.name_files {
            if seed_name != name {
                continue;
            }
            if unique_name_file(conn, name, seed_file)? {
                // Confirm the name is not massively ambiguous project-wide without
                // the file bind — uniqueness of (name, file) is the gate.
                let _ = count_symbols_named(conn, name)?;
                return Ok(Some(MatchCandidate::direct(MatchKind::HandlerSymbol)));
            }
        }
    }

    // Priority 2: handler_impl_file
    if let Some(ref impl_f) = route.impl_file
        && seeds.files.contains(impl_f)
    {
        return Ok(Some(MatchCandidate::direct(MatchKind::HandlerImplFile)));
    }

    // Priority 3: route_file (registration)
    if seeds.files.contains(&route.registration_file) {
        return Ok(Some(MatchCandidate::direct(MatchKind::RouteFile)));
    }

    // Priority 4–5: blast edges only (never must_touch_*)
    if let Some(br) = blast {
        if let Some(c) = try_blast_symbol(route, &br.edges, all_routes) {
            return Ok(Some(c));
        }
        if let Some(c) = try_blast_file(route, &br.edges, all_routes) {
            return Ok(Some(c));
        }
    }

    Ok(None)
}

fn entry_from_route(route: &RouteRow, cand: MatchCandidate) -> AffectedFlowEntry {
    // Prefer impl file for display; fall back to registration.
    let handler_file = route
        .impl_file
        .clone()
        .or_else(|| Some(route.registration_file.clone()));
    AffectedFlowEntry {
        method: route.method.clone(),
        path_pattern: route.path_pattern.clone(),
        handler_symbol_name: route.handler_symbol_name.clone(),
        handler_file,
        framework: route.framework.clone(),
        match_kind: cand.kind,
        route_confidence: route.route_confidence,
        confidence_class: cand.confidence_class,
        evidence: cand.evidence.or_else(|| {
            // Prefer short route evidence only for direct matches when present.
            if matches!(
                cand.kind,
                MatchKind::HandlerSymbol | MatchKind::HandlerImplFile | MatchKind::RouteFile
            ) {
                route.evidence.clone().and_then(|e| {
                    if e.is_empty() {
                        None
                    } else if e.len() > 120 {
                        Some(format!("{}…", &e[..119]))
                    } else {
                        Some(e)
                    }
                })
            } else {
                None
            }
        }),
    }
}

/// Dedupe key: (method, path_pattern, framework).
type DedupeKey = (String, String, String);

fn dedupe_keep_best(entries: Vec<(AffectedFlowEntry, MatchCandidate)>) -> Vec<AffectedFlowEntry> {
    // key → (entry, candidate)
    let mut best: HashMap<DedupeKey, (AffectedFlowEntry, MatchCandidate)> = HashMap::new();

    for (entry, cand) in entries {
        let key = (
            entry.method.clone(),
            entry.path_pattern.clone(),
            entry.framework.clone(),
        );
        match best.get(&key) {
            None => {
                best.insert(key, (entry, cand));
            }
            Some((prev_entry, prev_cand)) => {
                let better = cand.better_than(
                    prev_cand,
                    entry.route_confidence,
                    prev_entry.route_confidence,
                );
                // Tie-break: lex lower handler name wins when still equal.
                let take = if better {
                    true
                } else if cand.kind.priority() == prev_cand.kind.priority()
                    && cand.collapse_priority == prev_cand.collapse_priority
                    && (entry.route_confidence.unwrap_or(0.0)
                        - prev_entry.route_confidence.unwrap_or(0.0))
                    .abs()
                        < f64::EPSILON
                {
                    entry.handler_symbol_name.as_deref().unwrap_or("")
                        < prev_entry.handler_symbol_name.as_deref().unwrap_or("")
                } else {
                    false
                };
                if take {
                    best.insert(key, (entry, cand));
                }
            }
        }
    }

    let mut out: Vec<AffectedFlowEntry> = best.into_values().map(|(e, _)| e).collect();
    sort_affected_flows(&mut out);
    out
}

/// Outcome of uncapped route matching (before report packaging / [`FLOWS_CAP`]).
enum MatchOutcome {
    /// Probe/seed/load short-circuit (status-bearing report, zero matches).
    Early(AffectedFlowsReport),
    /// Successful available scan with full deduped flows (uncapped).
    Matched(Vec<AffectedFlowEntry>),
}

/// Shared match path used by report payloads and filter consumers.
///
/// Does **not** apply [`FLOWS_CAP`]. Report builders truncate; filter helpers keep
/// the full key set so registration-file edits with 50+ routes are not silently
/// dropped from `endpoints --changed`.
fn match_affected_flows_uncapped(
    conn: &Connection,
    changes: &[ChangedFile],
    blast: Option<&BlastRadius>,
    opts: &AffectedFlowsOpts,
) -> miette::Result<MatchOutcome> {
    if let Some(early) = probe_table(conn, opts) {
        return Ok(MatchOutcome::Early(early));
    }

    let seeds = match collect_change_seeds(conn, changes) {
        Ok(s) => s,
        Err(e) => {
            return Ok(MatchOutcome::Early(AffectedFlowsReport::with_status(
                AffectedFlowsStatus::Unavailable,
                vec![format!(
                    "change-seed resolution failed: {e}; not inventing flow matches"
                )],
            )));
        }
    };

    // no_change_seeds: no non-empty change set (no files at all).
    if seeds.files.is_empty() && changes.is_empty() {
        return Ok(MatchOutcome::Early(AffectedFlowsReport::with_status(
            AffectedFlowsStatus::NoChangeSeeds,
            collect_extra_notes(conn, opts),
        )));
    }
    // Also treat changes present but all empty paths as no seeds.
    if seeds.files.is_empty() {
        return Ok(MatchOutcome::Early(AffectedFlowsReport::with_status(
            AffectedFlowsStatus::NoChangeSeeds,
            collect_extra_notes(conn, opts),
        )));
    }

    let routes = match load_routes(conn) {
        Ok(r) => r,
        Err(e) => {
            return Ok(MatchOutcome::Early(AffectedFlowsReport::with_status(
                AffectedFlowsStatus::Unavailable,
                vec![format!(
                    "api_routes load failed: {e}; not inventing flow matches"
                )],
            )));
        }
    };

    let mut matched: Vec<(AffectedFlowEntry, MatchCandidate)> = Vec::new();
    for route in &routes {
        match match_route(conn, route, &seeds, blast, &routes) {
            Ok(Some(cand)) => {
                matched.push((entry_from_route(route, cand.clone()), cand));
            }
            Ok(None) => {}
            Err(e) => {
                return Ok(MatchOutcome::Early(AffectedFlowsReport::with_status(
                    AffectedFlowsStatus::Unavailable,
                    vec![format!(
                        "route match failed: {e}; not inventing partial flow results"
                    )],
                )));
            }
        }
    }

    Ok(MatchOutcome::Matched(dedupe_keep_best(matched)))
}

/// Full deduped `(method_upper, path_pattern)` match set **before** [`FLOWS_CAP`]
/// truncate — for filter consumers such as `endpoints --changed`.
///
/// Report payloads (impact / PR / change-context) still use
/// [`compute_affected_flows`], which caps `flows` at [`FLOWS_CAP`].
///
/// **Honest empty filter:** `empty_map`, `missing_table`, and `no_change_seeds`
/// yield `Ok(empty)`. **`unavailable` (probe/load/match fault) returns `Err`**
/// so filter callers cannot present a false "no endpoints changed" after a
/// matching failure. Soft-open report builders should keep using
/// [`compute_affected_flows`] / [`compute_pr_affected_flows_soft`].
pub fn match_affected_route_keys(
    conn: &Connection,
    changes: &[ChangedFile],
    blast: Option<&BlastRadius>,
    opts: &AffectedFlowsOpts,
) -> miette::Result<HashSet<(String, String)>> {
    match match_affected_flows_uncapped(conn, changes, blast, opts)? {
        MatchOutcome::Early(report) => {
            if report.status == AffectedFlowsStatus::Unavailable {
                let detail = if report.notes.is_empty() {
                    "affected route matching unavailable".to_string()
                } else {
                    report.notes.join("; ")
                };
                return Err(miette::miette!(
                    "failed to match affected routes for --changed: {detail}"
                ));
            }
            // empty_map / missing_table / no_change_seeds → honest empty filter
            Ok(HashSet::new())
        }
        MatchOutcome::Matched(flows) => Ok(flows
            .into_iter()
            .map(|f| (f.method.to_uppercase(), f.path_pattern))
            .collect()),
    }
}

/// Compute affected HTTP flows for a change set.
///
/// Probe-first: returns honest status reports rather than inventing empty maps.
/// Blast is optional — kinds 1–3 still run when blast is `None` or empty.
///
/// Returns `Ok` with a status-bearing report for all probe outcomes. Callers that
/// lack a connection should use [`AffectedFlowsReport::unavailable`] directly.
///
/// **Cap:** `flows` is truncated to [`FLOWS_CAP`] for report payloads. Filter
/// consumers that need every matched route should use
/// [`match_affected_route_keys`] instead.
pub fn compute_affected_flows(
    conn: &Connection,
    changes: &[ChangedFile],
    blast: Option<&BlastRadius>,
    opts: &AffectedFlowsOpts,
) -> miette::Result<AffectedFlowsReport> {
    match match_affected_flows_uncapped(conn, changes, blast, opts)? {
        MatchOutcome::Early(report) => Ok(report),
        MatchOutcome::Matched(mut flows) => {
            let flow_total = flows.len();
            let flow_capped = flow_total > FLOWS_CAP;
            flows.truncate(FLOWS_CAP);
            let flow_count = flows.len();

            let mut notes = default_notes();
            notes.extend(collect_extra_notes(conn, opts));
            notes.sort();
            notes.dedup();

            Ok(AffectedFlowsReport {
                status: AffectedFlowsStatus::Available,
                flow_count,
                flow_capped,
                flow_total,
                flows,
                notes,
            })
        }
    }
}

/// Soft-open helper for PR path: existence-check only, never creates state.
pub fn compute_pr_affected_flows_soft(
    conn: Option<&Connection>,
    changes: &[ChangedFile],
    blast: Option<&BlastRadius>,
    opts: &AffectedFlowsOpts,
) -> AffectedFlowsReport {
    match conn {
        None => AffectedFlowsReport::unavailable(),
        Some(c) => compute_affected_flows(c, changes, blast, opts).unwrap_or_else(|e| {
            AffectedFlowsReport::with_status(
                AffectedFlowsStatus::Unavailable,
                vec![format!("affected_flows compute failed: {e}")],
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{BlastEdge, BlastRadius, ChangedFile, FileAnalysisStatus};
    use crate::index::symbols::{Symbol, SymbolKind};
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn setup_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn bare_conn() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    fn insert_file(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, 'Rust', 'h', 1, '2026-01-01T00:00:00Z')",
            [path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_symbol(conn: &Connection, file_id: i64, name: &str, qn: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, 'Function', '2026-01-01T00:00:00Z')",
            rusqlite::params![file_id, qn, name],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_route(
        conn: &Connection,
        method: &str,
        path: &str,
        handler_sym_id: Option<i64>,
        handler_name: Option<&str>,
        handler_file_id: i64,
        framework: &str,
        confidence: f64,
    ) {
        conn.execute(
            "INSERT INTO api_routes
             (method, path_pattern, handler_symbol_id, handler_symbol_name, handler_file_id,
              framework, route_source, is_dynamic, route_confidence, evidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'DECORATOR', 0, ?7, 'test', '2026-01-01T00:00:00Z')",
            rusqlite::params![
                method,
                path,
                handler_sym_id,
                handler_name,
                handler_file_id,
                framework,
                confidence
            ],
        )
        .unwrap();
    }

    fn change_with_symbol(path: &str, name: &str, qn: Option<&str>) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: Some(vec![Symbol {
                name: name.to_string(),
                kind: SymbolKind::Function,
                is_public: true,
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: None,
                line_end: None,
                qualified_name: qn.map(|s| s.to_string()),
                byte_start: None,
                byte_end: None,
                entrypoint_kind: None,
                metadata: Default::default(),
            }]),
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }
    }

    fn change_file_only(path: &str) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }
    }

    fn blast_edge(
        from_sym: &str,
        from_file: &str,
        to_sym: &str,
        to_file: &str,
        status: &str,
        evidence: &str,
    ) -> BlastEdge {
        let class = confidence_class(status, evidence);
        BlastEdge {
            hop: 1,
            direction: "caller".to_string(),
            from_symbol: from_sym.to_string(),
            from_file: from_file.to_string(),
            to_symbol: to_sym.to_string(),
            to_file: to_file.to_string(),
            resolution_status: status.to_string(),
            evidence: evidence.to_string(),
            confidence: Some(0.9),
            expandable: matches!(
                class,
                ConfidenceClass::ScipBound | ConfidenceClass::Resolved
            ),
            confidence_class: class,
        }
    }

    #[test]
    fn affected_flow_handler_symbol_by_id() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/users.rs");
        let sym = insert_symbol(&conn, impl_f, "get_users", "crate::handlers::get_users");
        insert_route(
            &conn,
            "GET",
            "/api/users",
            Some(sym),
            Some("get_users"),
            reg,
            "Axum",
            1.0,
        );
        // Unrelated route so map is non-empty even if match failed.
        let other = insert_file(&conn, "src/other.rs");
        let osym = insert_symbol(&conn, other, "health", "crate::health");
        insert_route(
            &conn,
            "GET",
            "/health",
            Some(osym),
            Some("health"),
            other,
            "Axum",
            1.0,
        );

        let changes = vec![change_with_symbol(
            "src/handlers/users.rs",
            "get_users",
            Some("crate::handlers::get_users"),
        )];
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(report.flow_count, 1);
        assert_eq!(report.flows[0].match_kind, MatchKind::HandlerSymbol);
        assert_eq!(report.flows[0].method, "GET");
        assert_eq!(report.flows[0].path_pattern, "/api/users");
        assert!(report.flows[0].confidence_class.is_none());
    }

    #[test]
    fn affected_flow_handler_symbol_null_id_name_impl_file() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/login.rs");
        let _sym = insert_symbol(&conn, impl_f, "login", "crate::handlers::login");
        // Null handler_symbol_id — name + impl file unique via project_symbols.
        insert_route(
            &conn,
            "POST",
            "/api/login",
            None,
            Some("login"),
            reg,
            "Axum",
            0.8,
        );

        let changes = vec![change_with_symbol(
            "src/handlers/login.rs",
            "login",
            Some("crate::handlers::login"),
        )];
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(report.flow_count, 1);
        assert_eq!(report.flows[0].match_kind, MatchKind::HandlerSymbol);
        assert_eq!(report.flows[0].path_pattern, "/api/login");
    }

    #[test]
    fn affected_flow_handler_impl_file() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/orders.rs");
        let sym = insert_symbol(
            &conn,
            impl_f,
            "create_order",
            "crate::handlers::create_order",
        );
        insert_route(
            &conn,
            "POST",
            "/api/orders",
            Some(sym),
            Some("create_order"),
            reg,
            "Axum",
            1.0,
        );

        // File change only — no symbols resolved to handler id → impl file match.
        let changes = vec![change_file_only("src/handlers/orders.rs")];
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(report.flow_count, 1);
        assert_eq!(report.flows[0].match_kind, MatchKind::HandlerImplFile);
        assert!(report.flows[0].confidence_class.is_none());
    }

    #[test]
    fn affected_flow_route_file_registration() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/ping.rs");
        let sym = insert_symbol(&conn, impl_f, "ping", "crate::handlers::ping");
        insert_route(
            &conn,
            "GET",
            "/ping",
            Some(sym),
            Some("ping"),
            reg,
            "Axum",
            1.0,
        );

        let changes = vec![change_file_only("src/router.rs")];
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(report.flow_count, 1);
        assert_eq!(report.flows[0].match_kind, MatchKind::RouteFile);
    }

    #[test]
    fn affected_flow_blast_symbol_with_class() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/checkout.rs");
        let caller_f = insert_file(&conn, "src/svc/pay.rs");
        let _ = caller_f;
        let sym = insert_symbol(
            &conn,
            impl_f,
            "checkout_handler",
            "crate::handlers::checkout_handler",
        );
        insert_route(
            &conn,
            "POST",
            "/checkout",
            Some(sym),
            Some("checkout_handler"),
            reg,
            "Axum",
            1.0,
        );

        // Change an unrelated file so direct kinds miss; blast attaches the handler.
        let changes = vec![change_file_only("src/svc/pay.rs")];
        let blast = BlastRadius {
            depth_requested: 1,
            depth_applied: 1,
            edges: vec![blast_edge(
                "pay_fn",
                "src/svc/pay.rs",
                "checkout_handler",
                "src/handlers/checkout.rs",
                "RESOLVED",
                "call_expr",
            )],
            ..Default::default()
        };
        let report =
            compute_affected_flows(&conn, &changes, Some(&blast), &AffectedFlowsOpts::default())
                .unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(report.flow_count, 1);
        assert_eq!(report.flows[0].match_kind, MatchKind::BlastSymbol);
        assert_eq!(
            report.flows[0].confidence_class,
            Some(ConfidenceClass::Resolved)
        );
    }

    #[test]
    fn affected_flow_blast_file_unique_handler() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/solo.rs");
        let sym = insert_symbol(&conn, impl_f, "solo_h", "crate::handlers::solo_h");
        insert_route(
            &conn,
            "GET",
            "/solo",
            Some(sym),
            Some("solo_h"),
            reg,
            "Axum",
            1.0,
        );

        let changes = vec![change_file_only("src/caller.rs")];
        let _ = insert_file(&conn, "src/caller.rs");
        // Edge file = impl file; symbol side is helper (not handler name) but
        // unique handler in that file → blast_file.
        let blast = BlastRadius {
            depth_requested: 1,
            depth_applied: 1,
            edges: vec![blast_edge(
                "helper",
                "src/caller.rs",
                "internal",
                "src/handlers/solo.rs",
                "RESOLVED",
                "scip:ref",
            )],
            ..Default::default()
        };
        let report =
            compute_affected_flows(&conn, &changes, Some(&blast), &AffectedFlowsOpts::default())
                .unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(report.flow_count, 1);
        assert_eq!(report.flows[0].match_kind, MatchKind::BlastFile);
        assert_eq!(
            report.flows[0].confidence_class,
            Some(ConfidenceClass::ScipBound)
        );
    }

    #[test]
    fn affected_flow_available_zero_flows() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/x.rs");
        let sym = insert_symbol(&conn, impl_f, "x", "crate::x");
        insert_route(&conn, "GET", "/x", Some(sym), Some("x"), reg, "Axum", 1.0);

        let changes = vec![change_file_only("src/unrelated.rs")];
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(report.flow_count, 0);
        assert_eq!(report.flow_total, 0);
        assert!(report.flows.is_empty());
        assert!(report.notes.iter().any(|n| n.contains("Registered HTTP")));
    }

    #[test]
    fn affected_flow_dedupe_doubles_keeps_best_kind() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/dup.rs");
        let sym = insert_symbol(&conn, impl_f, "dup_h", "crate::dup_h");
        // Two identical route rows (common re-index residue).
        insert_route(
            &conn,
            "GET",
            "/dup",
            Some(sym),
            Some("dup_h"),
            reg,
            "Axum",
            0.5,
        );
        insert_route(
            &conn,
            "GET",
            "/dup",
            Some(sym),
            Some("dup_h"),
            reg,
            "Axum",
            0.9,
        );

        let changes = vec![change_with_symbol(
            "src/handlers/dup.rs",
            "dup_h",
            Some("crate::dup_h"),
        )];
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(report.flow_count, 1);
        assert_eq!(report.flows[0].match_kind, MatchKind::HandlerSymbol);
        // Higher route_confidence preferred on kind tie (both handler_symbol).
        assert_eq!(report.flows[0].route_confidence, Some(0.9));
    }

    #[test]
    fn affected_flow_blast_absent_still_runs_direct() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/y.rs");
        let sym = insert_symbol(&conn, impl_f, "y_h", "crate::y_h");
        insert_route(&conn, "GET", "/y", Some(sym), Some("y_h"), reg, "Axum", 1.0);

        let changes = vec![change_file_only("src/router.rs")];
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.flow_count, 1);
        assert_eq!(report.flows[0].match_kind, MatchKind::RouteFile);
        assert!(report.flows[0].confidence_class.is_none());
    }

    #[test]
    fn affected_flow_class_only_on_blast_path() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/z.rs");
        let sym = insert_symbol(&conn, impl_f, "z_h", "crate::z_h");
        insert_route(&conn, "GET", "/z", Some(sym), Some("z_h"), reg, "Axum", 1.0);

        // Direct match must omit confidenceClass even if blast also would match.
        let changes = vec![change_with_symbol(
            "src/handlers/z.rs",
            "z_h",
            Some("crate::z_h"),
        )];
        let blast = BlastRadius {
            depth_requested: 1,
            depth_applied: 1,
            edges: vec![blast_edge(
                "z_h",
                "src/handlers/z.rs",
                "dep",
                "src/dep.rs",
                "RESOLVED",
                "scip:ref",
            )],
            ..Default::default()
        };
        let report =
            compute_affected_flows(&conn, &changes, Some(&blast), &AffectedFlowsOpts::default())
                .unwrap();
        assert_eq!(report.flows[0].match_kind, MatchKind::HandlerSymbol);
        assert!(
            report.flows[0].confidence_class.is_none(),
            "direct matches must not stamp confidenceClass"
        );
    }

    #[test]
    fn affected_flow_bare_name_fp_rejection() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/handlers/real.rs");
        let unrelated = insert_file(&conn, "src/other/unrelated.rs");
        let _ = unrelated;
        let sym = insert_symbol(&conn, impl_f, "handle", "crate::handlers::handle");
        insert_route(
            &conn,
            "GET",
            "/real",
            Some(sym),
            Some("handle"),
            reg,
            "Axum",
            1.0,
        );

        // Change unrelated file; blast edge uses common name "handle" on a different file.
        let changes = vec![change_file_only("src/other/unrelated.rs")];
        let blast = BlastRadius {
            depth_requested: 1,
            depth_applied: 1,
            edges: vec![blast_edge(
                "handle",
                "src/other/unrelated.rs",
                "new",
                "src/other/elsewhere.rs",
                "RESOLVED",
                "call_expr",
            )],
            ..Default::default()
        };
        let report =
            compute_affected_flows(&conn, &changes, Some(&blast), &AffectedFlowsOpts::default())
                .unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::Available);
        assert_eq!(
            report.flow_count, 0,
            "common symbol name on unrelated edge file must NOT attach handler: {:?}",
            report.flows
        );
    }

    #[test]
    fn affected_flow_missing_table_status() {
        let conn = bare_conn();
        let report = compute_affected_flows(
            &conn,
            &[change_file_only("src/x.rs")],
            None,
            &AffectedFlowsOpts::default(),
        )
        .unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::MissingTable);
    }

    #[test]
    fn affected_flow_empty_map_status() {
        let conn = setup_conn();
        let report = compute_affected_flows(
            &conn,
            &[change_file_only("src/x.rs")],
            None,
            &AffectedFlowsOpts::default(),
        )
        .unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::EmptyMap);
        assert_ne!(report.status, AffectedFlowsStatus::MissingTable);
    }

    #[test]
    fn affected_flow_no_change_seeds_status() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let impl_f = insert_file(&conn, "src/h.rs");
        let sym = insert_symbol(&conn, impl_f, "h", "crate::h");
        insert_route(&conn, "GET", "/h", Some(sym), Some("h"), reg, "Axum", 1.0);

        let report =
            compute_affected_flows(&conn, &[], None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.status, AffectedFlowsStatus::NoChangeSeeds);
    }

    #[test]
    fn affected_flow_unavailable_without_conn() {
        let report = compute_pr_affected_flows_soft(
            None,
            &[change_file_only("src/x.rs")],
            None,
            &AffectedFlowsOpts::default(),
        );
        assert_eq!(report.status, AffectedFlowsStatus::Unavailable);
        assert!(report.notes.iter().any(|n| n.contains("Registered HTTP")));
    }

    #[test]
    fn affected_flow_caps_at_20() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        for i in 0..25 {
            let f = insert_file(&conn, &format!("src/h{i:02}.rs"));
            let s = insert_symbol(&conn, f, &format!("h{i:02}"), &format!("crate::h{i:02}"));
            insert_route(
                &conn,
                "GET",
                &format!("/p{i:02}"),
                Some(s),
                Some(&format!("h{i:02}")),
                reg,
                "Axum",
                1.0,
            );
        }
        let changes = vec![change_file_only("src/router.rs")];
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert!(report.flow_capped);
        assert_eq!(report.flows.len(), FLOWS_CAP);
        assert_eq!(report.flow_total, 25);
        assert_eq!(report.flow_count, FLOWS_CAP);
    }

    /// Unavailable (probe/load fault) must Err for filter consumers — not
    /// silent empty that would claim "no endpoints changed".
    #[test]
    fn match_affected_route_keys_unavailable_returns_err() {
        // Minimal table so probe sees non-empty map, but load_routes SELECT
        // fails (wrong schema) → Unavailable Early → Err for filter callers.
        let conn = bare_conn();
        conn.execute_batch(
            "CREATE TABLE api_routes (id INTEGER PRIMARY KEY);
             INSERT INTO api_routes (id) VALUES (1);",
        )
        .expect("stub api_routes");

        let err = match_affected_route_keys(
            &conn,
            &[change_file_only("src/h.rs")],
            None,
            &AffectedFlowsOpts::default(),
        )
        .expect_err("unavailable must not become Ok(empty)");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed to match affected routes")
                || msg.contains("unavailable")
                || msg.contains("load failed"),
            "expected failure context, got: {msg}"
        );
    }

    /// Honest empty statuses stay Ok(empty) — not Err.
    #[test]
    fn match_affected_route_keys_empty_map_is_ok_empty() {
        let conn = setup_conn();
        let keys = match_affected_route_keys(
            &conn,
            &[change_file_only("src/x.rs")],
            None,
            &AffectedFlowsOpts::default(),
        )
        .expect("empty_map is honest empty, not Err");
        assert!(keys.is_empty());
    }

    /// Filter consumers (`endpoints --changed`) must see every matched route key
    /// even when the report payload is capped at FLOWS_CAP.
    #[test]
    fn match_affected_route_keys_uncapped_beyond_flows_cap() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        for i in 0..25 {
            let f = insert_file(&conn, &format!("src/h{i:02}.rs"));
            let s = insert_symbol(&conn, f, &format!("h{i:02}"), &format!("crate::h{i:02}"));
            insert_route(
                &conn,
                "GET",
                &format!("/p{i:02}"),
                Some(s),
                Some(&format!("h{i:02}")),
                reg,
                "Axum",
                1.0,
            );
        }
        let changes = vec![change_file_only("src/router.rs")];
        let keys = match_affected_route_keys(&conn, &changes, None, &AffectedFlowsOpts::default())
            .unwrap();
        assert_eq!(
            keys.len(),
            25,
            "filter keys must not inherit FLOWS_CAP truncate"
        );
        for i in 0..25 {
            assert!(
                keys.contains(&("GET".to_string(), format!("/p{i:02}"))),
                "missing key for /p{i:02}: {:?}",
                keys
            );
        }
        // Report path still caps for payload consumers.
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.flows.len(), FLOWS_CAP);
        assert_eq!(report.flow_total, 25);
        assert!(report.flow_capped);
    }

    #[test]
    fn affected_flow_sort_order_deterministic() {
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        let fa = insert_file(&conn, "src/a.rs");
        let fb = insert_file(&conn, "src/b.rs");
        let sa = insert_symbol(&conn, fa, "a_h", "crate::a_h");
        let sb = insert_symbol(&conn, fb, "b_h", "crate::b_h");
        insert_route(&conn, "POST", "/b", Some(sb), Some("b_h"), reg, "Axum", 1.0);
        insert_route(&conn, "GET", "/a", Some(sa), Some("a_h"), reg, "Axum", 1.0);

        let changes = vec![change_file_only("src/router.rs")];
        let r1 =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        let r2 =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        let j1 = serde_json::to_string(&r1).unwrap();
        let j2 = serde_json::to_string(&r2).unwrap();
        assert_eq!(j1, j2);
        assert_eq!(r1.flows[0].path_pattern, "/a");
        assert_eq!(r1.flows[1].path_pattern, "/b");
        assert!(j1.contains("\"status\":\"available\""));
        assert!(j1.contains("flowCount"));
        assert!(j1.contains("matchKind"));
    }

    #[test]
    fn affected_flow_handler_symbol_not_via_registration_file() {
        // Null-id name match must use impl file via project_symbols, not router.rs.
        let conn = setup_conn();
        let reg = insert_file(&conn, "src/router.rs");
        // Symbol lives in impl file; name is common.
        let impl_f = insert_file(&conn, "src/handlers/common.rs");
        let _ = insert_symbol(&conn, impl_f, "index", "crate::handlers::index");
        insert_route(&conn, "GET", "/", None, Some("index"), reg, "Axum", 1.0);

        // Changing only the registration file with a same-named symbol entry must
        // not produce handler_symbol via registration site (route_file is OK).
        let changes = vec![change_with_symbol("src/router.rs", "index", None)];
        // No project_symbols row for index@router.rs → not handler_symbol.
        let report =
            compute_affected_flows(&conn, &changes, None, &AffectedFlowsOpts::default()).unwrap();
        assert_eq!(report.flow_count, 1);
        assert_eq!(
            report.flows[0].match_kind,
            MatchKind::RouteFile,
            "null-id name must not bind via registration file: {:?}",
            report.flows[0]
        );
    }

    #[test]
    fn affected_flow_json_snake_status_camel_fields() {
        let report = AffectedFlowsReport::unavailable();
        let j = serde_json::to_value(&report).unwrap();
        assert_eq!(j["status"], "unavailable");
        assert!(j.get("flowCount").is_some());
        assert!(j.get("flow_count").is_none());
        assert!(j.get("matchKind").is_none()); // no flows
    }
}
