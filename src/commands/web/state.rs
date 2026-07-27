//! Shared web dashboard state.

use crate::commands::web::api::KnowledgeGraphResponse;
use crate::commands::web::git_meta::GitMetaCacheEntry;
use crate::commands::web::server::csp::{embedded_csp, resolve_csp_for_spa_dir};
use crate::commands::web::types::DaemonEvent;
use crate::state::layout::Layout;
use axum::http::HeaderValue;
use camino::Utf8PathBuf;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, broadcast, watch};

type KgCacheEntry = Option<(Instant, (usize, bool), KnowledgeGraphResponse)>;

/// Per-IP, per-path sliding-window request timestamps.
pub type RateLimitMap = HashMap<(IpAddr, String), Vec<Instant>>;

/// Maximum distinct (IP, path) keys retained by the rate limiter.
pub const RATE_LIMIT_MAX_KEYS: usize = 10_000;

/// Capacity of the daemon-event broadcast fan-out channel.
const EVENT_BROADCAST_CAPACITY: usize = 64;

/// Lifetime of a single-use dashboard handoff code minted by `--open` (track 0090).
pub const HANDOFF_TTL: Duration = Duration::from_secs(120);

/// Single-use handoff code used by `POST /api/session/exchange` to bootstrap
/// the SPA without pasting the long-lived session token into the URL.
#[derive(Debug, Clone)]
pub struct HandoffCode {
    pub code: String,
    pub expires_at: Instant,
}

/// Outcome of evaluating a handoff exchange attempt (pure; no I/O).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandoffExchangeOutcome {
    /// Code matched; the handoff slot was consumed. Caller returns the session token.
    Matched,
    /// Absent, expired (and cleared), or wrong code.
    /// `record_auth_fail` is true only for a wrong-code guess (not absent/expired).
    Denied { record_auth_fail: bool },
}

/// Evaluate a handoff exchange against the in-memory slot.
///
/// Consume-on-match only: a mismatched guess does **not** burn the code (spec §2.6).
/// Expired entries are cleared and treated as absent. Never logs code values.
pub(crate) fn evaluate_handoff_exchange(
    slot: &mut Option<HandoffCode>,
    provided_code: &str,
    now: Instant,
) -> HandoffExchangeOutcome {
    let Some(entry) = slot.as_ref() else {
        return HandoffExchangeOutcome::Denied {
            record_auth_fail: false,
        };
    };
    if now >= entry.expires_at {
        *slot = None;
        return HandoffExchangeOutcome::Denied {
            record_auth_fail: false,
        };
    }

    // Constant-time compare; lengths must match for a positive result.
    let matched = provided_code
        .as_bytes()
        .ct_eq(entry.code.as_bytes())
        .unwrap_u8()
        == 1;
    if matched {
        *slot = None;
        HandoffExchangeOutcome::Matched
    } else {
        HandoffExchangeOutcome::Denied {
            record_auth_fail: true,
        }
    }
}

/// Application-wide state shared by all axum handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    pub layout: Layout,
    pub token: String,
    pub spa_dir: Option<Utf8PathBuf>,
    /// Resolved Content-Security-Policy for this process instance.
    /// Embedded SPA → vendored hash manifest; `--spa-dir` → sidecar or fallback.
    pub csp_header: HeaderValue,
    pub start_time: Instant,
    pub kg_cache: Arc<Mutex<KgCacheEntry>>,
    pub rate_limiter: Arc<Mutex<RateLimitMap>>,
    /// Separate sliding-window map for failed auth attempts (DoD-4).
    /// Keyed by (IP, path); does not share counts with [`Self::rate_limiter`].
    pub auth_fail_limiter: Arc<Mutex<RateLimitMap>>,
    /// When set (public bind mode), only these peer IPs may connect.
    /// `None` means no peer filter (loopback bind / private mode).
    pub peer_allowlist: Option<HashSet<IpAddr>>,
    /// Git metadata cache for `/api/hotspots` (Track TA29). Maps
    /// `file_path → (iso8601_timestamp, author_name)`. 5-minute TTL.
    /// Track TA30 will replace this with persisted `project_files` columns.
    pub git_meta_cache: Arc<Mutex<GitMetaCacheEntry>>,
    /// Fan-out of narrow daemon status events for `GET /api/events` (SSE).
    pub event_tx: broadcast::Sender<DaemonEvent>,
    /// Multi-consumer shutdown flag (`false` = running, `true` = shutdown).
    /// SSE streams and the change detector select on this and exit cleanly.
    pub shutdown: watch::Sender<bool>,
    /// Optional single-use handoff code minted only when `web start --open`.
    /// Exchanged once via `POST /api/session/exchange` then cleared.
    pub handoff: Arc<Mutex<Option<HandoffCode>>>,
}

impl AppState {
    /// Construct application state.
    ///
    /// CSP is resolved from `spa_dir` (sidecar when `Some`, embedded vendored
    /// manifest when `None`). Event broadcast + shutdown channels are created
    /// internally. `handoff` is set only by the foreground `--open` path.
    pub fn new(
        layout: Layout,
        token: String,
        spa_dir: Option<Utf8PathBuf>,
        peer_allowlist: Option<HashSet<IpAddr>>,
        handoff: Option<HandoffCode>,
    ) -> Self {
        let csp_string = match &spa_dir {
            Some(dir) => resolve_csp_for_spa_dir(dir),
            None => embedded_csp().to_string(),
        };
        let csp_header = HeaderValue::from_str(&csp_string).unwrap_or_else(|_| {
            // HeaderValue rejects only a narrow set of control chars; fall back
            // to a minimal safe CSP rather than panicking at startup.
            tracing::error!("Resolved CSP header value is invalid; using script-src 'self' only");
            HeaderValue::from_static(
                "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
                 style-src 'self' 'unsafe-inline'; script-src 'self'; \
                 object-src 'none'; base-uri 'self'; frame-ancestors 'none'",
            )
        });

        let (event_tx, _event_rx) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        let (shutdown, _shutdown_rx) = watch::channel(false);

        Self {
            layout,
            token,
            spa_dir,
            csp_header,
            start_time: Instant::now(),
            kg_cache: Arc::new(Mutex::new(None)),
            rate_limiter: Arc::new(Mutex::new(HashMap::new())),
            auth_fail_limiter: Arc::new(Mutex::new(HashMap::new())),
            peer_allowlist,
            git_meta_cache: Arc::new(Mutex::new(None)),
            event_tx,
            shutdown,
            handoff: Arc::new(Mutex::new(handoff)),
        }
    }

    /// Subscribe to daemon event broadcasts (SSE handlers, tests).
    pub fn subscribe_events(&self) -> broadcast::Receiver<DaemonEvent> {
        self.event_tx.subscribe()
    }

    /// Clone a receiver for the shutdown watch (`true` means shutting down).
    pub fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// Signal all SSE streams and the change detector to terminate.
    pub fn signal_shutdown(&self) {
        // Ignore error when all receivers already dropped.
        let _ = self.shutdown.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::web::auth::generate_token;
    use crate::state::layout::Layout;
    use std::time::Duration;

    fn test_layout() -> (tempfile::TempDir, Layout) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).expect("utf8 path"));
        (tmp, layout)
    }

    #[test]
    fn handoff_none_when_not_minted() {
        let (_tmp, layout) = test_layout();
        let state = AppState::new(layout, generate_token(), None, None, None);
        // Mutex is not async in unit test: use try_lock / blocking via tokio runtime free path.
        let guard = state.handoff.try_lock().expect("handoff lock free");
        assert!(guard.is_none(), "without --open, handoff must be None");
    }

    #[test]
    fn expired_handoff_cleared_on_inspect() {
        let mut slot = Some(HandoffCode {
            code: generate_token(),
            expires_at: Instant::now() - Duration::from_secs(1),
        });
        let outcome = evaluate_handoff_exchange(&mut slot, "irrelevant", Instant::now());
        assert_eq!(
            outcome,
            HandoffExchangeOutcome::Denied {
                record_auth_fail: false
            }
        );
        assert!(slot.is_none(), "expired handoff must be cleared");
    }

    #[test]
    fn handoff_match_consumes() {
        let code = generate_token();
        let mut slot = Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() + HANDOFF_TTL,
        });
        let outcome = evaluate_handoff_exchange(&mut slot, &code, Instant::now());
        assert_eq!(outcome, HandoffExchangeOutcome::Matched);
        assert!(slot.is_none(), "matched handoff must be consumed");
    }

    #[test]
    fn handoff_mismatch_does_not_burn() {
        let code = generate_token();
        let mut slot = Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() + HANDOFF_TTL,
        });
        let wrong = "0".repeat(64);
        let outcome = evaluate_handoff_exchange(&mut slot, &wrong, Instant::now());
        assert_eq!(
            outcome,
            HandoffExchangeOutcome::Denied {
                record_auth_fail: true
            }
        );
        assert!(
            slot.is_some(),
            "wrong guess must leave the handoff code usable"
        );
        // Correct code still works after a mismatch.
        let outcome2 = evaluate_handoff_exchange(&mut slot, &code, Instant::now());
        assert_eq!(outcome2, HandoffExchangeOutcome::Matched);
        assert!(slot.is_none());
    }

    #[test]
    fn handoff_absent_denies_without_auth_fail() {
        let mut slot: Option<HandoffCode> = None;
        let outcome = evaluate_handoff_exchange(&mut slot, &generate_token(), Instant::now());
        assert_eq!(
            outcome,
            HandoffExchangeOutcome::Denied {
                record_auth_fail: false
            }
        );
    }

    #[test]
    fn handoff_replay_after_consume_denies() {
        let code = generate_token();
        let mut slot = Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() + HANDOFF_TTL,
        });
        assert_eq!(
            evaluate_handoff_exchange(&mut slot, &code, Instant::now()),
            HandoffExchangeOutcome::Matched
        );
        assert_eq!(
            evaluate_handoff_exchange(&mut slot, &code, Instant::now()),
            HandoffExchangeOutcome::Denied {
                record_auth_fail: false
            }
        );
    }
}
