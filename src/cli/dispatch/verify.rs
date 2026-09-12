use crate::cli::args::VerifyArgs;
use crate::commands::verify::{ExecuteVerifyOpts, refuse_mixed_verify_diagnostics};
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
    // Mix reject first so `--json --health --signatures` never reaches execute
    // or the signatures walk (0321).
    refuse_mixed_verify_diagnostics(health, dry_run, signatures, chain, against_export.is_some())?;
    if signatures || chain || against_export.is_some() {
        crate::commands::verify::verify_ledger_signatures_with_options(
            layout,
            signatures,
            chain,
            strict_signatures,
            against_export.as_deref(),
            exact,
            json,
        )
    } else {
        crate::commands::verify::execute_verify(ExecuteVerifyOpts {
            command,
            tx_id,
            timeout_secs: timeout,
            no_predict,
            explain,
            entity,
            health,
            dry_run,
            scope: crate::verify::plan::resolve_verify_scope(scope, dry_run),
            auto_index,
            allow_full_fallback,
            json,
            verbose,
        })
    }
}

#[cfg(test)]
mod dispatch_verify_mix_tests {
    use crate::commands::verify::refuse_mixed_verify_diagnostics;

    #[test]
    fn dispatch_verify_json_health_signatures_refuses_here() {
        let err = refuse_mixed_verify_diagnostics(true, false, true, false, false)
            .expect_err("health+signatures");
        let msg = format!("{err}");
        assert!(
            msg.contains("--health") && msg.contains("--signatures"),
            "{msg}"
        );
    }

    #[test]
    fn dispatch_verify_json_dry_run_chain_refuses_here() {
        let err = refuse_mixed_verify_diagnostics(false, true, false, true, false)
            .expect_err("dry-run+chain");
        let msg = format!("{err}");
        assert!(
            msg.contains("--dry-run") && (msg.contains("--chain") || msg.contains("signatures")),
            "{msg}"
        );
    }

    #[test]
    fn dispatch_verify_health_dry_run_refuses_here() {
        assert!(refuse_mixed_verify_diagnostics(true, true, false, false, false).is_err());
    }
}
