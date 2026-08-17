use crate::cli::args::ConfigCommands;
use miette::Result;

pub(super) fn dispatch_config(command: ConfigCommands, global_verbose: bool) -> Result<()> {
    match command {
        ConfigCommands::Verify {
            json,
            section,
            verbose,
        } => crate::commands::config::execute_config_verify(json, section.as_deref(), verbose),
        ConfigCommands::View { json, section, key } => {
            crate::commands::config::execute_config_view(json, section, key)
        }
        ConfigCommands::Schema { json } => crate::commands::config::execute_config_schema(json),
        // TA19: for `config diff` the global `-v` flag is intercepted to
        // control the internal-env-var filter only; tracing is handled by
        // RUST_LOG and is suppressed for this command in main.rs.
        ConfigCommands::Diff {
            json,
            show_internal,
        } => crate::commands::config::execute_config_diff(json, show_internal || global_verbose),
        ConfigCommands::Set { key_value } => {
            crate::commands::config::execute_config_set(&key_value)
        }
        ConfigCommands::Unset { key } => crate::commands::config::execute_config_unset(&key),
    }
}
