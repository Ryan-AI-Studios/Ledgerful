use crate::sync::hlc::HLC;
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct SyncState {
    pub last_extract_hlc: Option<HLC>,
    pub last_apply_hlc: Option<HLC>,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub device_id: String,
}

impl SyncState {
    pub fn load(conn: &Connection) -> Result<Option<Self>> {
        let mut stmt = conn.prepare(
            "SELECT last_extract_hlc, last_apply_hlc, last_run_at, device_id FROM sync_state WHERE id = 1"
        ).into_diagnostic()?;

        let mut rows = stmt.query([]).into_diagnostic()?;

        if let Some(row) = rows.next().into_diagnostic()? {
            let last_extract_str: Option<String> = row.get(0).into_diagnostic()?;
            let last_apply_str: Option<String> = row.get(1).into_diagnostic()?;
            let last_run_at_str: Option<String> = row.get(2).into_diagnostic()?;
            let device_id: String = row.get(3).into_diagnostic()?;

            let last_extract_hlc = last_extract_str.and_then(|s| HLC::from_str(&s).ok());
            let last_apply_hlc = last_apply_str.and_then(|s| HLC::from_str(&s).ok());
            let last_run_at = last_run_at_str
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));

            Ok(Some(SyncState {
                last_extract_hlc,
                last_apply_hlc,
                last_run_at,
                device_id,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn save(&self, conn: &Connection) -> Result<()> {
        let last_extract_hlc = self.last_extract_hlc.as_ref().map(|h| h.to_string());
        let last_apply_hlc = self.last_apply_hlc.as_ref().map(|h| h.to_string());
        let last_run_at = self.last_run_at.as_ref().map(|dt| dt.to_rfc3339());

        conn.execute(
            "INSERT INTO sync_state (id, last_extract_hlc, last_apply_hlc, last_run_at, device_id) 
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET 
                last_extract_hlc = excluded.last_extract_hlc,
                last_apply_hlc = excluded.last_apply_hlc,
                last_run_at = excluded.last_run_at,
                device_id = excluded.device_id",
            (
                last_extract_hlc,
                last_apply_hlc,
                last_run_at,
                &self.device_id,
            ),
        )
        .into_diagnostic()?;

        Ok(())
    }
}
