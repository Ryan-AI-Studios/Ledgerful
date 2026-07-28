pub mod augment;
pub mod edges;
pub mod ingest;
pub mod orchestrator;
pub mod path_normalize;
pub mod range;
pub mod resolver;
pub mod stale_detect;

pub use augment::{ScipIndexJson, ScipRunStatus, execute_scip_index, maybe_run_scip_augment};
pub use edges::{ScipEdgeStats, augment_edges_from_scip};
pub use ingest::ScipIndex;
pub use orchestrator::ScipToolchain;
pub use path_normalize::normalize_scip_path;
pub use range::{ScipRange, parse_scip_range};
pub use resolver::{SCIP_EDGE_EVIDENCE, ScipNativeResolver, is_definition_role, resolve_innermost};
pub use stale_detect::{is_scip_stale, register_scip_index};
