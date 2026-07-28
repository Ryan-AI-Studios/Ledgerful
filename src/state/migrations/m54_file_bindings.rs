use rusqlite_migration::M;

/// Creates the `file_bindings` table (m54, Track 0092 Part 1).
///
/// Per-file bound names from imports and `mod` declarations. The lookup key is
/// the **bound name** that enters file scope (alias or last segment), not the
/// raw import path — so resolution can answer "does segment `X` bind locally?"
///
/// - `is_enumerable = 0` for wildcards (`use foo::*`) which never prove locality
/// - `is_local = 1` when the binding is proven local (`mod` decl; `use crate/self/super::…`)
///
/// Registered unconditionally so schema_version stays monotonic across binaries.
pub fn m54_file_bindings() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS file_bindings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            bound_name TEXT NOT NULL,
            source_path TEXT NOT NULL,
            binding_kind TEXT NOT NULL,
            is_enumerable INTEGER NOT NULL DEFAULT 1,
            is_local INTEGER NOT NULL DEFAULT 0,
            UNIQUE(file_id, bound_name, source_path, binding_kind)
        );
        CREATE INDEX IF NOT EXISTS idx_file_bindings_file ON file_bindings(file_id);
        CREATE INDEX IF NOT EXISTS idx_file_bindings_bound ON file_bindings(file_id, bound_name);",
    )]
}
