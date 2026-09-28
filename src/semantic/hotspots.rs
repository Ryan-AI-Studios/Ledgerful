use crate::state::storage_cozo::{CozoScriptOutcome, CozoStorage};
use crate::util::path::{display_path_under_work_root, path_is_under_work_root};
use cozo::{DataValue, Num};
use miette::Result;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Max snippets on one left-file page before windowing (0423).
pub const SEMANTIC_PAGE_SNIPPETS: usize = 64;

/// Local stop for the semantic self-join. Mapped to `CompletenessStop` in list.rs.
/// Do not import `crate::impact` from this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticStop {
    Budget,
    Cancelled,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SemanticMatch {
    pub file1: String,
    pub name1: String,
    pub offset1: usize,
    pub file2: String,
    pub name2: String,
    pub offset2: usize,
    pub similarity: f32,
}

/// Same Instant rules as `poll_overall_stop` (cancel wins, then deadline).
/// Kept local so `src/semantic/` does not depend on `crate::impact`.
fn semantic_poll_stop(deadline: Option<Instant>, cancel: &AtomicBool) -> Option<SemanticStop> {
    if cancel.load(Ordering::Relaxed) {
        Some(SemanticStop::Cancelled)
    } else if deadline.is_some_and(|d| Instant::now() >= d) {
        Some(SemanticStop::Budget)
    } else {
        None
    }
}

/// Per-page Cozo `:timeout` suffix.
///
/// `:timeout 0` only when `deadline` is `None` (unbounded). `deadline` Some +
/// remaining `<= 0` is `Budget` and must not emit `:timeout 0`.
pub(crate) fn page_timeout_clause(
    deadline: Option<Instant>,
    now: Instant,
) -> std::result::Result<String, SemanticStop> {
    match deadline {
        None => Ok(":timeout 0".to_string()),
        Some(d) => {
            let remaining = d
                .checked_duration_since(now)
                .map(|r| r.as_secs_f64())
                .unwrap_or(0.0);
            if remaining <= 0.0 {
                Err(SemanticStop::Budget)
            } else {
                Ok(format!(":timeout {}", remaining.max(0.001)))
            }
        }
    }
}

fn timeout_or_budget(deadline: Option<Instant>) -> std::result::Result<String, SemanticStop> {
    page_timeout_clause(deadline, Instant::now())
}

pub(crate) fn distinct_files_script(timeout: &str) -> String {
    format!("?[file_path] := *snippet_embedding{{file_path}} {timeout}")
}

#[cfg(test)]
pub(crate) fn snippet_keys_script(timeout: &str) -> String {
    format!(
        "?[name, line_offset] := *snippet_embedding{{file_path: $left, name, line_offset}} :order +name, +line_offset :limit {SEMANTIC_PAGE_SNIPPETS} {timeout}"
    )
}

#[cfg(test)]
pub(crate) fn snippet_keys_after_script(timeout: &str) -> String {
    format!(
        "?[name, line_offset] := *snippet_embedding{{file_path: $left, name, line_offset}}, name > $after_name
        ?[name, line_offset] := *snippet_embedding{{file_path: $left, name, line_offset}}, name = $after_name, line_offset > $after_off
        :order +name, +line_offset :limit {SEMANTIC_PAGE_SNIPPETS} {timeout}"
    )
}

#[cfg(test)]
pub(crate) fn same_file_page_script(timeout: &str) -> String {
    format!(
        "?[f1, n1, o1, f2, n2, o2, similarity] :=
            *snippet_embedding{{file_path: f1, name: n1, line_offset: o1, embedding: v1}},
            *snippet_embedding{{file_path: f2, name: n2, line_offset: o2, embedding: v2}},
            f1 = $left,
            f2 = $left,
            o1 < o2,
            dist = cos_dist(v1, v2),
            similarity = 1.0 - dist,
            similarity > $threshold
        {timeout}"
    )
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn cross_file_page_script(timeout: &str) -> String {
    format!(
        "?[f1, n1, o1, f2, n2, o2, similarity] :=
            *snippet_embedding{{file_path: f1, name: n1, line_offset: o1, embedding: v1}},
            *snippet_embedding{{file_path: f2, name: n2, line_offset: o2, embedding: v2}},
            f1 = $left,
            f2 = $right,
            dist = cos_dist(v1, v2),
            similarity = 1.0 - dist,
            similarity > $threshold
        {timeout}"
    )
}

#[cfg(test)]
pub(crate) fn same_file_window_script(timeout: &str) -> String {
    format!(
        "left_win[n, o] <- $left_window
        right_win[n, o] <- $right_window
        ?[f1, n1, o1, f2, n2, o2, similarity] :=
            left_win[n1, o1],
            right_win[n2, o2],
            *snippet_embedding{{file_path: f1, name: n1, line_offset: o1, embedding: v1}},
            *snippet_embedding{{file_path: f2, name: n2, line_offset: o2, embedding: v2}},
            f1 = $left,
            f2 = $left,
            o1 < o2,
            dist = cos_dist(v1, v2),
            similarity = 1.0 - dist,
            similarity > $threshold
        {timeout}"
    )
}

#[cfg(test)]
pub(crate) fn cross_file_window_script(timeout: &str) -> String {
    format!(
        "left_win[n, o] <- $left_window
        right_win[n, o] <- $right_window
        ?[f1, n1, o1, f2, n2, o2, similarity] :=
            left_win[n1, o1],
            right_win[n2, o2],
            *snippet_embedding{{file_path: f1, name: n1, line_offset: o1, embedding: v1}},
            *snippet_embedding{{file_path: f2, name: n2, line_offset: o2, embedding: v2}},
            f1 = $left,
            f2 = $right,
            dist = cos_dist(v1, v2),
            similarity = 1.0 - dist,
            similarity > $threshold
        {timeout}"
    )
}

#[cfg(test)]
fn threshold_params(left: &str, threshold: f32) -> BTreeMap<String, DataValue> {
    let mut params = BTreeMap::new();
    params.insert("left".to_string(), DataValue::from(left));
    params.insert(
        "threshold".to_string(),
        DataValue::from(f64::from(threshold)),
    );
    params
}

#[cfg(test)]
fn pair_params(left: &str, right: &str, threshold: f32) -> BTreeMap<String, DataValue> {
    let mut params = threshold_params(left, threshold);
    params.insert("right".to_string(), DataValue::from(right));
    params
}

#[cfg(test)]
fn snippet_windows(keys: &[(String, i64)]) -> Vec<&[(String, i64)]> {
    if keys.is_empty() {
        Vec::new()
    } else {
        keys.chunks(SEMANTIC_PAGE_SNIPPETS).collect()
    }
}

#[cfg(test)]
fn keys_to_datavalue(keys: &[(String, i64)]) -> DataValue {
    DataValue::List(Box::new(
        keys.iter()
            .map(|(n, o)| {
                DataValue::List(Box::new(vec![
                    DataValue::from(n.as_str()),
                    DataValue::from(*o),
                ]))
            })
            .collect(),
    ))
}

#[cfg(test)]
fn parse_match_row(row: &[DataValue]) -> Option<SemanticMatch> {
    let (
        Some(DataValue::Str(f1)),
        Some(DataValue::Str(n1)),
        Some(DataValue::Num(Num::Int(o1))),
        Some(DataValue::Str(f2)),
        Some(DataValue::Str(n2)),
        Some(DataValue::Num(Num::Int(o2))),
        Some(DataValue::Num(num)),
    ) = (
        row.first(),
        row.get(1),
        row.get(2),
        row.get(3),
        row.get(4),
        row.get(5),
        row.get(6),
    )
    else {
        return None;
    };
    let sim = match num {
        Num::Float(f) => *f as f32,
        Num::Int(i) => *i as f32,
    };
    Some(SemanticMatch {
        file1: f1.to_string(),
        name1: n1.to_string(),
        offset1: *o1 as usize,
        file2: f2.to_string(),
        name2: n2.to_string(),
        offset2: *o2 as usize,
        similarity: sim,
    })
}

#[cfg(test)]
fn parse_match_rows<R>(rows: impl IntoIterator<Item = R>) -> Vec<SemanticMatch>
where
    R: AsRef<[DataValue]>,
{
    rows.into_iter()
        .filter_map(|row| parse_match_row(row.as_ref()))
        .collect()
}

fn finalize_matches(mut results: Vec<SemanticMatch>, work_root: &Path) -> Vec<SemanticMatch> {
    results.retain(|m| {
        path_is_under_work_root(work_root, &m.file1) && path_is_under_work_root(work_root, &m.file2)
    });
    for m in &mut results {
        if let Some(rel) = display_path_under_work_root(work_root, &m.file1) {
            m.file1 = rel;
        }
        if let Some(rel) = display_path_under_work_root(work_root, &m.file2) {
            m.file2 = rel;
        }
    }
    results.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file1.cmp(&b.file1))
            .then_with(|| a.file2.cmp(&b.file2))
            .then_with(|| a.name1.cmp(&b.name1))
            .then_with(|| a.name2.cmp(&b.name2))
    });
    results
}

#[cfg(test)]
fn run_join_page(
    storage: &CozoStorage,
    script: &str,
    params: BTreeMap<String, DataValue>,
) -> Result<std::result::Result<Vec<SemanticMatch>, SemanticStop>> {
    match storage.run_script_classifying_kill(script, params)? {
        CozoScriptOutcome::Rows(res) => Ok(Ok(parse_match_rows(res.rows))),
        CozoScriptOutcome::Killed => Ok(Err(SemanticStop::Budget)),
    }
}

fn list_distinct_files(
    storage: &CozoStorage,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<std::result::Result<Vec<String>, SemanticStop>> {
    if let Some(stop) = semantic_poll_stop(deadline, cancel) {
        return Ok(Err(stop));
    }
    let timeout = match timeout_or_budget(deadline) {
        Ok(t) => t,
        Err(stop) => return Ok(Err(stop)),
    };
    let script = distinct_files_script(&timeout);
    match storage.run_script_classifying_kill(&script, BTreeMap::new())? {
        CozoScriptOutcome::Killed => Ok(Err(SemanticStop::Budget)),
        CozoScriptOutcome::Rows(res) => {
            let mut files = Vec::new();
            for row in res.rows {
                if let Some(DataValue::Str(fp)) = row.first() {
                    files.push(fp.to_string());
                }
            }
            files.sort();
            files.dedup();
            Ok(Ok(files))
        }
    }
}

#[cfg(test)]
fn list_snippet_keys(
    storage: &CozoStorage,
    left: &str,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<std::result::Result<Vec<(String, i64)>, SemanticStop>> {
    let mut keys: Vec<(String, i64)> = Vec::new();
    loop {
        if let Some(stop) = semantic_poll_stop(deadline, cancel) {
            return Ok(Err(stop));
        }
        let timeout = match timeout_or_budget(deadline) {
            Ok(t) => t,
            Err(stop) => return Ok(Err(stop)),
        };
        let mut params = BTreeMap::new();
        params.insert("left".to_string(), DataValue::from(left));
        let script = if let Some((name, off)) = keys.last() {
            params.insert("after_name".to_string(), DataValue::from(name.as_str()));
            params.insert("after_off".to_string(), DataValue::from(*off));
            snippet_keys_after_script(&timeout)
        } else {
            snippet_keys_script(&timeout)
        };
        match storage.run_script_classifying_kill(&script, params)? {
            CozoScriptOutcome::Killed => return Ok(Err(SemanticStop::Budget)),
            CozoScriptOutcome::Rows(res) => {
                let before = keys.len();
                for row in res.rows {
                    if let (Some(DataValue::Str(name)), Some(DataValue::Num(Num::Int(off)))) =
                        (row.first(), row.get(1))
                    {
                        keys.push((name.to_string(), *off));
                    }
                }
                let added = keys.len() - before;
                if added < SEMANTIC_PAGE_SNIPPETS {
                    break;
                }
                // A deadline-bound scan must not walk `keys_after` on a large
                // file: that cursor is one uninterruptible Rule 0. First page
                // only; caller treats a full page as truncated → Budget.
                if deadline.is_some() {
                    break;
                }
            }
        }
    }
    Ok(Ok(keys))
}

/// Page already-listed left files. Used for between-page Instant tests.
///
/// Cross-file pages bind one later `$right` at a time (and window both
/// sides at 64). A single `f1 < f2` against all later files is still one
/// uninterruptible Rule 0 and overruns EXEC.
#[cfg(test)]
pub(crate) fn page_semantic_hotspots(
    storage: &CozoStorage,
    work_root: &Path,
    threshold: f32,
    files: &[String],
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<(Vec<SemanticMatch>, Option<SemanticStop>)> {
    let mut acc = Vec::new();
    let mut keys_cache: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    let mut keys_truncated = false;
    for (i, left) in files.iter().enumerate() {
        if let Some(stop) = semantic_poll_stop(deadline, cancel) {
            return Ok((finalize_matches(acc, work_root), Some(stop)));
        }
        let left_keys = match cached_snippet_keys(storage, left, deadline, cancel, &mut keys_cache)?
        {
            Ok(k) => k,
            Err(stop) => return Ok((finalize_matches(acc, work_root), Some(stop))),
        };
        if deadline.is_some() && left_keys.len() == SEMANTIC_PAGE_SNIPPETS {
            keys_truncated = true;
        }
        match run_same_file_pages(storage, left, threshold, &left_keys, deadline, cancel)? {
            Ok(rows) => acc.extend(rows),
            Err(stop) => return Ok((finalize_matches(acc, work_root), Some(stop))),
        }
        for right in files.iter().skip(i + 1) {
            if let Some(stop) = semantic_poll_stop(deadline, cancel) {
                return Ok((finalize_matches(acc, work_root), Some(stop)));
            }
            let right_keys =
                match cached_snippet_keys(storage, right, deadline, cancel, &mut keys_cache)? {
                    Ok(k) => k,
                    Err(stop) => return Ok((finalize_matches(acc, work_root), Some(stop))),
                };
            if deadline.is_some() && right_keys.len() == SEMANTIC_PAGE_SNIPPETS {
                keys_truncated = true;
            }
            match run_cross_file_pages(
                storage,
                left,
                &left_keys,
                right,
                &right_keys,
                threshold,
                deadline,
                cancel,
            )? {
                Ok(rows) => acc.extend(rows),
                Err(stop) => return Ok((finalize_matches(acc, work_root), Some(stop))),
            }
        }
    }
    let stop = keys_truncated.then_some(SemanticStop::Budget);
    Ok((finalize_matches(acc, work_root), stop))
}

#[cfg(test)]
fn cached_snippet_keys(
    storage: &CozoStorage,
    path: &str,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
    cache: &mut BTreeMap<String, Vec<(String, i64)>>,
) -> Result<std::result::Result<Vec<(String, i64)>, SemanticStop>> {
    if let Some(existing) = cache.get(path) {
        return Ok(Ok(existing.clone()));
    }
    match list_snippet_keys(storage, path, deadline, cancel)? {
        Ok(keys) => {
            cache.insert(path.to_string(), keys.clone());
            Ok(Ok(keys))
        }
        Err(stop) => Ok(Err(stop)),
    }
}

#[cfg(test)]
fn run_same_file_pages(
    storage: &CozoStorage,
    left: &str,
    threshold: f32,
    keys: &[(String, i64)],
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<std::result::Result<Vec<SemanticMatch>, SemanticStop>> {
    // Always bind listed keys. The `$left`-only same-file script joins every
    // snippet in the file, so a first page of 64 keys on a large EXEC file
    // would still run one uninterruptible full-file cosine.
    let mut acc = Vec::new();
    let windows = snippet_windows(keys);
    // Full window×window product. Triangular skip(i) follows name order
    // (`:order +name, +line_offset`), not `o1 < o2`, and drops valid
    // same-file pairs when names are not monotonic with offsets.
    for left_win in &windows {
        for right_win in &windows {
            if let Some(stop) = semantic_poll_stop(deadline, cancel) {
                return Ok(Err(stop));
            }
            let timeout = match timeout_or_budget(deadline) {
                Ok(t) => t,
                Err(stop) => return Ok(Err(stop)),
            };
            let mut params = threshold_params(left, threshold);
            params.insert("left_window".to_string(), keys_to_datavalue(left_win));
            params.insert("right_window".to_string(), keys_to_datavalue(right_win));
            match run_join_page(storage, &same_file_window_script(&timeout), params)? {
                Ok(rows) => acc.extend(rows),
                Err(stop) => return Ok(Err(stop)),
            }
        }
    }
    Ok(Ok(acc))
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn run_cross_file_pages(
    storage: &CozoStorage,
    left: &str,
    left_keys: &[(String, i64)],
    right: &str,
    right_keys: &[(String, i64)],
    threshold: f32,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<std::result::Result<Vec<SemanticMatch>, SemanticStop>> {
    // Always bind listed keys. `$left`×`$right` without windows joins every
    // snippet in both files (DoD-1 hang on the first large EXEC pair).
    let mut acc = Vec::new();
    for left_win in snippet_windows(left_keys) {
        for right_win in snippet_windows(right_keys) {
            if let Some(stop) = semantic_poll_stop(deadline, cancel) {
                return Ok(Err(stop));
            }
            let timeout = match timeout_or_budget(deadline) {
                Ok(t) => t,
                Err(stop) => return Ok(Err(stop)),
            };
            let mut params = pair_params(left, right, threshold);
            params.insert("left_window".to_string(), keys_to_datavalue(left_win));
            params.insert("right_window".to_string(), keys_to_datavalue(right_win));
            match run_join_page(storage, &cross_file_window_script(&timeout), params)? {
                Ok(rows) => acc.extend(rows),
                Err(stop) => return Ok(Err(stop)),
            }
        }
    }
    Ok(Ok(acc))
}

/// Max owned snippets in one parallel scan task (0446). Shrink to 16, then 8,
/// if an in-pool task measures > 1s.
pub(crate) const SEMANTIC_SCAN_CHUNK: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct LoadedSnippet {
    name: String,
    line_offset: i64,
    embedding: Vec<f32>,
    /// f32 self-dot cast to f64 (`op_cos_dist` `a.dot(a) as f64`).
    norm: f64,
}

type LoadedStore = BTreeMap<String, Vec<LoadedSnippet>>;

fn file_snippets_script(timeout: &str) -> String {
    format!(
        "?[name, line_offset, embedding] := *snippet_embedding{{file_path: $left, name, line_offset, embedding}} {timeout}"
    )
}

fn embedding_f32(v: &cozo::Vector) -> Option<Vec<f32>> {
    match v {
        cozo::Vector::F32(vec) => Some(vec.to_vec()),
        cozo::Vector::F64(_) => None,
    }
}

fn f32_self_dot(emb: &[f32]) -> f32 {
    let mut acc: f32 = 0.0;
    for &x in emb {
        acc += x * x;
    }
    acc
}

fn snippet_is_usable(emb: &[f32]) -> bool {
    if emb.is_empty() {
        return false;
    }
    let mag_sq = f32_self_dot(emb);
    mag_sq.is_finite() && mag_sq > 0.0 && emb.iter().all(|x| x.is_finite())
}

fn op_cos_sim(left: &LoadedSnippet, right: &LoadedSnippet) -> Option<f64> {
    if left.embedding.len() != right.embedding.len() {
        return None;
    }
    let mut dot: f32 = 0.0;
    for i in 0..left.embedding.len() {
        dot += left.embedding[i] * right.embedding[i];
    }
    let denom = (left.norm * right.norm).sqrt();
    if denom == 0.0 || !denom.is_finite() {
        return None;
    }
    let sim = f64::from(dot) / denom;
    sim.is_finite().then_some(sim)
}

fn push_if_similar(
    acc: &mut Vec<SemanticMatch>,
    file1: &str,
    left: &LoadedSnippet,
    file2: &str,
    right: &LoadedSnippet,
    threshold: f32,
) {
    let Some(sim) = op_cos_sim(left, right) else {
        return;
    };
    if sim > f64::from(threshold) {
        acc.push(SemanticMatch {
            file1: file1.to_string(),
            name1: left.name.clone(),
            offset1: left.line_offset as usize,
            file2: file2.to_string(),
            name2: right.name.clone(),
            offset2: right.line_offset as usize,
            similarity: sim as f32,
        });
    }
}

fn load_file_snippets(
    storage: &CozoStorage,
    path: &str,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<std::result::Result<Vec<LoadedSnippet>, SemanticStop>> {
    if let Some(stop) = semantic_poll_stop(deadline, cancel) {
        return Ok(Err(stop));
    }
    let timeout = match timeout_or_budget(deadline) {
        Ok(t) => t,
        Err(stop) => return Ok(Err(stop)),
    };
    let mut params = BTreeMap::new();
    params.insert("left".to_string(), DataValue::from(path));
    match storage.run_script_classifying_kill(&file_snippets_script(&timeout), params)? {
        CozoScriptOutcome::Killed => Ok(Err(SemanticStop::Budget)),
        CozoScriptOutcome::Rows(res) => {
            let mut snippets = Vec::new();
            for row in res.rows {
                let (
                    Some(DataValue::Str(name)),
                    Some(DataValue::Num(Num::Int(off))),
                    Some(DataValue::Vec(v)),
                ) = (row.first(), row.get(1), row.get(2))
                else {
                    continue;
                };
                let Some(emb) = embedding_f32(v) else {
                    continue;
                };
                if !snippet_is_usable(&emb) {
                    continue;
                }
                let norm = f64::from(f32_self_dot(&emb));
                snippets.push(LoadedSnippet {
                    name: name.to_string(),
                    line_offset: *off,
                    embedding: emb,
                    norm,
                });
            }
            Ok(Ok(snippets))
        }
    }
}

/// Paged per-file embedding load. A stop returns an empty map and does not
/// start the pool.
pub(crate) fn load_semantic_embeddings(
    storage: &CozoStorage,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<(LoadedStore, Option<SemanticStop>)> {
    if let Some(stop) = semantic_poll_stop(deadline, cancel) {
        return Ok((BTreeMap::new(), Some(stop)));
    }
    let files = match list_distinct_files(storage, deadline, cancel)? {
        Ok(f) => f,
        Err(stop) => return Ok((BTreeMap::new(), Some(stop))),
    };
    let mut by_file = BTreeMap::new();
    for path in files {
        match load_file_snippets(storage, &path, deadline, cancel)? {
            Ok(snippets) => {
                by_file.insert(path, snippets);
            }
            Err(stop) => return Ok((BTreeMap::new(), Some(stop))),
        }
    }
    Ok((by_file, None))
}

fn scan_thread_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1)
}

fn compare_owned_chunk(
    files: &[String],
    by_file: &LoadedStore,
    file_idx: usize,
    owned: &[LoadedSnippet],
    threshold: f32,
) -> Vec<SemanticMatch> {
    let left_path = &files[file_idx];
    let Some(left_all) = by_file.get(left_path) else {
        return Vec::new();
    };
    let mut acc = Vec::new();
    for left in owned {
        for right in left_all {
            if left.line_offset < right.line_offset {
                push_if_similar(&mut acc, left_path, left, left_path, right, threshold);
            }
        }
        for right_path in files.iter().skip(file_idx + 1) {
            let Some(right_all) = by_file.get(right_path) else {
                continue;
            };
            for right in right_all {
                push_if_similar(&mut acc, left_path, left, right_path, right, threshold);
            }
        }
    }
    acc
}

/// Exact pairs on a private Rayon pool. `Budget` means a task skipped its
/// pairs; the clock is not read after `install` returns.
pub(crate) fn scan_semantic_pairs(
    by_file: &LoadedStore,
    threshold: f32,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
    chunk: usize,
) -> Result<(Vec<SemanticMatch>, Option<SemanticStop>)> {
    let files: Vec<String> = by_file.keys().cloned().collect();
    let chunk = chunk.max(1);
    let mut tasks: Vec<(usize, usize, usize)> = Vec::new();
    for (file_idx, path) in files.iter().enumerate() {
        let n = by_file.get(path).map(Vec::len).unwrap_or(0);
        let mut start = 0usize;
        while start < n {
            let end = (start + chunk).min(n);
            tasks.push((file_idx, start, end));
            start = end;
        }
    }
    let n_threads = scan_thread_count();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(n_threads)
        .build()
        .map_err(|e| miette::miette!("Failed to build semantic hotspot Rayon pool: {e}"))?;
    let skipped = Arc::new(AtomicBool::new(false));
    let matches = pool.install(|| {
        tasks
            .into_par_iter()
            .flat_map(|(file_idx, start, end)| {
                if cancel.load(Ordering::Relaxed) {
                    skipped.store(true, Ordering::Relaxed);
                    return Vec::new();
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    skipped.store(true, Ordering::Relaxed);
                    return Vec::new();
                }
                let path = &files[file_idx];
                let Some(snips) = by_file.get(path) else {
                    return Vec::new();
                };
                compare_owned_chunk(
                    files.as_slice(),
                    by_file,
                    file_idx,
                    &snips[start..end],
                    threshold,
                )
            })
            .collect::<Vec<SemanticMatch>>()
    });
    let stop = if cancel.load(Ordering::Relaxed) {
        Some(SemanticStop::Cancelled)
    } else if skipped.load(Ordering::Relaxed) {
        Some(SemanticStop::Budget)
    } else {
        None
    };
    Ok((matches, stop))
}

/// Find high-similarity snippet pairs under `work_root` (0152 B2-H).
///
/// Loads embeddings per file, then exact-cosines on a private Rayon pool.
/// Drops pairs where either path fails under-work-root (legacy absolute
/// foreign). Absolute-under-root legacy keys are rewritten to relative
/// for display.
pub fn find_semantic_hotspots(
    storage: &CozoStorage,
    work_root: &Path,
    threshold: f32,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> Result<(Vec<SemanticMatch>, Option<SemanticStop>)> {
    if let Some(stop) = semantic_poll_stop(deadline, cancel) {
        return Ok((Vec::new(), Some(stop)));
    }
    let (loaded, load_stop) = load_semantic_embeddings(storage, deadline, cancel)?;
    if let Some(stop) = load_stop {
        return Ok((Vec::new(), Some(stop)));
    }
    let (matches, scan_stop) =
        scan_semantic_pairs(&loaded, threshold, deadline, cancel, SEMANTIC_SCAN_CHUNK)?;
    Ok((finalize_matches(matches, work_root), scan_stop))
}

/// Legacy two-rule script (pre-0423). Tests only — pair-set equality.
#[cfg(test)]
pub(crate) fn find_semantic_hotspots_single_script(
    storage: &CozoStorage,
    work_root: &Path,
    threshold: f32,
) -> Result<Vec<SemanticMatch>> {
    let script = format!(
        "?[f1, n1, o1, f2, n2, o2, similarity] :=
            *snippet_embedding{{file_path: f1, name: n1, line_offset: o1, embedding: v1}},
            *snippet_embedding{{file_path: f2, name: n2, line_offset: o2, embedding: v2}},
            f1 < f2,
            dist = cos_dist(v1, v2),
            similarity = 1.0 - dist,
            similarity > {threshold}
        ?[f1, n1, o1, f2, n2, o2, similarity] :=
            *snippet_embedding{{file_path: f1, name: n1, line_offset: o1, embedding: v1}},
            *snippet_embedding{{file_path: f2, name: n2, line_offset: o2, embedding: v2}},
            f1 == f2,
            o1 < o2,
            dist = cos_dist(v1, v2),
            similarity = 1.0 - dist,
            similarity > {threshold}",
        threshold = threshold
    );
    let res = storage.run_script(&script)?;
    Ok(finalize_matches(parse_match_rows(res.rows), work_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::storage_cozo::is_cozo_eval_killed;
    use cozo::ScriptMutability;
    use std::time::Duration;

    fn plant(storage: &CozoStorage, file_path: &str, name: &str, offset: i64, embedding: Vec<f32>) {
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        let emb: Vec<f32> = embedding.iter().map(|x| x / norm).collect();
        let mut params = BTreeMap::new();
        params.insert(
            "data".to_string(),
            DataValue::from(vec![DataValue::List(Box::new(vec![
                DataValue::from(file_path),
                DataValue::from(name),
                DataValue::from(offset),
                DataValue::Vec(Box::new(cozo::Vector::F32(emb.into()))),
            ]))]),
        );
        storage
            .run_script(
                ":create snippet_embedding {file_path,name,line_offset=>embedding:<F32; 3>}",
            )
            .ok();
        storage
            .run_script_with_params(
                "?[file_path, name, line_offset, embedding] <- $data :put snippet_embedding",
                params,
                ScriptMutability::Mutable,
            )
            .expect("plant");
    }

    fn live_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn match_key(m: &SemanticMatch) -> (String, String, String, String, usize, usize) {
        (
            m.file1.clone(),
            m.file2.clone(),
            m.name1.clone(),
            m.name2.clone(),
            m.offset1,
            m.offset2,
        )
    }

    #[test]
    fn hotspots_drop_foreign_absolute_pairs() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let dir_a = tempfile::tempdir().expect("A");
        let dir_b = tempfile::tempdir().expect("B");
        let root_a = dir_a.path();
        let poison_b = dir_b
            .path()
            .join("poison.rs")
            .to_string_lossy()
            .replace('\\', "/");

        plant(&storage, "src/a.rs", "fn_a", 0, vec![1.0, 0.0, 0.0]);
        plant(&storage, "src/b.rs", "fn_b", 0, vec![1.0, 0.0, 0.0]);
        plant(&storage, &poison_b, "fn_poison", 0, vec![1.0, 0.0, 0.0]);

        let cancel = live_cancel();
        let (matches, stop) =
            find_semantic_hotspots(&storage, root_a, 0.5, None, &cancel).expect("hotspots");
        assert!(stop.is_none(), "finished plant must omit stop: {stop:?}");
        assert!(
            matches
                .iter()
                .all(|m| !m.file1.contains("poison") && !m.file2.contains("poison")),
            "foreign absolute pairs must be dropped: {matches:?}"
        );
        assert!(
            matches
                .iter()
                .any(|m| (m.file1 == "src/a.rs" && m.file2 == "src/b.rs")
                    || (m.file1 == "src/b.rs" && m.file2 == "src/a.rs")),
            "relative under-root pair should remain: {matches:?}"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__script__timeout_zero_only_when_unbounded() {
        let now = Instant::now();
        assert_eq!(
            page_timeout_clause(None, now).expect("unbounded"),
            ":timeout 0"
        );
        let future = now + Duration::from_secs(5);
        let clause = page_timeout_clause(Some(future), now).expect("bounded");
        assert_ne!(clause, ":timeout 0");
        assert!(
            clause.starts_with(":timeout "),
            "positive remaining must emit a timeout: {clause}"
        );
        let secs: f64 = clause
            .strip_prefix(":timeout ")
            .expect("prefix")
            .parse()
            .expect("secs");
        assert!(secs > 0.0, "bounded remaining must be positive: {clause}");
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__script__clamps_positive_remaining() {
        let now = Instant::now();
        let tiny = now + Duration::from_nanos(100);
        let clause = page_timeout_clause(Some(tiny), now).expect("tiny remaining");
        let secs: f64 = clause
            .strip_prefix(":timeout ")
            .expect("prefix")
            .parse()
            .expect("secs");
        assert!(
            (secs - 0.001).abs() < 1e-9,
            "remaining < 0.001 must clamp to 0.001, got {clause}"
        );
        assert_ne!(clause, ":timeout 0");
        assert!(!same_file_page_script(&clause).contains(":timeout 0\n"));
        assert!(same_file_page_script(&clause).contains(":timeout 0.001"));
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__deadline_some_zero_remaining__budget_no_cozo() {
        let now = Instant::now();
        let past = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
        assert_eq!(
            page_timeout_clause(Some(past), now),
            Err(SemanticStop::Budget)
        );
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let root = tempfile::tempdir().expect("root");
        let cancel = live_cancel();
        let (matches, stop) =
            find_semantic_hotspots(&storage, root.path(), 0.85, Some(past), &cancel)
                .expect("budget");
        assert!(
            matches.is_empty(),
            "elapsed Instant must not scan: {matches:?}"
        );
        assert_eq!(stop, Some(SemanticStop::Budget));
    }

    #[test]
    #[allow(non_snake_case)]
    fn classify_cozo_kill__eval_killed__budget() {
        let raw = miette::miette!(
            code = "eval::killed",
            "Running query is killed before completion"
        );
        assert_eq!(
            raw.code().map(|c| c.to_string()).as_deref(),
            Some("eval::killed"),
            "raw report must carry eval::killed"
        );
        assert!(
            is_cozo_eval_killed(&raw),
            "raw eval::killed must classify as kill"
        );
        let wrapped = miette::miette!(
            "CozoDB script error: {:?}. Script was: '{}'",
            raw,
            "?[x] := *snippet_embedding{file_path}"
        );
        assert!(
            wrapped.code().is_none(),
            "wrapped script-echo must not keep eval::killed"
        );
        assert!(
            !is_cozo_eval_killed(&wrapped),
            "wrapped script-echo must not be the classifier input: {wrapped}"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__expired_instant__budget_stop() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let dir_a = tempfile::tempdir().expect("A");
        let root_a = dir_a.path();
        plant(&storage, "src/a.rs", "fn_a", 0, vec![1.0, 0.0, 0.0]);
        plant(&storage, "src/b.rs", "fn_b", 0, vec![1.0, 0.0, 0.0]);
        let expired = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        let cancel = live_cancel();

        let (entry_matches, entry_stop) =
            find_semantic_hotspots(&storage, root_a, 0.5, Some(expired), &cancel).expect("entry");
        assert!(
            entry_matches.is_empty(),
            "entry poll must not scan: {entry_matches:?}"
        );
        assert_eq!(entry_stop, Some(SemanticStop::Budget));

        let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let (page_matches, page_stop) =
            page_semantic_hotspots(&storage, root_a, 0.5, &files, Some(expired), &cancel)
                .expect("between-page");
        assert!(
            page_matches.is_empty(),
            "between-page poll must not start the next join: {page_matches:?}"
        );
        assert_eq!(page_stop, Some(SemanticStop::Budget));
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__paged_complete__equals_single_script() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let dir_a = tempfile::tempdir().expect("A");
        let dir_b = tempfile::tempdir().expect("B");
        let root_a = dir_a.path();
        let poison_b = dir_b
            .path()
            .join("poison.rs")
            .to_string_lossy()
            .replace('\\', "/");

        plant(&storage, "src/a.rs", "fn_a", 0, vec![1.0, 0.0, 0.0]);
        plant(&storage, "src/a.rs", "fn_a2", 1, vec![1.0, 0.0, 0.0]);
        plant(&storage, "src/b.rs", "fn_b", 0, vec![1.0, 0.0, 0.0]);
        plant(&storage, &poison_b, "fn_poison", 0, vec![1.0, 0.0, 0.0]);

        let cancel = live_cancel();
        let (paged, stop) =
            find_semantic_hotspots(&storage, root_a, 0.5, None, &cancel).expect("paged");
        assert!(stop.is_none(), "finished scan must omit stop: {stop:?}");
        let single = find_semantic_hotspots_single_script(&storage, root_a, 0.5).expect("single");

        assert!(
            paged
                .iter()
                .any(|m| m.file1 == "src/a.rs" && m.file2 == "src/a.rs"),
            "same-file pair required: {paged:?}"
        );
        assert!(
            paged
                .iter()
                .any(|m| m.file1 == "src/a.rs" && m.file2 == "src/b.rs"),
            "cross-file pair required: {paged:?}"
        );
        assert!(
            paged
                .iter()
                .all(|m| !m.file1.contains("poison") && !m.file2.contains("poison")),
            "foreign drop must apply: {paged:?}"
        );

        let paged_keys: Vec<_> = paged.iter().map(match_key).collect();
        let single_keys: Vec<_> = single.iter().map(match_key).collect();
        assert_eq!(
            paged_keys, single_keys,
            "paged pair set/order must match single-script"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__windowed_file__equals_single_script() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let root = tempfile::tempdir().expect("root");
        for i in 0..=SEMANTIC_PAGE_SNIPPETS {
            plant(
                &storage,
                "src/wide.rs",
                &format!("fn_{i:03}"),
                i as i64,
                vec![1.0, 0.0, 0.0],
            );
        }
        plant(&storage, "src/other.rs", "fn_o", 0, vec![1.0, 0.0, 0.0]);
        let cancel = live_cancel();
        let (paged, stop) =
            find_semantic_hotspots(&storage, root.path(), 0.5, None, &cancel).expect("windowed");
        assert!(stop.is_none(), "windowed finish must omit stop: {stop:?}");
        let single =
            find_semantic_hotspots_single_script(&storage, root.path(), 0.5).expect("single");
        let paged_keys: Vec<_> = paged.iter().map(match_key).collect();
        let single_keys: Vec<_> = single.iter().map(match_key).collect();
        assert_eq!(
            paged_keys.len(),
            single_keys.len(),
            "windowed count {} vs single {}",
            paged_keys.len(),
            single_keys.len()
        );
        assert_eq!(
            paged_keys, single_keys,
            "64-snippet windows must preserve pair set"
        );
        assert!(
            paged
                .iter()
                .any(|m| m.file1 == "src/wide.rs" && m.file2 == "src/wide.rs"),
            "same-file window pairs required (n={})",
            paged.len()
        );
        assert!(
            paged.iter().any(|m| {
                (m.file1 == "src/wide.rs" && m.file2 == "src/other.rs")
                    || (m.file1 == "src/other.rs" && m.file2 == "src/wide.rs")
            }),
            "cross-file window pairs required (n={})",
            paged.len()
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__windowed_file_name_offset_disagree__equals_single_script() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let root = tempfile::tempdir().expect("root");
        // Name order is the reverse of offset order so triangular skip(i)
        // on `:order +name` windows would drop (low-offset, high-offset) pairs.
        for i in 0..=SEMANTIC_PAGE_SNIPPETS {
            let name_rank = SEMANTIC_PAGE_SNIPPETS - i;
            plant(
                &storage,
                "src/wide.rs",
                &format!("zz_{name_rank:03}"),
                i as i64,
                vec![1.0, 0.0, 0.0],
            );
        }
        plant(&storage, "src/other.rs", "fn_o", 0, vec![1.0, 0.0, 0.0]);
        let cancel = live_cancel();
        let (paged, stop) =
            find_semantic_hotspots(&storage, root.path(), 0.5, None, &cancel).expect("windowed");
        assert!(stop.is_none(), "finished scan must omit stop: {stop:?}");
        let single =
            find_semantic_hotspots_single_script(&storage, root.path(), 0.5).expect("single");
        let paged_keys: Vec<_> = paged.iter().map(match_key).collect();
        let single_keys: Vec<_> = single.iter().map(match_key).collect();
        assert_eq!(
            paged_keys.len(),
            single_keys.len(),
            "name/offset-disagree count {} vs single {}",
            paged_keys.len(),
            single_keys.len()
        );
        assert_eq!(
            paged_keys, single_keys,
            "DoD-4b: name order must not drop same-file pairs"
        );
        assert!(
            paged.iter().any(|m| {
                m.file1 == "src/wide.rs"
                    && m.file2 == "src/wide.rs"
                    && m.offset1 == 0
                    && m.offset2 == SEMANTIC_PAGE_SNIPPETS
            }),
            "cross-window low-offset/high-offset pair required (n={})",
            paged.len()
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__deadline_some__full_file__finishes() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let root = tempfile::tempdir().expect("root");
        for i in 0..=SEMANTIC_PAGE_SNIPPETS {
            plant(
                &storage,
                "src/wide.rs",
                &format!("fn_{i:03}"),
                i as i64,
                vec![1.0, 0.0, 0.0],
            );
        }
        let cancel = live_cancel();
        let deadline = Some(Instant::now() + Duration::from_secs(60));
        let (paged, stop) =
            find_semantic_hotspots(&storage, root.path(), 0.5, deadline, &cancel).expect("bounded");
        assert!(stop.is_none(), "65-snippet file must finish: {stop:?}");
        let single =
            find_semantic_hotspots_single_script(&storage, root.path(), 0.5).expect("single");
        let paged_keys: Vec<_> = paged.iter().map(match_key).collect();
        let single_keys: Vec<_> = single.iter().map(match_key).collect();
        assert_eq!(paged_keys, single_keys, "deadline must not truncate keys");
        assert!(
            paged.iter().any(|m| {
                m.file1 == "src/wide.rs"
                    && m.file2 == "src/wide.rs"
                    && m.offset1 == 0
                    && m.offset2 == SEMANTIC_PAGE_SNIPPETS
            }),
            "offsets 0 and 64 must be present: {paged:?}"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__window_script__two_keys_same_file() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        plant(&storage, "src/a.rs", "fn_a", 0, vec![1.0, 0.0, 0.0]);
        plant(&storage, "src/a.rs", "fn_a2", 1, vec![1.0, 0.0, 0.0]);
        let keys = [("fn_a".to_string(), 0_i64), ("fn_a2".to_string(), 1_i64)];
        let mut params = threshold_params("src/a.rs", 0.5);
        params.insert("left_window".to_string(), keys_to_datavalue(&keys));
        params.insert("right_window".to_string(), keys_to_datavalue(&keys));
        let script = same_file_window_script(":timeout 0");
        let rows = storage
            .run_script_with_params(&script, params, ScriptMutability::Immutable)
            .unwrap_or_else(|e| panic!("window script: {e}"));
        assert!(
            !rows.rows.is_empty(),
            "two-key window must emit the same-file pair"
        );
    }

    /// Phase 0 pin measure: `:timeout` cannot preempt Rule 0 on 14179d3.
    /// Ignored so CI stays fast; run with `--ignored` and record wall in review.md.
    #[test]
    #[ignore]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__cozo_timeout__rule0_not_interruptible() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let n = 600usize;
        for i in 0..n {
            plant(
                &storage,
                &format!("src/f{i:04}.rs"),
                "fn_x",
                0,
                vec![1.0, 0.0, 0.0],
            );
        }
        let script = "?[f1, n1, o1, f2, n2, o2, similarity] :=
                *snippet_embedding{file_path: f1, name: n1, line_offset: o1, embedding: v1},
                *snippet_embedding{file_path: f2, name: n2, line_offset: o2, embedding: v2},
                f1 < f2,
                dist = cos_dist(v1, v2),
                similarity = 1.0 - dist,
                similarity > 0.5
            :timeout 0.05";
        let started = Instant::now();
        let _ = storage.run_script(script);
        let wall = started.elapsed();
        eprintln!(
            "phase0_cozo_timeout_measure n={n} wall_ms={} interruptible_lt_500ms={}",
            wall.as_millis(),
            wall.as_millis() < 500
        );
    }

    #[test]
    #[ignore]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__exec_cozo_open__timing() {
        let path = std::path::Path::new(r"C:\dev\ledgerful\.ledgerful\state\ledger.cozo");
        let t0 = Instant::now();
        let storage = CozoStorage::new_read_only(path).expect("open");
        eprintln!("exec_cozo_open_ms={}", t0.elapsed().as_millis());
        let t1 = Instant::now();
        let mut params = BTreeMap::new();
        params.insert("after".to_string(), DataValue::from(""));
        let res = storage.run_script_classifying_kill(
            "?[file_path] := *snippet_embedding{file_path}, file_path > $after :limit 1 :timeout 2",
            params,
        );
        eprintln!(
            "exec_next_file_ms={} ok={}",
            t1.elapsed().as_millis(),
            res.is_ok()
        );
        if let Ok(crate::state::storage_cozo::CozoScriptOutcome::Rows(rows)) = res {
            let first = rows
                .rows
                .first()
                .and_then(|r| r.first())
                .map(|v| format!("{v:?}"))
                .unwrap_or_default();
            eprintln!("first_file={first}");
            let t2 = Instant::now();
            let mut kp = BTreeMap::new();
            if let Some(DataValue::Str(fp)) = rows.rows.first().and_then(|r| r.first()) {
                kp.insert("left".to_string(), DataValue::from(fp.as_str()));
            }
            let keys = storage.run_script_classifying_kill(
                "?[name, line_offset] := *snippet_embedding{file_path: $left, name, line_offset} :order +name, +line_offset :timeout 2",
                kp,
            );
            eprintln!(
                "exec_keys_ms={} ok={}",
                t2.elapsed().as_millis(),
                keys.is_ok()
            );
        }
        let t3 = Instant::now();
        let listed = storage.run_script("?[file_path] := *snippet_embedding{file_path}");
        eprintln!(
            "exec_distinct_ms={} n={}",
            t3.elapsed().as_millis(),
            listed.as_ref().map(|r| r.rows.len()).unwrap_or(0)
        );
        let cancel = live_cancel();
        let deadline = Some(Instant::now() + Duration::from_secs(5));
        let t4 = Instant::now();
        let (matches, stop) = find_semantic_hotspots(
            &storage,
            std::path::Path::new(r"C:\dev\ledgerful"),
            0.85,
            deadline,
            &cancel,
        )
        .expect("exec paged");
        eprintln!(
            "exec_find_ms={} stop={stop:?} n={}",
            t4.elapsed().as_millis(),
            matches.len()
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn load_semantic_embeddings__expired_instant__empty_map() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        plant(&storage, "src/a.rs", "fn_a", 0, vec![1.0, 0.0, 0.0]);
        let expired = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        let cancel = live_cancel();
        let (loaded, stop) =
            load_semantic_embeddings(&storage, Some(expired), &cancel).expect("load");
        assert!(
            loaded.is_empty(),
            "loader stop must not return a partial map: {loaded:?}"
        );
        assert_eq!(stop, Some(SemanticStop::Budget));
    }

    #[test]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__parallel_scan_deadline_budget_stops_mid_scan() {
        let snippets: Vec<LoadedSnippet> = (0..20_000)
            .map(|i| {
                let emb = if i < 2 {
                    vec![1.0_f32, 0.0, 0.0]
                } else {
                    let t = i as f32;
                    vec![t.sin(), t.cos(), 0.0]
                };
                LoadedSnippet {
                    name: format!("fn_{i}"),
                    line_offset: i as i64,
                    embedding: emb.clone(),
                    norm: f64::from(f32_self_dot(&emb)),
                }
            })
            .collect();
        let mut by_file = BTreeMap::new();
        by_file.insert("src/wide.rs".to_string(), snippets);
        let cancel = live_cancel();
        let deadline = Some(Instant::now() + Duration::from_millis(200));
        let started = Instant::now();
        let (pairs, stop) =
            scan_semantic_pairs(&by_file, 0.85, deadline, &cancel, SEMANTIC_SCAN_CHUNK)
                .expect("scan");
        let wall = started.elapsed();
        assert_eq!(stop, Some(SemanticStop::Budget));
        assert!(!pairs.is_empty(), "tasks that ran must keep their pairs");
        assert!(
            wall < Duration::from_secs(5),
            "abort must observe Budget well under 5s, wall={wall:?}"
        );
    }

    /// Phase 0 gate. Run `--ignored` in release; record walls in review.md.
    #[test]
    #[ignore]
    #[allow(non_snake_case)]
    fn find_semantic_hotspots__phase0_exec_parallel_scan_bench() {
        let path = std::path::Path::new(r"C:\dev\ledgerful\.ledgerful\state\ledger.cozo");
        let storage = CozoStorage::new_read_only(path).expect("open");
        let cancel = live_cancel();
        let t_load = Instant::now();
        let (by_file, stop) = load_semantic_embeddings(&storage, None, &cancel).expect("load");
        let load_wall = t_load.elapsed();
        assert!(stop.is_none(), "unbounded load must finish: {stop:?}");
        let n: usize = by_file.values().map(Vec::len).sum();
        let max_file = by_file.values().map(Vec::len).max().unwrap_or(0);
        let n_threads = scan_thread_count();
        let chunk = SEMANTIC_SCAN_CHUNK;

        let mut one_task = Duration::ZERO;
        let n_threads_pool = n_threads;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(n_threads_pool)
            .build()
            .expect("pool");
        pool.install(|| {
            if let Some((path, snips)) = by_file.iter().max_by_key(|(_, v)| v.len()) {
                let files: Vec<String> = by_file.keys().cloned().collect();
                let file_idx = files.iter().position(|p| p == path).unwrap_or(0);
                let owned = &snips[..chunk.min(snips.len())];
                let t = Instant::now();
                let _ = compare_owned_chunk(&files, &by_file, file_idx, owned, 0.85);
                one_task = t.elapsed();
            }
        });

        let t_scan = Instant::now();
        let (pairs, scan_stop) =
            scan_semantic_pairs(&by_file, 0.85, None, &cancel, chunk).expect("scan");
        let scan_wall = t_scan.elapsed();
        eprintln!(
            "phase0 threads={n_threads} files={} n={n} max_file={max_file} chunk={chunk} load_ms={} scan_ms={} load_scan_ms={} one_task_ms={} matches={} stop={scan_stop:?}",
            by_file.len(),
            load_wall.as_millis(),
            scan_wall.as_millis(),
            (load_wall + scan_wall).as_millis(),
            one_task.as_millis(),
            pairs.len(),
        );
        assert!(
            load_wall + scan_wall < Duration::from_secs(20),
            "load+scan {:?}",
            load_wall + scan_wall
        );
        assert!(
            one_task <= Duration::from_secs(1),
            "in-pool 64-left task {one_task:?}"
        );
        assert!(
            scan_stop.is_none(),
            "unbounded scan must finish: {scan_stop:?}"
        );
    }
}
