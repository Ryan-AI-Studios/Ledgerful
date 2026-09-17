use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const EVENT_EXTRACT: &str = "extract";
pub const EVENT_APPLY: &str = "apply";
pub const EVENT_QUARANTINE: &str = "quarantine";
pub const EVENT_ERROR: &str = "error";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncLogEvent {
    pub ts: String,
    pub event: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl SyncLogEvent {
    pub fn new(event: &str, ok: bool) -> Self {
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            event: event.to_string(),
            ok,
            bundle: None,
            detail: None,
        }
    }

    pub fn with_bundle(mut self, bundle: impl Into<String>) -> Self {
        self.bundle = Some(bundle.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

pub fn sync_log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sync").join("sync.log")
}

pub fn append_sync_event(state_dir: &Path, event: &SyncLogEvent) -> io::Result<()> {
    let path = sync_log_path(state_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    serde_json::to_writer(&mut file, event).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Best-effort: warn on I/O and do not abort the sync cycle.
pub fn try_append_sync_event(state_dir: &Path, event: &SyncLogEvent) {
    if let Err(e) = append_sync_event(state_dir, event) {
        eprintln!("Warning: failed to append to sync.log: {e}");
    }
}

pub fn parse_event_line(line: &str) -> Option<SyncLogEvent> {
    let parsed: SyncLogEvent = serde_json::from_str(line).ok()?;
    if parsed.ts.trim().is_empty() || parsed.event.trim().is_empty() {
        return None;
    }
    match parsed.event.as_str() {
        EVENT_EXTRACT | EVENT_APPLY | EVENT_QUARANTINE | EVENT_ERROR => Some(parsed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_event_line_rejects_missing_event_or_ts() {
        assert!(parse_event_line(r#"{"ts":"","event":"extract","ok":true}"#).is_none());
        assert!(
            parse_event_line(r#"{"ts":"2026-01-01T00:00:00Z","event":"","ok":true}"#).is_none()
        );
        assert!(
            parse_event_line(r#"{"ts":"2026-01-01T00:00:00Z","event":"nope","ok":true}"#).is_none()
        );
        assert!(parse_event_line("not-json").is_none());
    }

    #[test]
    fn parse_event_line_accepts_extract() {
        let ev = parse_event_line(
            r#"{"ts":"2026-01-01T00:00:00Z","event":"extract","ok":true,"bundle":"a.lfbundle"}"#,
        )
        .expect("parse");
        assert_eq!(ev.event, EVENT_EXTRACT);
        assert!(ev.ok);
        assert_eq!(ev.bundle.as_deref(), Some("a.lfbundle"));
    }

    #[test]
    fn append_sync_event_writes_two_lines_without_secret() {
        let tmp = tempdir().unwrap();
        let secret = "super-secret-team-phrase";
        let a = SyncLogEvent::new(EVENT_EXTRACT, true).with_bundle("one.lfbundle");
        let b = SyncLogEvent::new(EVENT_QUARANTINE, false)
            .with_bundle("peer/x.lfbundle")
            .with_detail("decrypt failed");
        append_sync_event(tmp.path(), &a).unwrap();
        append_sync_event(tmp.path(), &b).unwrap();
        let body = fs::read_to_string(sync_log_path(tmp.path())).unwrap();
        assert_eq!(body.lines().count(), 2);
        assert!(!body.contains(secret));
        let parsed: Vec<SyncLogEvent> = body.lines().filter_map(parse_event_line).collect();
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].ok);
        assert!(!parsed[1].ok);
    }
}
