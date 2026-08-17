use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum FederateCommands {
    /// Export public interfaces for other repositories to consume
    Export {
        /// Preview the schema without writing to .ledgerful/state/schema.json
        #[arg(long, short = 'd')]
        dry_run: bool,
        /// Custom output path for the schema file
        #[arg(long, short)]
        out: Option<String>,
    },
    /// Scan sibling directories for Ledgerful schemas
    Scan,
    /// Show status of federated links
    Status,
}

#[derive(Subcommand, Debug)]
#[command(after_help = "\
There is no `services list` inventory subcommand. Use `services diff` for boundary/topology changes. Service symbols remain searchable via `search` / graph surfaces.
")]
pub enum ServiceSubcommands {
    /// Show service boundary changes and topology
    Diff(crate::commands::services_diff::ServicesDiffArgs),
}
