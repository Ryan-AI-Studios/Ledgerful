use clap::{Args, Subcommand, ValueEnum};

use crate::commands::graph_export::{DEFAULT_LIMIT, MAX_LIMIT};

/// `graph export` format. Names match the stderr `format:` token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum GraphExportFormatArg {
    Graphml,
    Cypher,
}

#[derive(Subcommand, Debug)]
pub enum GraphCommands {
    /// Write GraphML or Cypher for the Cozo knowledge graph
    Export(GraphExportArgs),
}

#[derive(Args, Debug)]
pub struct GraphExportArgs {
    /// Interchange format (required)
    #[arg(long, value_enum)]
    pub format: GraphExportFormatArg,
    /// Write the document to PATH instead of stdout
    #[arg(long)]
    pub output: Option<String>,
    /// Maximum nodes to emit (1..=1000)
    #[arg(
        long,
        default_value_t = DEFAULT_LIMIT,
        value_parser = clap::value_parser!(u64).range(1..=MAX_LIMIT)
    )]
    pub limit: u64,
    /// Root node id. Omit to export an id prefix.
    #[arg(long)]
    pub entity: Option<String>,
    /// Walk depth from --entity. Omitted depth is 2. Requires --entity.
    #[arg(long)]
    pub depth: Option<u32>,
}
