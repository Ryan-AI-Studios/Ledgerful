use crate::git::{ChangeType, FileChange, RepoSnapshot};
use crate::impact::packet::ImpactPacket;
use crate::state::StateError;
use crate::state::layout::Layout;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::fs;

pub const LATEST_IMPACT_REPORT: &str = "latest-impact.json";
pub const LATEST_SCAN_REPORT: &str = "latest-scan.json";

/// Explicit marker written to `latest-impact.json` when `scan --impact` /
/// `impact` runs against a clean working tree, so the file never presents a
/// stale dirty-tree packet as the current state. This is a distinct, stable
/// shape (not a partial `ImpactPacket`) so callers can detect it directly
/// instead of inferring "clean" from a failed `ImpactPacket` deserialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CleanTreeTombstone {
    pub status: String,
    pub head_hash: Option<String>,
    pub branch_name: Option<String>,
    pub schema_version: String,
    pub tree_clean: bool,
    pub timestamp_utc: String,
    #[serde(default)]
    pub changes: Vec<serde_json::Value>,
}

impl CleanTreeTombstone {
    pub const STATUS: &'static str = "clean_tree";

    pub fn from_packet(packet: &ImpactPacket) -> Self {
        Self {
            status: Self::STATUS.to_string(),
            head_hash: packet.head_hash.clone(),
            branch_name: packet.branch_name.clone(),
            schema_version: packet.schema_version.clone(),
            tree_clean: true,
            timestamp_utc: packet.timestamp_utc.clone(),
            changes: Vec::new(),
        }
    }
}

/// The two valid shapes `latest-impact.json` can take under the freshness
/// contract: a real packet, or an explicit clean-tree tombstone.
#[derive(Debug, Clone)]
pub enum LatestImpactReport {
    Packet(Box<ImpactPacket>),
    CleanTree(CleanTreeTombstone),
}

/// Reads and classifies `latest-impact.json`, trying the clean-tree
/// tombstone shape before falling back to a full `ImpactPacket`. Returns
/// `Ok(None)` if the file does not exist. Callers that previously
/// deserialized straight to `ImpactPacket` should use this instead so a
/// clean-tree tombstone is recognized rather than treated as corrupt.
pub fn read_latest_impact_report(layout: &Layout) -> Result<Option<LatestImpactReport>> {
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let content = match fs::read_to_string(&report_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).into_diagnostic(),
    };

    if let Ok(tombstone) = serde_json::from_str::<CleanTreeTombstone>(&content)
        && tombstone.status == CleanTreeTombstone::STATUS
    {
        return Ok(Some(LatestImpactReport::CleanTree(tombstone)));
    }

    let mut packet: ImpactPacket = serde_json::from_str(&content).into_diagnostic()?;
    // Pre-0117 on-disk reports omit confidenceClass/confidenceSummary.
    if let Some(ref mut blast) = packet.blast_radius {
        blast.hydrate_confidence();
    }
    Ok(Some(LatestImpactReport::Packet(Box::new(packet))))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanDiffSummary {
    pub path: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanChange {
    pub path: String,
    pub change_type: String,
    pub is_staged: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub head_hash: Option<String>,
    pub branch_name: Option<String>,
    pub is_clean: bool,
    pub changes: Vec<ScanChange>,
    pub diff_summaries: Vec<ScanDiffSummary>,
}

impl ScanReport {
    pub fn from_snapshot(snapshot: &RepoSnapshot, diff_summaries: Vec<ScanDiffSummary>) -> Self {
        Self {
            head_hash: snapshot.head_hash.clone(),
            branch_name: snapshot.branch_name.clone(),
            is_clean: snapshot.is_clean,
            changes: snapshot.changes.iter().map(ScanChange::from).collect(),
            diff_summaries,
        }
    }
}

impl From<&FileChange> for ScanChange {
    fn from(change: &FileChange) -> Self {
        let change_type = match &change.change_type {
            ChangeType::Added => "Added".to_string(),
            ChangeType::Modified => "Modified".to_string(),
            ChangeType::Deleted => "Deleted".to_string(),
            ChangeType::Renamed { old_path } => {
                format!(
                    "Renamed: {} -> {}",
                    old_path.display(),
                    change.path.display()
                )
            }
        };

        Self {
            path: change.path.to_string_lossy().to_string(),
            change_type,
            is_staged: change.is_staged,
        }
    }
}

/// Whether [`write_impact_report`] rewrote `latest-impact.json`, left it
/// unchanged (stable CleanTree skip when HEAD is unchanged — track 0147), or
/// skipped under RO / unavailable write (0174).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactReportWriteOutcome {
    Written,
    Unchanged,
    /// Report not written: storage is read-only or write hit RO-class error.
    Skipped,
}

/// Greppable honesty when durable impact report write is unavailable (0174).
pub const IMPACT_REPORT_RO_HONESTY: &str = "report write unavailable under RO";

/// Greppable honesty when durable scan report write is unavailable (0174-E).
pub const SCAN_REPORT_RO_HONESTY: &str = "report write unavailable under RO";

/// True when a report-write error is RO-class (PermissionDenied / read-only).
pub fn is_report_ro_class_error(err: &miette::Report) -> bool {
    let s = format!("{err}").to_ascii_lowercase();
    s.contains("permission denied")
        || s.contains("access is denied")
        || s.contains("read-only file system")
        || s.contains("readonly database")
        || s.contains("attempt to write a readonly")
        || s.contains("read-only")
        || s.contains("readonly")
        || s.contains("os error 5")
        || s.contains("(os error 5)")
        || s.contains("os error 30")
        || s.contains("disk i/o error")
}

/// Soft-write impact report: skip when storage is RO or write fails RO-class.
///
/// Non-RO write failures still return `Err` (hard-fail).
pub fn soft_write_impact_report(
    layout: &Layout,
    packet: &ImpactPacket,
    storage_is_read_only: bool,
) -> Result<ImpactReportWriteOutcome> {
    if storage_is_read_only {
        return Ok(ImpactReportWriteOutcome::Skipped);
    }
    match write_impact_report(layout, packet) {
        Ok(outcome) => Ok(outcome),
        Err(e) => {
            if is_report_ro_class_error(&e) {
                tracing::warn!("{IMPACT_REPORT_RO_HONESTY}: {e}");
                Ok(ImpactReportWriteOutcome::Skipped)
            } else {
                Err(e)
            }
        }
    }
}

/// Soft-write scan report: RO-class fail → skip (no hard-fail).
pub fn soft_write_scan_report(layout: &Layout, report: &ScanReport) -> Result<bool> {
    match write_scan_report(layout, report) {
        Ok(()) => Ok(true),
        Err(e) => {
            if is_report_ro_class_error(&e) {
                tracing::warn!("{SCAN_REPORT_RO_HONESTY}: {e}");
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

/// Soft-write clean-tree tombstone: RO-class fail → skip (no hard-fail).
pub fn soft_write_clean_tree_tombstone(
    layout: &Layout,
    head_hash: Option<String>,
    branch_name: Option<String>,
) -> Result<bool> {
    match write_clean_tree_tombstone(layout, head_hash, branch_name) {
        Ok(()) => Ok(true),
        Err(e) => {
            if is_report_ro_class_error(&e) {
                tracing::warn!("{SCAN_REPORT_RO_HONESTY}: {e}");
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

pub fn write_impact_report(
    layout: &Layout,
    packet: &ImpactPacket,
) -> Result<ImpactReportWriteOutcome> {
    layout.ensure_state_dir()?;

    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);

    // 0147 B4: stable CleanTree — skip rewrite when existing tombstone has the
    // same head_hash (both-None equal) so content hash does not churn solely
    // via timestamp_utc on repeated clean impact runs.
    if packet.tree_clean
        && packet.changes.is_empty()
        && let Ok(Some(LatestImpactReport::CleanTree(existing))) = read_latest_impact_report(layout)
        && existing.head_hash == packet.head_hash
    {
        return Ok(ImpactReportWriteOutcome::Unchanged);
    }

    let json = if packet.tree_clean && packet.changes.is_empty() {
        serde_json::to_string_pretty(&CleanTreeTombstone::from_packet(packet))
            .map_err(std::io::Error::other)
            .map_err(|e| StateError::WriteReportFailed {
                path: report_path.to_string(),
                source: e,
            })?
    } else {
        serde_json::to_string_pretty(packet)
            .map_err(std::io::Error::other)
            .map_err(|e| StateError::WriteReportFailed {
                path: report_path.to_string(),
                source: e,
            })?
    };

    atomic_write_json(&report_path, json).map_err(|e| StateError::WriteReportFailed {
        path: report_path.to_string(),
        source: e,
    })?;

    Ok(ImpactReportWriteOutcome::Written)
}

pub fn write_clean_tree_tombstone(
    layout: &Layout,
    head_hash: Option<String>,
    branch_name: Option<String>,
) -> Result<()> {
    layout.ensure_state_dir()?;
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let tombstone = CleanTreeTombstone {
        status: CleanTreeTombstone::STATUS.to_string(),
        head_hash,
        branch_name,
        schema_version: ImpactPacket::default().schema_version,
        tree_clean: true,
        timestamp_utc: chrono::Utc::now().to_rfc3339(),
        changes: Vec::new(),
    };
    let json = serde_json::to_string_pretty(&tombstone)
        .map_err(std::io::Error::other)
        .map_err(|e| StateError::WriteReportFailed {
            path: report_path.to_string(),
            source: e,
        })?;

    atomic_write_json(&report_path, json).map_err(|e| StateError::WriteReportFailed {
        path: report_path.to_string(),
        source: e,
    })?;

    Ok(())
}

pub fn write_scan_report(layout: &Layout, report: &ScanReport) -> Result<()> {
    layout.ensure_state_dir()?;

    let report_path = layout.reports_dir().join(LATEST_SCAN_REPORT);
    let json = serde_json::to_string_pretty(report)
        .map_err(std::io::Error::other)
        .map_err(|e| StateError::WriteReportFailed {
            path: report_path.to_string(),
            source: e,
        })?;

    atomic_write_json(&report_path, json).map_err(|e| StateError::WriteReportFailed {
        path: report_path.to_string(),
        source: e,
    })?;

    Ok(())
}

/// Write JSON atomically by first writing to a sibling `.json.tmp` file and
/// then renaming it into place. The rename is atomic on the same filesystem
/// (POSIX; Windows `MoveFileEx` on NTFS), preventing partial-write corruption
/// if readers or concurrent writers access the report.
fn atomic_write_json(report_path: &camino::Utf8PathBuf, json: String) -> std::io::Result<()> {
    let tmp_path = report_path.with_extension("json.tmp");
    fs::write(&tmp_path, &json)?;
    fs::rename(&tmp_path, report_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::layout::Layout;
    use camino::Utf8Path;
    use tempfile::tempdir;

    #[test]
    fn test_write_impact_report() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let packet = ImpactPacket::default();

        write_impact_report(&layout, &packet).unwrap();

        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        assert!(report_path.exists());

        let content = fs::read_to_string(report_path).unwrap();
        let deserialized: ImpactPacket = serde_json::from_str(&content).unwrap();
        assert_eq!(deserialized.schema_version, packet.schema_version);
    }

    /// Regression for CG-F18: a clean-tree scan must overwrite a stale
    /// dirty-tree packet with an explicit, dated tombstone rather than
    /// leaving the old packet's data looking current.
    #[test]
    fn test_clean_tree_scan_overwrites_stale_dirty_tree_packet() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let dirty_packet = ImpactPacket {
            head_hash: Some("old-dirty-hash".to_string()),
            tree_clean: false,
            changes: vec![crate::impact::packet::ChangedFile::default()],
            ..ImpactPacket::default()
        };
        write_impact_report(&layout, &dirty_packet).unwrap();

        let clean_packet = ImpactPacket {
            head_hash: Some("new-clean-hash".to_string()),
            tree_clean: true,
            changes: Vec::new(),
            ..ImpactPacket::default()
        };
        write_impact_report(&layout, &clean_packet).unwrap();

        match read_latest_impact_report(&layout).unwrap().unwrap() {
            LatestImpactReport::CleanTree(tombstone) => {
                assert_eq!(tombstone.head_hash, Some("new-clean-hash".to_string()));
                assert!(!tombstone.timestamp_utc.is_empty());
                assert_eq!(tombstone.status, CleanTreeTombstone::STATUS);
            }
            LatestImpactReport::Packet(_) => {
                panic!("expected a clean-tree tombstone, got a full packet (stale data survived)")
            }
        }
    }

    #[test]
    fn test_read_latest_impact_report_missing_file_returns_none() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        assert!(read_latest_impact_report(&layout).unwrap().is_none());
    }

    /// Initializes a throwaway git repo with one commit at `path`, returning
    /// the HEAD commit hash. Used by `warn_if_impact_stale` tests below,
    /// which need a real `RepoSnapshot` (git HEAD + working-tree status)
    /// rather than just a `Layout`.
    fn init_test_repo(path: &std::path::Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("failed to run git")
        };
        run(&["init"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
        fs::write(path.join("file.txt"), "hello").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "initial"]);
    }

    #[test]
    fn test_warn_if_impact_stale_missing_report_returns_none() {
        let tmp = tempdir().unwrap();
        init_test_repo(tmp.path());
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        // No latest-impact.json has been written: ImpactFreshness::Missing,
        // which is not a "silently stale" problem, so no warning.
        assert!(warn_if_impact_stale(&layout, &crate::config::model::Config::default()).is_none());
    }

    #[test]
    fn test_warn_if_impact_stale_warns_when_head_moved() {
        let tmp = tempdir().unwrap();
        init_test_repo(tmp.path());
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        // Write a clean-tree tombstone pinned to a HEAD hash that does not
        // match the repo's real current HEAD, simulating a cache that has
        // gone stale because commits landed after the scan.
        let stale_packet = ImpactPacket {
            head_hash: Some("0000000000000000000000000000000000000000".to_string()),
            tree_clean: true,
            changes: Vec::new(),
            ..ImpactPacket::default()
        };
        write_impact_report(&layout, &stale_packet).unwrap();

        let reason = warn_if_impact_stale(&layout, &crate::config::model::Config::default());
        match reason.expect("expected a staleness warning when HEAD has moved") {
            StaleImpactReason::Stale(detail) => {
                assert!(
                    detail.contains("HEAD"),
                    "stale reason should mention HEAD moving: {detail}"
                );
            }
            StaleImpactReason::Corrupt(detail) => {
                panic!("expected Stale, got Corrupt({detail})")
            }
        }
    }

    #[test]
    fn test_warn_if_impact_stale_silent_when_current() {
        let tmp = tempdir().unwrap();
        init_test_repo(tmp.path());
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let repo = crate::git::repo::open_repo(tmp.path()).unwrap();
        let (head_hash, _branch) = crate::git::repo::get_head_info(&repo).unwrap();

        let current_packet = ImpactPacket {
            head_hash,
            tree_clean: true,
            changes: Vec::new(),
            ..ImpactPacket::default()
        };
        write_impact_report(&layout, &current_packet).unwrap();

        assert!(warn_if_impact_stale(&layout, &crate::config::model::Config::default()).is_none());
    }

    #[test]
    fn test_write_clean_tree_tombstone_creates_valid_tombstone() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        write_clean_tree_tombstone(
            &layout,
            Some("abc123".to_string()),
            Some("main".to_string()),
        )
        .unwrap();

        match read_latest_impact_report(&layout).unwrap().unwrap() {
            LatestImpactReport::CleanTree(tombstone) => {
                assert_eq!(tombstone.head_hash, Some("abc123".to_string()));
                assert_eq!(tombstone.branch_name, Some("main".to_string()));
                assert_eq!(tombstone.status, CleanTreeTombstone::STATUS);
                assert!(tombstone.tree_clean);
                assert!(!tombstone.timestamp_utc.is_empty());
            }
            LatestImpactReport::Packet(_) => {
                panic!("expected clean-tree tombstone, got full packet")
            }
        }
    }

    /// 0147 B4: two CleanTree writes with the same head_hash → second is
    /// Unchanged and on-disk content bytes are identical (no timestamp churn).
    #[test]
    fn test_write_impact_report_stable_clean_tree_same_head() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let packet = ImpactPacket {
            head_hash: Some("same-head-hash".to_string()),
            branch_name: Some("main".to_string()),
            tree_clean: true,
            changes: Vec::new(),
            timestamp_utc: "2026-01-01T00:00:00Z".to_string(),
            ..ImpactPacket::default()
        };

        let first = write_impact_report(&layout, &packet).unwrap();
        assert_eq!(first, ImpactReportWriteOutcome::Written);

        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let bytes_after_first = fs::read(&report_path).unwrap();

        // Different timestamp on packet must not rewrite when head matches.
        let packet2 = ImpactPacket {
            head_hash: Some("same-head-hash".to_string()),
            branch_name: Some("main".to_string()),
            tree_clean: true,
            changes: Vec::new(),
            timestamp_utc: "2026-08-05T12:00:00Z".to_string(),
            ..ImpactPacket::default()
        };
        let second = write_impact_report(&layout, &packet2).unwrap();
        assert_eq!(second, ImpactReportWriteOutcome::Unchanged);

        let bytes_after_second = fs::read(&report_path).unwrap();
        assert_eq!(
            bytes_after_first, bytes_after_second,
            "CleanTree report content must be stable across same-head rewrites"
        );
    }

    /// 0147 B4: head advance still rewrites CleanTree tombstone.
    #[test]
    fn test_write_impact_report_rewrites_when_head_changes() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let first_packet = ImpactPacket {
            head_hash: Some("head-a".to_string()),
            tree_clean: true,
            changes: Vec::new(),
            ..ImpactPacket::default()
        };
        assert_eq!(
            write_impact_report(&layout, &first_packet).unwrap(),
            ImpactReportWriteOutcome::Written
        );

        let second_packet = ImpactPacket {
            head_hash: Some("head-b".to_string()),
            tree_clean: true,
            changes: Vec::new(),
            ..ImpactPacket::default()
        };
        assert_eq!(
            write_impact_report(&layout, &second_packet).unwrap(),
            ImpactReportWriteOutcome::Written
        );

        match read_latest_impact_report(&layout).unwrap().unwrap() {
            LatestImpactReport::CleanTree(t) => {
                assert_eq!(t.head_hash, Some("head-b".to_string()));
            }
            LatestImpactReport::Packet(_) => panic!("expected CleanTree tombstone"),
        }
    }

    #[test]
    fn test_atomic_write_does_not_leave_tmp() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let packet = ImpactPacket::default();

        write_impact_report(&layout, &packet).unwrap();

        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let tmp_path = report_path.with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            "atomic write should not leave a temporary file behind"
        );
    }

    #[test]
    fn test_write_clean_tree_tombstone_atomic() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        write_clean_tree_tombstone(&layout, Some("abc123".to_string()), None).unwrap();

        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let tmp_path = report_path.with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            "atomic tombstone write should not leave a temporary file behind"
        );
    }

    /// 0174 T8/T9: RO soft-skip does not rewrite seeded latest-impact.json.
    #[test]
    fn soft_write_impact_report_skips_when_storage_ro() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let seed = ImpactPacket {
            head_hash: Some("SEED_MARKER_0174_RO".to_string()),
            risk_reasons: vec!["seed-do-not-clobber".to_string()],
            ..ImpactPacket::default()
        };
        write_impact_report(&layout, &seed).unwrap();
        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let before = std::fs::read_to_string(report_path.as_std_path()).unwrap();
        assert!(before.contains("SEED_MARKER_0174_RO"));

        let dirty = ImpactPacket {
            head_hash: Some("would-overwrite".to_string()),
            tree_clean: false,
            risk_reasons: vec!["new".to_string()],
            ..ImpactPacket::default()
        };
        let outcome = soft_write_impact_report(&layout, &dirty, true).unwrap();
        assert_eq!(outcome, ImpactReportWriteOutcome::Skipped);

        let after = std::fs::read_to_string(report_path.as_std_path()).unwrap();
        assert_eq!(
            before, after,
            "RO soft-skip must not rewrite latest-impact.json"
        );
        assert!(IMPACT_REPORT_RO_HONESTY.contains("report write unavailable under RO"));
    }

    /// 0174 T10: writable soft-write still writes when content changes.
    #[test]
    fn soft_write_impact_report_writes_when_writable() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let packet = ImpactPacket {
            head_hash: Some("writable-head".to_string()),
            tree_clean: true,
            changes: Vec::new(),
            ..ImpactPacket::default()
        };
        let outcome = soft_write_impact_report(&layout, &packet, false).unwrap();
        assert_eq!(outcome, ImpactReportWriteOutcome::Written);

        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let body = std::fs::read_to_string(report_path.as_std_path()).unwrap();
        assert!(body.contains("writable-head"));
    }

    #[test]
    fn is_report_ro_class_error_matches_permission_strings() {
        let err = miette::miette!("Failed to write report: /x: Permission denied (os error 5)");
        assert!(is_report_ro_class_error(&err));
        let other = miette::miette!("disk full somehow unrelated");
        // "disk i/o error" is RO-class; plain "disk full" is not necessarily
        assert!(!is_report_ro_class_error(&other) || format!("{other}").contains("disk i/o"));
        let read_only = miette::miette!("read-only file system");
        assert!(is_report_ro_class_error(&read_only));
    }

    /// 0174 T12: soft_write_scan_report returns false on RO-class (simulated via
    /// storage_is_read_only-style skip is impact-only; here we assert writable success
    /// and RO classifier used by soft path).
    #[test]
    fn soft_write_scan_report_writable_succeeds() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let report = ScanReport {
            head_hash: Some("scan-head".to_string()),
            branch_name: Some("main".to_string()),
            is_clean: true,
            changes: Vec::new(),
            diff_summaries: Vec::new(),
        };
        assert!(soft_write_scan_report(&layout, &report).unwrap());
        let path = layout.reports_dir().join(LATEST_SCAN_REPORT);
        assert!(path.exists());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImpactFreshness {
    Missing,
    CurrentClean,
    CurrentDirty,
    Stale { reason: String },
    Corrupt { reason: String },
}

pub fn check_impact_freshness(layout: &Layout, snapshot: &RepoSnapshot) -> ImpactFreshness {
    let report_opt = match read_latest_impact_report(layout) {
        Ok(Some(r)) => r,
        Ok(None) => return ImpactFreshness::Missing,
        Err(e) => {
            return ImpactFreshness::Corrupt {
                reason: e.to_string(),
            };
        }
    };

    match report_opt {
        LatestImpactReport::CleanTree(tombstone) => {
            if tombstone.head_hash != snapshot.head_hash {
                return ImpactFreshness::Stale {
                    reason: "HEAD has changed since clean impact scan".to_string(),
                };
            }
            if !snapshot.is_clean {
                return ImpactFreshness::Stale {
                    reason: "Working tree is dirty but impact scan was clean".to_string(),
                };
            }
            ImpactFreshness::CurrentClean
        }
        LatestImpactReport::Packet(packet) => {
            if packet.head_hash != snapshot.head_hash {
                return ImpactFreshness::Stale {
                    reason: "HEAD has changed since impact scan".to_string(),
                };
            }
            if snapshot.is_clean {
                return ImpactFreshness::Stale {
                    reason: "Working tree is clean but impact scan has dirty changes".to_string(),
                };
            }
            ImpactFreshness::CurrentDirty
        }
    }
}

/// Builds a `RepoSnapshot` for the repo at `layout.root`, mirroring the exact
/// construction `doctor.rs`'s "Impact Report Freshness" check uses (open
/// repo, read HEAD, get working-tree status, filter via
/// `config.watch.ignore_patterns`). Returns `None` if the directory is not a
/// git repo or HEAD cannot be resolved (e.g. an unborn branch) — in that case
/// freshness cannot be evaluated and callers should treat it the same as
/// "nothing to warn about" rather than failing the calling command.
fn build_repo_snapshot(layout: &Layout, ignore_patterns: &[String]) -> Option<RepoSnapshot> {
    let repo = crate::git::repo::open_repo(layout.root.as_std_path()).ok()?;
    let (head_hash, branch_name) = crate::git::repo::get_head_info(&repo).ok()?;
    let changes = crate::git::status::get_repo_status(&repo).unwrap_or_default();
    let filtered =
        crate::git::ignore::filter_ignored_changes(changes, ignore_patterns, true).ok()?;

    Some(RepoSnapshot {
        head_hash,
        branch_name,
        is_clean: filtered.is_empty(),
        changes: filtered,
    })
}

/// Short, context-agnostic description of why a cached impact packet should
/// not be trusted as current. Callers append their own consequence clause
/// (e.g. "...used as ask context anyway", "...export reflects this") so the
/// same helper reads naturally across `ask`, `verify`, and `federate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleImpactReason {
    Stale(String),
    Corrupt(String),
}

impl std::fmt::Display for StaleImpactReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StaleImpactReason::Stale(reason) => {
                write!(f, "cached impact report is stale ({reason})")
            }
            StaleImpactReason::Corrupt(reason) => {
                write!(f, "cached impact report is corrupt ({reason})")
            }
        }
    }
}

/// CG-F35 (requirement #1, #6): shared freshness-warning helper for cached
/// `latest-impact.json` consumers outside `doctor` (currently `ask`,
/// `verify`, and `federate scan`). `doctor` keeps its own inline check
/// because it renders every state (`Missing`/`Current*`/`Stale`/`Corrupt`)
/// as part of a full health report; these lighter-weight consumers only need
/// to know when they are about to silently treat a stale or corrupt cache as
/// authoritative, so this only returns `Some(reason)` for those two cases.
///
/// Returns `None` when the cache is missing, current, or when freshness
/// can't be evaluated at all (e.g. not a git repo) — none of those are a
/// "silently implying freshness when stale" problem on their own, and the
/// caller's existing `Missing`/empty-packet handling already covers them.
pub fn warn_if_impact_stale(
    layout: &Layout,
    config: &crate::config::model::Config,
) -> Option<StaleImpactReason> {
    let snapshot = build_repo_snapshot(layout, &config.watch.ignore_patterns)?;
    match check_impact_freshness(layout, &snapshot) {
        ImpactFreshness::Stale { reason } => Some(StaleImpactReason::Stale(reason)),
        ImpactFreshness::Corrupt { reason } => Some(StaleImpactReason::Corrupt(reason)),
        ImpactFreshness::Missing
        | ImpactFreshness::CurrentClean
        | ImpactFreshness::CurrentDirty => None,
    }
}
