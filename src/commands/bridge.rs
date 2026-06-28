use crate::bridge::export::ExportArgs;
use clap::Subcommand;
use miette::Result;

#[derive(Subcommand, Debug)]
pub enum BridgeCommands {
    /// Export Ledgerful state for AI-Brains
    Export {
        /// Output path
        #[arg(long, short)]
        out: Option<String>,
        /// Print to stdout instead of writing to a file
        #[arg(long)]
        stdout: bool,
        /// Pretty print JSON output
        #[arg(long)]
        pretty: bool,
        /// Include hotspots
        #[arg(long)]
        hotspots: bool,
        /// Include ledger entries
        #[arg(long)]
        ledger: bool,
        /// Path scope for hotspots
        #[arg(long)]
        scope: Option<String>,
        /// Export structured MADR fields
        #[arg(long)]
        madr: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Import AI-Brains insights into Ledgerful
    Import {
        /// Input path (NDJSON)
        #[arg(long, short)]
        input: String,
    },
    /// Query AI-Brains for context
    Query {
        /// Query string
        query: String,
    },
}

pub fn execute(command: BridgeCommands) -> Result<()> {
    match command {
        BridgeCommands::Export {
            out,
            stdout,
            pretty,
            hotspots,
            ledger,
            scope,
            madr,
            json,
        } => {
            let scope_vec = scope.map(|s| s.split(',').map(|p| p.trim().to_string()).collect());
            let args = ExportArgs {
                out_path: out,
                stdout,
                pretty,
                hotspots,
                ledger,
                scope: scope_vec,
                madr,
                json,
            };
            crate::bridge::export::execute_export(args)
        }
        BridgeCommands::Import { input } => crate::bridge::import::execute_import(input),
        BridgeCommands::Query { query } => crate::bridge::client::execute_query(query),
    }
}
