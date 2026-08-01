pub mod cursor;
pub mod init;
pub mod log;
pub mod pair;
pub mod readiness;
pub mod run;
pub mod setup;
pub mod status;
pub mod verify;

use crate::cli::args::SyncSubcommands;
use miette::Result;

pub fn handle(subcommand: SyncSubcommands) -> Result<()> {
    match subcommand {
        SyncSubcommands::Init { force, with_secret } => init::handle(force, with_secret),
        SyncSubcommands::Pair {
            code,
            list,
            revoke,
            force,
        } => pair::handle(code, list, revoke, force),
        SyncSubcommands::Run { once } => run::handle(once),
        SyncSubcommands::Setup { enable, json } => setup::handle(enable, json),
        SyncSubcommands::Status { json } => status::handle(json),
        SyncSubcommands::Verify { path } => verify::handle(&path),
        SyncSubcommands::Cursor { set } => cursor::handle(set),
        SyncSubcommands::Log { tail } => log::handle(tail),
    }
}
