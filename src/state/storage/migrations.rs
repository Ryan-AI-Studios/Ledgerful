#[cfg(not(test))]
use miette::{IntoDiagnostic, Result};
#[cfg(not(test))]
use rusqlite::Connection;

#[cfg(not(test))]
pub(crate) fn verify_schema_is_current(conn: &Connection) -> Result<()> {
    let migrations = crate::state::migrations::get_migrations();
    let current_version = migrations.current_version(conn).into_diagnostic()?;
    let latest_version = crate::state::migrations::get_migrations_count();
    let is_mismatch = match current_version {
        rusqlite_migration::SchemaVersion::NoneSet => latest_version > 0,
        rusqlite_migration::SchemaVersion::Inside(v) => v.get() < latest_version,
        rusqlite_migration::SchemaVersion::Outside(v) => v.get() < latest_version,
    };
    if is_mismatch {
        return Err(crate::state::StateError::SchemaMismatch.into());
    }
    Ok(())
}
