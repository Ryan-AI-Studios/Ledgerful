pub mod diff;
pub mod edit;
pub mod env;
pub mod schema;
pub mod verify;
pub mod view;

// Preserve the original public API surface used by `src/cli/dispatch.rs`.
pub use diff::execute_config_diff;
pub(crate) use edit::{
    DOCTOR_CONFIG_REFUSAL, execute_existing_config_ack_in_quiet,
    execute_existing_config_set_in_quiet,
};
pub use edit::{
    execute_config_set, execute_config_set_in, execute_config_set_in_quiet, execute_config_unset,
    execute_config_unset_in,
};
pub use schema::execute_config_schema;
pub use verify::execute_config_verify;
pub use view::execute_config_view;
