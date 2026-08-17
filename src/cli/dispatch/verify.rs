use crate::cli::args::VerifyArgs;
use miette::Result;

pub(super) fn dispatch_verify(
    layout: &crate::state::layout::Layout,
    args: VerifyArgs,
    verbose: bool,
) -> Result<()> {
    let VerifyArgs {
        command,
        tx_id,
        timeout,
        no_predict,
        explain,
        entity,
        health,
        signatures,
        chain,
        against_export,
        exact,
        strict_signatures,
        dry_run,
        scope,
        auto_index,
        allow_full_fallback,
        json,
    } = args;
    if exact && against_export.is_none() {
        return Err(miette::miette!(
            "--exact requires --against-export <path> (snapshot equality against a retained head)"
        ));
    }
    if signatures || chain || against_export.is_some() {
        if json {
            return Err(miette::miette!(
                "verify --json cannot be combined with --signatures, --chain, or --against-export"
            ));
        }
        crate::commands::verify::verify_ledger_signatures_with_options(
            layout,
            signatures,
            chain,
            strict_signatures,
            against_export.as_deref(),
            exact,
        )
    } else {
        crate::commands::verify::execute_verify(
            command,
            tx_id,
            timeout,
            no_predict,
            explain,
            entity,
            health,
            dry_run,
            scope,
            auto_index,
            allow_full_fallback,
            json,
            verbose,
        )
    }
}
