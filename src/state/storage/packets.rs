//! Snapshot packet persistence.
//!
//! Verify predict hydrates at most [`PREDICTOR_SNAPSHOT_HISTORY_CAP`] recent
//! snapshots via [`StorageManager::get_recent_packets`]. [`StorageManager::get_all_packets`]
//! remains unbounded for debug/admin.

use crate::impact::packet::{ChangedFile, ImpactPacket};
use crate::index::storage::persist_symbols;
use crate::state::storage::connection::StorageManager;
use miette::{IntoDiagnostic, Result};

/// Maximum snapshot rows the verify predictor deserializes for historical scoring.
///
/// Tests must exercise this production value (65-row loops). Do not override
/// with `#[cfg(test)]`.
pub const PREDICTOR_SNAPSHOT_HISTORY_CAP: usize = 64;

/// Capped snapshot history for verify predict.
///
/// `packets` is oldest→newest after reverse. `truncated` is true when more than
/// `limit` snapshot rows exist. `total_count` is `COUNT(*)` only when truncated;
/// otherwise it equals `packets.len()` with no extra query.
#[derive(Debug, Clone)]
pub struct PacketHistory {
    pub packets: Vec<ImpactPacket>,
    pub truncated: bool,
    pub total_count: usize,
}

impl StorageManager {
    pub fn save_packet(&self, packet: &ImpactPacket) -> Result<i64> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        let packet_json = serde_json::to_string(packet).into_diagnostic()?;
        let is_clean = if packet.changes.is_empty() { 1 } else { 0 };

        self.conn
            .execute(
                "INSERT INTO snapshots (timestamp, head_hash, branch_name, is_clean, packet_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    &packet.timestamp_utc,
                    &packet.head_hash,
                    &packet.branch_name,
                    is_clean,
                    &packet_json,
                ),
            )
            .into_diagnostic()?;

        let snapshot_id = self.conn.last_insert_rowid();
        self.save_changed_files(snapshot_id, &packet.changes)?;
        persist_symbols(&self.conn, snapshot_id, &packet.changes)?;

        Ok(snapshot_id)
    }

    pub fn get_latest_packet(&self) -> Result<Option<ImpactPacket>> {
        let mut stmt = self
            .conn
            .prepare("SELECT packet_json FROM snapshots ORDER BY id DESC LIMIT 1")
            .into_diagnostic()?;

        let mut rows = stmt.query([]).into_diagnostic()?;

        if let Some(row) = rows.next().into_diagnostic()? {
            let json: String = row.get(0).into_diagnostic()?;
            let mut packet: ImpactPacket = serde_json::from_str(&json).into_diagnostic()?;
            // Pre-0117 snapshots omit confidenceClass/confidenceSummary; recompute.
            if let Some(ref mut blast) = packet.blast_radius {
                blast.hydrate_confidence();
            }
            Ok(Some(packet))
        } else {
            Ok(None)
        }
    }

    pub fn get_all_packets(&self) -> Result<Vec<ImpactPacket>> {
        let mut stmt = self
            .conn
            .prepare("SELECT packet_json FROM snapshots ORDER BY id ASC")
            .into_diagnostic()?;

        let rows = stmt
            .query_map([], |row| {
                let json: String = row.get(0)?;
                serde_json::from_str(&json).map_err(|_e| rusqlite::Error::InvalidQuery)
            })
            .into_diagnostic()?;

        let mut packets = Vec::new();
        for row in rows {
            let mut packet: ImpactPacket = row.into_diagnostic()?;
            if let Some(ref mut blast) = packet.blast_radius {
                blast.hydrate_confidence();
            }
            packets.push(packet);
        }
        Ok(packets)
    }

    /// Load up to `limit` most recent snapshot packets (oldest→newest).
    ///
    /// Fetches `LIMIT limit+1` to detect truncation without a window function.
    /// `COUNT(*)` runs only when truncated (for `{M}` in the predict warning).
    ///
    /// `ORDER BY id DESC` is insertion/rowid chronological (newest first in the
    /// query). Do not rewrite this to timestamp. Reverse after hydrate so callers
    /// see oldest-first.
    pub fn get_recent_packets(&self, limit: usize) -> Result<PacketHistory> {
        let fetch_limit = limit.saturating_add(1) as i64;
        let mut stmt = self
            .conn
            .prepare("SELECT packet_json FROM snapshots ORDER BY id DESC LIMIT ?1")
            .into_diagnostic()?;

        let rows = stmt
            .query_map([fetch_limit], |row| {
                let json: String = row.get(0)?;
                serde_json::from_str(&json).map_err(|_e| rusqlite::Error::InvalidQuery)
            })
            .into_diagnostic()?;

        let mut packets = Vec::new();
        for row in rows {
            let packet: ImpactPacket = row.into_diagnostic()?;
            packets.push(packet);
        }

        let truncated = packets.len() > limit;
        if truncated {
            packets.truncate(limit);
        }

        let total_count = if truncated {
            let count: i64 = self
                .conn
                .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
                .into_diagnostic()?;
            count as usize
        } else {
            packets.len()
        };

        for packet in &mut packets {
            if let Some(ref mut blast) = packet.blast_radius {
                blast.hydrate_confidence();
            }
        }

        packets.reverse();

        Ok(PacketHistory {
            packets,
            truncated,
            total_count,
        })
    }

    pub fn save_batch(&self, timestamp: &str, event_count: u32, batch_json: &str) -> Result<i64> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        self.conn
            .execute(
                "INSERT INTO batches (timestamp, event_count, batch_json) VALUES (?1, ?2, ?3)",
                (timestamp, event_count, batch_json),
            )
            .into_diagnostic()?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn save_changed_files(&self, snapshot_id: i64, files: &[ChangedFile]) -> Result<()> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        for file in files {
            self.conn
                .execute(
                    "INSERT INTO changed_files (snapshot_id, path, status, is_staged) VALUES (?1, ?2, ?3, ?4)",
                    (snapshot_id, file.path.to_string_lossy().as_ref(), &file.status, file.is_staged as i32),
                )
                .into_diagnostic()?;
        }
        Ok(())
    }

    pub fn update_changed_files_stats(
        &self,
        snapshot_id: i64,
        stats: &std::collections::HashMap<String, crate::git::numstat::FileNumstat>,
    ) -> Result<()> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        let mut stmt = self
            .conn
            .prepare(
                "UPDATE changed_files
                 SET additions = ?1, deletions = ?2, is_binary = ?3
                 WHERE snapshot_id = ?4 AND path = ?5",
            )
            .into_diagnostic()?;
        for (path, numstat) in stats {
            let adds: Option<i64> = numstat.additions.map(|v| v as i64);
            let dels: Option<i64> = numstat.deletions.map(|v| v as i64);
            let is_binary = (adds.is_none() && dels.is_none()) as i64;
            stmt.execute(rusqlite::params![adds, dels, is_binary, snapshot_id, path])
                .into_diagnostic()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::enrichment::edge_confidence::ConfidenceClass;
    use crate::impact::packet::{BlastEdge, BlastRadius};
    use crate::state::layout::Layout;
    use crate::state::storage::connection::in_memory_storage;

    #[test]
    fn test_storage_basic_ops() {
        let storage = in_memory_storage();

        let packet = ImpactPacket {
            head_hash: Some("test_hash".to_string()),
            ..Default::default()
        };

        storage.save_packet(&packet).unwrap();

        let latest = storage.get_latest_packet().unwrap().unwrap();
        assert_eq!(latest.head_hash, Some("test_hash".to_string()));
    }

    #[test]
    fn test_save_batch() {
        let storage = in_memory_storage();
        let id = storage
            .save_batch("2026-01-01T00:00:00Z", 3, r#"{"events":[]}"#)
            .unwrap();
        assert!(id > 0);
    }

    #[test]
    fn get_recent_packets_sixty_five_snapshots_returns_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let storage = StorageManager::init_with_layout(&layout).unwrap();

        for i in 0..65 {
            storage
                .save_packet(&ImpactPacket {
                    head_hash: Some(format!("h{i}")),
                    ..Default::default()
                })
                .unwrap();
        }

        let history = storage
            .get_recent_packets(PREDICTOR_SNAPSHOT_HISTORY_CAP)
            .unwrap();
        assert_eq!(history.packets.len(), 64);
        assert!(history.truncated);
        assert_eq!(history.total_count, 65);
        assert_eq!(history.packets[0].head_hash.as_deref(), Some("h1"));
        assert_eq!(history.packets[63].head_hash.as_deref(), Some("h64"));
        assert_eq!(storage.get_all_packets().unwrap().len(), 65);
    }

    #[test]
    fn get_recent_packets_two_snapshots_not_truncated_oldest_first() {
        let storage = in_memory_storage();
        storage
            .save_packet(&ImpactPacket {
                head_hash: Some("h0".to_string()),
                ..Default::default()
            })
            .unwrap();
        storage
            .save_packet(&ImpactPacket {
                head_hash: Some("h1".to_string()),
                ..Default::default()
            })
            .unwrap();

        let history = storage
            .get_recent_packets(PREDICTOR_SNAPSHOT_HISTORY_CAP)
            .unwrap();
        assert_eq!(history.packets.len(), 2);
        assert!(!history.truncated);
        assert_eq!(history.total_count, 2);
        assert_eq!(history.packets[0].head_hash.as_deref(), Some("h0"));
        assert_eq!(history.packets[1].head_hash.as_deref(), Some("h1"));
    }

    #[test]
    fn get_recent_packets_oldest_blast_packet_has_confidence_class() {
        let storage = in_memory_storage();
        let blast = BlastRadius {
            depth_requested: 1,
            depth_applied: 1,
            edges: vec![BlastEdge {
                hop: 1,
                direction: "caller".to_string(),
                from_symbol: "caller_fn".to_string(),
                from_file: "src/a.rs".to_string(),
                to_symbol: "seed_fn".to_string(),
                to_file: "src/b.rs".to_string(),
                resolution_status: "RESOLVED".to_string(),
                evidence: "scip:ref".to_string(),
                confidence: None,
                expandable: true,
                confidence_class: ConfidenceClass::Unknown,
            }],
            ..Default::default()
        };
        storage
            .save_packet(&ImpactPacket {
                head_hash: Some("h0".to_string()),
                blast_radius: Some(blast),
                ..Default::default()
            })
            .unwrap();
        storage
            .save_packet(&ImpactPacket {
                head_hash: Some("h1".to_string()),
                ..Default::default()
            })
            .unwrap();

        let history = storage
            .get_recent_packets(PREDICTOR_SNAPSHOT_HISTORY_CAP)
            .unwrap();
        let oldest = &history.packets[0];
        assert_eq!(oldest.head_hash.as_deref(), Some("h0"));
        let class = oldest
            .blast_radius
            .as_ref()
            .expect("blast present on oldest")
            .edges[0]
            .confidence_class;
        assert_eq!(class, ConfidenceClass::ScipBound);
        assert_eq!(class.as_str(), "SCIP_BOUND");
    }
}
