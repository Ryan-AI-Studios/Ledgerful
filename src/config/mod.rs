pub mod defaults;
pub mod error;
pub mod load;
pub mod model;

// Auto-included submodules of model (Rust 2024 edition loads sibling files automatically).
pub mod redact;
pub mod starter;
pub mod validate;

pub use error::ConfigError;
pub use load::load_config;
pub use model::Config;
pub use validate::validate_config;
