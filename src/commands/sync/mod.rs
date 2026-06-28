pub mod cursor;
pub mod init;
pub mod log;
pub mod pair;
pub mod run;
pub mod status;
pub mod verify;

use crate::cli::args::SyncSubcommands;
use miette::Result;

pub fn handle(subcommand: SyncSubcommands) -> Result<()> {
    match subcommand {
        SyncSubcommands::Init { force, with_secret } => init::handle(force, with_secret),
        SyncSubcommands::Pair { code } => pair::handle(code),
        SyncSubcommands::Run { once } => run::handle(once),
        SyncSubcommands::Status => status::handle(),
        SyncSubcommands::Verify { path } => verify::handle(&path),
        SyncSubcommands::Cursor { set } => cursor::handle(set),
        SyncSubcommands::Log { tail } => log::handle(tail),
    }
}
