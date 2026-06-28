use rusqlite_migration::M;

pub fn m32_symbol_metadata() -> Vec<M<'static>> {
    vec![
        M::up("ALTER TABLE project_symbols ADD COLUMN metadata TEXT;"),
        M::up("ALTER TABLE symbols ADD COLUMN metadata TEXT;"),
    ]
}
