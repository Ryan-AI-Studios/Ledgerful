use rusqlite_migration::M;

/// Adds `last_touched_at` and `last_contributor` columns to `project_files`
/// (Track TA30). Both are nullable TEXT:
/// - `last_touched_at`: ISO-8601 timestamp of the most recent commit that
///   touched the file (committer time), or NULL if no git history found.
/// - `last_contributor`: author name from that most recent commit, or NULL.
///
/// Existing rows are backfilled during the next `index --incremental` run —
/// the indexer checks for NULL rows and includes them in the git history walk
/// even if the working tree has no changes.
pub fn m47_project_files_git_meta() -> Vec<M<'static>> {
    vec![M::up(
        "ALTER TABLE project_files ADD COLUMN last_touched_at TEXT;
         ALTER TABLE project_files ADD COLUMN last_contributor TEXT;",
    )]
}
