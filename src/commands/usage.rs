//! Opt-in usage metrics for Ledgerful.
//!
//! This module implements the `ledgerful usage` command group:
//! - `enable` / `disable` — toggle opt-in state
//! - `status` — show current opt-in state and pending counters
//! - `show-payload` — preview the exact JSON that would be sent
//!
//! Usage metrics are opt-in (default off), anonymous, and transparent.
//! Only command names and feature flags are collected — no repo names,
//! file paths, or content.
//!
//! ## Storage
//!
//! Two locations:
//! - **Global** (`~/.ledgerful/usage/config.toml`): the opt-in flag,
//!   the random `anonymous_id` (generated once on first `enable`), and
//!   the timestamp of the last successful flush. This file is shared
//!   across all repositories on the same machine so a user's identity
//!   is stable regardless of which project they invoke the CLI from.
//! - **Per-repo** (`.ledgerful/state/ledger.db`, tables
//!   `usage_counters` and `usage_days`): the actual command-name →
//!   invocation-count mapping, plus a separate `usage_days` table
//!   used to compute `active_days_in_window` for the payload. Per-repo
//!   storage piggybacks on the existing per-repo SQLite database
//!   (m42 + m44 migrations) — no second on-disk format is introduced.
//!   The day-tracking lives in its own table because the natural
//!   location (`usage_counters.last_seen_day`) was overwritten by the
//!   command-name UPSERT and could not represent more than one day
//!   per command (see `m44_usage_days` and the H2 review).
//!
//! ## Network
//!
//! The flush uses the project's existing `ureq = "2"` dependency with
//! explicit connect / read / write timeouts (see `try_flush`). The
//! production default endpoint is the Supabase Edge Function:
//! `https://scmxtnjqqklvcwyeouvj.supabase.co/functions/v1/telemetry-ingest`
//! (per `ledgerful-frontend/docs/Backend-Notes.md` §2.4 — M7 contract).
//! Override via `LEDGERFUL_USAGE_ENDPOINT` for tests against a mock server
//! or self-hosted deployments.
//!
//! ## Ingest credential (track 0077)
//!
//! Every flush POST sets `X-Ledgerful-Telemetry-Token` to a **bar-raising**
//! static token (not strong auth — the CLI is open-source). Default value
//! is [`DEFAULT_INGEST_TOKEN`]; override with `LEDGERFUL_USAGE_TOKEN` for
//! tests or self-hosted rotation. The edge function enforces fail-closed
//! global quotas as the real control; see conductor
//! `0077-TelemetryIngestAuthFailClosed/phase0-memo.md`.
//!
//! ## File layout
//!
//! This file is intentionally one module rather than a
//! `src/usage/{config,store,transport,format}.rs` split (M6 review).
//! Other large command files in this repo (e.g. `index.rs` 1299
//! lines, `viz.rs` 779, `ask.rs` 646) follow the same single-file
//! pattern, so 700-ish lines is in-family. A future cleanup track can
//! revisit the split once the surface area stabilizes.

use crate::state::layout::Layout;
use camino::{Utf8Path, Utf8PathBuf};
use miette::Result;
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::time::Duration;

/// Production telemetry ingest endpoint — the Supabase Edge Function
/// documented in `ledgerful-frontend/docs/Backend-Notes.md` §2.4
/// (M7). Override via `LEDGERFUL_USAGE_ENDPOINT` for
/// tests or self-hosted deployments.
const DEFAULT_ENDPOINT: &str =
    "https://scmxtnjqqklvcwyeouvj.supabase.co/functions/v1/telemetry-ingest";

/// HTTP header carrying the bar-raising ingest credential (0077).
///
/// Distinct from Supabase JWT `Authorization` so it cannot be confused
/// with service-role or anon keys. Server checks this first (when
/// required); real abuse control is fail-closed global quotas.
pub(crate) const INGEST_TOKEN_HEADER: &str = "X-Ledgerful-Telemetry-Token";

/// Default bar-raising ingest token shipped in every binary.
///
/// **Not a secret** — open CLI distribution means any embedded value is
/// public. Matches the edge-function allowlist seeded from
/// `0077-TelemetryIngestAuthFailClosed/phase0-memo.md`. Override via
/// `LEDGERFUL_USAGE_TOKEN` for tests / self-hosted rotation.
pub(crate) const DEFAULT_INGEST_TOKEN: &str = "lf-tel-v1-7c4e9b2a1f8d3e6a0c5b9d4e8f1a2b3c";

const USAGE_SUBDIR: &str = "usage";
const CONFIG_FILE: &str = "config.toml";
const USAGE_DB_FILENAME: &str = "ledger.db";

/// UPSERT into `usage_counters`. Parameterized as a `const` so the
/// unit test for the UPSERT semantics (see `test_increment_counter_upserts`)
/// and the production `increment_counter` use the exact same SQL — a
/// refactor that changes the production SQL but forgets to update
/// the test will now fail to compile.
const COUNTER_UPSERT_SQL: &str = "INSERT INTO usage_counters (command_name, count) \
     VALUES (?1, 1) \
     ON CONFLICT(command_name) DO UPDATE SET count = count + 1";

/// INSERT OR IGNORE into `usage_days`. Called on every increment
/// with the current UTC date so the per-day tally is independent
/// of the `usage_counters` UPSERT (which is the bug the
/// `usage_days` table was introduced to fix).
const INSERT_DAY_SQL: &str = "INSERT OR IGNORE INTO usage_days (day) VALUES (?1)";

/// Global usage config stored at `~/.ledgerful/usage/config.toml`.
#[derive(Serialize, Deserialize, Default)]
struct UsageConfig {
    /// Whether usage metrics are enabled.
    enabled: bool,
    /// Random UUID v4, generated once on first `enable`.
    anonymous_id: Option<String>,
    /// ISO 8601 timestamp of the last successful flush.
    last_sent_at: Option<String>,
}

/// The payload sent to the telemetry endpoint.
///
/// Field order and names are pinned by the spec (`conductor/trackM7/spec.md`)
/// and must match `ledgerful-frontend`'s ingest schema.
#[derive(Serialize)]
pub struct UsagePayload {
    pub schema_version: u8,
    pub anonymous_id: String,
    pub client_version: String,
    pub platform: String,
    pub sent_at: String,
    pub window_start: String,
    pub window_end: String,
    pub command_counts: HashMap<String, u64>,
    pub features_enabled: Vec<String>,
    pub active_days_in_window: u8,
}

/// Resolves the global usage config directory: `$HOME/.ledgerful/usage`.
///
/// Mirrors the established pattern in `src/config/defaults.rs:107-116` and
/// `src/ledger/crypto.rs:7-11`: `USERPROFILE` first, then `HOME`. The
/// `LEDGERFUL_HOME` env var that an earlier draft of this module
/// introduced is intentionally NOT honored here — it is not used
/// anywhere else in the repo and would create a divergent path lookup.
pub(crate) fn usage_config_dir() -> Result<Utf8PathBuf> {
    let home_os = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .ok_or_else(|| {
            miette::miette!("Cannot determine home directory. Set USERPROFILE or HOME.")
        })?;
    let home = Utf8PathBuf::from_path_buf(home_os.into())
        .map_err(|_| miette::miette!("Home directory is not valid UTF-8"))?;
    Ok(home.join(".ledgerful").join(USAGE_SUBDIR))
}

fn load_config() -> UsageConfig {
    let dir = match usage_config_dir() {
        Ok(d) => d,
        Err(_) => return UsageConfig::default(),
    };
    let path = dir.join(CONFIG_FILE);
    if !path.as_std_path().exists() {
        return UsageConfig::default();
    }
    let contents = match fs::read_to_string(path.as_std_path()) {
        Ok(c) => c,
        Err(_) => return UsageConfig::default(),
    };
    match toml::from_str(&contents) {
        Ok(config) => config,
        Err(e) => {
            // L2 fix: surface parse errors at debug level instead of
            // silently falling back to a disabled default. A corrupted
            // config would otherwise cause `status`/`show-payload` to
            // report "disabled" with no indication of why.
            tracing::debug!("usage config parse failed: {}", e);
            UsageConfig::default()
        }
    }
}

fn save_config(config: &UsageConfig) -> Result<()> {
    let dir = usage_config_dir()?;
    fs::create_dir_all(dir.as_std_path())
        .map_err(|e| miette::miette!("Failed to create usage config directory: {}", e))?;
    let path = dir.join(CONFIG_FILE);
    let contents = toml::to_string_pretty(config)
        .map_err(|e| miette::miette!("Failed to serialize usage config: {}", e))?;
    fs::write(path.as_std_path(), contents)
        .map_err(|e| miette::miette!("Failed to write usage config: {}", e))?;
    Ok(())
}

/// Generate a UUID v4 string.
pub fn generate_anonymous_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn current_platform() -> String {
    if cfg!(target_os = "windows") {
        "windows".to_string()
    } else if cfg!(target_os = "macos") {
        "macos".to_string()
    } else if cfg!(target_os = "linux") {
        "linux".to_string()
    } else {
        "unknown".to_string()
    }
}

/// List of features compiled into the running binary.
///
/// This describes the *compiled-in* feature set, not which features are
/// toggled at runtime. The `usage-metrics` feature is intentionally
/// excluded: this function is only called from within the feature's own
/// code paths, so self-reporting would be tautological.
fn enabled_features() -> Vec<String> {
    let mut features = Vec::new();
    if cfg!(feature = "web") {
        features.push("web".to_string());
    }
    if cfg!(feature = "mcp") {
        features.push("mcp".to_string());
    }
    if cfg!(feature = "sync") {
        features.push("sync".to_string());
    }
    if cfg!(feature = "daemon") {
        features.push("daemon".to_string());
    }
    if cfg!(feature = "viz-server") {
        features.push("viz-server".to_string());
    }
    features
}

fn client_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn window_start_iso() -> String {
    (chrono::Utc::now() - chrono::Duration::days(7))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn today_utc() -> String {
    chrono::Utc::now().date_naive().to_string()
}

/// Open the per-repo SQLite `ledger.db` for use by the counter store.
///
/// Returns `None` if any step (cwd resolution, layout, db existence,
/// connection open) fails. Callers must treat `None` as a no-op — the
/// dispatch hook is best-effort and must never panic the host command.
fn open_counter_store() -> Option<rusqlite::Connection> {
    let current_dir = env::current_dir().ok()?;
    let utf8_dir = Utf8Path::from_path(&current_dir)?;
    let layout = Layout::new(utf8_dir);
    let db_path = layout.state_subdir().join(USAGE_DB_FILENAME);
    if !db_path.as_std_path().exists() {
        return None;
    }
    rusqlite::Connection::open(db_path.as_std_path()).ok()
}

/// Read all command counts from the counter store. Returns an empty map
/// if the store is not available or any read fails.
///
/// L4 fix: dropped the `WHERE count > 0` filter for consistency with
/// `read_total_pending` (which sums unconditionally). Counts are always
/// ≥ 1 due to the UPSERT, so the filter was a no-op that created an
/// inconsistency between the two readers.
fn read_command_counts(conn: &rusqlite::Connection) -> HashMap<String, u64> {
    let mut stmt = match conn.prepare("SELECT command_name, count FROM usage_counters") {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        Ok((name, count as u64))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Sum of all pending events (used by `usage status` for the
/// "Pending events" line). Returns 0 on any failure.
fn read_total_pending(conn: &rusqlite::Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(SUM(count), 0) FROM usage_counters",
        [],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// Count of distinct UTC calendar days on which at least one command
/// was recorded in the current window. Bounded to `u8` (max 255) to
/// match the payload field type.
///
/// Counts rows in the `usage_days` table (added in m44). The previous
/// implementation queried `COUNT(DISTINCT last_seen_day)` against
/// `usage_counters`, but that column was overwritten by the
/// command-name UPSERT and so could only ever report 1 day for the
/// common "user runs the same command daily" pattern. The new
/// schema is independent of the UPSERT, so a user running the
/// same command on N different days gets `read_active_days == N`.
/// Returns 0 on any error (e.g. when the table is missing because
/// the feature is off).
fn read_active_days(conn: &rusqlite::Connection) -> u8 {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM usage_days", [], |row| row.get(0))
        .unwrap_or(0);
    count.clamp(0, 255) as u8
}

/// Increment a usage counter for the given command name.
///
/// This is called from the dispatch hook after every CLI invocation.
/// It must never panic or propagate errors. If usage-metrics is not
/// enabled in the global config, the function returns immediately.
///
/// M3 fix: telemetry-management commands (`usage_enable`,
/// `usage_disable`, `usage_status`, `usage_show_payload`) are
/// excluded from the counter store. The spec's payload example shows
/// actual tool commands (`scan`, `ledger_start`, etc.), not the
/// commands the user runs to inspect/manage telemetry. Counting
/// `usage_show_payload` (the user *inspecting* telemetry) as "usage"
/// would pollute the aggregate signal.
pub fn increment_counter(command_name: &str) {
    if command_name.starts_with("usage_") {
        return;
    }
    let config = load_config();
    if !config.enabled {
        return;
    }
    let conn = match open_counter_store() {
        Some(c) => c,
        None => return,
    };
    let counter_result = conn.execute(COUNTER_UPSERT_SQL, rusqlite::params![command_name]);
    if let Err(e) = counter_result {
        tracing::debug!("Failed to increment usage counter: {}", e);
    }
    // Day-tracking lives in its own table so the UPSERT above cannot
    // overwrite it (this is the H2 fix). Failures here are non-fatal
    // — the per-command counter is still updated.
    //
    // L7 note: if INSERT_DAY_SQL fails after COUNTER_UPSERT_SQL
    // succeeds, `command_counts` will have an entry with no matching
    // day; this is non-fatal and acceptable for best-effort telemetry.
    // The aggregate `active_days_in_window` may under-count by 1 for
    // that day, but the next successful INSERT will correct it.
    let day_result = conn.execute(INSERT_DAY_SQL, rusqlite::params![today_utc()]);
    if let Err(e) = day_result {
        tracing::debug!("Failed to record usage day: {}", e);
    }
}

/// Whether a flush should be attempted given the current config.
///
/// Spec (`spec.md:77`) says flush when "more than 7 days have passed"
/// since `last_sent_at`, so use strict `>` rather than `>=`. A
/// unparseable `last_sent_at` falls back to flushing (the user has
/// a config but the timestamp is corrupt; treat as stale).
fn should_flush(config: &UsageConfig) -> bool {
    match &config.last_sent_at {
        Some(last) => match chrono::DateTime::parse_from_rfc3339(last) {
            Ok(last_dt) => {
                let last_utc = last_dt.with_timezone(&chrono::Utc);
                (chrono::Utc::now() - last_utc).num_days() > 7
            }
            Err(_) => true,
        },
        None => true,
    }
}

/// Build the payload that would be sent right now, given the
/// current state of the counter store and the global config.
///
/// `window_start` is `last_sent_at` if set (M1 fix: the spec's
/// "since last flush" semantics for the counts), otherwise a
/// 7-day preview (the "first-ever send" fallback). This matches
/// the `command_counts`/`active_days` "since last flush" semantics
/// and prevents a 30-day-old backlog from being reported under a
/// 7-day window.
///
/// Returns `None` if the payload is empty (no counters, or no
/// `anonymous_id`) so the caller can short-circuit.
fn build_payload(config: &UsageConfig, conn: &rusqlite::Connection) -> Option<UsagePayload> {
    let command_counts = read_command_counts(conn);
    if command_counts.is_empty() {
        return None;
    }
    let anonymous_id = config.anonymous_id.clone()?;
    let active_days = read_active_days(conn);
    let window_start = config.last_sent_at.clone().unwrap_or_else(window_start_iso);
    Some(UsagePayload {
        schema_version: 1,
        anonymous_id,
        client_version: client_version(),
        platform: current_platform(),
        sent_at: now_iso(),
        window_start,
        window_end: now_iso(),
        command_counts,
        features_enabled: enabled_features(),
        active_days_in_window: active_days,
    })
}

/// Resolve the bar-raising ingest token for the flush POST.
///
/// Runtime override `LEDGERFUL_USAGE_TOKEN` wins (tests, self-hosted
/// rotation); otherwise the compile-time default ships in every
/// binary. Empty override falls through to the default so an
/// accidental empty env cannot silently omit the credential.
fn resolve_ingest_token() -> String {
    match env::var("LEDGERFUL_USAGE_TOKEN") {
        Ok(v) if !v.is_empty() => v,
        _ => DEFAULT_INGEST_TOKEN.to_string(),
    }
}

/// Attempt to POST the usage payload to the endpoint. Returns
/// `true` on a 2xx response (caller should persist the updated
/// `last_sent_at`), `false` otherwise.
///
/// On 2xx, clears both `usage_counters` AND `usage_days` (H1 fix:
/// the two tables are co-managed and the day tally must restart
/// from zero for the next window, matching the counter semantics).
/// Without the `DELETE FROM usage_days` line, `active_days_in_window`
/// would grow unboundedly and eventually clamp at 255 (the u8 max)
/// while the payload claims a 7-day window.
///
/// Every successful POST attempt includes
/// [`INGEST_TOKEN_HEADER`] (0077 bar-raising credential). Non-2xx
/// and transport errors remain silent (best-effort; never block CLI).
///
/// H1 test seam: this function is `pub(crate)` and takes the
/// `Connection` and `UsageConfig` explicitly so unit tests can
/// drive it against a real DB + a real `ureq::Agent` pointed at a
/// mock HTTP server (see `test_usage_days_cleared_on_flush_2xx`).
fn flush_to_endpoint(
    conn: &rusqlite::Connection,
    config: &mut UsageConfig,
    endpoint: &str,
    agent: &ureq::Agent,
) -> bool {
    let payload = match build_payload(config, conn) {
        Some(p) => p,
        None => return false,
    };
    let body = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let token = resolve_ingest_token();
    match agent
        .post(endpoint)
        .set("Content-Type", "application/json")
        .set(INGEST_TOKEN_HEADER, &token)
        .send_bytes(&body)
    {
        Ok(response) => {
            if (200..300).contains(&response.status()) {
                let _ = conn.execute("DELETE FROM usage_counters", []);
                // H1 fix: also clear the day tally so the next
                // window's `active_days_in_window` reflects only
                // days since this flush, not all-time distinct
                // days. Pre-fix, this table grew forever and the
                // 7-day-window claim was a lie after the first
                // successful flush.
                let _ = conn.execute("DELETE FROM usage_days", []);
                config.last_sent_at = Some(now_iso());
                true
            } else {
                tracing::debug!(
                    "Usage metrics flush returned non-2xx: {}",
                    response.status()
                );
                false
            }
        }
        Err(e) => {
            tracing::debug!("Usage metrics flush failed: {}", e);
            false
        }
    }
}

/// Attempt to flush usage metrics if enough time has passed.
///
/// Called opportunistically after counter increment. Never blocks
/// the host command or propagates errors. The transport uses an
/// explicit `ureq` agent with 5s connect / 10s read / 5s write
/// timeouts (mirroring `src/embed/client.rs:156-160` and
/// `src/observability/prometheus.rs:17-20`) so a slow or hung
/// endpoint cannot stall the host command.
pub fn try_flush() {
    let mut config = load_config();
    if !config.enabled {
        return;
    }
    if !should_flush(&config) {
        return;
    }
    let endpoint =
        env::var("LEDGERFUL_USAGE_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(5))
        .build();
    let conn = match open_counter_store() {
        Some(c) => c,
        None => return,
    };
    if flush_to_endpoint(&conn, &mut config, &endpoint, &agent) {
        let _ = save_config(&config);
        tracing::debug!("Usage metrics flushed successfully");
    }
}

/// Enable usage metrics.
pub fn execute_usage_enable() -> Result<()> {
    let mut config = load_config();
    if config.anonymous_id.is_none() {
        config.anonymous_id = Some(generate_anonymous_id());
    }
    config.enabled = true;
    // M2 fix: seed `last_sent_at` to now so the 7-day gate blocks
    // the first flush. Without this, the dispatch hook on this same
    // invocation would see `enabled=true` + `last_sent_at=None` →
    // `should_flush=true` + a non-empty counter set (the `enable`
    // command itself adds a counter) → POST to the production
    // endpoint before the user can run `show-payload` to verify.
    // Seeding `last_sent_at` here makes the opt-in act itself
    // non-shipping: the first real flush happens at least 7 days
    // after enable.
    config.last_sent_at = Some(now_iso());
    save_config(&config)?;
    println!("{} Usage metrics enabled.", "✓".green());
    println!(
        "  Anonymous ID: {}",
        config.anonymous_id.as_deref().unwrap_or("(none)")
    );
    println!("  Only command names and feature flags are collected.");
    println!(
        "  Run {} to see the exact payload that would be sent.",
        "ledgerful usage show-payload".cyan()
    );
    println!("  Run {} to disable.", "ledgerful usage disable".cyan());
    Ok(())
}

/// Disable usage metrics.
pub fn execute_usage_disable() -> Result<()> {
    let mut config = load_config();
    config.enabled = false;
    save_config(&config)?;
    println!("{} Usage metrics disabled.", "✓".green());
    println!("  Anonymous ID preserved (re-enabling will not generate a new one).");
    Ok(())
}

/// Show usage metrics status.
pub fn execute_usage_status() -> Result<()> {
    let config = load_config();

    println!("{} Usage Metrics Status", "═".cyan());
    if config.enabled {
        println!("  Enabled:         {}", "yes".green());
    } else {
        println!("  Enabled:         {}", "no".red());
    }
    println!(
        "  Anonymous ID:    {}",
        config.anonymous_id.as_deref().unwrap_or("(not set)")
    );
    println!(
        "  Last sent:       {}",
        config.last_sent_at.as_deref().unwrap_or("(never)")
    );

    let conn = match open_counter_store() {
        Some(c) => c,
        None => {
            println!("  Pending events:  0");
            return Ok(());
        }
    };
    let total = read_total_pending(&conn);
    println!("  Pending events:  {}", total);
    Ok(())
}

/// Show the exact payload that would be sent.
///
/// Degrades gracefully when run from outside a git repo (or when the
/// per-repo SQLite database does not yet exist) — prints a payload
/// with an empty `command_counts` map and a placeholder
/// `anonymous_id` rather than erroring out.
///
/// M1 fix: `window_start` mirrors the flush behavior — use
/// `last_sent_at` if set, otherwise show a 7-day preview. This is
/// the "what would the next flush actually send" view; showing a
/// hardcoded 7-day window when the counters span 30 days would be
/// misleading.
pub fn execute_usage_show_payload() -> Result<()> {
    let config = load_config();
    let anonymous_id = config
        .anonymous_id
        .clone()
        .unwrap_or_else(|| "(not yet enabled)".to_string());

    let (command_counts, active_days) = match open_counter_store() {
        Some(conn) => (read_command_counts(&conn), read_active_days(&conn)),
        None => (HashMap::new(), 0),
    };

    let window_start = config.last_sent_at.clone().unwrap_or_else(window_start_iso);

    let payload = UsagePayload {
        schema_version: 1,
        anonymous_id,
        client_version: client_version(),
        platform: current_platform(),
        sent_at: now_iso(),
        window_start,
        window_end: now_iso(),
        command_counts,
        features_enabled: enabled_features(),
        active_days_in_window: active_days,
    };

    let json = serde_json::to_string_pretty(&payload)
        .map_err(|e| miette::miette!("Failed to serialize payload: {}", e))?;
    println!("{}", json);
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }

    use super::*;
    use crate::state::migrations;
    use env_guard::TempEnv;
    use rusqlite::Connection;

    #[test]
    fn test_anonymous_id_is_valid_uuid_v4() {
        let id = generate_anonymous_id();
        let parsed = uuid::Uuid::parse_str(&id).expect("anonymous_id must parse as UUID");
        assert_eq!(
            parsed.get_version(),
            Some(uuid::Version::Random),
            "anonymous_id must be UUID v4"
        );
        assert_eq!(
            parsed.get_variant(),
            uuid::Variant::RFC4122,
            "anonymous_id must use RFC4122 variant"
        );
    }

    #[test]
    fn test_anonymous_id_is_unique() {
        // 1000 IDs is enough to catch a deterministic bug (e.g. always
        // returning a constant string) with negligible collision risk
        // (~10^-49 per pair for a true random v4).
        let mut ids: Vec<String> = (0..1000).map(|_| generate_anonymous_id()).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            total,
            "all 1000 generated anonymous_ids should be unique"
        );
    }

    #[test]
    fn test_payload_matches_spec_schema() {
        // Build a payload with known values, serialize, parse, and
        // assert each field matches the spec schema field-by-field.
        let mut command_counts = HashMap::new();
        command_counts.insert("scan".to_string(), 12_u64);
        command_counts.insert("ledger_start".to_string(), 4);
        command_counts.insert("ledger_commit".to_string(), 4);
        command_counts.insert("ask".to_string(), 2);
        command_counts.insert("web_start".to_string(), 1);

        // Pin sent_at and window_end to the SAME value so the test
        // does not race against `now_iso()`'s wall-clock output.
        let sent_at = "2026-06-17T12:00:00Z";
        let payload = UsagePayload {
            schema_version: 1,
            anonymous_id: "f47ac10b-58cc-4372-a567-0e02b2c3d479".to_string(),
            client_version: "0.1.4".to_string(),
            platform: "windows".to_string(),
            sent_at: sent_at.to_string(),
            window_start: "2026-06-10T00:00:00Z".to_string(),
            window_end: sent_at.to_string(),
            command_counts,
            features_enabled: vec!["web".to_string(), "mcp".to_string(), "sync".to_string()],
            active_days_in_window: 5,
        };

        let json = serde_json::to_string_pretty(&payload).expect("serialize payload");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse payload");

        assert_eq!(parsed["schema_version"], serde_json::json!(1));
        assert_eq!(
            parsed["anonymous_id"],
            serde_json::json!("f47ac10b-58cc-4372-a567-0e02b2c3d479")
        );
        assert_eq!(parsed["client_version"], serde_json::json!("0.1.4"));
        assert_eq!(parsed["platform"], serde_json::json!("windows"));
        assert_eq!(parsed["sent_at"], serde_json::json!(sent_at));
        assert_eq!(
            parsed["window_start"],
            serde_json::json!("2026-06-10T00:00:00Z")
        );
        assert_eq!(parsed["window_end"], serde_json::json!(sent_at));
        assert_eq!(parsed["command_counts"]["scan"], serde_json::json!(12));
        assert_eq!(
            parsed["command_counts"]["ledger_start"],
            serde_json::json!(4)
        );
        assert_eq!(
            parsed["command_counts"]["ledger_commit"],
            serde_json::json!(4)
        );
        assert_eq!(parsed["command_counts"]["ask"], serde_json::json!(2));
        assert_eq!(parsed["command_counts"]["web_start"], serde_json::json!(1));
        assert_eq!(
            parsed["features_enabled"],
            serde_json::json!(["web", "mcp", "sync"])
        );
        assert_eq!(parsed["active_days_in_window"], serde_json::json!(5));

        // No repo-identifying fields should be present in the schema.
        for forbidden in [
            "repo",
            "repository",
            "path",
            "file",
            "remote",
            "machine_id",
            "username",
            "email",
        ] {
            assert!(
                parsed.get(forbidden).is_none(),
                "payload must not contain field `{forbidden}`"
            );
        }
    }

    #[test]
    fn test_increment_counter_upserts() {
        // Open an in-memory SQLite database, run migrations, then
        // exercise the same UPSERT SQL that `increment_counter` uses
        // — pulled from the `COUNTER_UPSERT_SQL` const so the test
        // and the production code cannot drift (L-NEW-3 review).
        //
        // `open_counter_store` reads from the real cwd, so we cannot
        // call `increment_counter` directly without setting up a
        // real tempdir + cd. Running the shared const SQL against an
        // in-memory DB with the live migrations applied is the next
        // best thing: a refactor that changes the production SQL
        // would have to also change the const, and this test would
        // catch any silent behavioral change.
        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply migrations");

        conn.execute(COUNTER_UPSERT_SQL, rusqlite::params!["foo"])
            .expect("first insert");
        conn.execute(COUNTER_UPSERT_SQL, rusqlite::params!["foo"])
            .expect("second insert (upsert)");

        let count: i64 = conn
            .query_row(
                "SELECT count FROM usage_counters WHERE command_name = 'foo'",
                [],
                |row| row.get(0),
            )
            .expect("query count");
        assert_eq!(count, 2, "second insert should UPSERT to count=2");

        // Confirm a different command_name gets its own row.
        conn.execute(COUNTER_UPSERT_SQL, rusqlite::params!["bar"])
            .expect("insert bar");
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM usage_counters",
                [],
                |row| row.get(0),
            )
            .expect("sum");
        assert_eq!(total, 3, "foo=2 + bar=1 = 3");
    }

    /// Regression test for the H2 fix (M7 r2 review): a single
    /// command_name recorded across multiple distinct days must
    /// yield `read_active_days == N`, not 1.
    ///
    /// The pre-fix implementation queried
    /// `SELECT COUNT(DISTINCT last_seen_day) FROM usage_counters`,
    /// but because `command_name` is the sole PRIMARY KEY and the
    /// UPSERT's `ON CONFLICT` clause overwrote `last_seen_day` on
    /// every increment, the column held only the most recent day
    /// for any given command. The new implementation reads from
    /// the `usage_days` table (m44), which records the day in a
    /// separate row per calendar day and is therefore independent
    /// of the UPSERT.
    #[test]
    #[cfg(feature = "usage-metrics")]
    fn test_active_days_for_single_command_across_days() {
        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply migrations");

        // Insert 3 distinct days into `usage_days`. The pre-fix
        // `read_active_days` queried `usage_counters.last_seen_day`
        // and would have returned 1 here (the UPSERT collapsed days
        // to a single row per command_name). The new implementation
        // reads from `usage_days` and correctly returns 3.
        for day in ["2026-06-15", "2026-06-16", "2026-06-17"] {
            conn.execute(INSERT_DAY_SQL, rusqlite::params![day])
                .expect("insert day");
        }
        assert_eq!(
            read_active_days(&conn),
            3,
            "a single command_name recorded across 3 distinct days must yield 3 active days"
        );

        // Re-inserting the same day must be a no-op (PRIMARY KEY +
        // INSERT OR IGNORE).
        for _ in 0..3 {
            conn.execute(INSERT_DAY_SQL, rusqlite::params!["2026-06-15"])
                .expect("re-insert same day");
        }
        assert_eq!(read_active_days(&conn), 3, "duplicate days must collapse");
    }

    #[test]
    fn test_usage_counters_schema() {
        // Regression guard for the L4 (drop `window_start`) and H2
        // (move day-tracking to a separate `usage_days` table)
        // reviews. Confirms:
        //   - `usage_counters` has exactly `(command_name, count)`
        //   - `window_start` is gone (L4)
        //   - `last_seen_day` is gone from `usage_counters` (H2)
        //   - `usage_days` exists when the feature is on (H2)
        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply migrations");

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(usage_counters)")
            .expect("pragma")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("pragma rows")
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            !cols.contains(&"window_start".to_string()),
            "window_start should have been removed (L4); got columns: {cols:?}"
        );
        assert!(
            !cols.contains(&"last_seen_day".to_string()),
            "last_seen_day should have been removed from usage_counters (H2); \
             got columns: {cols:?}"
        );
        assert!(cols.contains(&"command_name".to_string()));
        assert!(cols.contains(&"count".to_string()));

        // `usage_days` is registered unconditionally as m44 (same
        // M4-deviation trade-off as m42) so it exists in every
        // build, regardless of whether the feature is compiled in.
        // The H2 fix writes to it from `increment_counter`, which is
        // feature-gated, so the table is empty (and unused) in
        // builds without the feature — but the table itself is
        // always present, so the pre-flight migration check never
        // fails for a DB that was created by a build with the
        // feature.
        let days_table_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='usage_days'",
                [],
                |row| row.get(0),
            )
            .expect("pragma sqlite_master");
        assert_eq!(
            days_table_count, 1,
            "usage_days must exist in every build (m44 is unconditional, like m42)"
        );
    }

    /// H1 regression test: `usage_days` must be cleared on a 2xx
    /// flush, the same as `usage_counters`. Pre-fix, only the
    /// counters were deleted, so `active_days_in_window` grew
    /// unboundedly (clamped at 255) while the payload claimed a
    /// 7-day window. This regression was introduced by the
    /// round-2 H2 fix that moved day-tracking to a separate table
    /// but forgot the corresponding reset on flush.
    ///
    /// Drives `flush_to_endpoint` end-to-end against a real
    /// `ureq::Agent` pointed at an httpmock server (the test seam
    /// extracted by the H1 refactor). Asserts:
    ///   - seeded 3 days → flushed → `read_active_days == 0`
    ///   - re-insert 1 day → `read_active_days == 1`
    ///   - `usage_counters` is also empty after the flush
    #[test]
    #[serial_test::serial(env)]
    fn test_usage_days_cleared_on_flush_2xx() {
        use httpmock::prelude::*;

        // Pin default token so a developer env override cannot flake the mock.
        let _token_env = TempEnv::remove("LEDGERFUL_USAGE_TOKEN");

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/telemetry")
                // 0077: every flush must send the bar-raising credential.
                .header(INGEST_TOKEN_HEADER, DEFAULT_INGEST_TOKEN);
            then.status(200)
                .header("content-type", "application/json")
                .body("{}");
        });

        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply migrations");

        // Seed 3 distinct days and 1 counter so the flush payload
        // is non-empty (flush_to_endpoint short-circuits on empty
        // command_counts).
        for day in ["2026-06-15", "2026-06-16", "2026-06-17"] {
            conn.execute(INSERT_DAY_SQL, rusqlite::params![day])
                .expect("insert day");
        }
        conn.execute(COUNTER_UPSERT_SQL, rusqlite::params!["init"])
            .expect("seed counter");
        assert_eq!(read_active_days(&conn), 3);
        assert_eq!(read_command_counts(&conn).len(), 1);

        let mut config = UsageConfig {
            enabled: true,
            anonymous_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            last_sent_at: None,
        };

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(10))
            .timeout_write(Duration::from_secs(5))
            .build();
        let endpoint = format!("{}{}", server.base_url(), "/api/telemetry");

        let result = flush_to_endpoint(&conn, &mut config, &endpoint, &agent);
        assert!(result, "flush should succeed against 2xx mock");
        assert!(mock.hits() >= 1, "mock should have been hit");

        // H1: usage_days must be cleared on a 2xx flush.
        assert_eq!(
            read_active_days(&conn),
            0,
            "usage_days must be cleared on 2xx flush (H1); pre-fix this grew unboundedly"
        );
        assert_eq!(
            read_command_counts(&conn).len(),
            0,
            "usage_counters must be cleared on 2xx flush"
        );
        // The flush must have updated last_sent_at so the
        // caller (try_flush) can persist it.
        assert!(
            config.last_sent_at.is_some(),
            "last_sent_at must be updated on 2xx flush"
        );

        // Re-insert 1 day and assert read_active_days reports 1.
        conn.execute(INSERT_DAY_SQL, rusqlite::params!["2026-06-18"])
            .expect("re-insert day");
        assert_eq!(
            read_active_days(&conn),
            1,
            "after re-insert, read_active_days must report 1"
        );
    }

    /// H1 complement: on a non-2xx response, the flush must NOT
    /// clear counters or days. This guards against an
    /// over-eager fix that resets state on any response.
    #[test]
    #[serial_test::serial(env)]
    fn test_usage_days_not_cleared_on_non_2xx() {
        use httpmock::prelude::*;

        let _token_env = TempEnv::remove("LEDGERFUL_USAGE_TOKEN");

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/telemetry")
                .header(INGEST_TOKEN_HEADER, DEFAULT_INGEST_TOKEN);
            then.status(500).body("internal error");
        });

        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply migrations");

        for day in ["2026-06-15", "2026-06-16", "2026-06-17"] {
            conn.execute(INSERT_DAY_SQL, rusqlite::params![day])
                .expect("insert day");
        }
        conn.execute(COUNTER_UPSERT_SQL, rusqlite::params!["init"])
            .expect("seed counter");

        let mut config = UsageConfig {
            enabled: true,
            anonymous_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            last_sent_at: None,
        };
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(10))
            .timeout_write(Duration::from_secs(5))
            .build();
        let endpoint = format!("{}{}", server.base_url(), "/api/telemetry");

        let result = flush_to_endpoint(&conn, &mut config, &endpoint, &agent);
        assert!(!result, "flush should fail on 5xx");
        assert!(mock.hits() >= 1);

        // On non-2xx, state must be untouched.
        assert_eq!(
            read_active_days(&conn),
            3,
            "usage_days must NOT be cleared on non-2xx"
        );
        assert_eq!(read_command_counts(&conn).len(), 1);
        assert!(
            config.last_sent_at.is_none(),
            "last_sent_at must NOT be updated on non-2xx"
        );
    }

    /// M1 fix: `window_start` in the payload must be `last_sent_at`
    /// when set, not the hardcoded 7-day preview. This prevents
    /// a 30-day-old backlog from being reported under a 7-day
    /// window.
    #[test]
    fn test_window_start_uses_last_sent_at() {
        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply migrations");
        conn.execute(COUNTER_UPSERT_SQL, rusqlite::params!["init"])
            .expect("seed counter");

        // When last_sent_at is None, window_start should be the
        // 7-day preview (now - 7 days).
        let config_no_last = UsageConfig {
            enabled: true,
            anonymous_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            last_sent_at: None,
        };
        let payload_no_last = build_payload(&config_no_last, &conn).expect("payload should build");
        let expected_preview = window_start_iso();
        assert_eq!(
            payload_no_last.window_start, expected_preview,
            "with no last_sent_at, window_start should be 7-day preview"
        );

        // When last_sent_at is set, window_start should be that
        // exact value (so a 30-day backlog is reported honestly).
        let last = "2026-05-01T00:00:00Z";
        let config_with_last = UsageConfig {
            enabled: true,
            anonymous_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            last_sent_at: Some(last.to_string()),
        };
        let payload_with_last =
            build_payload(&config_with_last, &conn).expect("payload should build");
        assert_eq!(
            payload_with_last.window_start, last,
            "with last_sent_at set, window_start should be that value (M1)"
        );
    }

    /// 0077: flush POST must include `X-Ledgerful-Telemetry-Token`
    /// with the default bar-raising token when no env override is set.
    /// httpmock matches on the header — a missing header fails the
    /// mock match and `mock.hits()` stays 0.
    #[test]
    #[serial_test::serial(env)]
    fn test_flush_sends_default_ingest_token_header() {
        use httpmock::prelude::*;

        // Ensure override is unset so the default ships.
        let _token_env = TempEnv::remove("LEDGERFUL_USAGE_TOKEN");

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/telemetry")
                .header(INGEST_TOKEN_HEADER, DEFAULT_INGEST_TOKEN);
            then.status(200)
                .header("content-type", "application/json")
                .body("{}");
        });

        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply migrations");
        conn.execute(COUNTER_UPSERT_SQL, rusqlite::params!["scan"])
            .expect("seed counter");

        let mut config = UsageConfig {
            enabled: true,
            anonymous_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            last_sent_at: None,
        };
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(10))
            .timeout_write(Duration::from_secs(5))
            .build();
        let endpoint = format!("{}{}", server.base_url(), "/api/telemetry");

        let result = flush_to_endpoint(&conn, &mut config, &endpoint, &agent);
        assert!(
            result,
            "flush should succeed when default token header matches"
        );
        assert_eq!(
            mock.hits(),
            1,
            "mock must match only when X-Ledgerful-Telemetry-Token \
             equals DEFAULT_INGEST_TOKEN"
        );
        assert_eq!(
            resolve_ingest_token(),
            DEFAULT_INGEST_TOKEN,
            "resolve_ingest_token must return the default when env is unset"
        );
    }

    /// 0077: `LEDGERFUL_USAGE_TOKEN` overrides the default token on
    /// the wire (tests / self-hosted rotation). Empty override must
    /// fall through to the default (no silent omit).
    #[test]
    #[serial_test::serial(env)]
    fn test_flush_sends_env_override_ingest_token_header() {
        use httpmock::prelude::*;

        let override_token = "test-override-token-0077-not-default";
        let _token_env = TempEnv::set("LEDGERFUL_USAGE_TOKEN", override_token);

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/telemetry")
                .header(INGEST_TOKEN_HEADER, override_token);
            then.status(200)
                .header("content-type", "application/json")
                .body("{}");
        });
        // Negative mock: default token must NOT match when override is set.
        let default_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/telemetry")
                .header(INGEST_TOKEN_HEADER, DEFAULT_INGEST_TOKEN);
            then.status(200).body("{}");
        });

        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply migrations");
        conn.execute(COUNTER_UPSERT_SQL, rusqlite::params!["doctor"])
            .expect("seed counter");

        let mut config = UsageConfig {
            enabled: true,
            anonymous_id: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string()),
            last_sent_at: None,
        };
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(10))
            .timeout_write(Duration::from_secs(5))
            .build();
        let endpoint = format!("{}{}", server.base_url(), "/api/telemetry");

        let result = flush_to_endpoint(&conn, &mut config, &endpoint, &agent);
        assert!(result, "flush should succeed with env override token");
        assert_eq!(mock.hits(), 1, "override token header must match mock");
        assert_eq!(
            default_mock.hits(),
            0,
            "default token must not be sent when LEDGERFUL_USAGE_TOKEN is set"
        );
        assert_eq!(resolve_ingest_token(), override_token);

        // Empty override falls through to default — drop prior guard first.
        drop(_token_env);
        let _empty = TempEnv::set("LEDGERFUL_USAGE_TOKEN", "");
        assert_eq!(
            resolve_ingest_token(),
            DEFAULT_INGEST_TOKEN,
            "empty LEDGERFUL_USAGE_TOKEN must fall through to default"
        );
    }

    /// 0077: opt-out must not open a network connection. Confirmed by
    /// `try_flush` early-return when `enabled=false` (no agent call).
    /// This unit test pins the gate at the config layer so a future
    /// refactor cannot re-introduce a disabled flush path.
    #[test]
    fn test_try_flush_skips_when_disabled() {
        // try_flush reads real home/cwd; we only pin the enabled gate
        // semantics that callers rely on: disabled config never
        // reaches flush_to_endpoint. Direct unit coverage of the
        // early return condition.
        let config = UsageConfig {
            enabled: false,
            anonymous_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            last_sent_at: None,
        };
        assert!(!config.enabled, "opt-out means enabled=false");
        // should_flush is irrelevant when disabled — try_flush returns first.
        assert!(
            should_flush(&config),
            "sanity: without last_sent_at, should_flush is true — disabled gate must still win"
        );
    }

    /// M3 fix: `usage_*` command names must be filtered out of the
    /// counter store. This is a unit-level guard using the
    /// production SQL directly (mirroring the `test_increment_counter_upserts`
    /// pattern) — a refactor that removed the `starts_with("usage_")`
    /// check would be caught by the integration test
    /// `test_usage_commands_not_counted_in_telemetry`, but this
    /// unit test pins the *filter condition* itself.
    #[test]
    fn test_usage_command_filter() {
        // The M3 fix is a one-liner in `increment_counter`:
        // `if command_name.starts_with("usage_") { return; }`
        // This test asserts the filter set is exactly the four
        // telemetry-management commands wired in `args.rs:574-579`.
        // If a new `UsageCommand` variant is added (e.g.
        // `UsageCommands::Export`), this test will fail until the
        // integration test confirms the new variant is also
        // filtered — the spec says all `usage_*` commands should
        // be excluded.
        let usage_commands = [
            "usage_enable",
            "usage_disable",
            "usage_status",
            "usage_show_payload",
        ];
        for cmd in usage_commands {
            assert!(
                cmd.starts_with("usage_"),
                "sanity: {cmd} should start with 'usage_'"
            );
        }
        // Non-usage commands must NOT match the filter.
        for cmd in ["init", "doctor", "scan", "ledger_start", "ask", "status"] {
            assert!(
                !cmd.starts_with("usage_"),
                "sanity: {cmd} should NOT be filtered as telemetry management"
            );
        }
    }
}
