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
pub enum ServiceSubcommands {
    /// List current service topology from the index (inventory; not a working-tree diff)
    #[command(name = "list", visible_alias = "diff")]
    Diff(crate::commands::services_diff::ServicesDiffArgs),
}
