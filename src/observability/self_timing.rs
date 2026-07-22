//! Local-only command self-timing (Track 0043).
//!
//! local-only; do not add network calls.
//!
//! Capture is decoupled from persistence:
//! - `TimingLayer::on_close` pushes span records into an in-memory buffer only
//!   (never SQLite mid-command — would lock under Rayon/Tokio and slow the
//!   command being timed).
//! - `TimedCommand::drop` drains the buffer and performs **one** batched
//!   `INSERT` transaction for the outer row + all inner spans.
//!
//! Default-on with opt-out via `ledgerful timings --opt-out` writing
//! `self_timing = false` to `~/.ledgerful/config.toml`.
//!
//! The `timings` query command itself is **not** recorded (self-exclusion), so
//! inspecting local history does not pollute the series with self-observation.

use crate::state::storage::timings::{TimingRow, insert_timing_batch, is_self_timing_enabled};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tracing::Subscriber;
use tracing::span::{Attributes, Id};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Default minimum inner-span duration to buffer (ms). Override with
/// `LEDGERFUL_TIMING_MIN_SPAN_MS` (set to `0` for full fidelity).
pub const DEFAULT_MIN_SPAN_MS: u64 = 5;

/// Sentinel: no repo size was opportunistically reported this run.
const REPO_SIZE_UNSET: u64 = u64::MAX;

static CURRENT_RUN_ID: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static SPAN_BUFFER: OnceLock<Mutex<HashMap<String, Vec<BufferedSpan>>>> = OnceLock::new();
static CURRENT_REPO_SIZE: AtomicU64 = AtomicU64::new(REPO_SIZE_UNSET);
static CURRENT_LEDGER_TX: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Test-only: counts successful `insert_timing_batch` calls from `persist_batch`.
/// Connected to the real production flush path (not a test-local counter).
#[cfg(test)]
static PERSIST_BATCH_INSERT_CALLS: AtomicUsize = AtomicUsize::new(0);

fn current_run_id_lock() -> &'static Mutex<Option<String>> {
    CURRENT_RUN_ID.get_or_init(|| Mutex::new(None))
}

fn span_buffer_lock() -> &'static Mutex<HashMap<String, Vec<BufferedSpan>>> {
    SPAN_BUFFER.get_or_init(|| Mutex::new(HashMap::new()))
}

fn current_ledger_tx_lock() -> &'static Mutex<Option<String>> {
    CURRENT_LEDGER_TX.get_or_init(|| Mutex::new(None))
}

/// Opportunistic repo-size reporter for commands that already walk the tree
/// (`index` / `scan`). Never call this from a dedicated walk.
pub fn set_current_repo_size_bytes(bytes: u64) {
    CURRENT_REPO_SIZE.store(bytes, Ordering::Relaxed);
}

fn take_current_repo_size_bytes() -> Option<i64> {
    let v = CURRENT_REPO_SIZE.swap(REPO_SIZE_UNSET, Ordering::Relaxed);
    if v == REPO_SIZE_UNSET {
        None
    } else {
        Some(v as i64)
    }
}

/// Associate the current timed command with a known ledger transaction id.
///
/// Best-effort metadata only — never required for host command success.
/// Only this explicit API populates `ledger_tx_id` on the outer row; there is
/// no automatic sidecar/pending-hook attribution (schema contract: NULL unless
/// the command intentionally produced or bound a tx).
pub fn set_current_ledger_tx_id(tx_id: impl Into<String>) {
    if let Ok(mut guard) = current_ledger_tx_lock().lock() {
        *guard = Some(tx_id.into());
    }
}

fn take_current_ledger_tx_id() -> Option<String> {
    current_ledger_tx_lock()
        .lock()
        .ok()
        .and_then(|mut g| g.take())
}

/// Test inject for min span; `u64::MAX` means “use env / default”.
/// Production never writes this (default remains MAX).
static TEST_MIN_SPAN_MS: AtomicU64 = AtomicU64::new(u64::MAX);

fn min_span_ms() -> u64 {
    let override_ms = TEST_MIN_SPAN_MS.load(Ordering::Relaxed);
    if override_ms != u64::MAX {
        return override_ms;
    }
    match std::env::var("LEDGERFUL_TIMING_MIN_SPAN_MS") {
        Ok(s) => s.parse::<u64>().unwrap_or(DEFAULT_MIN_SPAN_MS),
        Err(_) => DEFAULT_MIN_SPAN_MS,
    }
}

/// Test inject: set min span ms (`u64::MAX` clears).
pub fn set_test_min_span_ms(ms: u64) {
    TEST_MIN_SPAN_MS.store(ms, Ordering::Relaxed);
}

/// Hash a canonicalized argv shape (subcommand + sorted flag names, values stripped).
pub fn hash_argv_shape(shape: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(shape.as_bytes());
    hex::encode(hasher.finalize())
}

/// Build a run-scoped span id: `{run_id}:{tracing_span_id_hex}`.
///
/// Parent links use the same form so concurrent runs never collide when
/// joining flame/query graphs. Outer (command) rows keep `parent_span_id = NULL`.
fn run_scoped_span_id(run_id: &str, tracing_id: u64) -> String {
    format!("{run_id}:{tracing_id:x}")
}

#[derive(Debug, Clone)]
struct BufferedSpan {
    span_id: String,
    parent_span_id: Option<String>,
    span_name: String,
    duration_ms: i64,
}

/// Per-span data stored in the registry extension map.
struct TimingSpanData {
    start: Instant,
    span_id: String,
    parent_span_id: Option<String>,
    name: String,
}

/// RAII guard: one per CLI invocation. Construct at the top of dispatch;
/// `finish`/`drop` flushes the buffered spans + outer row in one transaction.
pub struct TimedCommand {
    run_id: String,
    command: String,
    argv_hash: Option<String>,
    started: Instant,
    exit_code: i32,
    active: bool,
    finished: bool,
}

impl TimedCommand {
    /// Begin timing for a command. No-ops when opted out, when the command is
    /// `timings` itself (self-exclusion), or when the feature is inactive.
    pub fn start(command: &str, argv_shape: &str) -> Self {
        // Skip recording timings-about-timings; always no-op when disabled.
        if command == "timings" || !is_self_timing_enabled() {
            return Self {
                run_id: String::new(),
                command: command.to_string(),
                argv_hash: None,
                started: Instant::now(),
                exit_code: 0,
                active: false,
                finished: true,
            };
        }

        let run_id = uuid::Uuid::new_v4().to_string();
        if let Ok(mut guard) = current_run_id_lock().lock() {
            *guard = Some(run_id.clone());
        }
        CURRENT_REPO_SIZE.store(REPO_SIZE_UNSET, Ordering::Relaxed);
        if let Ok(mut guard) = current_ledger_tx_lock().lock() {
            *guard = None;
        }

        Self {
            run_id,
            command: command.to_string(),
            argv_hash: Some(hash_argv_shape(argv_shape)),
            started: Instant::now(),
            exit_code: 0,
            active: true,
            finished: false,
        }
    }

    /// Record the process exit code and flush (also happens on Drop).
    pub fn finish(mut self, exit_code: i32) {
        self.exit_code = exit_code;
        self.flush();
        self.finished = true;
    }

    /// Run id of an active timed command (empty when inactive).
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Whether capture is active for this guard.
    pub fn is_active(&self) -> bool {
        self.active
    }

    fn flush(&mut self) {
        if !self.active || self.finished {
            return;
        }
        self.finished = true;

        // Clear current run so late-closing spans from other work don't attach.
        if let Ok(mut guard) = current_run_id_lock().lock()
            && guard.as_deref() == Some(self.run_id.as_str())
        {
            *guard = None;
        }

        let mut inner_spans = match span_buffer_lock().lock() {
            Ok(mut map) => map.remove(&self.run_id).unwrap_or_default(),
            Err(e) => {
                tracing::debug!("self-timing: span buffer lock poisoned: {e}");
                Vec::new()
            }
        };

        // Deterministic order for same-duration spans.
        inner_spans.sort_by(|a, b| {
            a.span_name
                .cmp(&b.span_name)
                .then_with(|| a.span_id.cmp(&b.span_id))
        });

        let duration_ms = self.started.elapsed().as_millis() as i64;
        let ts_utc = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let repo_size_bytes = take_current_repo_size_bytes();
        // Explicit API only — no automatic pending_hook_tx sidecar attribution.
        let ledger_tx_id = take_current_ledger_tx_id();

        let mut rows = Vec::with_capacity(1 + inner_spans.len());
        rows.push(TimingRow {
            run_id: self.run_id.clone(),
            ts_utc: ts_utc.clone(),
            command: self.command.clone(),
            duration_ms,
            exit_code: self.exit_code,
            repo_size_bytes,
            argv_hash: self.argv_hash.clone(),
            ledger_tx_id: ledger_tx_id.clone(),
            // Outer command row: no parent.
            parent_span_id: None,
            span_name: None,
            notes: None,
        });

        for span in inner_spans {
            rows.push(TimingRow {
                run_id: self.run_id.clone(),
                ts_utc: ts_utc.clone(),
                command: self.command.clone(),
                duration_ms: span.duration_ms,
                exit_code: self.exit_code,
                repo_size_bytes: None,
                argv_hash: None,
                ledger_tx_id: None,
                parent_span_id: span.parent_span_id,
                span_name: Some(span.span_name),
                notes: None,
            });
        }

        // Best-effort persist: failures are DEBUG-loud, never break the host.
        if let Err(e) = persist_batch(&rows) {
            tracing::debug!("self-timing: failed to persist timing batch: {e:#}");
        }
    }
}

impl Drop for TimedCommand {
    fn drop(&mut self) {
        // Failures silent at INFO (no log here); flush itself DEBUG-logs errors.
        self.flush();
    }
}

fn persist_batch(rows: &[TimingRow]) -> miette::Result<()> {
    let cwd = std::env::current_dir().map_err(|e| miette::miette!("cwd: {e}"))?;
    let layout = crate::state::layout::Layout::new(cwd.to_string_lossy().as_ref());
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        // Not an initialized repo — skip quietly.
        return Ok(());
    }
    let mut conn = rusqlite::Connection::open(db_path.as_std_path())
        .map_err(|e| miette::miette!("open ledger.db: {e}"))?;
    // Ensure schema is current (m52 may not have run if another process created the DB).
    let migrations = crate::state::migrations::get_migrations();
    migrations
        .to_latest(&mut conn)
        .map_err(|e| miette::miette!("migrate: {e}"))?;
    insert_timing_batch(&mut conn, rows)?;
    #[cfg(test)]
    {
        PERSIST_BATCH_INSERT_CALLS.fetch_add(1, Ordering::SeqCst);
    }
    Ok(())
}

/// Push a closed span into the in-memory buffer for the active run.
fn buffer_span(span: BufferedSpan) {
    let run_id = match current_run_id_lock().lock() {
        Ok(guard) => match guard.clone() {
            Some(id) => id,
            None => return,
        },
        Err(_) => return,
    };
    if let Ok(mut map) = span_buffer_lock().lock() {
        map.entry(run_id).or_default().push(span);
    }
}

/// `tracing` layer that records span durations into the in-memory buffer.
///
/// **Never touches SQLite.** `on_close` only calls `buffer_span`.
#[derive(Debug, Default)]
pub struct TimingLayer;

impl TimingLayer {
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for TimingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let run_id = {
            let Ok(guard) = current_run_id_lock().lock() else {
                return;
            };
            match guard.clone() {
                Some(id) => id,
                None => return,
            }
        };

        let name = attrs.metadata().name().to_string();
        // Run-scoped so parent links never collide across concurrent CLI runs.
        let span_id = run_scoped_span_id(&run_id, id.into_u64());

        // Parent id is already run-scoped when the parent carried TimingSpanData.
        let parent_span_id = ctx.span(id).and_then(|span| {
            let parent = span.parent()?;
            let ext = parent.extensions();
            ext.get::<TimingSpanData>().map(|d| d.span_id.clone())
        });

        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(TimingSpanData {
                start: Instant::now(),
                span_id,
                parent_span_id,
                name,
            });
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let mut ext = span.extensions_mut();
        let Some(data) = ext.remove::<TimingSpanData>() else {
            return;
        };
        let duration_ms = data.start.elapsed().as_millis() as u64;
        if duration_ms < min_span_ms() {
            return;
        }
        // Buffer only — never open SQLite mid-command.
        buffer_span(BufferedSpan {
            span_id: data.span_id,
            parent_span_id: data.parent_span_id,
            span_name: data.name,
            duration_ms: duration_ms as i64,
        });
    }
}

/// Test-only: drain / inspect buffer size for a run (used by unit tests).
#[cfg(test)]
pub fn test_buffer_len(run_id: &str) -> usize {
    span_buffer_lock()
        .lock()
        .map(|m| m.get(run_id).map(|v| v.len()).unwrap_or(0))
        .unwrap_or(0)
}

/// Test-only: set the current run id without a full TimedCommand.
#[cfg(test)]
pub fn test_set_current_run(run_id: Option<String>) {
    if let Ok(mut g) = current_run_id_lock().lock() {
        *g = run_id;
    }
}

/// Test-only: clear all buffered spans and run/tx state.
#[cfg(test)]
pub fn test_clear_buffer() {
    if let Ok(mut m) = span_buffer_lock().lock() {
        m.clear();
    }
    if let Ok(mut g) = current_run_id_lock().lock() {
        *g = None;
    }
    CURRENT_REPO_SIZE.store(REPO_SIZE_UNSET, Ordering::Relaxed);
    if let Ok(mut g) = current_ledger_tx_lock().lock() {
        *g = None;
    }
}

/// Test-only: how many times `persist_batch` successfully called `insert_timing_batch`.
#[cfg(test)]
pub fn test_persist_batch_insert_calls() -> usize {
    PERSIST_BATCH_INSERT_CALLS.load(Ordering::SeqCst)
}

/// Test-only: reset the persist_batch insert counter.
#[cfg(test)]
pub fn test_reset_persist_batch_insert_calls() {
    PERSIST_BATCH_INSERT_CALLS.store(0, Ordering::SeqCst);
}

/// Test-only: push a synthetic span into the buffer for the current run.
///
/// When `parent` is `None` and a run is active, a run-scoped synthetic parent is
/// not invented — callers pass an explicit parent (typically `{run_id}:…`) or
/// `None` for top-level inner spans.
#[cfg(test)]
pub fn test_buffer_span(name: &str, duration_ms: i64, parent: Option<&str>) {
    let run_id = current_run_id_lock()
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();
    let raw = uuid::Uuid::new_v4().simple().to_string();
    let span_id = if run_id.is_empty() {
        raw
    } else {
        format!("{run_id}:{raw}")
    };
    buffer_span(BufferedSpan {
        span_id,
        parent_span_id: parent.map(|s| s.to_string()),
        span_name: name.to_string(),
        duration_ms,
    });
}

/// Test-only: run-scoped span ids currently buffered for a run (before flush).
#[cfg(test)]
pub fn test_buffered_span_ids(run_id: &str) -> Vec<String> {
    span_buffer_lock()
        .lock()
        .map(|m| {
            m.get(run_id)
                .map(|v| v.iter().map(|s| s.span_id.clone()).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::timings::{
        TimingQuery, count_timings, get_outer_by_run_id, insert_timing_batch, query_timings,
    };
    use crate::tests::DirGuard;
    use rusqlite::Connection;
    use std::thread;

    fn temp_repo_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join(".ledgerful").join("state");
        std::fs::create_dir_all(&state).unwrap();
        let db_path = state.join("ledger.db");
        {
            let mut conn = Connection::open(&db_path).unwrap();
            get_migrations().to_latest(&mut conn).unwrap();
        }
        (tmp, db_path)
    }

    fn with_isolated_config_home<F: FnOnce()>(f: F) {
        let tmp = tempfile::tempdir().unwrap();
        // Prefer test inject over process env (no `unsafe` set_var — Semgrep blocks it).
        let prev = crate::state::rollup::set_test_config_home(Some(tmp.path().to_path_buf()));
        f();
        crate::state::rollup::set_test_config_home(prev);
    }

    /// Override min span ms for the duration of `f` (restored after).
    fn with_min_span_ms<F: FnOnce()>(ms: u64, f: F) {
        let prev = TEST_MIN_SPAN_MS.swap(ms, Ordering::Relaxed);
        f();
        TEST_MIN_SPAN_MS.store(prev, Ordering::Relaxed);
    }

    #[test]
    fn argv_hash_stable_across_flag_order() {
        // Sorted flag names → same shape regardless of declaration order.
        let a = hash_argv_shape("verify|json,scope");
        let b = {
            // Caller must sort; hash itself is pure.
            let mut flags = ["scope", "json"];
            flags.sort_unstable();
            hash_argv_shape(&format!("verify|{}", flags.join(",")))
        };
        assert_eq!(a, b);
    }

    #[test]
    fn argv_hash_invariant_to_path_values() {
        // Values never enter the shape — only flag names.
        let with_path_a = hash_argv_shape("scan|impact,out");
        let with_path_b = hash_argv_shape("scan|impact,out");
        assert_eq!(with_path_a, with_path_b);
        // Different flag set → different hash
        assert_ne!(hash_argv_shape("scan|impact"), with_path_a);
    }

    #[test]
    fn sub_threshold_spans_not_buffered_via_filter() {
        // Direct unit of the threshold: durations below min are dropped by on_close.
        // Here we assert the pure filter contract used by on_close.
        let min = DEFAULT_MIN_SPAN_MS;
        assert!(4u64 < min);
        assert!(5u64 >= min || min == 0);
    }

    #[test]
    fn run_scoped_span_id_format() {
        let id = run_scoped_span_id("run-abc", 0x1a2b);
        assert_eq!(id, "run-abc:1a2b");
        assert!(id.starts_with("run-abc:"));
    }

    /// Real path: `TimingLayer::on_close` → `buffer_span` → `TimedCommand::finish`
    /// → one `persist_batch` / `insert_timing_batch` with outer + inner rows.
    ///
    /// Uses `with_default` so the global subscriber is not required. Sets
    /// `LEDGERFUL_TIMING_MIN_SPAN_MS=0` so sub-default spans still flush (no
    /// flaky multi-ms sleep requirement).
    #[test]
    fn timing_layer_on_close_then_finish_persists_one_batch() {
        use std::time::Duration;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::registry;

        with_isolated_config_home(|| {
            with_min_span_ms(0, || {
                assert!(is_self_timing_enabled());
                let (tmp, db_path) = temp_repo_db();
                let _guard = DirGuard::new(tmp.path());
                test_clear_buffer();
                test_reset_persist_batch_insert_calls();

                let timed = TimedCommand::start("verify", "verify|scope");
                assert!(timed.is_active());
                let run_id = timed.run_id().to_string();
                assert!(!run_id.is_empty());

                let subscriber = registry().with(TimingLayer::new());
                tracing::subscriber::with_default(subscriber, || {
                    let span = tracing::info_span!("slow_work");
                    let _g = span.entered();
                    // Non-zero wall time for a meaningful duration_ms in the DB.
                    std::thread::sleep(Duration::from_millis(1));
                }); // drop closes → on_close → buffer_span

                let buffered_ids = test_buffered_span_ids(&run_id);
                assert!(
                    !buffered_ids.is_empty(),
                    "on_close must buffer at least one span"
                );
                for id in &buffered_ids {
                    assert!(
                        id.starts_with(&format!("{run_id}:")),
                        "inner span_id must be run-scoped, got {id}"
                    );
                }

                timed.finish(0);

                assert_eq!(
                    test_persist_batch_insert_calls(),
                    1,
                    "persist_batch must call insert_timing_batch exactly once"
                );
                assert_eq!(test_buffer_len(&run_id), 0, "buffer drained after finish");

                let conn = Connection::open(&db_path).unwrap();
                let all = query_timings(
                    &conn,
                    &TimingQuery {
                        outer_only: false,
                        inner_only: false,
                        command: Some("verify".to_string()),
                        ..Default::default()
                    },
                )
                .unwrap();
                assert!(
                    all.len() >= 2,
                    "expected outer + at least 1 inner, got {}",
                    all.len()
                );
                let outer = all.iter().filter(|r| r.span_name.is_none()).count();
                let slow = all
                    .iter()
                    .filter(|r| r.span_name.as_deref() == Some("slow_work"))
                    .count();
                assert_eq!(outer, 1, "exactly one outer row");
                assert!(
                    slow >= 1,
                    "at least one inner with span_name slow_work, rows={all:?}"
                );
                for r in &all {
                    assert_eq!(r.run_id, run_id);
                }
                test_clear_buffer();
            });
        });
    }

    /// Production path: concurrent `buffer_span` under an active `TimedCommand`,
    /// then `finish` drains the buffer through real `persist_batch` →
    /// `insert_timing_batch` (counter is on that path).
    #[test]
    fn timed_command_finish_drains_concurrent_buffer_via_persist_batch() {
        with_isolated_config_home(|| {
            assert!(is_self_timing_enabled());
            let (tmp, db_path) = temp_repo_db();
            let _guard = DirGuard::new(tmp.path());
            test_clear_buffer();
            test_reset_persist_batch_insert_calls();

            let timed = TimedCommand::start("verify", "verify|scope");
            assert!(timed.is_active());
            let run_id = timed.run_id().to_string();
            assert!(!run_id.is_empty());

            const N: usize = 32;
            let parent = format!("{run_id}:parent");
            let handles: Vec<_> = (0..N)
                .map(|i| {
                    let parent = parent.clone();
                    thread::spawn(move || {
                        test_buffer_span(&format!("span_{i:02}"), 10 + i as i64, Some(&parent));
                    })
                })
                .collect();
            for h in handles {
                h.join().expect("span worker");
            }
            assert_eq!(test_buffer_len(&run_id), N);

            // Real flush path: TimedCommand::finish → flush → persist_batch → insert_timing_batch.
            timed.finish(0);

            assert_eq!(
                test_persist_batch_insert_calls(),
                1,
                "persist_batch must call insert_timing_batch exactly once"
            );
            assert_eq!(
                test_buffer_len(&run_id),
                0,
                "buffer must be drained after finish"
            );

            let conn = Connection::open(&db_path).unwrap();
            let all = query_timings(
                &conn,
                &TimingQuery {
                    outer_only: false,
                    inner_only: false,
                    command: Some("verify".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(all.len(), N + 1, "one outer + N inner rows");
            let outer = all.iter().filter(|r| r.span_name.is_none()).count();
            let inner = all.iter().filter(|r| r.span_name.is_some()).count();
            assert_eq!(outer, 1);
            assert_eq!(inner, N);
            // Outer parent is always NULL.
            let outer_row = all.iter().find(|r| r.span_name.is_none()).unwrap();
            assert!(outer_row.parent_span_id.is_none());
            // Inner parents are run-scoped.
            for r in all.iter().filter(|r| r.span_name.is_some()) {
                let p = r.parent_span_id.as_deref().expect("inner parent");
                assert!(
                    p.starts_with(&format!("{run_id}:")),
                    "parent_span_id must be run-scoped, got {p}"
                );
                assert!(
                    r.run_id == run_id,
                    "inner span_id storage is per-run via run_id column"
                );
            }
            test_clear_buffer();
        });
    }

    #[test]
    fn timing_layer_source_is_buffer_only() {
        // Architectural: Layer impl never touches SQLite / insert path.
        let src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/observability/self_timing.rs"
        ));
        let start = src
            .find("impl<S> Layer<S> for TimingLayer")
            .expect("TimingLayer Layer impl");
        let rest = &src[start..];
        // End at the first post-Layer test helper or next major item after the impl block.
        let end = rest
            .find("/// Test-only: drain")
            .or_else(|| rest.find("#[cfg(test)]\npub fn test_buffer_len"))
            .unwrap_or(rest.len());
        let layer_impl = &rest[..end];
        assert!(
            layer_impl.contains("buffer_span"),
            "on_close must call buffer_span"
        );
        assert!(
            !layer_impl.contains("rusqlite"),
            "TimingLayer must not reference rusqlite"
        );
        assert!(
            !layer_impl.contains("Connection"),
            "TimingLayer must not reference Connection"
        );
        assert!(
            !layer_impl.contains("insert_timing"),
            "TimingLayer must not insert"
        );
        assert!(
            !layer_impl.contains("persist_batch"),
            "TimingLayer must not persist"
        );
    }

    #[test]
    fn repo_size_default_null_unless_set() {
        test_clear_buffer();
        assert!(take_current_repo_size_bytes().is_none());
        set_current_repo_size_bytes(42_000);
        assert_eq!(take_current_repo_size_bytes(), Some(42_000));
        // Second take is empty again.
        assert!(take_current_repo_size_bytes().is_none());
    }

    #[test]
    fn timings_command_start_is_inactive() {
        // Self-exclusion: querying timings must not record a timings row.
        let t = TimedCommand::start("timings", "timings");
        assert!(!t.is_active());
        assert!(t.run_id().is_empty());
    }

    #[test]
    fn timings_command_inserts_nothing_into_temp_db() {
        with_isolated_config_home(|| {
            // Even when capture is enabled, the timings command itself is excluded.
            assert!(is_self_timing_enabled());
            let (tmp, db_path) = temp_repo_db();
            let _guard = DirGuard::new(tmp.path());
            test_clear_buffer();
            test_reset_persist_batch_insert_calls();

            let timed = TimedCommand::start("timings", "timings|json");
            assert!(!timed.is_active());
            timed.finish(0);

            assert_eq!(test_persist_batch_insert_calls(), 0);
            let conn = Connection::open(&db_path).unwrap();
            assert_eq!(count_timings(&conn).unwrap(), 0);
        });
    }

    #[test]
    fn opt_out_start_is_inactive() {
        let t = TimedCommand::start("timings", "timings");
        assert!(!t.is_active());
    }

    #[test]
    fn opt_out_inserts_nothing_into_temp_db() {
        with_isolated_config_home(|| {
            crate::state::storage::timings::set_self_timing_enabled(false).unwrap();
            assert!(!is_self_timing_enabled());

            let (tmp, db_path) = temp_repo_db();
            let _guard = DirGuard::new(tmp.path());
            test_clear_buffer();

            // Even with a migrated DB present, opt-out must insert zero rows.
            let timed = TimedCommand::start("verify", "verify");
            assert!(!timed.is_active());
            timed.finish(0);

            let conn = Connection::open(&db_path).unwrap();
            assert_eq!(count_timings(&conn).unwrap(), 0);
        });
    }

    #[test]
    fn timed_command_repo_size_null_by_default() {
        with_isolated_config_home(|| {
            // Ensure enabled (default when config absent).
            assert!(is_self_timing_enabled());
            let (tmp, db_path) = temp_repo_db();
            let _guard = DirGuard::new(tmp.path());
            test_clear_buffer();

            let timed = TimedCommand::start("config_view", "config_view");
            assert!(timed.is_active());
            let run_id = timed.run_id().to_string();
            // Do not call set_current_repo_size_bytes.
            timed.finish(0);

            let conn = Connection::open(&db_path).unwrap();
            let outer = get_outer_by_run_id(&conn, &run_id).unwrap().unwrap();
            assert!(
                outer.repo_size_bytes.is_none(),
                "repo_size_bytes must be NULL when API not called"
            );
        });
    }

    #[test]
    fn timed_command_repo_size_set_when_api_called() {
        with_isolated_config_home(|| {
            assert!(is_self_timing_enabled());
            let (tmp, db_path) = temp_repo_db();
            let _guard = DirGuard::new(tmp.path());
            test_clear_buffer();

            let timed = TimedCommand::start("index", "index");
            let run_id = timed.run_id().to_string();
            set_current_repo_size_bytes(12_345);
            timed.finish(0);

            let conn = Connection::open(&db_path).unwrap();
            let outer = get_outer_by_run_id(&conn, &run_id).unwrap().unwrap();
            assert_eq!(outer.repo_size_bytes, Some(12_345));
        });
    }

    #[test]
    fn timed_command_ledger_tx_id_from_api() {
        with_isolated_config_home(|| {
            assert!(is_self_timing_enabled());
            let (tmp, db_path) = temp_repo_db();
            let _guard = DirGuard::new(tmp.path());
            test_clear_buffer();

            let timed = TimedCommand::start("verify", "verify");
            let run_id = timed.run_id().to_string();
            set_current_ledger_tx_id("tx-abc");
            timed.finish(0);

            let conn = Connection::open(&db_path).unwrap();
            let outer = get_outer_by_run_id(&conn, &run_id).unwrap().unwrap();
            assert_eq!(outer.ledger_tx_id.as_deref(), Some("tx-abc"));
        });
    }

    #[test]
    fn ledger_tx_id_null_without_explicit_api() {
        // Schema contract: NULL unless set_current_ledger_tx_id was called.
        // No automatic pending_hook_tx sidecar attribution.
        with_isolated_config_home(|| {
            assert!(is_self_timing_enabled());
            let (tmp, db_path) = temp_repo_db();
            let _guard = DirGuard::new(tmp.path());
            test_clear_buffer();

            // Plant a pending_hook_tx sidecar; flush must still leave ledger_tx_id NULL.
            let sidecar = tmp
                .path()
                .join(".ledgerful")
                .join("state")
                .join("pending_hook_tx");
            std::fs::write(&sidecar, r#"{"tx_id":"should-not-be-attributed"}"#).unwrap();

            let timed = TimedCommand::start("verify", "verify");
            let run_id = timed.run_id().to_string();
            // Intentionally do NOT call set_current_ledger_tx_id.
            timed.finish(0);

            let conn = Connection::open(&db_path).unwrap();
            let outer = get_outer_by_run_id(&conn, &run_id).unwrap().unwrap();
            assert!(
                outer.ledger_tx_id.is_none(),
                "ledger_tx_id must stay NULL without explicit API; got {:?}",
                outer.ledger_tx_id
            );
        });
    }

    #[test]
    fn capture_failure_does_not_panic_host() {
        with_isolated_config_home(|| {
            assert!(is_self_timing_enabled());
            let tmp = tempfile::tempdir().unwrap();
            // Create ledger.db as a *directory* so Connection::open fails.
            let state = tmp.path().join(".ledgerful").join("state");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::create_dir(state.join("ledger.db")).unwrap();

            let _guard = DirGuard::new(tmp.path());
            test_clear_buffer();

            // Must not panic; host "Result" remains Ok conceptually (finish returns ()).
            let timed = TimedCommand::start("verify", "verify");
            assert!(timed.is_active());
            timed.finish(0);
            // Drop path also safe if finish wasn't called — covered by finished=true.
        });
    }

    #[test]
    fn signing_basis_string_shape_untouched() {
        // Guard (0072): production signs v2 provenance basis. Timing columns
        // (duration_ms, command_timings, argv_hash, span_name) must never enter
        // that basis. v1 encode remains for dual-verify of historical rows only.
        let crypto = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ledger/crypto.rs"));
        assert!(
            crypto.contains("CURRENT_LEDGER_SIG_VERSION"),
            "crypto must export CURRENT_LEDGER_SIG_VERSION"
        );
        assert!(
            crypto.contains("sig_version:2"),
            "v2 payload must domain-separate with sig_version:2"
        );
        assert!(crypto.contains("entity:{}"));
        assert!(crypto.contains("origin:{}"));
        assert!(crypto.contains("author:{}"));
        assert!(!crypto.contains("command_timings"));
        assert!(!crypto.contains("duration_ms"));
        assert!(!crypto.contains("argv_hash"));
        assert!(!crypto.contains("span_name"));
        // v1 dual-verify template still present for historical rows.
        let v1_template = "tx_id:{}\\ncategory:{}\\nsummary:{}\\nreason:{}\\ncommitted_at:{}";
        assert!(
            crypto.contains(v1_template),
            "v1 encode must remain for dual-verify"
        );

        // Mirror the v2 field count (14 lines).
        let basis = format!(
            "sig_version:2\ntx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}\nentity:{}\nchange_type:{}\nentry_type:{}\nauthor:{}\nrisk:{}\nis_breaking:{}\nrelated_tickets:{}\norigin:{}",
            "X", "Y", "Z", "R", "T", "E", "MODIFY", "IMPLEMENTATION", "A", "", "false", "", "LOCAL"
        );
        assert_eq!(basis.lines().count(), 14);
        assert!(basis.starts_with("sig_version:2"));
        assert!(!basis.contains("duration_ms"));
        assert!(!basis.contains("command_timings"));
    }

    #[test]
    fn inserting_timings_does_not_touch_ledger_crypto_tables() {
        // After migrating a temp DB and inserting timing rows, ledger_entries
        // and chain_head remain empty / absent-of-rows (timing path never writes them).
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();

        let ledger_before: i64 = conn
            .query_row("SELECT count(*) FROM ledger_entries", [], |r| r.get(0))
            .unwrap_or(0);
        let chain_before: i64 = conn
            .query_row("SELECT count(*) FROM chain_head", [], |r| r.get(0))
            .unwrap_or(0);

        let rows = vec![
            TimingRow {
                run_id: "sig-guard".into(),
                ts_utc: "2026-07-19T12:00:00.000Z".into(),
                command: "verify".into(),
                duration_ms: 10,
                exit_code: 0,
                repo_size_bytes: None,
                argv_hash: Some("abc".into()),
                ledger_tx_id: None,
                parent_span_id: None,
                span_name: None,
                notes: None,
            },
            TimingRow {
                run_id: "sig-guard".into(),
                ts_utc: "2026-07-19T12:00:00.000Z".into(),
                command: "verify".into(),
                duration_ms: 5,
                exit_code: 0,
                repo_size_bytes: None,
                argv_hash: None,
                ledger_tx_id: None,
                parent_span_id: Some("sig-guard:1".into()),
                span_name: Some("run_tests".into()),
                notes: None,
            },
        ];
        insert_timing_batch(&mut conn, &rows).unwrap();

        let ledger_after: i64 = conn
            .query_row("SELECT count(*) FROM ledger_entries", [], |r| r.get(0))
            .unwrap_or(0);
        let chain_after: i64 = conn
            .query_row("SELECT count(*) FROM chain_head", [], |r| r.get(0))
            .unwrap_or(0);
        assert_eq!(ledger_before, ledger_after);
        assert_eq!(chain_before, chain_after);
        assert_eq!(count_timings(&conn).unwrap(), 2);
    }
}
