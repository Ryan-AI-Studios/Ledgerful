use crate::cli::args::IndexArgs;
use miette::Result;

pub(super) fn dispatch_index(args: IndexArgs) -> Result<()> {
    crate::commands::index::execute_index(crate::commands::index::IndexArgs {
        incremental: args.incremental,
        full: args.full,
        check: args.check,
        strict: args.strict,
        json: args.json,
        analyze_graph: args.analyze_graph,
        docs: args.docs,
        contracts: args.contracts,
        semantic: args.semantic,
        scip: args.scip,
        auto_scip: args.auto_scip,
        export_docs: args.export_docs,
        doc_type: args.doc_type,
        concurrency: args.concurrency,
        semantic_dry_run: args.semantic_dry_run,
        fast: args.fast,
        repair_metadata: args.repair_metadata,
        dry_run: args.dry_run,
        yes: args.yes,
    })
}
