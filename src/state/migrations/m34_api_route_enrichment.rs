use rusqlite_migration::M;

pub fn m34_api_route_enrichment() -> Vec<M<'static>> {
    vec![M::up(
        "ALTER TABLE api_routes ADD COLUMN auth_requirements TEXT;
         ALTER TABLE api_routes ADD COLUMN schema_refs TEXT;
         ALTER TABLE api_routes ADD COLUMN owning_service TEXT;
         ALTER TABLE api_routes ADD COLUMN consumers TEXT;",
    )]
}
