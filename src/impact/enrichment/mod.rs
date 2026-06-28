use crate::config::model::Config;
use crate::impact::packet::ImpactPacket;
pub mod adr_provider;
pub mod api;
pub mod ci_gates;
pub mod ci_predictor;
pub mod ci_self_awareness;
pub mod contracts;
pub mod coupling;
pub mod coverage;
pub mod data_models;
pub mod dead_code;
pub mod deploy;
pub mod environment;
pub mod federated;
pub mod hotspots;
pub mod infrastructure;
pub mod kg_provider;
pub mod knowledge;
pub mod observability;
pub mod runtime_usage;
pub mod services;
use crate::state::storage::StorageManager;
use miette::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Context provided to every enrichment provider during the impact analysis lifecycle.
pub struct EnrichmentContext<'a> {
    pub storage: &'a StorageManager,
    pub config: &'a Config,
    pub file_id_map: HashMap<PathBuf, i64>,
    pub project_root: PathBuf,
    pub warnings: Arc<Mutex<Vec<String>>>,
}

impl<'a> EnrichmentContext<'a> {
    pub fn add_warning(&self, warning: String) {
        if let Ok(mut warnings) = self.warnings.lock() {
            warnings.push(warning);
        }
    }
}

/// A modular component responsible for enriching an ImpactPacket with specific domain data.
pub trait EnrichmentProvider: Send + Sync {
    /// Returns the human-readable name of the provider (for logging/diagnostics).
    fn name(&self) -> &'static str;

    /// Executes the enrichment logic.
    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()>;
}
