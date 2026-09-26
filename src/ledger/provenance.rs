//! Engine provenance adapter: DTOs from `ledgerful_ledger`; `compute_symbol_diff`
//! keeps the `Symbol` signature for integration tests and `TransactionManager`.

pub use ledgerful_ledger::provenance::{ProvenanceAction, TokenProvenance};

use crate::index::symbols::Symbol;

/// Compute the difference between two sets of symbols.
pub fn compute_symbol_diff(
    old_symbols: &[Symbol],
    new_symbols: &[Symbol],
) -> Vec<(Symbol, ProvenanceAction)> {
    let mut diff = Vec::new();

    for ns in new_symbols {
        if let Some(os) = old_symbols
            .iter()
            .find(|s| s.name == ns.name && s.kind == ns.kind)
        {
            if os != ns {
                diff.push((ns.clone(), ProvenanceAction::Modified));
            }
        } else {
            diff.push((ns.clone(), ProvenanceAction::Added));
        }
    }

    for os in old_symbols {
        if !new_symbols
            .iter()
            .any(|s| s.name == os.name && s.kind == os.kind)
        {
            diff.push((os.clone(), ProvenanceAction::Deleted));
        }
    }

    diff
}
