//! Release in-process search p95 gate (0415). Ignored so `profile.ci` does
//! not run it. The slowdown stays in this module.

use std::thread;
use std::time::{Duration, Instant};

use camino::Utf8Path;

use crate::search::{TantivySearchEngine, rebuild_tantivy_index};
use crate::state::layout::Layout;
use crate::state::storage::timings::percentile_sorted;

const FIXTURE: &str = "\
pub struct TantivySearchEngine;
pub fn search_gate() {}
let alpha_beta = 1;
let a = b;
";

/// Same rank as [`percentile_sorted`]: `round(pct/100 * (n-1))`.
pub(crate) fn percentile_index(n: usize, pct: u8) -> usize {
    if n == 0 {
        return 0;
    }
    let rank = ((f64::from(pct) / 100.0) * (n as f64 - 1.0)).round() as usize;
    rank.min(n - 1)
}

pub(crate) fn percentile_ms(sorted: &[u128], pct: u8) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[percentile_index(sorted.len(), pct)]
}

/// `max(50, ceil_to_10(ceil(p95 * 5 / 4)))`.
pub(crate) fn budget_ms_from_p95(p95: u128) -> u128 {
    let scaled = p95.saturating_mul(5).div_ceil(4);
    let ceiled = scaled.div_ceil(10).saturating_mul(10);
    ceiled.max(50)
}

pub(crate) fn within_budget(p95: u128, budget: u128) -> bool {
    p95 <= budget
}

/// Filled from the Phase 0 release sample via [`budget_ms_from_p95`].
const WARM_P95_BUDGET_MS: u128 = 50;

fn write_fixture(root: &Utf8Path) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("src dir");
    std::fs::write(src.join("lib.rs"), FIXTURE).expect("fixture");
}

fn open_indexed_engine(layout: &Layout) -> TantivySearchEngine {
    rebuild_tantivy_index(layout).expect("index fixture");
    TantivySearchEngine::open_or_create(layout.search_index_dir().as_std_path()).expect("reopen")
}

fn measure_query(engine: &TantivySearchEngine) -> u128 {
    let started = Instant::now();
    engine.search("alpha beta", 10).expect("query");
    started.elapsed().as_millis()
}

#[test]
fn percentile_indexes_match_stored_helper() {
    let samples: Vec<i64> = (0..30).collect();
    assert_eq!(percentile_index(30, 50), 15);
    assert_eq!(percentile_index(30, 95), 28);
    assert_eq!(percentile_sorted(&samples, 50), samples[15]);
    assert_eq!(percentile_sorted(&samples, 95), samples[28]);
}

#[test]
fn budget_formula_floor_and_scale() {
    assert_eq!(budget_ms_from_p95(2), 50);
    assert_eq!(budget_ms_from_p95(40), 50);
    assert_eq!(budget_ms_from_p95(80), 100);
}

#[test]
fn synthetic_over_budget_p95_is_rejected() {
    let budget = budget_ms_from_p95(80);
    assert!(!within_budget(budget + 1, budget));
}

#[test]
fn test_local_sleep_around_query_rejects_budget() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 temp");
    write_fixture(root);
    let layout = Layout::new(root);
    let engine = open_indexed_engine(&layout);
    let budget = WARM_P95_BUDGET_MS;
    let started = Instant::now();
    engine.search("alpha beta", 10).expect("query");
    thread::sleep(Duration::from_millis(
        u64::try_from(budget + 100).unwrap_or(150),
    ));
    let elapsed = started.elapsed().as_millis();
    assert!(
        !within_budget(elapsed, budget),
        "sleep around the query must exceed budget {budget}, elapsed {elapsed}"
    );
}

#[ignore = "0415 release warm p95 gate; cargo test --release --lib -- --ignored"]
#[test]
fn search_perf_warm_query_budget() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 temp");
    write_fixture(root);
    let layout = Layout::new(root);
    let engine = open_indexed_engine(&layout);

    let cold_index_and_query_ms = measure_query(&engine);
    for _ in 0..5 {
        engine.search("alpha beta", 10).expect("warmup");
    }
    let mut warm_samples_ms = Vec::with_capacity(30);
    for _ in 0..30 {
        warm_samples_ms.push(measure_query(&engine));
    }
    warm_samples_ms.sort_unstable();
    assert_eq!(warm_samples_ms.len(), 30, "empty warm sample is a failure");
    let warm_p50_ms = percentile_ms(&warm_samples_ms, 50);
    let warm_p95_ms = percentile_ms(&warm_samples_ms, 95);
    let doc = serde_json::json!({
        "schemaVersion": 1,
        "kind": "searchPerf",
        "warmSamplesMs": warm_samples_ms,
        "warmP50Ms": warm_p50_ms,
        "warmP95Ms": warm_p95_ms,
        "budgetMs": WARM_P95_BUDGET_MS,
        "coldIndexAndQueryMs": cold_index_and_query_ms,
    });
    println!("{doc}");
    assert!(
        within_budget(warm_p95_ms, WARM_P95_BUDGET_MS),
        "warm p95 {warm_p95_ms} exceeds {WARM_P95_BUDGET_MS}: {doc}"
    );
}
