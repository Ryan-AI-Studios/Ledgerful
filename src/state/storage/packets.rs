use crate::impact::packet::{ChangedFile, ImpactPacket};
use crate::index::storage::persist_symbols;
use crate::state::storage::connection::StorageManager;
use miette::{IntoDiagnostic, Result};

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
            let packet: ImpactPacket = serde_json::from_str(&json).into_diagnostic()?;
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
        for packet in rows {
            packets.push(packet.into_diagnostic()?);
        }
        Ok(packets)
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
}
