use rusqlite_migration::M;

pub fn m40_validator_management() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_commit_validators_name_cat ON commit_validators(name, category);",
    )]
}
