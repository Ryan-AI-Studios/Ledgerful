use super::ProjectIndexer;
use crate::index::centrality::CentralityComputer;
use miette::Result;

pub fn compute_centrality(
    indexer: &ProjectIndexer,
) -> Result<crate::index::centrality::CentralityStats> {
    CentralityComputer::new(&indexer.storage).compute()
}
