//! Soft-open storage for change-context (CLI + MCP).

use super::packet::NotReadyErrorClass;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;

pub(crate) fn storage_unavailable_reason(
    err: &miette::Report,
    class: NotReadyErrorClass,
) -> String {
    match class {
        NotReadyErrorClass::PermissionDenied => {
            format!("storage unavailable: state directory not writable: {err}")
        }
        _ => format!("storage unavailable: {err}"),
    }
}

/// Map open/init failures to B5 classes for nextActions.
///
/// Order matters: open/permission strings often appear inside messages that also
/// mention `PRAGMA user_version` (schema probe). Prefer PermissionDenied over
/// SchemaStale when both could match — never advise migration for pure RO open fail.
pub(crate) fn classify_storage_error(err: &miette::Report, db_exists: bool) -> NotReadyErrorClass {
    let s = format!("{err}").to_ascii_lowercase();

    // Permission / pure-RO open failures first (before schema keyword scan).
    if s.contains("permission denied")
        || s.contains("access is denied")
        || s.contains("read-only file system")
        || s.contains("readonly database")
        || s.contains("attempt to write a readonly")
        || s.contains("state directory not writable")
        || s.contains("os error 5")
        || s.contains("(os error 5)")
        || s.contains("os error 30")
        || s.contains("unable to open database")
        || s.contains("disk i/o error")
    {
        return if db_exists || s.contains("unable to open database") || s.contains("readonly") {
            NotReadyErrorClass::PermissionDenied
        } else {
            NotReadyErrorClass::MissingDb
        };
    }

    // True schema mismatch (StateError::SchemaMismatch / migration probe).
    // Do NOT match bare "user_version" alone — open failures often embed
    // `PRAGMA user_version ... unable to open database file`.
    if s.contains("schema mismatch")
        || s.contains("migration required")
        || s.contains("schema is not current")
        || s.contains("schema not current")
        || (s.contains("schema") && s.contains("not current"))
        || s.contains("schema_version")
    {
        return NotReadyErrorClass::SchemaStale;
    }

    if s.contains("not initialized")
        || s.contains("no such file")
        || s.contains("does not exist")
        || s.contains("the system cannot find the file")
    {
        return if db_exists {
            // Exists but still reported missing path fragments → prefer permission.
            NotReadyErrorClass::PermissionDenied
        } else {
            NotReadyErrorClass::MissingDb
        };
    }

    if !db_exists {
        return NotReadyErrorClass::MissingDb;
    }

    NotReadyErrorClass::Other
}

/// Soft-open change-context storage (B6): prefer true RO when `ledger.db` exists.
///
/// On RO permission/schema failure, do **not** fall through to write-open.
/// When the DB is missing, attempt write init (writable env creates state).
/// Shared by CLI, `build_change_context_from_cwd`, and MCP `change_context`.
pub(crate) fn open_storage_for_change_context(
    layout: &Layout,
) -> std::result::Result<StorageManager, (miette::Report, NotReadyErrorClass)> {
    let db_path = layout.state_subdir().join("ledger.db");
    let db_exists = db_path.exists();

    if db_exists {
        match StorageManager::open_read_only(layout) {
            Ok(s) => return Ok(s),
            Err(e) => {
                let class = classify_storage_error(&e, true);
                // Permission / schema: honest not_ready — do not try write open.
                if matches!(
                    class,
                    NotReadyErrorClass::PermissionDenied | NotReadyErrorClass::SchemaStale
                ) {
                    return Err((e, class));
                }
                // Other full-RO failures (often Cozo): try SQLite-only RO so
                // reviewer packets still work without mutating state.
                tracing::debug!(
                    "change-context RO open failed ({class:?}); trying sqlite-only RO: {e}"
                );
                match StorageManager::open_read_only_sqlite_only(layout) {
                    Ok(s) => return Ok(s),
                    Err(e2) => {
                        tracing::debug!(
                            "change-context sqlite-only RO also failed; trying write open: {e2}"
                        );
                    }
                }
            }
        }
    }

    match StorageManager::init_with_layout(layout) {
        Ok(s) => Ok(s),
        Err(e) => {
            let class = classify_storage_error(&e, db_path.exists());
            Err((e, class))
        }
    }
}
