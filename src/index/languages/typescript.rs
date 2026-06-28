mod calls;
mod common;
mod models;
mod observability;
mod routes;
mod symbols;

pub use calls::extract_calls;
pub use models::extract_data_models;
pub use observability::{
    extract_error_handling, extract_logging_patterns, extract_telemetry_patterns,
};
pub use routes::extract_routes;
pub use symbols::extract_symbols;

// Tests are co-located in their respective sub-modules.
