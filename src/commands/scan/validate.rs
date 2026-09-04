use crate::cli::args::ScanImpactMode;
use miette::Result;
use std::path::PathBuf;

/// Validate combinations of `scan` flags.
///
/// Enforces: `--pr` is mutually exclusive with `--impact` and `--base-ref`;
/// `--format` requires `--pr`; `--summary`/`--json` are not valid with `--pr`;
/// `--out` with `--pr` requires `--format json`; `--summary` alone still requires
/// `--impact` (impact brief). Bare `--json`/`--out` without `--impact` are allowed
/// (0180 gitScan envelope) — they do **not** auto-run impact.
pub(super) fn validate_scan_args(
    pr: &Option<String>,
    base_ref: &Option<String>,
    format: &Option<String>,
    impact: bool,
    summary: bool,
    json: bool,
    out: &Option<PathBuf>,
) -> Result<()> {
    if pr.is_some() && impact {
        return Err(miette::miette!(
            "`--pr` and `--impact` are mutually exclusive"
        ));
    }

    if pr.is_some() && base_ref.is_some() {
        return Err(miette::miette!(
            "--pr and --base-ref are mutually exclusive"
        ));
    }

    // --format (any value, including "text") requires --pr. An explicit
    // `--format text` without `--pr` is rejected — it is indistinguishable
    // from the default only when the flag is absent, not when the user sets
    // it explicitly.
    if pr.is_none() && format.is_some() {
        return Err(miette::miette!("--format requires --pr"));
    }

    if let Some(fmt) = format
        && !matches!(fmt.as_str(), "json" | "text")
    {
        return Err(miette::miette!(
            "unsupported --format '{}'; use 'json' or 'text'",
            fmt
        ));
    }

    if pr.is_some() && (summary || json) {
        return Err(miette::miette!(
            "--summary and --json are not compatible with --pr; use --format json or --format text"
        ));
    }

    if pr.is_some() && out.is_some() && format.as_deref() != Some("json") {
        return Err(miette::miette!("--out with --pr requires --format json"));
    }

    // 0180-E: --summary still means impact brief — no PR --format tip here.
    if pr.is_none() && !impact && summary {
        return Err(miette::miette!(
            "--summary requires --impact (impact brief summary)"
        ));
    }

    Ok(())
}

/// Reject `--blast-depth` without a path that runs impact enrichment (no silent no-op).
pub(super) fn validate_blast_depth_requires_impact(
    run_impact: bool,
    pr: &Option<String>,
    blast_depth: Option<u32>,
) -> Result<()> {
    if blast_depth.is_some() && !run_impact {
        // --pr is a separate PR-scan surface and does not run impact enrichment.
        if pr.is_some() {
            return Err(miette::miette!(
                "--blast-depth is not used with --pr; use scan --impact --blast-depth N (or impact --blast-depth N)"
            ));
        }
        return Err(miette::miette!(
            "--blast-depth requires --impact (structural blast is part of impact enrichment)"
        ));
    }
    Ok(())
}

/// Reject `--mode` without `--impact` before gitScan dispatch (0227).
pub(super) fn validate_mode_requires_impact(
    run_impact: bool,
    mode: Option<ScanImpactMode>,
) -> Result<()> {
    if mode.is_some() && !run_impact {
        return Err(miette::miette!("--mode requires --impact"));
    }
    Ok(())
}
