use rusqlite_migration::M;

/// Adds job-level qualifier columns on `ci_gates` (m55, Track 0326).
///
/// `job_if` / `needs` / `uses` hold declared GitHub Actions job properties.
/// Empty/absent values are SQL NULL. `needs` is a comma-space sorted id list.
/// Registered unconditionally so schema_version stays monotonic.
pub fn m55_ci_gate_qualifiers() -> Vec<M<'static>> {
    vec![M::up(
        "ALTER TABLE ci_gates ADD COLUMN job_if TEXT;
         ALTER TABLE ci_gates ADD COLUMN needs TEXT;
         ALTER TABLE ci_gates ADD COLUMN uses TEXT;",
    )]
}
