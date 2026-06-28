//! Migration prompt on DB open (Track TA31 R3).
//!
//! When `ledgerful` opens an EXISTING project's `ledger.db` that was created
//! or last touched by an older binary, the user should be informed before
//! (or instead of, depending on interactivity) the migration silently runs.
//! A genuinely brand-new project (no prior `ledger.db` file) must never see
//! this prompt/notice — there is no real "migration" happening for a fresh
//! database, just normal first-time initialization at version 0.
//!
//! This module is deliberately small: it is not a generic "confirmation
//! framework," just the one check `StorageManager::init` needs.

use rusqlite::Connection;
use std::io::BufRead;

/// The versions involved in a pending migration, when one is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingMigration {
    pub(crate) old: usize,
    pub(crate) new: usize,
}

/// Determines whether `conn`'s schema is behind the binary's latest known
/// migration count. Returns `None` when already current (mirrors the
/// `SchemaVersion::NoneSet => latest_version > 0` pattern already used by
/// `StorageManager::open_read_only_with_options`).
pub(crate) fn pending_migration(conn: &Connection) -> miette::Result<Option<PendingMigration>> {
    use miette::IntoDiagnostic;

    let migrations = crate::state::migrations::get_migrations();
    let current_version = migrations.current_version(conn).into_diagnostic()?;
    let latest_version = crate::state::migrations::get_migrations_count();

    let old = match current_version {
        rusqlite_migration::SchemaVersion::NoneSet => 0,
        rusqlite_migration::SchemaVersion::Inside(v) => v.get(),
        rusqlite_migration::SchemaVersion::Outside(v) => v.get(),
    };

    if old >= latest_version {
        return Ok(None);
    }

    Ok(Some(PendingMigration {
        old,
        new: latest_version,
    }))
}

/// Builds the interactive prompt text shown before migrating an existing,
/// stale project database. Exact wording per spec.md R3 point 2.
pub(crate) fn prompt_message(pending: PendingMigration) -> String {
    format!(
        "Ledgerful: This project's database needs migration (v{old} → v{new}).\nThis will update .ledgerful/state/ledger.db in place.\nProceed? [Y/n] ",
        old = pending.old,
        new = pending.new
    )
}

/// Builds the unavoidable non-interactive notice (printed to stderr instead
/// of prompting) per spec.md R3's "Non-interactive notification" paragraph.
/// Exact format required by spec.
pub(crate) fn auto_migrated_notice(pending: PendingMigration) -> String {
    format!(
        "[INFO] Ledgerful database auto-migrated from v{old} to v{new}",
        old = pending.old,
        new = pending.new
    )
}

/// Testable core of the migration gate. `interactive` and `input` are
/// injected so unit tests can drive deterministic answers without a real TTY
/// (same split as `crate::util::term::prompt_yes_no_with`). `notice_sink`
/// receives the exact non-interactive `[INFO]` line so tests can assert on
/// it without capturing real stderr.
///
/// Returns `Ok(())` if the caller should proceed to run migrations, or `Err`
/// if the user declined (in which case the caller must NOT run migrations).
pub(crate) fn check_and_prompt_migration_with(
    conn: &Connection,
    db_existed_before_open: bool,
    interactive: bool,
    input: &mut impl BufRead,
    mut notice_sink: impl FnMut(&str),
) -> miette::Result<()> {
    let Some(pending) = pending_migration(conn)? else {
        // Already current: idempotent, no prompt (spec rule 6).
        return Ok(());
    };

    if !db_existed_before_open {
        // Brand-new project: version 0 -> latest is normal first-time setup,
        // not a "migration" the user needs to be told about (footgun #1).
        return Ok(());
    }

    if interactive {
        let msg = prompt_message(pending);
        if crate::util::term::prompt_yes_no_stderr_with(&msg, true, input) {
            Ok(())
        } else {
            Err(miette::miette!(
                "Migration declined; cannot proceed without upgrading .ledgerful/state/ledger.db from v{} to v{}.",
                pending.old,
                pending.new
            ))
        }
    } else {
        notice_sink(&auto_migrated_notice(pending));
        Ok(())
    }
}

/// Production entry point used by `StorageManager::init`. Prompts on a real
/// TTY (reading real stdin), or prints the unavoidable `[INFO]` auto-migrate
/// notice to real stderr when bypassed (non-TTY, `CI`, `NON_INTERACTIVE`, or
/// `LEDGERFUL_NON_INTERACTIVE`).
pub(crate) fn check_and_prompt_migration(
    conn: &Connection,
    db_existed_before_open: bool,
) -> miette::Result<()> {
    let interactive = crate::util::term::is_interactive();
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    check_and_prompt_migration_with(
        conn,
        db_existed_before_open,
        interactive,
        &mut reader,
        |msg| {
            eprintln!("{msg}");
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Opens a fresh in-memory connection and migrates only the FIRST batch
    /// (`m1_to_m10`), leaving `current_version` deliberately behind
    /// `get_migrations_count()` so `pending_migration` reports a gap.
    fn old_version_conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        let first_batch =
            rusqlite_migration::Migrations::new(crate::state::migrations::m1_to_m10::m1_to_m10());
        first_batch
            .to_latest(&mut conn)
            .expect("apply first migration batch");
        conn
    }

    fn current_version_conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .expect("apply all migrations");
        conn
    }

    #[test]
    fn up_to_date_db_has_no_pending_migration() {
        let conn = current_version_conn();
        assert_eq!(pending_migration(&conn).unwrap(), None);
    }

    #[test]
    fn stale_db_reports_pending_migration_with_correct_versions() {
        let conn = old_version_conn();
        let pending = pending_migration(&conn).unwrap().expect("should be stale");
        assert_eq!(
            pending.old,
            crate::state::migrations::m1_to_m10::m1_to_m10().len()
        );
        assert_eq!(
            pending.new,
            crate::state::migrations::get_migrations_count()
        );
        assert!(pending.old < pending.new);
    }

    #[test]
    fn prompt_message_matches_expected_format() {
        let pending = PendingMigration { old: 10, new: 47 };
        let msg = prompt_message(pending);
        assert_eq!(
            msg,
            "Ledgerful: This project's database needs migration (v10 → v47).\nThis will update .ledgerful/state/ledger.db in place.\nProceed? [Y/n] "
        );
    }

    #[test]
    fn auto_migrated_notice_matches_expected_format() {
        let pending = PendingMigration { old: 10, new: 47 };
        assert_eq!(
            auto_migrated_notice(pending),
            "[INFO] Ledgerful database auto-migrated from v10 to v47"
        );
    }

    #[test]
    fn idempotent_current_version_no_prompt_no_notice() {
        let conn = current_version_conn();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut notices = Vec::new();
        let result = check_and_prompt_migration_with(&conn, true, true, &mut input, |msg| {
            notices.push(msg.to_string());
        });
        assert!(result.is_ok());
        assert!(
            notices.is_empty(),
            "no notice expected when already current"
        );
    }

    #[test]
    fn brand_new_db_skips_prompt_even_though_version_is_behind() {
        // Regression guard for footgun #1: a fresh tempdir DB has version 0
        // behind latest, but `db_existed_before_open == false` must short
        // circuit before any prompt or notice — exactly what every one of
        // the ~165 existing `StorageManager::init` call sites relies on.
        let conn = old_version_conn();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut notices = Vec::new();
        let result = check_and_prompt_migration_with(&conn, false, true, &mut input, |msg| {
            notices.push(msg.to_string());
        });
        assert!(result.is_ok());
        assert!(
            notices.is_empty(),
            "no notice expected for brand-new db, even though interactive=true"
        );
    }

    #[test]
    fn old_version_interactive_yes_proceeds() {
        let conn = old_version_conn();
        let mut input = Cursor::new(b"y\n".to_vec());
        let mut notices = Vec::new();
        let result = check_and_prompt_migration_with(&conn, true, true, &mut input, |msg| {
            notices.push(msg.to_string());
        });
        assert!(result.is_ok());
        assert!(
            notices.is_empty(),
            "interactive path uses the prompt, not the non-interactive notice"
        );
    }

    #[test]
    fn old_version_interactive_default_yes_on_empty_line_proceeds() {
        let conn = old_version_conn();
        let mut input = Cursor::new(b"\n".to_vec());
        let result = check_and_prompt_migration_with(&conn, true, true, &mut input, |_| {});
        assert!(result.is_ok());
    }

    #[test]
    fn old_version_interactive_no_declines_with_error() {
        let conn = old_version_conn();
        let mut input = Cursor::new(b"n\n".to_vec());
        let result = check_and_prompt_migration_with(&conn, true, true, &mut input, |_| {});
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Migration declined"), "got: {msg}");

        // The connection's own user_version must be unchanged: this function
        // only inspects/decides, it never calls `migrations.to_latest` itself.
        let pending_after = pending_migration(&conn).unwrap();
        assert!(
            pending_after.is_some(),
            "version must remain stale after decline"
        );
    }

    #[test]
    fn old_version_non_interactive_proceeds_and_emits_notice() {
        let conn = old_version_conn();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut notices = Vec::new();
        let result = check_and_prompt_migration_with(&conn, true, false, &mut input, |msg| {
            notices.push(msg.to_string());
        });
        assert!(result.is_ok());
        assert_eq!(notices.len(), 1);
        assert!(notices[0].starts_with("[INFO] Ledgerful database auto-migrated from v"));
    }

    #[test]
    fn old_version_non_interactive_never_reads_input() {
        // Cursor with no data: if the non-interactive branch ever tried to
        // read it, `prompt_yes_no_with` would just see EOF, but more
        // importantly this proves the non-interactive branch in
        // `check_and_prompt_migration_with` doesn't go through the prompt
        // path's stdout write or block at all -- it returns Ok immediately
        // via the notice sink.
        let conn = old_version_conn();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut notice_count = 0;
        let result = check_and_prompt_migration_with(&conn, true, false, &mut input, |_| {
            notice_count += 1;
        });
        assert!(result.is_ok());
        assert_eq!(notice_count, 1);
    }
}
