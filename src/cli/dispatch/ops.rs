use crate::cli::args::{IntentCommands, InternalCommands};
use miette::Result;

pub(super) fn dispatch_intent(command: IntentCommands) -> Result<()> {
    match command {
        IntentCommands::Demo => crate::commands::intent::execute_intent_demo(),
    }
}
pub(super) fn dispatch_internal(command: InternalCommands) -> Result<()> {
    match command {
        InternalCommands::HookCommitMsg { msg_file } => {
            crate::commands::hook_commit_msg::execute_hook_commit_msg(&msg_file)
        }
        InternalCommands::HookPostCommit => {
            crate::commands::hook_post_commit::execute_hook_post_commit()
        }
    }
}

#[cfg(feature = "usage-metrics")]
pub(super) fn dispatch_usage(command: crate::cli::args::UsageCommands) -> Result<()> {
    match command {
        crate::cli::args::UsageCommands::Enable => crate::commands::usage::execute_usage_enable(),
        crate::cli::args::UsageCommands::Disable => crate::commands::usage::execute_usage_disable(),
        crate::cli::args::UsageCommands::Status => crate::commands::usage::execute_usage_status(),
        crate::cli::args::UsageCommands::ShowPayload => {
            crate::commands::usage::execute_usage_show_payload()
        }
    }
}
pub(super) fn dispatch_schedule(
    subcommand: crate::commands::schedule::ScheduleSubcommands,
) -> Result<()> {
    match subcommand {
        crate::commands::schedule::ScheduleSubcommands::SetupNightly { dry_run, uninstall } => {
            crate::commands::schedule::execute_setup_nightly(dry_run, uninstall)
        }
        crate::commands::schedule::ScheduleSubcommands::RunNightly => {
            crate::commands::schedule::execute_run_nightly()
        }
    }
}

#[cfg(feature = "mcp")]
pub(super) fn dispatch_mcp(command: Option<crate::cli::args::McpCommands>) -> Result<()> {
    match command {
        None | Some(crate::cli::args::McpCommands::Serve) => {
            crate::commands::mcp::execute_mcp_server()
        }
        Some(crate::cli::args::McpCommands::Install {
            platforms,
            scope,
            launcher,
            dry_run,
            force,
            no_backup,
            json,
        }) => crate::commands::mcp::install::execute_install(
            platforms, scope, launcher, dry_run, force, !no_backup, json,
        ),
        Some(crate::cli::args::McpCommands::Uninstall {
            platforms,
            scope,
            dry_run,
            json,
        }) => crate::commands::mcp::install::execute_uninstall(platforms, scope, dry_run, json),
        Some(crate::cli::args::McpCommands::Status { json }) => {
            crate::commands::mcp::install::execute_status(json)
        }
    }
}
