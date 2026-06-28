use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use miette::Result;

pub const MAX_FILES: usize = 10_000;
pub const BATCH_SIZE: usize = 500;
pub const PARSER_VERSION: &str = "1";

pub const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "ico", "woff", "woff2", "ttf", "eot", "pdf", "zip", "tar", "gz",
    "exe", "dll", "so", "dylib", "wasm", "class", "jar", "pyc",
];

pub const SUPPORTED_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "py", "go"];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub parse_failures: usize,
    pub skipped_binary: usize,
    pub skipped_unsupported: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexStatus {
    pub total_files: usize,
    pub total_symbols: usize,
    pub stale_files: usize,
    pub last_indexed_at: Option<String>,
    pub assessment: Option<crate::index::staleness::IndexFreshnessAssessment>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServiceIndexStats {
    pub services_inferred: usize,
    pub files_assigned: usize,
}

use crate::config::model::Config;

pub struct ProjectIndexer {
    storage: StorageManager,
    repo_path: Utf8PathBuf,
    config: Config,
}

// ---------------------------------------------------------------------------
// Capability modules
// ---------------------------------------------------------------------------

mod centrality;
mod discovery;
mod docs;
mod extraction;
pub(crate) mod git_meta;
pub(crate) mod graph;
mod lifecycle;
mod parsing;
mod topology;

impl ProjectIndexer {
    pub fn new(storage: StorageManager, repo_path: Utf8PathBuf, config: Config) -> Self {
        Self {
            storage,
            repo_path,
            config,
        }
    }

    pub fn cozo(&self) -> Option<&crate::state::storage_cozo::CozoStorage> {
        self.storage.cozo.as_ref()
    }

    pub fn storage(&self) -> &StorageManager {
        &self.storage
    }

    pub fn storage_mut(&mut self) -> &mut StorageManager {
        &mut self.storage
    }

    pub fn new_for_worker(repo_path: Utf8PathBuf) -> Self {
        Self {
            storage: StorageManager::init_from_conn(
                rusqlite::Connection::open_in_memory()
                    .expect("SQLite in-memory open is infallible"),
            ),
            repo_path,
            config: Config::default(),
        }
    }

    // -----------------------------------------------------------------------
    // Graph
    // -----------------------------------------------------------------------

    pub fn build_kg_native(
        &self,
        local_model_config: &crate::config::model::LocalModelConfig,
        gemini_config: &crate::config::model::GeminiConfig,
        enable_semantic: bool,
        fast: bool,
    ) -> Result<()> {
        graph::build_kg_native(
            self,
            local_model_config,
            gemini_config,
            enable_semantic,
            fast,
        )
    }

    // -----------------------------------------------------------------------
    // Parsing
    // -----------------------------------------------------------------------

    pub fn index_file(
        &self,
        path: &camino::Utf8Path,
    ) -> Result<(
        crate::index::types::ProjectFile,
        Vec<crate::index::types::ProjectSymbol>,
    )> {
        parsing::index_file(self, path)
    }

    pub fn index_file_with_edges(
        &self,
        path: &camino::Utf8Path,
    ) -> Result<(
        crate::index::types::ProjectFile,
        Vec<crate::index::types::ProjectSymbol>,
        Vec<crate::index::call_graph::CallEdge>,
    )> {
        parsing::index_file_with_edges(self, path)
    }

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    pub fn check_status(&self) -> Result<IndexStatus> {
        lifecycle::check_status(self)
    }

    pub fn full_index(&mut self) -> Result<IndexStats> {
        lifecycle::full_index(self)
    }

    pub fn incremental_index(&mut self) -> Result<IndexStats> {
        lifecycle::incremental_index(self)
    }

    /// Backfill `last_touched_at` and `last_contributor` in `project_files`
    /// by walking git history (Track TA30). Only updates rows with NULL
    /// values. Skips the walk entirely if no rows need backfill (fast path).
    pub fn backfill_git_metadata(&mut self) -> Result<()> {
        git_meta::backfill_git_metadata(&mut self.storage, &self.repo_path)
    }

    // -----------------------------------------------------------------------
    // Discovery
    // -----------------------------------------------------------------------

    pub fn discover_files(&self) -> Result<Vec<Utf8PathBuf>> {
        discovery::discover_files(self)
    }

    pub fn discover_doc_files(&self) -> Result<Vec<Utf8PathBuf>> {
        discovery::discover_doc_files(self)
    }

    // -----------------------------------------------------------------------
    // Docs
    // -----------------------------------------------------------------------

    pub fn index_docs(&mut self) -> Result<crate::index::docs::DocIndexStats> {
        docs::index_docs(self)
    }

    // -----------------------------------------------------------------------
    // Topology
    // -----------------------------------------------------------------------

    pub fn index_topology(&mut self) -> Result<crate::index::topology::TopologyIndexStats> {
        topology::index_topology(self)
    }

    pub fn classify_entrypoints(&mut self) -> Result<crate::index::entrypoint::EntrypointStats> {
        topology::classify_entrypoints(self)
    }

    pub fn infer_services(&mut self) -> Result<ServiceIndexStats> {
        topology::infer_services(self)
    }

    // -----------------------------------------------------------------------
    // Extraction
    // -----------------------------------------------------------------------

    pub fn build_call_graph(&self) -> Result<crate::index::call_graph::CallGraphStats> {
        extraction::build_call_graph(self)
    }

    pub fn extract_routes(&self) -> Result<crate::index::routes::RouteStats> {
        extraction::extract_routes(self)
    }

    pub fn into_storage(self) -> StorageManager {
        self.storage
    }

    pub fn clear_routes(&self, file_ids: &[i64]) -> Result<()> {
        extraction::clear_routes(self, file_ids)
    }

    pub fn clear_structural_edges(&self, file_ids: &[i64]) -> Result<()> {
        extraction::clear_structural_edges(self, file_ids)
    }

    pub fn extract_data_models(&self) -> Result<crate::index::data_models::DataModelStats> {
        extraction::extract_data_models(self)
    }

    pub fn clear_data_models(&self, file_ids: &[i64]) -> Result<()> {
        extraction::clear_data_models(self, file_ids)
    }

    pub fn extract_observability(&self) -> Result<crate::index::observability::ObservabilityStats> {
        extraction::extract_observability(self)
    }

    pub fn compute_centrality(&self) -> Result<crate::index::centrality::CentralityStats> {
        centrality::compute_centrality(self)
    }

    pub fn get_all_call_edges(&self) -> Result<Vec<crate::index::call_graph::CallEdge>> {
        extraction::get_all_call_edges(self)
    }

    pub fn extract_test_mappings(&self) -> Result<crate::index::test_mapping::TestMappingStats> {
        extraction::extract_test_mappings(self)
    }

    pub fn extract_ci_gates(&self) -> Result<crate::index::ci_gates::CIGateStats> {
        extraction::extract_ci_gates(self)
    }

    pub fn extract_env_schema(&self) -> Result<crate::index::env_schema::EnvSchemaStats> {
        extraction::extract_env_schema(self)
    }

    pub fn delete_file_symbols(&mut self, file_path: &str) -> Result<()> {
        crate::index::rows::delete_file_symbols(&mut self.storage, file_path)
    }
}
