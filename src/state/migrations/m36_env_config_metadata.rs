use rusqlite_migration::M;

pub fn m36_env_config_metadata() -> Vec<M<'static>> {
    vec![M::up(
        "ALTER TABLE env_declarations ADD COLUMN is_secret INTEGER DEFAULT 0; \
         ALTER TABLE env_declarations ADD COLUMN owner TEXT; \
         ALTER TABLE env_declarations ADD COLUMN environment TEXT;",
    )]
}
