//! Durable analysis baselines (Track 0601).
//!
//! One table, one insert path. The module deliberately has no `update_*` or
//! `delete_*` function: rows are refuse-overwrite by name and the m57 triggers
//! abort direct `UPDATE`/`DELETE`. A plain `INSERT` is the only write; a
//! primary-key violation maps to [`BaselineError::Exists`]. Replace-style
//! writes can bypass SQLite delete triggers and are forbidden here.

use super::StorageManager;
use miette::{IntoDiagnostic, Result};
use rusqlite::{Transaction, TransactionBehavior, ffi};

/// Longest accepted baseline name (the regex allows 1 + up to 63).
const NAME_MAX_LEN: usize = 64;

/// Stored baseline row as written by `baseline create`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaselineRow {
    pub name: String,
    pub baseline_schema: u32,
    pub payload_kind: String,
    pub identity_json: String,
    pub payload_json: String,
    pub payload_digest: String,
    pub created_at: String,
}

/// Typed baseline storage failures.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub(crate) enum BaselineError {
    #[error("baseline `{name}` already exists; baselines are immutable (BASELINE_EXISTS)")]
    #[diagnostic(code(ledgerful::baseline::exists))]
    Exists { name: String },

    #[error("baseline `{name}` not found (BASELINE_NOT_FOUND)")]
    #[diagnostic(code(ledgerful::baseline::not_found))]
    NotFound { name: String },

    #[error(
        "invalid baseline name `{name}`: use 1-64 characters, lowercase letters/digits first, then a-z 0-9 . _ - (BASELINE_NAME_INVALID)"
    )]
    #[diagnostic(code(ledgerful::baseline::name_invalid))]
    InvalidName { name: String },

    #[error("baseline `{name}` has a payload digest mismatch (BASELINE_DIGEST_MISMATCH)")]
    #[diagnostic(code(ledgerful::baseline::digest_mismatch))]
    DigestMismatch { name: String },

    #[error("baseline `{name}` has an unsupported row schema or payload kind (SCHEMA_MISMATCH)")]
    #[diagnostic(code(ledgerful::baseline::row_schema_mismatch))]
    RowSchemaMismatch { name: String },

    #[error("baseline `{name}` has malformed stored fields: {message} (BASELINE_CORRUPT)")]
    #[diagnostic(code(ledgerful::baseline::corrupt))]
    Corrupt { name: String, message: String },

    #[error(
        "baseline database schema {found} exceeds supported schema {supported} (BASELINE_SCHEMA_TOO_NEW)"
    )]
    #[diagnostic(code(ledgerful::baseline::schema_too_new))]
    SchemaTooNew { found: i64, supported: i64 },

    #[error("baseline storage error: {source}")]
    #[diagnostic(code(ledgerful::baseline::sqlite))]
    Sqlite {
        #[from]
        source: rusqlite::Error,
    },

    #[error("baseline storage error: {message}")]
    #[diagnostic(code(ledgerful::baseline::storage))]
    Storage { message: String },
}

/// `^[a-z0-9][a-z0-9._-]{0,63}$`, checked before any storage access.
///
/// Lowercase-only so Windows case-insensitivity cannot create near-duplicates.
pub(crate) fn validate_baseline_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > NAME_MAX_LEN {
        return Err(BaselineError::InvalidName {
            name: name.to_string(),
        }
        .into());
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(BaselineError::InvalidName {
            name: name.to_string(),
        }
        .into());
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(BaselineError::InvalidName {
            name: name.to_string(),
        }
        .into());
    }
    if chars
        .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')))
    {
        return Err(BaselineError::InvalidName {
            name: name.to_string(),
        }
        .into());
    }
    Ok(())
}

/// `"blake3:" + hex(blake3(identity_json || 0x00 || payload_json))`.
///
/// Both inputs are the exact canonical (compact) strings that are stored, so
/// read-side recomputation is stable.
pub(crate) fn compute_payload_digest(identity_json: &str, payload_json: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(identity_json.as_bytes());
    hasher.update(&[0u8]);
    hasher.update(payload_json.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Whether the stored digest still matches the stored canonical bytes.
pub(crate) fn payload_digest_verified(row: &BaselineRow) -> bool {
    compute_payload_digest(&row.identity_json, &row.payload_json) == row.payload_digest
}

/// Insert one baseline. The only write path in this module.
///
/// The name is re-validated here so the store-level invariant does not depend
/// on a caller remembering to check it.
pub(crate) fn insert_baseline(storage: &StorageManager, row: &BaselineRow) -> Result<()> {
    validate_baseline_name(&row.name)?;
    let sql = "INSERT INTO analysis_baselines (
            name, baseline_schema, payload_kind, identity_json, payload_json, payload_digest, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    let transaction =
        Transaction::new_unchecked(storage.get_connection(), TransactionBehavior::Immediate)
            .map_err(BaselineError::from)?;
    let result = transaction.execute(
        sql,
        rusqlite::params![
            row.name,
            row.baseline_schema,
            row.payload_kind,
            row.identity_json,
            row.payload_json,
            row.payload_digest,
            row.created_at,
        ],
    );
    match result {
        Ok(1) => Ok(transaction.commit().map_err(BaselineError::from)?),
        Ok(other) => Err(BaselineError::Storage {
            message: format!("insert wrote {other} rows (expected 1)"),
        }
        .into()),
        Err(e) if is_primary_key_violation(&e) => Err(BaselineError::Exists {
            name: row.name.clone(),
        }
        .into()),
        Err(e) => Err(BaselineError::from(e).into()),
    }
}

fn is_primary_key_violation(err: &rusqlite::Error) -> bool {
    matches!(err, rusqlite::Error::SqliteFailure(code, _)
        if code.extended_code == ffi::SQLITE_CONSTRAINT_PRIMARYKEY)
}

/// Load one baseline by exact name.
pub(crate) fn get_baseline(storage: &StorageManager, name: &str) -> Result<BaselineRow> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT name, baseline_schema, payload_kind, identity_json, payload_json,
                    payload_digest, created_at
             FROM analysis_baselines WHERE name = ?1",
        )
        .into_diagnostic()?;
    let mut rows = stmt.query([name]).into_diagnostic()?;
    match rows.next().into_diagnostic()? {
        Some(row) => decode_sql_row(row),
        None => Err(BaselineError::NotFound {
            name: name.to_string(),
        }
        .into()),
    }
}

/// List baselines ordered by `created_at`, then `name`.
pub(crate) fn list_baselines(storage: &StorageManager) -> Result<Vec<BaselineRow>> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT name, baseline_schema, payload_kind, identity_json, payload_json,
                    payload_digest, created_at
             FROM analysis_baselines ORDER BY created_at, name",
        )
        .into_diagnostic()?;
    let mut rows = stmt.query([]).into_diagnostic()?;
    let mut decoded = Vec::new();
    while let Some(row) = rows.next().into_diagnostic()? {
        decoded.push(decode_sql_row(row)?);
    }
    Ok(decoded)
}

fn decode_sql_row(row: &rusqlite::Row<'_>) -> Result<BaselineRow> {
    let name = row
        .get::<_, String>(0)
        .map_err(|error| BaselineError::Corrupt {
            name: "<unreadable name>".to_string(),
            message: error.to_string(),
        })?;
    row_from_sql(row, name.clone()).map_err(|error| {
        BaselineError::Corrupt {
            name,
            message: error.to_string(),
        }
        .into()
    })
}

fn row_from_sql(row: &rusqlite::Row<'_>, name: String) -> rusqlite::Result<BaselineRow> {
    Ok(BaselineRow {
        name,
        baseline_schema: row.get(1)?,
        payload_kind: row.get(2)?,
        identity_json: row.get(3)?,
        payload_json: row.get(4)?,
        payload_digest: row.get(5)?,
        created_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn sample_row(name: &str) -> BaselineRow {
        let identity_json =
            r#"{"baselineSchema":1,"payloadKind":"fileDependencyEdges"}"#.to_string();
        let payload_json = r#"{"coverage":{},"edges":[]}"#.to_string();
        let payload_digest = compute_payload_digest(&identity_json, &payload_json);
        BaselineRow {
            name: name.to_string(),
            baseline_schema: 1,
            payload_kind: "fileDependencyEdges".to_string(),
            identity_json,
            payload_json,
            payload_digest,
            created_at: "2026-11-21T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn baseline_store_round_trips_identity_payload_and_digest() {
        let storage = in_memory_storage();
        let row = sample_row("base1");
        insert_baseline(&storage, &row).unwrap();
        let loaded = get_baseline(&storage, "base1").unwrap();
        assert_eq!(loaded, row);
        assert!(payload_digest_verified(&loaded));
    }

    #[test]
    fn baseline_store_duplicate_name_is_refused_and_row_unchanged() {
        let storage = in_memory_storage();
        let row = sample_row("base1");
        insert_baseline(&storage, &row).unwrap();
        let mut second = sample_row("base1");
        second.payload_json =
            r#"{"coverage":{},"edges":[{"srcPath":"x","dstPath":"y","weight":1}]}"#.to_string();
        second.payload_digest = compute_payload_digest(&second.identity_json, &second.payload_json);
        let err = insert_baseline(&storage, &second).unwrap_err();
        assert_eq!(
            err.downcast_ref::<BaselineError>()
                .map(|e| matches!(e, BaselineError::Exists { .. })),
            Some(true),
            "unexpected error: {err}"
        );
        assert_eq!(get_baseline(&storage, "base1").unwrap(), row);
    }

    #[test]
    fn baseline_store_direct_update_and_delete_are_aborted() {
        let storage = in_memory_storage();
        insert_baseline(&storage, &sample_row("base1")).unwrap();
        let conn = storage.get_connection();
        let update = conn.execute(
            "UPDATE analysis_baselines SET payload_json = '{}' WHERE name = 'base1'",
            [],
        );
        let update_err = update.expect_err("direct UPDATE must abort");
        assert!(
            update_err
                .to_string()
                .contains("analysis_baselines rows are immutable"),
            "UPDATE must surface the trigger message: {update_err}"
        );
        let delete = conn.execute("DELETE FROM analysis_baselines WHERE name = 'base1'", []);
        let delete_err = delete.expect_err("direct DELETE must abort");
        assert!(
            delete_err
                .to_string()
                .contains("analysis_baselines rows are immutable"),
            "DELETE must surface the trigger message: {delete_err}"
        );
        assert!(get_baseline(&storage, "base1").is_ok());
    }

    #[test]
    fn baseline_store_different_name_with_identical_payload_is_allowed() {
        let storage = in_memory_storage();
        insert_baseline(&storage, &sample_row("base1")).unwrap();
        let mut twin = sample_row("base2");
        twin.created_at = "2026-11-21T00:00:01Z".to_string();
        insert_baseline(&storage, &twin).unwrap();
        let names: Vec<_> = list_baselines(&storage)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["base1".to_string(), "base2".to_string()]);
    }

    #[test]
    fn baseline_store_lists_created_at_before_name() {
        let storage = in_memory_storage();
        // `zzz` was created first, so created_at must win over name ordering.
        let mut first = sample_row("zzz");
        first.created_at = "2026-11-21T00:00:00Z".to_string();
        insert_baseline(&storage, &first).unwrap();
        let mut second = sample_row("aaa");
        second.created_at = "2026-11-21T00:00:01Z".to_string();
        insert_baseline(&storage, &second).unwrap();
        let names: Vec<_> = list_baselines(&storage)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["zzz".to_string(), "aaa".to_string()]);
    }

    #[test]
    fn baseline_store_invalid_names_are_refused_before_any_write() {
        let long = "x".repeat(65);
        for bad in ["", "Upper", "_lead", "has space", "a/b", long.as_str()] {
            assert!(
                validate_baseline_name(bad).is_err(),
                "expected `{bad}` to be refused"
            );
            let storage = in_memory_storage();
            let mut row = sample_row("ok");
            row.name = bad.to_string();
            let err = insert_baseline(&storage, &row)
                .expect_err("insert must refuse an invalid name before any SQL");
            assert!(
                err.downcast_ref::<BaselineError>()
                    .is_some_and(|e| matches!(e, BaselineError::InvalidName { .. })),
                "unexpected error for `{bad}`: {err}"
            );
            assert!(
                list_baselines(&storage).unwrap().is_empty(),
                "no row may be written for `{bad}`"
            );
        }
        let longest_ok = "y".repeat(64);
        for good in ["a", "base-1", "v1.2_x", longest_ok.as_str()] {
            assert!(
                validate_baseline_name(good).is_ok(),
                "expected `{good}` to be accepted"
            );
        }
    }

    #[test]
    fn baseline_store_out_of_band_payload_edit_fails_digest_verification() {
        let storage = in_memory_storage();
        insert_baseline(&storage, &sample_row("base1")).unwrap();
        let conn = storage.get_connection();
        conn.execute_batch("DROP TRIGGER analysis_baselines_no_update;")
            .unwrap();
        conn.execute(
            "UPDATE analysis_baselines SET payload_json = '{\"coverage\":{},\"edges\":[{\"srcPath\":\"a\",\"dstPath\":\"b\",\"weight\":9}]}' WHERE name = 'base1'",
            [],
        )
        .unwrap();
        let loaded = get_baseline(&storage, "base1").unwrap();
        assert!(
            !payload_digest_verified(&loaded),
            "an out-of-band payload edit must fail digest verification"
        );
    }

    #[test]
    fn baseline_store_concurrent_writer_and_duplicate_preserve_winner() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ledger.db");
        let mut first = Connection::open(&db).unwrap();
        let mode: String = first
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        get_migrations().to_latest(&mut first).unwrap();
        let second = Connection::open(db).unwrap();
        second.busy_timeout(std::time::Duration::ZERO).unwrap();
        let first = StorageManager::init_from_conn(first);
        let second = StorageManager::init_from_conn(second);

        let winner = sample_row("same");
        let mut contender = winner.clone();
        contender.payload_json = "different bytes".into();
        let lock =
            Transaction::new_unchecked(first.get_connection(), TransactionBehavior::Immediate)
                .unwrap();
        let busy = insert_baseline(&second, &contender).unwrap_err();
        assert!(matches!(busy.downcast_ref::<BaselineError>(),
            Some(BaselineError::Sqlite { source: rusqlite::Error::SqliteFailure(code, _) })
            if code.code == rusqlite::ErrorCode::DatabaseBusy));
        lock.rollback().unwrap();
        insert_baseline(&first, &winner).unwrap();
        let duplicate = insert_baseline(&second, &contender).unwrap_err();
        assert!(matches!(duplicate.downcast_ref::<BaselineError>(),
            Some(BaselineError::Exists { name }) if name == "same"));
        assert_eq!(get_baseline(&second, "same").unwrap(), winner);
    }

    #[test]
    fn baseline_store_malformed_sql_fields_identify_row() {
        let storage = in_memory_storage();
        insert_baseline(&storage, &sample_row("bad-schema")).unwrap();
        storage
            .get_connection()
            .execute_batch(
                "DROP TRIGGER analysis_baselines_no_update;
             UPDATE analysis_baselines SET baseline_schema = 'bad' WHERE name = 'bad-schema';",
            )
            .unwrap();
        for error in [
            get_baseline(&storage, "bad-schema").unwrap_err(),
            list_baselines(&storage).unwrap_err(),
        ] {
            assert!(matches!(error.downcast_ref::<BaselineError>(),
                Some(BaselineError::Corrupt { name, .. }) if name == "bad-schema"));
        }
    }

    #[test]
    fn baseline_store_not_found_is_typed() {
        let storage = in_memory_storage();
        let err = get_baseline(&storage, "missing").unwrap_err();
        assert_eq!(
            err.downcast_ref::<BaselineError>()
                .map(|e| matches!(e, BaselineError::NotFound { .. })),
            Some(true),
            "unexpected error: {err}"
        );
    }
}
