use rusqlite_migration::M;

pub fn m39_ledger_neighborhood() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS transaction_links (
            id                 INTEGER PRIMARY KEY AUTOINCREMENT,
            tx_id              TEXT NOT NULL REFERENCES transactions(tx_id),
            entity_type        TEXT NOT NULL, -- 'SYMBOL', 'ENDPOINT', 'FILE', 'ADR', 'TEST', 'HOTSPOT', 'SECURITY_BOUNDARY', etc.
            entity_name        TEXT NOT NULL, -- qualified name or path
            entity_normalized  TEXT NOT NULL,
            metadata           TEXT, -- JSON for extra context
            linked_at          TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_transaction_links_tx_id ON transaction_links(tx_id);
        CREATE INDEX IF NOT EXISTS idx_transaction_links_entity ON transaction_links(entity_normalized, entity_type);",
    )]
}
