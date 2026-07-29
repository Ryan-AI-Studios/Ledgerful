#[cfg(feature = "daemon")]
use crate::daemon::{Backend, lifecycle::DaemonLifecycle, state::ReadOnlyStorage};
#[cfg(feature = "daemon")]
use miette::{IntoDiagnostic, Result};
#[cfg(feature = "daemon")]
use std::env;
#[cfg(feature = "daemon")]
use tokio::runtime::Builder;
#[cfg(feature = "daemon")]
use tower_lsp_server::{LspService, Server};

#[cfg(feature = "daemon")]
pub fn execute_daemon(_interval_ms: u64) -> Result<()> {
    // 1. Resolve work root + shared state (linked worktrees → main .ledgerful).
    // Non-git cwd → Layout::new(cwd). Resolve errors after discover fail closed.
    let layout = crate::commands::helpers::get_layout_or_cwd_if_not_git()?;
    let root = layout.root.clone();
    let db_path = layout.state_subdir().join("ledger.db");

    let parent_pid = env::var("LEDGERFUL_PARENT_PID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok());

    // 2. Build constrained tokio runtime
    let rt = Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .into_diagnostic()?;

    rt.block_on(async move {
        let lifecycle = DaemonLifecycle::new(root.as_std_path(), parent_pid);
        lifecycle.setup()?;

        let storage = ReadOnlyStorage::new(db_path.as_std_path());

        let (service, socket) =
            LspService::build(|client| Backend::new(client, lifecycle, storage)).finish();

        Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
            .serve(service)
            .await;

        Ok(())
    })
}

#[cfg(not(feature = "daemon"))]
pub fn execute_daemon(_interval_ms: u64) -> miette::Result<()> {
    Err(miette::miette!(
        "The daemon feature is not enabled in this build. Recompile with --features daemon."
    ))
}
