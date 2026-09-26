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
