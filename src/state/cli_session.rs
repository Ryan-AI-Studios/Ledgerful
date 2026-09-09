//! Gitignored CLI session cookie for 0300 session-once notices.
//!
//! Persistence only. Notice-id mapping and print-layer collapse live in
//! `output::session_notice`. Fail-open: IO / parse / TTL errors become a
//! fresh first-show session; callers must not fail the host command.

use crate::state::layout::Layout;
use camino::Utf8PathBuf;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use uuid::Uuid;

pub const CLI_SESSION_SCHEMA_VERSION: u32 = 1;
pub const CLI_SESSION_TTL: Duration = Duration::hours(8);
pub const LEDGERFUL_SESSION_ID_ENV: &str = "LEDGERFUL_SESSION_ID";

const MAX_SESSION_ID_LEN: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliSessionFile {
    schema_version: u32,
    id: String,
    started_at: String,
    #[serde(default)]
    shown: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CliSession {
    pub id: String,
    pub started_at: DateTime<Utc>,
    shown: BTreeSet<String>,
    path: Utf8PathBuf,
}

impl CliSession {
    pub fn load(layout: &Layout, session_id: Option<&str>, now: DateTime<Utc>) -> Self {
        let sanitized = session_id.and_then(sanitize_session_id);
        let path = cookie_path(layout, sanitized);
        match read_file(&path, sanitized, now) {
            Some(session) => session,
            None => fresh(sanitized, now, path),
        }
    }

    pub fn is_shown(&self, id: &str) -> bool {
        self.shown.contains(id)
    }

    pub fn mark_shown(&mut self, id: &str) {
        self.shown.insert(id.to_string());
    }

    pub fn path(&self) -> &Utf8PathBuf {
        &self.path
    }

    /// Persist after a participating notice was marked. Fail-open on IO.
    pub fn persist(&self) {
        if self.shown.is_empty() {
            return;
        }
        if let Some(parent) = self.path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            tracing::debug!(
                error = %err,
                path = %self.path,
                "cli-session parent mkdir failed"
            );
            return;
        }
        let file = CliSessionFile {
            schema_version: CLI_SESSION_SCHEMA_VERSION,
            id: self.id.clone(),
            started_at: self.started_at.to_rfc3339(),
            shown: self.shown.iter().cloned().collect(),
        };
        let Ok(body) = serde_json::to_string_pretty(&file) else {
            tracing::debug!(path = %self.path, "cli-session serialize failed");
            return;
        };
        if let Err(err) = fs::write(&self.path, body) {
            tracing::debug!(
                error = %err,
                path = %self.path,
                "cli-session persist failed"
            );
        }
    }
}

pub fn env_session_id() -> Option<String> {
    std::env::var(LEDGERFUL_SESSION_ID_ENV)
        .ok()
        .and_then(|raw| sanitize_session_id(&raw).map(str::to_string))
}

pub fn sanitize_session_id(raw: &str) -> Option<&str> {
    if raw.is_empty() || raw.len() > MAX_SESSION_ID_LEN {
        return None;
    }
    if raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        Some(raw)
    } else {
        None
    }
}

pub fn cookie_path(layout: &Layout, session_id: Option<&str>) -> Utf8PathBuf {
    match session_id {
        Some(id) => layout.cli_session_file_for_id(id),
        None => layout.cli_session_file(),
    }
}

fn fresh(session_id: Option<&str>, now: DateTime<Utc>, path: Utf8PathBuf) -> CliSession {
    CliSession {
        id: session_id
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        started_at: now,
        shown: BTreeSet::new(),
        path,
    }
}

fn read_file(
    path: &Utf8PathBuf,
    session_id: Option<&str>,
    now: DateTime<Utc>,
) -> Option<CliSession> {
    let bytes = fs::read_to_string(path).ok()?;
    let parsed: CliSessionFile = serde_json::from_str(&bytes).ok()?;
    if parsed.schema_version != CLI_SESSION_SCHEMA_VERSION {
        return None;
    }
    let started_at = DateTime::parse_from_rfc3339(&parsed.started_at)
        .ok()?
        .with_timezone(&Utc);
    let age = now.signed_duration_since(started_at);
    if age >= CLI_SESSION_TTL || age < Duration::zero() {
        return None;
    }
    let id = session_id
        .map(str::to_string)
        .unwrap_or_else(|| parsed.id.clone());
    Some(CliSession {
        id,
        started_at,
        shown: parsed.shown.into_iter().collect(),
        path: path.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn layout_tmp() -> (tempfile::TempDir, Layout) {
        let tmp = tempdir().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp path");
        let layout = Layout::from_roots(&root, root.join(".ledgerful"));
        (tmp, layout)
    }

    #[test]
    fn session_notice_ttl_expired_full_again() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let mut session = CliSession::load(&layout, None, now);
        let old_id = session.id.clone();
        session.mark_shown("coverage.global");
        session.persist();

        let later = now + CLI_SESSION_TTL + Duration::seconds(1);
        let session = CliSession::load(&layout, None, later);
        assert_ne!(session.id, old_id);
        assert!(!session.is_shown("coverage.global"));
    }

    #[test]
    fn session_notice_ttl_boundary() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let mut session = CliSession::load(&layout, None, now);
        let id = session.id.clone();
        session.mark_shown("coverage.global");
        session.persist();

        let keep = CliSession::load(&layout, None, now + CLI_SESSION_TTL - Duration::seconds(1));
        assert_eq!(keep.id, id);
        assert!(keep.is_shown("coverage.global"));

        let reset = CliSession::load(&layout, None, now + CLI_SESSION_TTL + Duration::seconds(1));
        assert_ne!(reset.id, id);
        assert!(!reset.is_shown("coverage.global"));
    }

    #[test]
    fn session_notice_corrupt_started_at_fails_open() {
        let (_tmp, layout) = layout_tmp();
        let path = layout.cli_session_file();
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"id":"keep-me","startedAt":"not-a-date","shown":["coverage.global"]}"#,
        )
        .expect("write");
        let session = CliSession::load(&layout, None, Utc::now());
        assert!(!session.is_shown("coverage.global"));
        assert_ne!(session.id, "keep-me");
    }

    #[test]
    fn session_notice_env_id_isolates() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let mut a = CliSession::load(&layout, Some("agent-a"), now);
        a.mark_shown("coverage.global");
        a.persist();
        let mut b = CliSession::load(&layout, Some("agent-b"), now);
        assert!(!b.is_shown("coverage.global"));
        b.mark_shown("coverage.services");
        b.persist();

        assert!(layout.cli_session_file_for_id("agent-a").is_file());
        assert!(layout.cli_session_file_for_id("agent-b").is_file());
        assert!(!layout.cli_session_file().is_file());

        let default = CliSession::load(&layout, None, now);
        assert!(!default.is_shown("coverage.global"));
        assert!(!default.is_shown("coverage.services"));
    }

    #[test]
    fn session_notice_corrupt_file_fail_open() {
        let (_tmp, layout) = layout_tmp();
        let path = layout.cli_session_file();
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "not-json{{{").expect("write");
        let session = CliSession::load(&layout, None, Utc::now());
        assert!(session.shown.is_empty());
    }

    #[test]
    fn session_notice_unknown_schema_version_fails_open() {
        let (_tmp, layout) = layout_tmp();
        let path = layout.cli_session_file();
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &path,
            r#"{"schemaVersion":2,"id":"keep-me","startedAt":"2026-09-08T00:00:00Z","shown":["coverage.global"]}"#,
        )
        .expect("write");
        let session = CliSession::load(&layout, None, Utc::now());
        assert!(!session.is_shown("coverage.global"));
        assert_ne!(session.id, "keep-me");
    }

    #[test]
    fn future_started_at_is_expired() {
        let (_tmp, layout) = layout_tmp();
        let path = layout.cli_session_file();
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let future = (Utc::now() + Duration::hours(1)).to_rfc3339();
        fs::write(
            &path,
            format!(
                r#"{{"schemaVersion":1,"id":"future","startedAt":"{future}","shown":["coverage.global"]}}"#
            ),
        )
        .expect("write");
        let session = CliSession::load(&layout, None, Utc::now());
        assert!(!session.is_shown("coverage.global"));
    }
}
