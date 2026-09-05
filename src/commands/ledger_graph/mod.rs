//! `ledgerful ledger graph` command: query primitives, assemble, print.
//!
//! Barrel split of the former `commands/ledger_graph.rs` (0269). Public path
//! stays `crate::commands::ledger_graph`.

mod assemble;
mod print;
mod query;

use crate::commands::helpers::get_layout;
use crate::state::storage::StorageManager;
use clap::Args;
use miette::Result;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct LedgerGraphArgs {
    /// Transaction ID (or prefix)
    pub tx_id: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphRelation {
    pub entity_id: String,
    pub label: String,
    pub category: String,
    pub relation: String,
    pub exactness: String,          // "exact", "derived", "heuristic"
    pub attribution_source: String, // "token_provenance", "changed_files", "ledger_link", "knowledge_graph", "heuristic_fallback"
}

pub fn execute_ledger_graph(args: LedgerGraphArgs) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let cozo = storage
        .cozo()
        .ok_or_else(|| miette::miette!("CozoDB not available"))?;

    let db = crate::ledger::db::LedgerDb::new(storage.get_connection());
    let full_id = query::resolve_tx_id(&db, &args.tx_id)?;
    let data = assemble::assemble_ledger_graph(&layout, &storage, cozo, &db, &full_id)?;
    print::print_ledger_graph(&args, &full_id, &data)
}
