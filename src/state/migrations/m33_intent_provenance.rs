use rusqlite_migration::M;

pub fn m33_intent_provenance() -> Vec<M<'static>> {
    vec![M::up(
        "ALTER TABLE ledger_entries ADD COLUMN signature TEXT;
         ALTER TABLE ledger_entries ADD COLUMN public_key TEXT;
         ALTER TABLE ledger_entries ADD COLUMN risk TEXT;
         ALTER TABLE ledger_entries ADD COLUMN related_tickets TEXT;",
    )]
}
