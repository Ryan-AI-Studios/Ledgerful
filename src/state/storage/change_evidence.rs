//! Internal, scope-qualified storage seams for change evidence.
//!
//! Callers must supply an already qualified canonical repository guard and a
//! verified local database path. This module does not qualify filesystem paths,
//! run migrations, decode payloads or publish response data.
#![allow(
    dead_code,
    reason = "The private storage seam precedes producer and route wiring"
)]
use crate::state::migrations::get_migrations_count;
use rusqlite::{Connection, Error, ErrorCode, OpenFlags, TransactionBehavior, types::ValueRef};
use std::{path::Path, sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EvidenceStorageError {
    Uninitialized,
    SchemaIncompatible,
    ScopeUnavailable,
    StorageBusy,
    Corrupt,
    StorageUnavailable,
    PayloadOverBudget,
    InvalidInput,
    ForeignScope,
}
type Result<T> = std::result::Result<T, EvidenceStorageError>;

pub(crate) fn sql_error(error: Error) -> EvidenceStorageError {
    match error {
        Error::SqliteFailure(code, _) => match code.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => {
                EvidenceStorageError::StorageBusy
            }
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase | ErrorCode::Unknown => {
                EvidenceStorageError::Corrupt
            }
            _ => EvidenceStorageError::StorageUnavailable,
        },
        Error::InvalidColumnType(..) | Error::IntegralValueOutOfRange(..) => {
            EvidenceStorageError::Corrupt
        }
        _ => EvidenceStorageError::StorageUnavailable,
    }
}

fn integer(row: &rusqlite::Row<'_>, i: usize) -> Result<i64> {
    match row.get_ref(i).map_err(sql_error)? {
        ValueRef::Integer(value) => Ok(value),
        _ => Err(EvidenceStorageError::Corrupt),
    }
}
fn text<'a>(row: &'a rusqlite::Row<'_>, i: usize) -> Result<&'a str> {
    match row.get_ref(i).map_err(sql_error)? {
        ValueRef::Text(bytes) => {
            std::str::from_utf8(bytes).map_err(|_| EvidenceStorageError::Corrupt)
        }
        _ => Err(EvidenceStorageError::Corrupt),
    }
}
fn blob<'a>(row: &'a rusqlite::Row<'_>, i: usize) -> Result<&'a [u8]> {
    match row.get_ref(i).map_err(sql_error)? {
        ValueRef::Blob(value) => Ok(value),
        _ => Err(EvidenceStorageError::Corrupt),
    }
}
fn exact_schema(conn: &Connection) -> Result<()> {
    let expected = i64::try_from(get_migrations_count())
        .map_err(|_| EvidenceStorageError::StorageUnavailable)?;
    let mut stmt = conn.prepare("PRAGMA user_version").map_err(sql_error)?;
    let mut rows = stmt.query([]).map_err(sql_error)?;
    let row = rows
        .next()
        .map_err(sql_error)?
        .ok_or(EvidenceStorageError::Corrupt)?;
    let found = integer(row, 0)?;
    if found < 0 {
        return Err(EvidenceStorageError::Corrupt);
    }
    if found == 0 {
        return Err(EvidenceStorageError::Uninitialized);
    }
    if found != expected {
        return Err(EvidenceStorageError::SchemaIncompatible);
    }
    Ok(())
}

/// `None` means no binding exists. A foreign guard and an ambiguous scope both
/// return the same unavailable result; no UUID is returned on either path.
fn scope(conn: &Connection, guard: &[u8; 32]) -> Result<Option<Uuid>> {
    let mut stmt=conn.prepare("SELECT typeof(repo_scope),octet_length(repo_scope),typeof(scope_guard),octet_length(scope_guard),repo_scope,scope_guard,typeof(created_at),octet_length(created_at) FROM change_evidence_scopes LIMIT 2").map_err(sql_error)?;
    let mut rows = stmt.query([]).map_err(sql_error)?;
    let first = match rows.next().map_err(sql_error)? {
        Some(row) => {
            if text(row, 0)? != "text"
                || integer(row, 1)? != 36
                || text(row, 2)? != "blob"
                || integer(row, 3)? != 32
            {
                return Err(EvidenceStorageError::Corrupt);
            }
            let raw = text(row, 4)?;
            let id = Uuid::parse_str(raw).map_err(|_| EvidenceStorageError::Corrupt)?;
            if id.hyphenated().to_string() != raw {
                return Err(EvidenceStorageError::Corrupt);
            }
            let stored = blob(row, 5)?;
            if stored != guard {
                return Err(EvidenceStorageError::ScopeUnavailable);
            }
            if text(row, 6)? != "text" || integer(row, 7)? <= 0 {
                return Err(EvidenceStorageError::Corrupt);
            }
            id
        }
        None => {
            // Canonical legacy baseline/ledger rows are allowed before first scope;
            // orphaned evidence rows are not a fresh, unbound state.
            let orphan: i64=conn.query_row("SELECT EXISTS(SELECT 1 FROM change_evidence_names LIMIT 1) OR EXISTS(SELECT 1 FROM change_evidence_attestations LIMIT 1) OR EXISTS(SELECT 1 FROM change_evidence_prepared LIMIT 1) OR EXISTS(SELECT 1 FROM change_evidence_attempts LIMIT 1) OR EXISTS(SELECT 1 FROM change_evidence_samples LIMIT 1)",[],|r|r.get(0)).map_err(sql_error)?;
            if orphan != 0 {
                return Err(EvidenceStorageError::ScopeUnavailable);
            }
            return Ok(None);
        }
    };
    if rows.next().map_err(sql_error)?.is_some() {
        return Err(EvidenceStorageError::ScopeUnavailable);
    }
    Ok(Some(first))
}

/// Recheck the same scope guard inside an already-open read or write transaction.
/// A producer must use this on the same connection and transaction as its write;
/// a previously returned UUID does not itself attest the current database.
pub(crate) fn resolve_qualified_tx(conn: &Connection, guard: &[u8; 32]) -> Result<Uuid> {
    resolve_optional_qualified_tx(conn, guard)?.ok_or(EvidenceStorageError::Uninitialized)
}

/// None proves an admitted schema with no scope or orphan evidence.
pub(crate) fn resolve_optional_qualified_tx(
    conn: &Connection,
    guard: &[u8; 32],
) -> Result<Option<Uuid>> {
    if conn.is_autocommit() {
        return Err(EvidenceStorageError::StorageUnavailable);
    }
    exact_schema(conn)?;
    scope(conn, guard)
}

fn require_foreign_keys(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA foreign_keys").map_err(sql_error)?;
    let mut rows = stmt.query([]).map_err(sql_error)?;
    let row = rows
        .next()
        .map_err(sql_error)?
        .ok_or(EvidenceStorageError::Corrupt)?;
    if integer(row, 0)? != 1 {
        return Err(EvidenceStorageError::StorageUnavailable);
    }
    Ok(())
}

/// A writer must first open its own IMMEDIATE transaction and then recheck the
/// guard and foreign-key setting on that same connection before changing rows.
pub(crate) fn resolve_qualified_write_tx(conn: &Connection, guard: &[u8; 32]) -> Result<Uuid> {
    let id = resolve_qualified_tx(conn, guard)?;
    require_foreign_keys(conn)?;
    Ok(id)
}

/// Provision only an empty, previously qualified state. An existing different
/// guard must never be reused, updated, or supplemented with another UUID.
pub(crate) fn resolve_or_provision(
    conn: &mut Connection,
    guard: &[u8; 32],
    created_at: &str,
) -> Result<Uuid> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_error)?;
    let id = resolve_or_provision_write_tx(&tx, guard, created_at)?;
    tx.commit().map_err(sql_error)?;
    Ok(id)
}

/// Reuse the first-scope policy in a caller-owned IMMEDIATE transaction. This
/// helper never commits, so provisioning shares the owning write's rollback.
pub(crate) fn resolve_or_provision_write_tx(
    conn: &Connection,
    guard: &[u8; 32],
    created_at: &str,
) -> Result<Uuid> {
    if conn.is_autocommit() {
        return Err(EvidenceStorageError::StorageUnavailable);
    }
    exact_schema(conn)?;
    let existing = scope(conn, guard)?;
    require_foreign_keys(conn)?;
    if let Some(id) = existing {
        return Ok(id);
    }
    if created_at.is_empty() {
        return Err(EvidenceStorageError::InvalidInput);
    }
    let id = Uuid::new_v4();
    if conn.execute(
        "INSERT INTO change_evidence_scopes(repo_scope,scope_guard,created_at) VALUES(?1,?2,?3)",
        rusqlite::params![id.hyphenated().to_string(), guard.as_slice(), created_at],
    )
    .map_err(sql_error)? != 1 {
        return Err(EvidenceStorageError::StorageUnavailable);
    }
    if scope(conn, guard)? != Some(id) {
        return Err(EvidenceStorageError::ScopeUnavailable);
    }
    Ok(id)
}

/// Validate exact OID, baseline name and digest once before binding SQL parameters.
pub(crate) struct ExactPreparedKey<'a> {
    head: &'a str,
    base: &'a str,
    baseline_name: &'a str,
    baseline_digest: &'a str,
}
impl<'a> ExactPreparedKey<'a> {
    pub(crate) fn new(
        head: &'a str,
        base: &'a str,
        baseline_name: &'a str,
        baseline_digest: &'a str,
    ) -> Result<Self> {
        fn lower_hex(value: &str, len: usize) -> bool {
            value.len() == len
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        let n = baseline_name.as_bytes();
        let name_ok = !n.is_empty()
            && n.len() <= 64
            && (n[0].is_ascii_lowercase() || n[0].is_ascii_digit())
            && n.iter().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-')
            });
        if !lower_hex(head, 40)
            || !lower_hex(base, 40)
            || !name_ok
            || !baseline_digest
                .strip_prefix("blake3:")
                .is_some_and(|d| lower_hex(d, 64))
        {
            return Err(EvidenceStorageError::InvalidInput);
        }
        Ok(Self {
            head,
            base,
            baseline_name,
            baseline_digest,
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedMetadata {
    pub generation: i64,
    pub payload_bytes: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DetachedMetadata {
    pub repo_scope: Uuid,
    pub prepared: Option<PreparedMetadata>,
}

pub(crate) struct EvidenceReader {
    conn: Connection,
    interrupt: Arc<rusqlite::InterruptHandle>,
}
impl EvidenceReader {
    /// The caller has already qualified `db_path`; no create and no Cozo open.
    pub(crate) fn open_existing(db_path: &Path) -> Result<Self> {
        match std::fs::metadata(db_path) {
            Ok(m) if m.is_file() => {}
            Ok(_) => return Err(EvidenceStorageError::StorageUnavailable),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(EvidenceStorageError::Uninitialized);
            }
            Err(_) => return Err(EvidenceStorageError::StorageUnavailable),
        }
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NOFOLLOW
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql_error)?;
        Self::from_existing_connection(conn)
    }

    #[cfg(windows)]
    pub(crate) fn from_immutable_connection(conn: Connection) -> Result<Self> {
        Self::from_existing_connection(conn)
    }

    fn from_existing_connection(conn: Connection) -> Result<Self> {
        if !conn.is_readonly("main").map_err(sql_error)? {
            return Err(EvidenceStorageError::StorageUnavailable);
        }
        conn.busy_timeout(Duration::from_millis(100))
            .map_err(sql_error)?;
        let interrupt = Arc::new(conn.get_interrupt_handle());
        Ok(Self { conn, interrupt })
    }
    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
    pub(crate) fn interrupt_handle(&self) -> Arc<rusqlite::InterruptHandle> {
        self.interrupt.clone()
    }
    /// Returns owned metadata only after the read transaction is closed. It is
    /// not a payload validation or a positive comparison verdict.
    pub(crate) fn read_metadata(
        &self,
        guard: &[u8; 32],
        requested_scope: Uuid,
        key: &ExactPreparedKey<'_>,
        payload_version: i64,
        payload_cap: u64,
    ) -> Result<DetachedMetadata> {
        if payload_version <= 0 {
            return Err(EvidenceStorageError::InvalidInput);
        }
        let tx = self.conn.unchecked_transaction().map_err(sql_error)?;
        let resolved = resolve_qualified_tx(&tx, guard)?;
        if resolved != requested_scope {
            return Err(EvidenceStorageError::ScopeUnavailable);
        }
        let prepared = {
            let mut stmt=tx.prepare("SELECT generation,payload_version,typeof(payload_json),octet_length(payload_json),typeof(payload_digest),octet_length(payload_digest),typeof(captured_at),octet_length(captured_at) FROM change_evidence_prepared WHERE repo_scope=?1 AND head=?2 AND base=?3 AND baseline_name=?4 AND baseline_digest=?5 LIMIT 1").map_err(sql_error)?;
            let mut rows = stmt
                .query(rusqlite::params![
                    resolved.hyphenated().to_string(),
                    key.head,
                    key.base,
                    key.baseline_name,
                    key.baseline_digest
                ])
                .map_err(sql_error)?;
            match rows.next().map_err(sql_error)? {
                None => None,
                Some(row) => {
                    let generation = integer(row, 0)?;
                    let version = integer(row, 1)?;
                    if generation <= 0
                        || version != payload_version
                        || text(row, 2)? != "text"
                        || text(row, 4)? != "text"
                        || text(row, 6)? != "text"
                    {
                        return Err(EvidenceStorageError::Corrupt);
                    }
                    let bytes = integer(row, 3)?;
                    let digest_len = integer(row, 5)?;
                    let captured_len = integer(row, 7)?;
                    if bytes < 0 || digest_len <= 0 || captured_len <= 0 {
                        return Err(EvidenceStorageError::Corrupt);
                    }
                    let payload_bytes =
                        u64::try_from(bytes).map_err(|_| EvidenceStorageError::Corrupt)?;
                    if payload_bytes > payload_cap {
                        return Err(EvidenceStorageError::PayloadOverBudget);
                    }
                    Some(PreparedMetadata {
                        generation,
                        payload_bytes,
                    })
                }
            }
        };
        tx.commit().map_err(sql_error)?;
        Ok(DetachedMetadata {
            repo_scope: resolved,
            prepared,
        })
    }
}

#[path = "change_evidence_attempts.rs"]
pub(crate) mod attempts;

#[path = "change_evidence_writes.rs"]
pub(crate) mod writes;

#[path = "change_evidence_http.rs"]
pub(crate) mod http;
