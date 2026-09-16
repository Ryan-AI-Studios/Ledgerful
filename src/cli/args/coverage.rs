use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum FederateCommands {
    /// Export public interfaces for other repositories to consume
    Export {
        /// Preview the schema without writing to .ledgerful/state/schema.json
        #[arg(long, short = 'd', conflicts_with = "out")]
        dry_run: bool,
        /// Pure camelCase preview JSON on stdout (`kind: federateExportPreview`); does not write
        #[arg(long, conflicts_with = "out")]
        json: bool,
        /// Custom output path for the schema file
        #[arg(long, short)]
        out: Option<String>,
        /// Preview only: maximum interfaces to display (default 200; ignored on write)
        #[arg(
            short = 'l',
            long,
            default_value_t = 200,
            value_parser = clap::value_parser!(u64).range(1..=5000)
        )]
        limit: u64,
    },
    /// Scan sibling directories for Ledgerful schemas
    Scan,
    /// Show status of federated links
    Status {
        /// Emit pure camelCase federated-peer JSON on stdout (`schemaVersion: 1`)
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServiceSubcommands {
    /// List current service topology from the index (inventory; not a working-tree diff)
    #[command(name = "list", visible_alias = "diff")]
    Diff(crate::commands::services_diff::ServicesDiffArgs),
}
