use crate::bridge::export::ExportArgs;
use clap::Subcommand;
use miette::Result;

#[derive(Subcommand, Debug)]
pub enum BridgeCommands {
    /// Export a BridgeRecord JSON snapshot (compact is NDJSON-compatible)
    Export {
        /// Output path (`-` means stdout)
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
        /// Path prefixes for `--hotspots` (comma-separated; slash-normalized; trailing `/` scopes a directory)
        #[arg(long, requires = "hotspots")]
        scope: Option<String>,
        /// Export structured MADR fields
        #[arg(long)]
        madr: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Overall emit budget seconds for `--hotspots` (`0` disables the wall clock; ignored without `--hotspots`)
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Import external insights (BridgeRecord NDJSON) into Ledgerful
    Import {
        /// Input path (NDJSON)
        #[arg(long, short)]
        input: String,
    },
    /// Query the bridge for context
    Query {
        /// Query words (unquoted multi-word OK). Flags may appear before or after.
        #[arg(value_name = "QUERY", num_args = 1.., required = true)]
        query: Vec<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
            timeout,
        } => {
            let scope_vec = normalize_export_scope(scope);
            let args = ExportArgs {
                out_path: out,
                stdout,
                pretty,
                hotspots,
                ledger,
                scope: scope_vec,
                madr,
                json,
                timeout,
            };
            crate::bridge::export::execute_export(args)
        }
        BridgeCommands::Import { input } => crate::bridge::import::execute_import(input),
        BridgeCommands::Query { query, json } => {
            crate::bridge::client::execute_query(query.join(" "), json)
        }
    }
}

fn normalize_export_scope(scope: Option<String>) -> Option<Vec<String>> {
    crate::bridge::export::normalize_scope_prefixes(scope.map(|s| vec![s]))
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::normalize_export_scope;

    #[test]
    fn normalize_export_scope__backslash_and_comma__forward_slash_prefixes() {
        let mut scoped = String::from("src");
        scoped.push(char::from_u32(92).expect("backslash"));
        scoped.push_str("bridge, docs");
        assert_eq!(
            normalize_export_scope(Some(scoped)),
            Some(vec!["src/bridge".to_string(), "docs".to_string()])
        );
    }

    #[test]
    fn normalize_export_scope__empty_tokens__none() {
        assert_eq!(normalize_export_scope(Some("  ,  ".to_string())), None);
        assert_eq!(normalize_export_scope(None), None);
    }
}
