//! Compiled performance budget table and `bench --check` (0564).
//!
//! One row per `(workload, hostClass, os, targetTriple)`. A ceiling is written
//! only from a recorded baseline by the published arithmetic in spec §3.2; the
//! table is compiled and reviewed, and `check_with_budgets` exists so tests can
//! inject a synthetic table. There is no CLI override.

use crate::commands::bench::{StoredBench, os_name};
use crate::state::storage::timings::percentile_sorted;
use serde::Serialize;
use std::collections::BTreeSet;

pub(crate) const KIND_PERF_BUDGET_CHECK: &str = "perfBudgetCheck";
pub(crate) const PERF_BUDGET_SCHEMA_VERSION: u32 = 1;

pub(crate) const STATUS_WITHIN_BUDGET: &str = "within-budget";
pub(crate) const STATUS_NOT_WITHIN_BUDGET: &str = "not-within-budget";
pub(crate) const STATUS_OVER_BUDGET: &str = "over-budget";
pub(crate) const STATUS_INSUFFICIENT_SAMPLES: &str = "insufficient-samples";
pub(crate) const STATUS_FIXTURE_OUT_OF_BAND: &str = "fixture-out-of-band";
pub(crate) const STATUS_UNQUALIFIED_HOST: &str = "unqualified-host";
pub(crate) const STATUS_UNQUALIFIED_WORKLOAD: &str = "unqualified-workload";
pub(crate) const STATUS_UNMEASURED_RESOURCE: &str = "unmeasured-resource";
pub(crate) const STATUS_FOREIGN_BUILD: &str = "foreign-build";
pub(crate) const STATUS_DUPLICATE_RECEIPT: &str = "duplicate-receipt";
pub(crate) const STATUS_INVALID_RECEIPT: &str = "invalid-receipt";

/// One budget row: a regression ceiling for a declared host class and fixture band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BudgetRow {
    pub(crate) workload: &'static str,
    pub(crate) host_class: &'static str,
    pub(crate) os: &'static str,
    pub(crate) target_triple: &'static str,
    pub(crate) fixture_min_files: u64,
    pub(crate) fixture_max_files: u64,
    pub(crate) fixture_min_commits: u64,
    pub(crate) fixture_max_commits: u64,
    pub(crate) n_min: usize,
    pub(crate) p50_budget_ms: i64,
    pub(crate) p95_budget_ms: i64,
    pub(crate) peak_rss_budget_bytes: Option<u64>,
    pub(crate) interactive_target_ms: Option<i64>,
    pub(crate) expected_child_args: &'static [&'static str],
    pub(crate) expected_start_state: &'static str,
    pub(crate) expects_setup: bool,
    pub(crate) baseline: Baseline,
}

/// The measured receipt values a row's ceilings were derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Baseline {
    pub(crate) date: &'static str,
    pub(crate) build_sha: &'static str,
    pub(crate) n: usize,
    pub(crate) p50_ms: i64,
    pub(crate) p95_ms: i64,
    pub(crate) sampler_wall_ms: u64,
    pub(crate) peak_rss_bytes: Option<u64>,
    pub(crate) rss_measured_samples: u64,
    pub(crate) fixture_id: &'static str,
    pub(crate) fixture_files: u64,
    pub(crate) fixture_commits: u64,
    pub(crate) fixture_root_commit: &'static str,
    pub(crate) host_class: &'static str,
}

/// Filled by the Phase 3/4 measurement legs, one row per measured workload.
pub(crate) const BUDGETS: &[BudgetRow] = &[];

pub(crate) fn check(receipts: &[StoredBench], embed: &str) -> PerfBudgetCheck {
    check_with_budgets(receipts, embed, BUDGETS)
}

pub(crate) fn check_with_budgets(
    receipts: &[StoredBench],
    embed: &str,
    budgets: &[BudgetRow],
) -> PerfBudgetCheck {
    // An empty slice is never a vacuous pass: one synthetic no-receipts outcome.
    if receipts.is_empty() {
        return PerfBudgetCheck {
            schema_version: PERF_BUDGET_SCHEMA_VERSION,
            kind: KIND_PERF_BUDGET_CHECK,
            results: Vec::new(),
            status: STATUS_NOT_WITHIN_BUDGET,
            violations: 0,
            unqualified: 1,
        };
    }

    let mut ordered: Vec<&StoredBench> = receipts.iter().collect();
    ordered.sort_by(|a, b| {
        (
            a.workload.as_str(),
            a.host_class.as_str(),
            a.receipt_path.as_str(),
        )
            .cmp(&(
                b.workload.as_str(),
                b.host_class.as_str(),
                b.receipt_path.as_str(),
            ))
    });
    // The ceiling arithmetic is the only way a ceiling may be written; a table
    // that fails its own invariants must never be trusted by a check.
    debug_assert!(
        validate_rows(budgets).is_empty(),
        "budget table violates its own invariants: {:?}",
        validate_rows(budgets)
    );

    let mut seen_keys: BTreeSet<(String, String)> = BTreeSet::new();
    let mut results = Vec::with_capacity(ordered.len());
    for stored in ordered {
        let structural = structural_violation(stored, budgets);
        let duplicate = if structural.is_none() {
            !seen_keys.insert((stored.workload.clone(), stored.host_class.clone()))
        } else {
            false
        };
        results.push(evaluate(stored, embed, budgets, structural, duplicate));
    }

    let violations = results
        .iter()
        .filter(|result| result.status == STATUS_OVER_BUDGET)
        .count();
    let unqualified = results
        .iter()
        .filter(|result| {
            result.status != STATUS_WITHIN_BUDGET && result.status != STATUS_OVER_BUDGET
        })
        .count();
    let status = if violations + unqualified == 0 {
        STATUS_WITHIN_BUDGET
    } else {
        STATUS_NOT_WITHIN_BUDGET
    };
    PerfBudgetCheck {
        schema_version: PERF_BUDGET_SCHEMA_VERSION,
        kind: KIND_PERF_BUDGET_CHECK,
        results,
        status,
        violations,
        unqualified,
    }
}

fn evaluate(
    stored: &StoredBench,
    embed: &str,
    budgets: &[BudgetRow],
    structural: Option<String>,
    duplicate: bool,
) -> BudgetResult {
    let workload_rows = budgets
        .iter()
        .filter(|row| row.workload == stored.workload)
        .count();
    let matched = if structural.is_none() && !duplicate {
        budgets.iter().find(|row| {
            row.workload == stored.workload
                && row.host_class == stored.host_class
                && row.os == stored.os
                && row.target_triple == stored.target_triple
        })
    } else {
        None
    };

    let (status, reason) = if let Some(violation) = structural {
        (STATUS_INVALID_RECEIPT, Some(violation))
    } else if duplicate {
        (
            STATUS_DUPLICATE_RECEIPT,
            Some(format!(
                "another receipt already covers {} on {}",
                stored.workload, stored.host_class
            )),
        )
    } else if stored.build_sha != embed {
        (
            STATUS_FOREIGN_BUILD,
            Some(format!(
                "receipt buildSha {} is not the running embed {embed}",
                stored.build_sha
            )),
        )
    } else if workload_rows == 0 {
        (
            STATUS_UNQUALIFIED_WORKLOAD,
            Some(format!(
                "no budget row exists for workload {} on any host class",
                stored.workload
            )),
        )
    } else if matched.is_none() || stored.os != os_name() || stored.target_triple != env!("TARGET")
    {
        (
            STATUS_UNQUALIFIED_HOST,
            Some(format!(
                "no row matches hostClass {} / os {} / targetTriple {} for {} (running os {} / {})",
                stored.host_class,
                stored.os,
                stored.target_triple,
                stored.workload,
                os_name(),
                env!("TARGET")
            )),
        )
    } else {
        match matched {
            Some(row) => compare_to_row(stored, row),
            None => (
                STATUS_UNQUALIFIED_HOST,
                Some("no matching row for this host class".to_string()),
            ),
        }
    };

    let row_evaluated = matches!(
        status,
        STATUS_FIXTURE_OUT_OF_BAND
            | STATUS_INSUFFICIENT_SAMPLES
            | STATUS_UNMEASURED_RESOURCE
            | STATUS_OVER_BUDGET
            | STATUS_WITHIN_BUDGET
    );
    let row_fields = if row_evaluated { matched } else { None };

    let target_status = match row_fields.and_then(|row| row.interactive_target_ms) {
        Some(target) if stored.p95_ms <= target => "met",
        Some(_) => "not-met",
        None => "no-target",
    }
    .to_string();

    BudgetResult {
        workload: stored.workload.clone(),
        host_class: stored.host_class.clone(),
        receipt_path: stored.receipt_path.clone(),
        n: stored.n,
        p50_ms: stored.p50_ms,
        p95_ms: stored.p95_ms,
        p50_budget_ms: row_fields.map(|row| row.p50_budget_ms),
        p95_budget_ms: row_fields.map(|row| row.p95_budget_ms),
        interactive_target_ms: row_fields.and_then(|row| row.interactive_target_ms),
        peak_rss_bytes: stored.peak_rss_bytes,
        peak_rss_budget_bytes: row_fields.and_then(|row| row.peak_rss_budget_bytes),
        target_status,
        status,
        reason,
    }
}

fn compare_to_row(stored: &StoredBench, row: &BudgetRow) -> (&'static str, Option<String>) {
    if stored.fixture_root_commit.as_deref() != Some(row.baseline.fixture_root_commit) {
        return (
            STATUS_FIXTURE_OUT_OF_BAND,
            Some(format!(
                "fixtureRootCommit {:?} is not the row's root {}",
                stored.fixture_root_commit, row.baseline.fixture_root_commit
            )),
        );
    }
    if stored.fixture_files < row.fixture_min_files || stored.fixture_files > row.fixture_max_files
    {
        return (
            STATUS_FIXTURE_OUT_OF_BAND,
            Some(format!(
                "fixtureFiles {} is outside the band {}..={}",
                stored.fixture_files, row.fixture_min_files, row.fixture_max_files
            )),
        );
    }
    if stored.fixture_commits < row.fixture_min_commits
        || stored.fixture_commits > row.fixture_max_commits
    {
        return (
            STATUS_FIXTURE_OUT_OF_BAND,
            Some(format!(
                "fixtureCommits {} is outside the band {}..={}",
                stored.fixture_commits, row.fixture_min_commits, row.fixture_max_commits
            )),
        );
    }
    if stored.n < row.n_min {
        return (
            STATUS_INSUFFICIENT_SAMPLES,
            Some(format!(
                "{} samples is below the check floor {}",
                stored.n, row.n_min
            )),
        );
    }
    if row.peak_rss_budget_bytes.is_some() {
        if stored.peak_rss_bytes.is_none() {
            return (
                STATUS_UNMEASURED_RESOURCE,
                Some("the row declares a peak-RSS ceiling but the receipt has no peak".to_string()),
            );
        }
        if stored.rss_measured_samples < stored.n as u64 {
            return (
                STATUS_UNMEASURED_RESOURCE,
                Some(format!(
                    "the row declares a peak-RSS ceiling but only {}/{} samples were measured",
                    stored.rss_measured_samples, stored.n
                )),
            );
        }
    }
    if let (Some(peak), Some(budget)) = (stored.peak_rss_bytes, row.peak_rss_budget_bytes)
        && peak > budget
    {
        return (
            STATUS_OVER_BUDGET,
            Some(format!(
                "peak RSS {peak} bytes exceeds the {budget} byte ceiling"
            )),
        );
    }
    if stored.p50_ms > row.p50_budget_ms {
        return (
            STATUS_OVER_BUDGET,
            Some(format!(
                "p50 {} ms exceeds the {} ms ceiling",
                stored.p50_ms, row.p50_budget_ms
            )),
        );
    }
    if stored.p95_ms > row.p95_budget_ms {
        return (
            STATUS_OVER_BUDGET,
            Some(format!(
                "p95 {} ms exceeds the {} ms ceiling",
                stored.p95_ms, row.p95_budget_ms
            )),
        );
    }
    (STATUS_WITHIN_BUDGET, None)
}

/// Structural consistency, checked on every receipt before duplicate or row logic.
fn structural_violation(stored: &StoredBench, budgets: &[BudgetRow]) -> Option<String> {
    if stored.n == 0 {
        return Some("n must be greater than zero".to_string());
    }
    if stored.samples_ms.len() != stored.n {
        return Some(format!(
            "samplesMs has {} entries but n is {}",
            stored.samples_ms.len(),
            stored.n
        ));
    }
    if stored.observed_ms.len() != stored.n {
        return Some(format!(
            "observedMs has {} entries but n is {}",
            stored.observed_ms.len(),
            stored.n
        ));
    }
    if stored.samples_ms.iter().any(|sample| *sample < 0) {
        return Some("samplesMs contains a negative sample".to_string());
    }
    if !stored.samples_ms.windows(2).all(|pair| pair[0] <= pair[1]) {
        return Some("samplesMs is not sorted ascending".to_string());
    }
    if percentile_sorted(&stored.samples_ms, 50) != stored.p50_ms
        || percentile_sorted(&stored.samples_ms, 95) != stored.p95_ms
    {
        return Some("p50Ms/p95Ms are not recomputable from samplesMs".to_string());
    }
    let mut sorted_observed = stored.observed_ms.clone();
    sorted_observed.sort_unstable();
    if sorted_observed != stored.samples_ms {
        return Some("observedMs is not a permutation of samplesMs".to_string());
    }
    let (fixture_count, _) = match stored.fixture_id.split_once(':') {
        Some(parts) => parts,
        None => return Some("fixtureId is not \"<count>:<hash>\"".to_string()),
    };
    match fixture_count.parse::<u64>() {
        Ok(count) if count == stored.fixture_files => {}
        Ok(count) => {
            return Some(format!(
                "fixtureFiles {} does not match the fixtureId count {count}",
                stored.fixture_files
            ));
        }
        Err(_) => return Some("fixtureId count is not a number".to_string()),
    }
    if stored.rss_measured_samples > stored.n as u64 {
        return Some("rssMeasuredSamples exceeds n".to_string());
    }
    if stored.rss_ticks < stored.rss_measured_samples {
        return Some("rssTicks is smaller than rssMeasuredSamples".to_string());
    }
    if (stored.rss_ticks == 0) != (stored.rss_measured_samples == 0) {
        return Some("rssTicks and rssMeasuredSamples disagree about coverage".to_string());
    }
    if stored.peak_rss_bytes.is_some() != (stored.rss_measured_samples > 0) {
        return Some("peakRssBytes presence disagrees with rssMeasuredSamples".to_string());
    }
    for (name, value) in [("setupMs", stored.setup_ms), ("warmupMs", stored.warmup_ms)] {
        if value.is_some_and(|v| v < 0) {
            return Some(format!("{name} is negative"));
        }
    }
    for (name, value) in [
        ("workload", &stored.workload),
        ("hostClass", &stored.host_class),
        ("buildSha", &stored.build_sha),
        ("os", &stored.os),
        ("targetTriple", &stored.target_triple),
    ] {
        if value.is_empty() {
            return Some(format!("{name} is empty"));
        }
    }
    if stored.fixture_root_commit.as_deref() == Some("") {
        return Some("fixtureRootCommit is an empty string".to_string());
    }

    // Row-specific checks run only when the workload has a row at all; an
    // empty table must report `unqualified-workload`, not `invalid-receipt`.
    let row = budgets.iter().find(|row| row.workload == stored.workload)?;
    if !stored
        .child_args
        .iter()
        .map(String::as_str)
        .eq(row.expected_child_args.iter().copied())
    {
        return Some(format!(
            "childArgs {:?} is not the locked argv {:?}",
            stored.child_args, row.expected_child_args
        ));
    }
    if stored.start_state != row.expected_start_state {
        return Some(format!(
            "startState {} is not the locked {}",
            stored.start_state, row.expected_start_state
        ));
    }
    if stored.setup_ms.is_some() != row.expects_setup
        || stored.warmup_ms.is_some() != row.expects_setup
    {
        return Some(format!(
            "setupMs/warmupMs presence does not match expectsSetup {}",
            row.expects_setup
        ));
    }
    None
}

/// Ceiling derivation: `max(50, ceil10(baseline * 5 / 4))`.
pub(crate) fn derive_p50(baseline_p50_ms: i64) -> i64 {
    std::cmp::max(50, ceil10(baseline_p50_ms.saturating_mul(5) / 4))
}

/// Ceiling derivation: `max(50, ceil10(baseline * 5 / 4))`.
pub(crate) fn derive_p95(baseline_p95_ms: i64) -> i64 {
    std::cmp::max(50, ceil10(baseline_p95_ms.saturating_mul(5) / 4))
}

/// Peak-RSS ceiling: integer ceiling division for 1.5x, then round up to 16 MiB.
pub(crate) fn derive_rss(baseline_bytes: u64) -> u64 {
    ceil16mib(baseline_bytes.saturating_mul(3).saturating_add(1) / 2)
}

pub(crate) fn ceil10(value: i64) -> i64 {
    value.saturating_add(9) / 10 * 10
}

pub(crate) fn ceil16mib(bytes: u64) -> u64 {
    const UNIT: u64 = 16 * 1024 * 1024;
    bytes.saturating_add(UNIT - 1) / UNIT * UNIT
}

/// Row invariants. Returns one message per violation (empty means valid).
pub(crate) fn validate_rows(rows: &[BudgetRow]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut keys = BTreeSet::new();
    for row in rows {
        let key = (row.workload, row.host_class, row.os, row.target_triple);
        if !keys.insert(key) {
            violations.push(format!(
                "duplicate key {}/{}/{}/{}",
                row.workload, row.host_class, row.os, row.target_triple
            ));
        }
        let baseline = &row.baseline;
        if row.p50_budget_ms != derive_p50(baseline.p50_ms) {
            violations.push(format!("{}: p50 ceiling not derived", row.workload));
        }
        if row.p95_budget_ms != derive_p95(baseline.p95_ms) {
            violations.push(format!("{}: p95 ceiling not derived", row.workload));
        }
        match row.peak_rss_budget_bytes {
            Some(bytes) => {
                if baseline.peak_rss_bytes.is_none() {
                    violations.push(format!(
                        "{}: RSS ceiling without a baseline peak",
                        row.workload
                    ));
                } else if Some(bytes) != baseline.peak_rss_bytes.map(derive_rss) {
                    violations.push(format!("{}: RSS ceiling not derived", row.workload));
                }
                if baseline.p95_ms < 500 {
                    violations.push(format!("{}: RSS ceiling on a sub-500 ms row", row.workload));
                }
                if baseline.rss_measured_samples != baseline.n as u64 {
                    violations.push(format!(
                        "{}: RSS ceiling without full coverage",
                        row.workload
                    ));
                }
            }
            None => {
                if let Some(bytes) = baseline.peak_rss_bytes
                    && baseline.p95_ms >= 500
                    && baseline.rss_measured_samples == baseline.n as u64
                    && derive_rss(bytes) == 0
                {
                    violations.push(format!("{}: zero RSS ceiling", row.workload));
                }
            }
        }
        if baseline.n < 20 {
            violations.push(format!("{}: baseline.n {} < 20", row.workload, baseline.n));
        }
        if row.n_min < 10 || row.n_min > baseline.n {
            violations.push(format!(
                "{}: n_min {} outside 10..=baseline.n {}",
                row.workload, row.n_min, baseline.n
            ));
        }
        if row.fixture_min_files >= row.fixture_max_files {
            violations.push(format!(
                "{}: fixture file band is not min < max",
                row.workload
            ));
        }
        if row.fixture_min_commits >= row.fixture_max_commits {
            violations.push(format!(
                "{}: fixture commit band is not min < max",
                row.workload
            ));
        }
        if row.fixture_min_files == 0 || row.fixture_min_commits == 0 {
            violations.push(format!(
                "{}: fixture band must start above zero",
                row.workload
            ));
        }
        if baseline.fixture_files < row.fixture_min_files
            || baseline.fixture_files > row.fixture_max_files
        {
            violations.push(format!("{}: baseline files outside the band", row.workload));
        }
        if baseline.fixture_commits < row.fixture_min_commits
            || baseline.fixture_commits > row.fixture_max_commits
        {
            violations.push(format!(
                "{}: baseline commits outside the band",
                row.workload
            ));
        }
        if baseline.p50_ms <= 0 {
            violations.push(format!(
                "{}: baseline.p50_ms must be positive",
                row.workload
            ));
        }
        if baseline.p50_ms > baseline.p95_ms {
            violations.push(format!("{}: baseline p50 > p95", row.workload));
        }
        if row.p50_budget_ms > row.p95_budget_ms {
            violations.push(format!("{}: p50 ceiling > p95 ceiling", row.workload));
        }
        if baseline.host_class != row.host_class {
            violations.push(format!(
                "{}: baseline host class differs from the row",
                row.workload
            ));
        }
        if baseline.fixture_root_commit.is_empty() {
            violations.push(format!("{}: baseline root commit is empty", row.workload));
        }
        if row.p95_budget_ms >= 120_000 {
            violations.push(format!(
                "{}: p95 ceiling is not below the 120 s clock",
                row.workload
            ));
        }
        let refresh_limit = std::cmp::max(50, (baseline.p50_ms.saturating_mul(5) / 100) as u64);
        if baseline.sampler_wall_ms / baseline.n as u64 >= refresh_limit {
            violations.push(format!(
                "{}: sampler refresh cost {} ms/sample exceeds {} ms",
                row.workload,
                baseline.sampler_wall_ms / baseline.n as u64,
                refresh_limit
            ));
        }
        if row.expected_child_args.is_empty() {
            violations.push(format!("{}: expected argv is empty", row.workload));
        }
        if row.expected_start_state != (if row.expects_setup { "warm" } else { "cold" }) {
            violations.push(format!(
                "{}: expected start state disagrees with expectsSetup",
                row.workload
            ));
        }
    }
    violations
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PerfBudgetCheck {
    pub(crate) schema_version: u32,
    pub(crate) kind: &'static str,
    pub(crate) results: Vec<BudgetResult>,
    pub(crate) status: &'static str,
    pub(crate) violations: usize,
    pub(crate) unqualified: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BudgetResult {
    pub(crate) workload: String,
    pub(crate) host_class: String,
    pub(crate) receipt_path: String,
    pub(crate) n: usize,
    pub(crate) p50_ms: i64,
    pub(crate) p95_ms: i64,
    pub(crate) p50_budget_ms: Option<i64>,
    pub(crate) p95_budget_ms: Option<i64>,
    pub(crate) interactive_target_ms: Option<i64>,
    pub(crate) peak_rss_bytes: Option<u64>,
    pub(crate) peak_rss_budget_bytes: Option<u64>,
    pub(crate) target_status: String,
    pub(crate) status: &'static str,
    pub(crate) reason: Option<String>,
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    const EMBED: &str = "abcdef123456";

    fn synthetic_row(workload: &'static str, host_class: &'static str) -> BudgetRow {
        let p50_ms = 100;
        let p95_ms = 200;
        BudgetRow {
            workload,
            host_class,
            os: os_name(),
            target_triple: env!("TARGET"),
            fixture_min_files: 900,
            fixture_max_files: 1100,
            fixture_min_commits: 500,
            fixture_max_commits: 700,
            n_min: 10,
            p50_budget_ms: derive_p50(p50_ms),
            p95_budget_ms: derive_p95(p95_ms),
            peak_rss_budget_bytes: None,
            interactive_target_ms: Some(150),
            expected_child_args: &["doctor", "--json"],
            expected_start_state: "warm",
            expects_setup: true,
            baseline: Baseline {
                date: "2026-11-04",
                build_sha: EMBED,
                n: 20,
                p50_ms,
                p95_ms,
                sampler_wall_ms: 200,
                peak_rss_bytes: None,
                rss_measured_samples: 0,
                fixture_id: "1000:abcdef123456",
                fixture_files: 1000,
                fixture_commits: 600,
                fixture_root_commit: "root1",
                host_class,
            },
        }
    }

    fn synthetic_row_with_rss(workload: &'static str, host_class: &'static str) -> BudgetRow {
        let mut row = synthetic_row(workload, host_class);
        row.baseline.p50_ms = 600;
        row.baseline.p95_ms = 900;
        row.p50_budget_ms = derive_p50(row.baseline.p50_ms);
        row.p95_budget_ms = derive_p95(row.baseline.p95_ms);
        row.baseline.peak_rss_bytes = Some(400 * 1024 * 1024);
        row.baseline.rss_measured_samples = row.baseline.n as u64;
        row.peak_rss_budget_bytes = Some(derive_rss(400 * 1024 * 1024));
        row
    }

    fn receipt_for(workload: &str, host_class: &str, path: &str, samples: &[i64]) -> StoredBench {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let n = sorted.len();
        StoredBench {
            schema_version: 1,
            kind: "workloadBench".to_string(),
            workload: workload.to_string(),
            build_sha: EMBED.to_string(),
            p50_ms: percentile_sorted(&sorted, 50),
            p95_ms: percentile_sorted(&sorted, 95),
            fixture_id: "1000:abcdef123456".to_string(),
            os: os_name().to_string(),
            target_triple: env!("TARGET").to_string(),
            n,
            samples_ms: sorted,
            observed_ms: samples.to_vec(),
            sampler_wall_ms: 10,
            fixture_root_commit: Some("root1".to_string()),
            child_args: vec!["doctor".to_string(), "--json".to_string()],
            start_state: "warm".to_string(),
            setup_ms: Some(5),
            warmup_ms: Some(5),
            host_class: host_class.to_string(),
            fixture_files: 1000,
            fixture_commits: 600,
            peak_rss_bytes: None,
            rss_ticks: 0,
            rss_measured_samples: 0,
            receipt_path: path.to_string(),
        }
    }

    fn ten_samples(value: i64) -> Vec<i64> {
        vec![value; 10]
    }

    #[test]
    fn derive_arithmetic__floor_and_rounding__matches_spec() {
        assert_eq!(derive_p50(10), 50, "the 50 ms floor wins over ceil10(12)");
        assert_eq!(
            derive_p50(20),
            50,
            "ceil10(25) is 30, and the 50 ms floor wins"
        );
        assert_eq!(derive_p95(40), 50);
        assert_eq!(derive_p95(80), 100);
        assert_eq!(derive_p95(1000), 1250);
        assert_eq!(derive_p95(1001), 1260);
        assert_eq!(derive_rss(0), 0);
        assert_eq!(derive_rss(10 * 1024 * 1024), 16 * 1024 * 1024);
        assert_eq!(derive_rss(32 * 1024 * 1024), 48 * 1024 * 1024);
        assert_eq!(derive_rss(16 * 1024 * 1024), 32 * 1024 * 1024);
        assert_eq!(ceil16mib(1), 16 * 1024 * 1024);
    }

    #[test]
    fn check__peak_rss_over_ceiling__over_budget() {
        let rows = vec![synthetic_row_with_rss(
            "impact-warm",
            "local-windows-2026-11",
        )];
        let mut receipt = receipt_for(
            "impact-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(700),
        );
        receipt.peak_rss_bytes = Some(1024);
        receipt.rss_ticks = 10;
        receipt.rss_measured_samples = 10;
        assert_eq!(
            check_with_budgets(&[receipt], EMBED, &rows).results[0].status,
            STATUS_WITHIN_BUDGET
        );

        let mut regressed = receipt_for(
            "impact-warm",
            "local-windows-2026-11",
            "b.json",
            &ten_samples(700),
        );
        regressed.peak_rss_bytes = Some(derive_rss(400 * 1024 * 1024) + 1);
        regressed.rss_ticks = 10;
        regressed.rss_measured_samples = 10;
        let packet = check_with_budgets(&[regressed], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_OVER_BUDGET);
        assert!(
            packet.results[0]
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("peak RSS"),
            "{:?}",
            packet.results[0].reason
        );
    }

    #[test]
    fn validate_rows__live_table__clean() {
        let violations = validate_rows(BUDGETS);
        assert!(violations.is_empty(), "live BUDGETS table: {violations:?}");
    }

    #[test]
    fn validate_rows__synthetic_valid_set__clean() {
        let rows = vec![
            synthetic_row("doctor-warm", "local-windows-2026-11"),
            synthetic_row_with_rss("impact-warm", "local-windows-2026-11"),
        ];
        let violations = validate_rows(&rows);
        assert!(
            violations.is_empty(),
            "expected a clean set, got {violations:?}"
        );
    }

    #[test]
    fn validate_rows__undervied_ceiling__flagged() {
        let mut row = synthetic_row("doctor-warm", "local-windows-2026-11");
        row.p95_budget_ms += 10;
        let violations = validate_rows(&[row]);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("p95 ceiling not derived")),
            "{violations:?}"
        );
    }

    #[test]
    fn validate_rows__thin_baseline__flagged() {
        let mut row = synthetic_row("doctor-warm", "local-windows-2026-11");
        row.baseline.n = 10;
        row.n_min = 10;
        let violations = validate_rows(&[row]);
        assert!(
            violations.iter().any(|v| v.contains("< 20")),
            "{violations:?}"
        );
    }

    #[test]
    fn validate_rows__rss_ceiling_without_coverage__flagged() {
        let mut row = synthetic_row_with_rss("impact-warm", "local-windows-2026-11");
        row.baseline.rss_measured_samples = 19;
        let violations = validate_rows(&[row]);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("without full coverage")),
            "{violations:?}"
        );
    }

    #[test]
    fn validate_rows__duplicate_key__flagged() {
        let rows = vec![
            synthetic_row("doctor-warm", "local-windows-2026-11"),
            synthetic_row("doctor-warm", "local-windows-2026-11"),
        ];
        let violations = validate_rows(&rows);
        assert!(
            violations.iter().any(|v| v.contains("duplicate key")),
            "{violations:?}"
        );
    }

    #[test]
    fn check__empty_slice__one_synthetic_outcome_and_no_results() {
        let packet = check_with_budgets(&[], EMBED, &[]);
        assert_eq!(packet.status, STATUS_NOT_WITHIN_BUDGET);
        assert_eq!(packet.violations, 0);
        assert_eq!(packet.unqualified, 1);
        assert!(packet.results.is_empty());
        assert_eq!(packet.kind, KIND_PERF_BUDGET_CHECK);
        assert_eq!(packet.schema_version, PERF_BUDGET_SCHEMA_VERSION);
    }

    #[test]
    fn check__clean_receipt__within_budget() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let receipt = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.status, STATUS_WITHIN_BUDGET);
        assert_eq!(packet.results[0].status, STATUS_WITHIN_BUDGET);
        assert_eq!(packet.results[0].target_status, "met");
        assert_eq!(packet.results[0].p95_budget_ms, Some(derive_p95(200)));
        assert!(packet.results[0].reason.is_none());
    }

    #[test]
    fn check__over_budget__counted_as_violation() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let receipt = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(500),
        );
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.status, STATUS_NOT_WITHIN_BUDGET);
        assert_eq!(packet.violations, 1);
        assert_eq!(packet.unqualified, 0);
        assert_eq!(packet.results[0].status, STATUS_OVER_BUDGET);
        assert!(
            packet.results[0]
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("exceeds the")
        );
        assert!(
            packet.results[0]
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("ceiling")
        );
    }

    #[test]
    fn check__insufficient_samples__row_derived_fields_still_present() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let receipt = receipt_for("doctor-warm", "local-windows-2026-11", "a.json", &[80, 90]);
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_INSUFFICIENT_SAMPLES);
        assert_eq!(packet.results[0].p95_budget_ms, Some(derive_p95(200)));
        assert_eq!(packet.status, STATUS_NOT_WITHIN_BUDGET);
    }

    #[test]
    fn check__fixture_root_mismatch__out_of_band() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let mut receipt = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        receipt.fixture_root_commit = Some("other-root".to_string());
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_FIXTURE_OUT_OF_BAND);
    }

    #[test]
    fn check__null_fixture_root__out_of_band_not_invalid() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let mut receipt = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        receipt.fixture_root_commit = None;
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_FIXTURE_OUT_OF_BAND);
    }

    #[test]
    fn check__unqualified_host__when_only_another_class_has_a_row() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let receipt = receipt_for(
            "doctor-warm",
            "ci-windows-latest",
            "a.json",
            &ten_samples(100),
        );
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_UNQUALIFIED_HOST);
        assert!(packet.results[0].p95_budget_ms.is_none());
    }

    #[test]
    fn check__empty_table__unqualified_workload_not_host() {
        let receipt = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        let packet = check_with_budgets(&[receipt], EMBED, &[]);
        assert_eq!(packet.results[0].status, STATUS_UNQUALIFIED_WORKLOAD);
        assert_eq!(packet.unqualified, 1);
    }

    #[test]
    fn check__unmeasured_resource__null_peak_and_partial_coverage() {
        let rows = vec![synthetic_row_with_rss(
            "impact-warm",
            "local-windows-2026-11",
        )];
        let mut receipt = receipt_for(
            "impact-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(700),
        );
        receipt.peak_rss_bytes = None;
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_UNMEASURED_RESOURCE);

        let mut covered = receipt_for(
            "impact-warm",
            "local-windows-2026-11",
            "b.json",
            &ten_samples(700),
        );
        covered.peak_rss_bytes = Some(1024);
        covered.rss_ticks = 5;
        covered.rss_measured_samples = 5;
        let packet = check_with_budgets(&[covered], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_UNMEASURED_RESOURCE);

        let mut measured = receipt_for(
            "impact-warm",
            "local-windows-2026-11",
            "c.json",
            &ten_samples(700),
        );
        measured.peak_rss_bytes = Some(1024);
        measured.rss_ticks = 10;
        measured.rss_measured_samples = 10;
        let packet = check_with_budgets(&[measured], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_WITHIN_BUDGET);
    }

    #[test]
    fn check__foreign_build__wins_over_over_budget() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let mut receipt = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(500),
        );
        receipt.build_sha = "ffffffffffff".to_string();
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_FOREIGN_BUILD);
        assert!(packet.results[0].p95_budget_ms.is_none());
    }

    #[test]
    fn check__duplicate_receipt__second_copy_flagged() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let a = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        let b = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "b.json",
            &ten_samples(100),
        );
        let packet = check_with_budgets(&[b, a], EMBED, &rows);
        assert_eq!(packet.results.len(), 2);
        assert_eq!(packet.results[0].status, STATUS_WITHIN_BUDGET);
        assert_eq!(packet.results[1].status, STATUS_DUPLICATE_RECEIPT);
    }

    #[test]
    fn check__invalid_receipt__n_zero_and_array_mismatch() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let mut zero = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        zero.n = 0;
        zero.samples_ms.clear();
        zero.observed_ms.clear();
        assert_eq!(
            check_with_budgets(&[zero], EMBED, &rows).results[0].status,
            STATUS_INVALID_RECEIPT
        );

        let mut mismatch = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "b.json",
            &ten_samples(100),
        );
        mismatch.observed_ms.pop();
        let packet = check_with_budgets(&[mismatch], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_INVALID_RECEIPT);
        assert!(
            packet.results[0]
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("observedMs")
        );
    }

    #[test]
    fn check__invalid_receipt__argv_and_start_state_and_setup() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];

        let mut argv = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        argv.child_args = vec!["index".to_string()];
        assert_eq!(
            check_with_budgets(&[argv], EMBED, &rows).results[0].status,
            STATUS_INVALID_RECEIPT
        );

        let mut state = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "b.json",
            &ten_samples(100),
        );
        state.start_state = "cold".to_string();
        assert_eq!(
            check_with_budgets(&[state], EMBED, &rows).results[0].status,
            STATUS_INVALID_RECEIPT
        );

        let mut setup = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "c.json",
            &ten_samples(100),
        );
        setup.setup_ms = None;
        assert_eq!(
            check_with_budgets(&[setup], EMBED, &rows).results[0].status,
            STATUS_INVALID_RECEIPT
        );
    }

    #[test]
    fn check__invalid_receipt__rss_bookkeeping() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let mut receipt = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        receipt.rss_measured_samples = 1;
        receipt.rss_ticks = 0;
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.results[0].status, STATUS_INVALID_RECEIPT);
    }

    #[test]
    fn check__target_status__p95_over_target_is_not_met() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        // p50 100 ms is under the 150 ms target; p95 200 ms is over it, while
        // still inside the 250 ms p95 ceiling.
        let mut samples = vec![100_i64; 9];
        samples.push(200);
        let receipt = receipt_for("doctor-warm", "local-windows-2026-11", "a.json", &samples);
        let packet = check_with_budgets(&[receipt], EMBED, &rows);
        assert_eq!(packet.results[0].p50_ms, 100);
        assert_eq!(packet.results[0].p95_ms, 200);
        assert_eq!(packet.results[0].target_status, "not-met");
        assert_eq!(packet.results[0].status, STATUS_WITHIN_BUDGET);
    }

    #[test]
    fn check__results_sorted_by_receipt_path__both_receipts_present() {
        let rows = vec![
            synthetic_row("doctor-warm", "local-windows-2026-11"),
            synthetic_row("impact-warm", "local-windows-2026-11"),
        ];
        let doctor = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "z.json",
            &ten_samples(100),
        );
        let impact = receipt_for(
            "impact-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        let packet = check_with_budgets(&[doctor, impact], EMBED, &rows);
        let paths = packet
            .results
            .iter()
            .map(|result| result.receipt_path.as_str())
            .collect::<Vec<_>>();
        // Sorted by (workload, hostClass, receiptPath): doctor-warm before impact-warm.
        assert_eq!(paths, vec!["z.json", "a.json"]);
    }

    #[test]
    fn check__counters__violations_plus_unqualified_equals_non_clean() {
        let rows = vec![synthetic_row("doctor-warm", "local-windows-2026-11")];
        let clean = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "a.json",
            &ten_samples(100),
        );
        let slow = receipt_for(
            "doctor-warm",
            "local-windows-2026-11",
            "b.json",
            &ten_samples(500),
        );
        let strange = receipt_for(
            "nope-warm",
            "local-windows-2026-11",
            "c.json",
            &ten_samples(100),
        );
        let packet = check_with_budgets(&[clean, slow, strange], EMBED, &rows);
        let non_clean = packet
            .results
            .iter()
            .filter(|result| result.status != STATUS_WITHIN_BUDGET)
            .count();
        assert_eq!(packet.violations + packet.unqualified, non_clean);
        assert_ne!(packet.status, STATUS_WITHIN_BUDGET);
    }
}
