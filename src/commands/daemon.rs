#[cfg(feature = "daemon")]
use crate::daemon::{Backend, lifecycle::DaemonLifecycle, state::ReadOnlyStorage};
#[cfg(feature = "daemon")]
use camino::Utf8PathBuf;
#[cfg(feature = "daemon")]
use miette::{IntoDiagnostic, Result, miette};
#[cfg(feature = "daemon")]
use std::env;
#[cfg(feature = "daemon")]
use tokio::runtime::Builder;
#[cfg(feature = "daemon")]
use tower_lsp_server::{LspService, Server};

#[cfg(feature = "daemon")]
pub fn execute_daemon(_interval_ms: u64) -> Result<()> {
    // 1. Resolve repository root
    let current_dir = env::current_dir().into_diagnostic()?;
    let root = match gix::discover(&current_dir) {
        Ok(repo) => {
            let path = repo
                .workdir()
                .ok_or_else(|| miette!("Repo discovery failed: no workdir found"))?
                .to_path_buf();
            Utf8PathBuf::from_path_buf(path)
                .map_err(|_| miette!("Invalid UTF-8 path in repo root"))?
        }
        Err(_) => Utf8PathBuf::from_path_buf(current_dir)
            .map_err(|_| miette!("Invalid UTF-8 path in current directory"))?,
    };

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

        let db_path = root.join(".ledgerful").join("state").join("ledger.db");
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
