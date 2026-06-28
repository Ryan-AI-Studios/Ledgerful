use crate::index::symbols::Symbol;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProvenanceAction {
    Added,
    Modified,
    Deleted,
}

impl fmt::Display for ProvenanceAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ProvenanceAction::Added => "ADDED",
            ProvenanceAction::Modified => "MODIFIED",
            ProvenanceAction::Deleted => "DELETED",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for ProvenanceAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "ADDED" => Ok(ProvenanceAction::Added),
            "MODIFIED" => Ok(ProvenanceAction::Modified),
            "DELETED" => Ok(ProvenanceAction::Deleted),
            _ => Err(format!("Unknown provenance action: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenProvenance {
    pub id: Option<i64>,
    pub tx_id: String,
    pub entity: String,
    pub entity_normalized: String,
    pub symbol_name: String,
    pub symbol_type: String,
    pub action: ProvenanceAction,
}

/// Compute the difference between two sets of symbols.
pub fn compute_symbol_diff(
    old_symbols: &[Symbol],
    new_symbols: &[Symbol],
) -> Vec<(Symbol, ProvenanceAction)> {
    let mut diff = Vec::new();

    // Find added and modified
    for ns in new_symbols {
        if let Some(os) = old_symbols
            .iter()
            .find(|s| s.name == ns.name && s.kind == ns.kind)
        {
            // Check if modified (complexity or visibility changes)
            if os != ns {
                diff.push((ns.clone(), ProvenanceAction::Modified));
            }
        } else {
            // New symbol
            diff.push((ns.clone(), ProvenanceAction::Added));
        }
    }

    // Find deleted
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
