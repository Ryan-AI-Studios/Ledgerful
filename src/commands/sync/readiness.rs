//! Shared team-sync readiness checklist (0113).
//!
//! Used by `sync setup` and `sync status`. Never prompts for the team secret.
//! All `dir://` path work goes through [`crate::sync::transport::SyncTarget::parse`].

use crate::config::model::Config;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::sync::transport::SyncTarget;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Wall-clock deadline for shared-folder / NAS probes (status + setup only).
pub const TARGET_REACHABLE_TIMEOUT: Duration = Duration::from_secs(3);

/// High-level readiness for operator next-action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadinessKind {
    Disabled,
    Incomplete,
    Ready,
}

impl ReadinessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Incomplete => "incomplete",
            Self::Ready => "ready",
        }
    }
}

/// Bounded target-path probe result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetReachable {
    Yes,
    No,
    Timeout,
    #[serde(rename = "n-a")]
    NotApplicable,
}

impl TargetReachable {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Timeout => "timeout",
            Self::NotApplicable => "n-a",
        }
    }

    /// True only when the probe confirmed a regular directory in time.
    pub fn is_reachable(self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// Structured readiness report shared by setup/status (and `--json`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessReport {
    pub schema_version: u32,
    pub readiness: ReadinessKind,
    pub initialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub peer_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers_error: Option<String>,
    pub target: String,
    pub target_set: bool,
    pub target_parse_ok: bool,
    pub target_reachable: TargetReachable,
    pub secret_env_set: bool,
    pub enabled: bool,
    pub quarantine_count: u64,
    /// Human-only honesty when quarantine count failed or timed out (not in JSON schema).
    #[serde(skip)]
    pub quarantine_note: Option<String>,
    pub next_action: String,
}

/// Hang-bounded shared-root count outcome for status counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxOutboxScan {
    pub inbox: u64,
    pub outbox: u64,
    pub last_bundle: Option<String>,
    /// When set, human status should print this instead of numeric counters.
    pub note: Option<String>,
}

/// Quarantine file count with optional human-only honesty note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineScan {
    pub count: u64,
    pub note: Option<String>,
}

impl ReadinessReport {
    /// Pure camelCase JSON for `--json` (0093).
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": self.schema_version,
            "readiness": self.readiness.as_str(),
            "initialized": self.initialized,
            "deviceId": self.device_id,
            "peerCount": self.peer_count,
            "peersError": self.peers_error,
            "target": self.target,
            "targetSet": self.target_set,
            "targetParseOk": self.target_parse_ok,
            "targetReachable": self.target_reachable.as_str(),
            "secretEnvSet": self.secret_env_set,
            "enabled": self.enabled,
            "quarantineCount": self.quarantine_count,
            "nextAction": self.next_action,
        })
    }

    /// Gates required for `setup --enable` (secret is not required to enable).
    pub fn enable_failures(&self) -> Vec<&'static str> {
        let mut failed = Vec::new();
        if !self.initialized {
            failed.push("initialized (need device.key + device.pub + SoT device_id)");
        }
        match self.peer_count {
            Some(n) if n >= 1 => {}
            Some(_) => failed.push("peer_count >= 1 (pair at least one peer)"),
            None => failed.push("peer list readable (peers_error)"),
        }
        if !self.target_set {
            failed.push("target set (config set sync.target=dir://...)");
        } else if !self.target_parse_ok {
            failed.push("target parseable via SyncTarget::parse (dir://...)");
        }
        if self.target_set && self.target_parse_ok && !self.target_reachable.is_reachable() {
            failed.push("target_reachable (shared folder exists as a directory; check NAS/path)");
        }
        failed
    }

    pub fn can_enable(&self) -> bool {
        self.enable_failures().is_empty()
    }
}

/// Collect readiness for `layout` + loaded `config`.
pub fn collect_readiness(layout: &Layout, config: &Config) -> Result<ReadinessReport> {
    collect_readiness_with_timeout(layout, config, TARGET_REACHABLE_TIMEOUT)
}

/// Same as [`collect_readiness`] with an explicit probe deadline (tests).
pub fn collect_readiness_with_timeout(
    layout: &Layout,
    config: &Config,
    probe_timeout: Duration,
) -> Result<ReadinessReport> {
    let storage = StorageManager::init_with_layout(layout)
        .map_err(|e| miette!("Failed to open storage for sync readiness: {e}"))?;
    let conn = storage.get_connection();

    let device_id: Option<String> = conn
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|e| miette!("Failed to query sync_state: {e}"))?
        .filter(|id: &String| !id.trim().is_empty() && id != "unknown");

    let key_path = layout.state_dir.join("sync").join("device.key");
    let pub_path = layout.state_dir.join("sync").join("device.pub");
    let sot_ok = device_id.is_some();
    let initialized = key_path.exists() && pub_path.exists() && sot_ok;

    let sync_dir = layout.state_dir.join("sync");
    let (peer_count, peers_error) = match crate::sync::peers::list_peers(sync_dir.as_std_path()) {
        Ok(peers) => (Some(peers.len()), None),
        Err(e) => (None, Some(e)),
    };

    let target = config.sync.target.clone();
    let target_set = !target.trim().is_empty();
    let parsed = if target_set {
        SyncTarget::parse(&target).ok()
    } else {
        None
    };
    let target_parse_ok = parsed.is_some();

    let target_reachable = match &parsed {
        Some(SyncTarget::Dir(path)) => probe_target_reachable(path.clone(), probe_timeout),
        None if !target_set => TargetReachable::NotApplicable,
        None => TargetReachable::No,
    };

    let secret_env_set = std::env::var_os("LEDGERFUL_SYNC_SECRET").is_some_and(|v| !v.is_empty());

    let enabled = config.sync.enabled;

    // Shared-root FS scans only when the target probe already confirmed a regular dir.
    // Counts themselves are hang-bounded with the same deadline (spec §2.3#9).
    let quarantine = match (&device_id, &parsed, target_reachable) {
        (Some(did), Some(SyncTarget::Dir(base)), TargetReachable::Yes) => {
            count_quarantine_files_bounded(base.clone(), did.clone(), probe_timeout)
        }
        _ => QuarantineScan {
            count: 0,
            note: None,
        },
    };

    let readiness = classify_readiness(
        enabled,
        initialized,
        peer_count,
        target_set,
        target_parse_ok,
        target_reachable,
    );

    let next_action = compute_next_action(
        enabled,
        initialized,
        peer_count,
        peers_error.as_deref(),
        target_set,
        target_parse_ok,
        target_reachable,
        secret_env_set,
    );

    Ok(ReadinessReport {
        schema_version: 1,
        readiness,
        initialized,
        device_id,
        peer_count,
        peers_error,
        target,
        target_set,
        target_parse_ok,
        target_reachable,
        secret_env_set,
        enabled,
        quarantine_count: quarantine.count,
        quarantine_note: quarantine.note,
        next_action,
    })
}

fn classify_readiness(
    enabled: bool,
    initialized: bool,
    peer_count: Option<usize>,
    target_set: bool,
    target_parse_ok: bool,
    target_reachable: TargetReachable,
) -> ReadinessKind {
    if !enabled {
        return ReadinessKind::Disabled;
    }
    let peers_ok = matches!(peer_count, Some(n) if n >= 1);
    if initialized && peers_ok && target_set && target_parse_ok && target_reachable.is_reachable() {
        ReadinessKind::Ready
    } else {
        ReadinessKind::Incomplete
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_next_action(
    enabled: bool,
    initialized: bool,
    peer_count: Option<usize>,
    peers_error: Option<&str>,
    target_set: bool,
    target_parse_ok: bool,
    target_reachable: TargetReachable,
    secret_env_set: bool,
) -> String {
    if !initialized {
        return "ledgerful sync init".to_string();
    }
    if peers_error.is_some() {
        return "Fix permissions on .ledgerful/sync/peers/ then re-run ledgerful sync setup"
            .to_string();
    }
    match peer_count {
        Some(0) | None => {
            return "ledgerful sync pair  # then peer accepts; reverse for mutual trust"
                .to_string();
        }
        Some(_) => {}
    }
    if !target_set {
        return "ledgerful config set sync.target=\"dir:///path/to/shared\"".to_string();
    }
    if !target_parse_ok {
        return "Fix sync.target to a parseable dir:// absolute path (see docs/team-sync.md)"
            .to_string();
    }
    if !target_reachable.is_reachable() {
        return match target_reachable {
            TargetReachable::Timeout => {
                "Shared folder probe timed out — check NAS/cloud-drive connectivity, then re-run ledgerful sync setup"
                    .to_string()
            }
            _ => {
                "Ensure sync.target path exists as a directory and is reachable, then re-run ledgerful sync setup"
                    .to_string()
            }
        };
    }
    if !enabled {
        return "ledgerful sync setup --enable".to_string();
    }
    if !secret_env_set {
        if crate::util::term::is_interactive() {
            return "ledgerful sync run --once  # will prompt for team secret (or set LEDGERFUL_SYNC_SECRET)"
                .to_string();
        }
        return "set LEDGERFUL_SYNC_SECRET for non-interactive execution, then ledgerful sync run --once"
            .to_string();
    }
    "ledgerful sync run --once".to_string()
}

/// Run `work` on a helper thread with a hard wall-clock deadline.
///
/// NAS/SMB/OneDrive can hang on metadata/`read_dir`; never block the CLI unbounded.
fn run_with_timeout<T: Send + 'static>(
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, mpsc::RecvTimeoutError> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = work();
        let _ = tx.send(result);
    });
    rx.recv_timeout(timeout)
}

/// Probe whether `path` is a **regular non-symlink directory**, with a hard wall-clock timeout.
///
/// Uses [`is_regular_non_symlink_dir`] (not `Path::is_dir()`, which follows symlinks).
pub fn probe_target_reachable(path: PathBuf, timeout: Duration) -> TargetReachable {
    match run_with_timeout(timeout, move || is_regular_non_symlink_dir(&path)) {
        Ok(true) => TargetReachable::Yes,
        Ok(false) => TargetReachable::No,
        Err(mpsc::RecvTimeoutError::Timeout) => TargetReachable::Timeout,
        Err(mpsc::RecvTimeoutError::Disconnected) => TargetReachable::No,
    }
}

/// Hang-bounded quarantine count under `devices/<local_id>/quarantine/`.
pub fn count_quarantine_files_bounded(
    base: PathBuf,
    device_id: String,
    timeout: Duration,
) -> QuarantineScan {
    match run_with_timeout(timeout, move || count_quarantine_files(&base, &device_id)) {
        Ok(scan) => scan,
        Err(mpsc::RecvTimeoutError::Timeout) => QuarantineScan {
            count: 0,
            note: Some("unavailable (probe timed out)".to_string()),
        },
        Err(mpsc::RecvTimeoutError::Disconnected) => QuarantineScan {
            count: 0,
            note: Some("unavailable (probe failed)".to_string()),
        },
    }
}

/// Count regular non-symlink files under `devices/<local_id>/quarantine/`.
///
/// Matches [`crate::sync::transport::dir::DirTransport`] symlink discipline
/// (`symlink_metadata` + is_file + not symlink). Local device only.
///
/// On `read_dir` failure while the quarantine path is a regular dir, returns
/// count 0 with an honesty note (does not pretend healthy zero silently).
pub fn count_quarantine_files(base: &Path, device_id: &str) -> QuarantineScan {
    let qdir = base.join("devices").join(device_id).join("quarantine");
    match count_regular_non_symlink_files_detailed(&qdir) {
        Ok(n) => QuarantineScan {
            count: n,
            note: None,
        },
        Err(e) => QuarantineScan {
            count: 0,
            note: Some(format!("unavailable ({e})")),
        },
    }
}

/// Count regular non-symlink files in `dir`. Missing / non-regular dir → 0.
/// `read_dir` failure on a present regular dir → `Err`.
fn count_regular_non_symlink_files_detailed(dir: &Path) -> Result<u64, String> {
    if !is_regular_non_symlink_dir(dir) {
        return Ok(0);
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir failed: {e}"))?;
    let mut n = 0u64;
    for entry in entries.flatten() {
        if is_regular_non_symlink_file(&entry.path()) {
            n += 1;
        }
    }
    Ok(n)
}

/// Regular file that is not a symlink (match DirTransport).
fn is_regular_non_symlink_file(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => meta.is_file() && !meta.file_type().is_symlink(),
        Err(_) => false,
    }
}

/// Regular directory that is not a symlink (match DirTransport enable gate).
pub fn is_regular_non_symlink_dir(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => meta.is_dir() && !meta.file_type().is_symlink(),
        Err(_) => false,
    }
}

/// Hang-bounded inbox/outbox scan for status (call only when target is reachable).
pub fn count_inbox_outbox_bounded(
    base: PathBuf,
    device_id: String,
    timeout: Duration,
) -> InboxOutboxScan {
    match run_with_timeout(timeout, move || count_inbox_outbox(&base, &device_id)) {
        Ok(scan) => scan,
        Err(mpsc::RecvTimeoutError::Timeout) => InboxOutboxScan {
            inbox: 0,
            outbox: 0,
            last_bundle: None,
            note: Some("unavailable (probe timed out)".to_string()),
        },
        Err(mpsc::RecvTimeoutError::Disconnected) => InboxOutboxScan {
            inbox: 0,
            outbox: 0,
            last_bundle: None,
            note: Some("unavailable (probe failed)".to_string()),
        },
    }
}

/// Count inbox/outbox bundle files under a parsed `dir://` root (status counters).
///
/// Uses regular non-symlink file checks (DirTransport discipline). Symlink dirs
/// and files are ignored. Top-level `read_dir` failures set [`InboxOutboxScan::note`].
pub fn count_inbox_outbox(base: &Path, device_id: &str) -> InboxOutboxScan {
    let mut inbox_count = 0u64;
    let mut outbox_count = 0u64;
    let mut last_bundle_name: Option<String> = None;
    let mut error: Option<String> = None;

    let outbox_path = base.join("devices").join(device_id);
    if is_regular_non_symlink_dir(&outbox_path) {
        match std::fs::read_dir(&outbox_path) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if is_regular_non_symlink_file(&path)
                        && let Some(name) = path.file_name().and_then(|n| n.to_str())
                        && crate::sync::transport::is_bundle_filename(name)
                    {
                        outbox_count += 1;
                    }
                }
            }
            Err(e) => error = Some(format!("outbox read_dir failed: {e}")),
        }
    }

    let devices_path = base.join("devices");
    if is_regular_non_symlink_dir(&devices_path) {
        match std::fs::read_dir(&devices_path) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    if entry.file_name() == device_id {
                        continue;
                    }
                    let peer_path = entry.path();
                    if !is_regular_non_symlink_dir(&peer_path) {
                        continue;
                    }
                    match std::fs::read_dir(&peer_path) {
                        Ok(peer_entries) => {
                            for peer_entry in peer_entries.flatten() {
                                let path = peer_entry.path();
                                if is_regular_non_symlink_file(&path)
                                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                                    && crate::sync::transport::is_bundle_filename(name)
                                {
                                    inbox_count += 1;
                                    match &last_bundle_name {
                                        Some(cur) if name <= cur.as_str() => {}
                                        _ => last_bundle_name = Some(name.to_string()),
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            if error.is_none() {
                                error = Some(format!("inbox peer read_dir failed: {e}"));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                if error.is_none() {
                    error = Some(format!("devices read_dir failed: {e}"));
                }
            }
        }
    }

    let note = error.map(|e| format!("unavailable ({e})"));
    // On I/O error, do not present partial counters as healthy zeros alone —
    // status human path prints `note`; JSON still gets numeric 0s.
    if note.is_some() {
        InboxOutboxScan {
            inbox: 0,
            outbox: 0,
            last_bundle: None,
            note,
        }
    } else {
        InboxOutboxScan {
            inbox: inbox_count,
            outbox: outbox_count,
            last_bundle: last_bundle_name,
            note: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::SyncConfig;
    use crate::sync::transport::SyncTarget;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use std::fs;
    use std::time::Instant;
    use tempfile::tempdir;

    fn sample_config(enabled: bool, target: &str) -> Config {
        Config {
            sync: SyncConfig {
                enabled,
                target: target.to_string(),
                ..SyncConfig::default()
            },
            ..Config::default()
        }
    }

    fn write_keys_and_sot(layout: &Layout, device_id: &str) {
        let sync_dir = layout.state_dir.join("sync");
        fs::create_dir_all(sync_dir.as_std_path()).unwrap();
        let sk = SigningKey::generate(&mut OsRng);
        fs::write(sync_dir.join("device.key").as_std_path(), sk.to_bytes()).unwrap();
        fs::write(
            sync_dir.join("device.pub").as_std_path(),
            sk.verifying_key().to_bytes(),
        )
        .unwrap();
        layout.ensure_state_dir().unwrap();
        let storage = StorageManager::init_with_layout(layout).unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO sync_state (id, device_id) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET device_id = excluded.device_id",
                [device_id],
            )
            .unwrap();
    }

    fn add_peer(layout: &Layout, peer_id: &str) {
        let sync_dir = layout.state_dir.join("sync");
        let sk = SigningKey::generate(&mut OsRng);
        crate::sync::peers::trust_peer(
            sync_dir.as_std_path(),
            peer_id,
            &sk.verifying_key().to_bytes(),
            false,
        )
        .unwrap();
    }

    #[test]
    fn windows_style_dir_triple_slash_parse_matches_synctarget() {
        let raw = "dir:///C:/Users/shared/team-sync";
        let parsed = SyncTarget::parse(raw).expect("parse");
        match parsed {
            SyncTarget::Dir(p) => {
                let s = p.to_string_lossy();
                // SyncTarget strips the leading slash before drive letter.
                assert!(
                    s.starts_with("C:") || s.starts_with("C:/") || s.starts_with("C:\\"),
                    "expected Windows drive path, got {s}"
                );
                assert!(
                    !s.starts_with("/C:"),
                    "naive strip_prefix would keep /C:; got {s}"
                );
            }
        }
    }

    #[test]
    fn readiness_disabled_when_not_enabled() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        layout.ensure_state_dir().unwrap();
        let _ = StorageManager::init_with_layout(&layout).unwrap();
        let cfg = sample_config(false, "");
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert_eq!(r.readiness, ReadinessKind::Disabled);
        assert!(!r.initialized);
        assert!(r.next_action.contains("sync init"));
        assert_eq!(r.schema_version, 1);
    }

    #[test]
    fn readiness_next_action_peers_when_init_done() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        write_keys_and_sot(&layout, "device-test0001");
        let cfg = sample_config(false, "");
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert!(r.initialized);
        assert_eq!(r.peer_count, Some(0));
        assert!(r.next_action.contains("sync pair"));
    }

    #[test]
    fn readiness_next_action_target_when_peers_present() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        write_keys_and_sot(&layout, "device-test0002");
        add_peer(&layout, "device-peer0001");
        let cfg = sample_config(false, "");
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert_eq!(r.peer_count, Some(1));
        assert!(r.next_action.contains("sync.target") || r.next_action.contains("target"));
    }

    #[test]
    fn readiness_ready_when_enabled_all_green() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let share = root.join("share");
        fs::create_dir_all(&share).unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
        write_keys_and_sot(&layout, "device-test0003");
        add_peer(&layout, "device-peer0002");
        let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
        let cfg = sample_config(true, &target);
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert!(r.can_enable() || r.enabled);
        assert!(r.target_parse_ok);
        assert_eq!(r.target_reachable, TargetReachable::Yes);
        assert_eq!(r.readiness, ReadinessKind::Ready);
        assert!(r.next_action.contains("run --once") || r.next_action.contains("LEDGERFUL_SYNC"));
    }

    #[test]
    fn enable_refuse_matrix_peers_zero() {
        let tmp = tempdir().unwrap();
        let share = tmp.path().join("share");
        fs::create_dir_all(&share).unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        write_keys_and_sot(&layout, "device-test0004");
        let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
        let cfg = sample_config(false, &target);
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert!(!r.can_enable());
        let fails = r.enable_failures();
        assert!(
            fails.iter().any(|f| f.contains("peer_count")),
            "expected peer failure, got {fails:?}"
        );
    }

    #[test]
    fn enable_refuse_not_initialized() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        layout.ensure_state_dir().unwrap();
        let _ = StorageManager::init_with_layout(&layout).unwrap();
        let share = tmp.path().join("share");
        fs::create_dir_all(&share).unwrap();
        let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
        let cfg = sample_config(false, &target);
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert!(!r.can_enable());
        assert!(
            r.enable_failures()
                .iter()
                .any(|f| f.contains("initialized"))
        );
    }

    #[test]
    fn enable_refuse_bad_target() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        write_keys_and_sot(&layout, "device-test0005");
        add_peer(&layout, "device-peer0003");
        let cfg = sample_config(false, "not-a-dir-uri");
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert!(!r.can_enable());
        assert!(!r.target_parse_ok);
    }

    #[test]
    fn enable_refuse_unreachable_target() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        write_keys_and_sot(&layout, "device-test0006");
        add_peer(&layout, "device-peer0004");
        let missing = tmp.path().join("no-such-share-dir");
        let target = format!("dir://{}", missing.display().to_string().replace('\\', "/"));
        let cfg = sample_config(false, &target);
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert_eq!(r.target_reachable, TargetReachable::No);
        assert!(!r.can_enable());
    }

    #[test]
    fn enable_success_when_all_green() {
        let tmp = tempdir().unwrap();
        let share = tmp.path().join("share");
        fs::create_dir_all(&share).unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        write_keys_and_sot(&layout, "device-test0007");
        add_peer(&layout, "device-peer0005");
        let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
        let cfg = sample_config(false, &target);
        let r = collect_readiness(&layout, &cfg).unwrap();
        assert!(r.can_enable(), "failures: {:?}", r.enable_failures());
        assert_eq!(r.readiness, ReadinessKind::Disabled);
        assert_eq!(r.next_action, "ledgerful sync setup --enable");
    }

    #[test]
    fn json_schema_version_and_camel_case_keys() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        layout.ensure_state_dir().unwrap();
        let _ = StorageManager::init_with_layout(&layout).unwrap();
        let cfg = sample_config(false, "");
        let r = collect_readiness(&layout, &cfg).unwrap();
        let v = r.to_json_value();
        assert_eq!(v["schemaVersion"], 1);
        assert!(v.get("nextAction").is_some());
        assert!(v.get("targetReachable").is_some());
        assert!(v.get("secretEnvSet").is_some());
        assert!(v.get("quarantineCount").is_some());
        assert!(v.get("targetParseOk").is_some());
        // Must not use snake_case keys on the public surface.
        assert!(v.get("schema_version").is_none());
        assert!(v.get("next_action").is_none());
    }

    #[test]
    fn probe_timeout_does_not_hang_long() {
        // Non-existent path returns quickly as No (not timeout).
        let missing = PathBuf::from("/this/path/should/not/exist/ledgerful-0113-probe");
        let start = Instant::now();
        let r = probe_target_reachable(missing, Duration::from_millis(200));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(matches!(r, TargetReachable::No | TargetReachable::Timeout));
    }

    #[test]
    fn probe_regular_dir_yes_file_path_no() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("share");
        fs::create_dir_all(&dir).unwrap();
        let file = tmp.path().join("not-a-dir.txt");
        fs::write(&file, b"x").unwrap();

        assert_eq!(
            probe_target_reachable(dir, Duration::from_secs(2)),
            TargetReachable::Yes
        );
        assert_eq!(
            probe_target_reachable(file, Duration::from_secs(2)),
            TargetReachable::No
        );
    }

    #[test]
    fn probe_symlink_to_dir_is_not_yes() {
        // Symlink-to-dir must not satisfy the regular-directory enable gate.
        // On Windows, creating directory symlinks often requires elevation — skip if create fails.
        let tmp = tempdir().unwrap();
        let real = tmp.path().join("real-share");
        fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link-share");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, &link).expect("unix symlink dir");
        }
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(&real, &link).is_err() {
                // No privilege / developer mode — cannot exercise symlink gate on this host.
                return;
            }
        }

        // path.is_dir() would follow and return true; we require non-symlink.
        assert!(
            real.is_dir(),
            "sanity: Path::is_dir follows; real dir exists"
        );
        assert_eq!(
            probe_target_reachable(link, Duration::from_secs(2)),
            TargetReachable::No,
            "symlink-to-dir must not be TargetReachable::Yes"
        );
        assert_eq!(
            probe_target_reachable(real, Duration::from_secs(2)),
            TargetReachable::Yes
        );
    }

    #[test]
    fn quarantine_count_ignores_symlinks() {
        let tmp = tempdir().unwrap();
        let base = tmp.path();
        let qdir = base.join("devices").join("device-local").join("quarantine");
        fs::create_dir_all(&qdir).unwrap();
        fs::write(qdir.join("bad.lfbundle"), b"x").unwrap();
        fs::write(qdir.join("also.gpg"), b"y").unwrap();

        // Symlink (if platform supports) should not be counted.
        #[cfg(unix)]
        {
            let _ =
                std::os::unix::fs::symlink(qdir.join("bad.lfbundle"), qdir.join("link.lfbundle"));
        }
        #[cfg(windows)]
        {
            // Symlink creation may require elevation; skip silently if it fails.
            let _ = std::os::windows::fs::symlink_file(
                qdir.join("bad.lfbundle"),
                qdir.join("link.lfbundle"),
            );
        }

        let scan = count_quarantine_files(base, "device-local");
        assert!(scan.note.is_none(), "healthy count must not set note");
        let n = scan.count;
        // Two regular files; symlink (if created) ignored.
        assert!(n >= 2, "expected at least 2 regular files, got {n}");
        let regular = fs::read_dir(&qdir)
            .unwrap()
            .flatten()
            .filter(|e| {
                fs::symlink_metadata(e.path())
                    .map(|m| m.is_file() && !m.file_type().is_symlink())
                    .unwrap_or(false)
            })
            .count() as u64;
        assert_eq!(n, regular);
    }

    #[test]
    fn quarantine_missing_dir_is_zero_without_note() {
        let tmp = tempdir().unwrap();
        let scan = count_quarantine_files(tmp.path(), "no-device");
        assert_eq!(scan.count, 0);
        assert!(scan.note.is_none());
    }

    #[test]
    fn windows_target_string_parse_ok_in_report() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        write_keys_and_sot(&layout, "device-test0008");
        add_peer(&layout, "device-peer0006");
        // Even on non-Windows, parse must accept the form and set target_parse_ok.
        let cfg = sample_config(false, "dir:///C:/Shared/ledgerful-sync");
        let r = collect_readiness_with_timeout(&layout, &cfg, Duration::from_millis(500)).unwrap();
        assert!(r.target_set);
        assert!(
            r.target_parse_ok,
            "SyncTarget::parse must accept dir:///C:/…"
        );
        // Path almost certainly does not exist on this host → not reachable.
        assert!(!r.target_reachable.is_reachable());
    }
}
