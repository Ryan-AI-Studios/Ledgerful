//! Parsers, classifier, and fetch for `ledgerful release pins` (0201).
//!
//! Own Latest fetch (`tag_name` + archive `assets[].digest`; optional peel).
//! Do not call SHA-required `fetch_github_latest` as the only Latest source.

mod classify;
mod emit;
mod fetch;
mod parse;
mod types;

use crate::commands::doctor::is_ledgerful_engine_worktree;
use classify::classify_pins;
use fetch::{fetch_latest_pins, remotes_from_fetch};
use miette::{IntoDiagnostic, Result};
use parse::{read_advisory, read_locals};
use types::{ClassifyPinsInput, LocalPins, ReleasePinsEnvelope, RemotePins};

pub(crate) use emit::{emit_release_pins, exit_code_for};
pub(crate) use types::PinFetchEndpoints;

fn resolve_engine_root() -> Result<Option<std::path::PathBuf>> {
    let current_dir = std::env::current_dir().into_diagnostic()?;
    if let Ok(layout) = crate::commands::helpers::get_layout() {
        let root = layout.root.as_std_path();
        if is_ledgerful_engine_worktree(root) {
            return Ok(Some(layout.root.into_std_path_buf()));
        }
    }
    if is_ledgerful_engine_worktree(&current_dir) {
        return Ok(Some(current_dir));
    }
    Ok(None)
}

pub(crate) fn collect_release_pins() -> Result<ReleasePinsEnvelope> {
    collect_release_pins_with(&PinFetchEndpoints::production())
}

pub(crate) fn collect_release_pins_with(
    endpoints: &PinFetchEndpoints,
) -> Result<ReleasePinsEnvelope> {
    let Some(engine_root) = resolve_engine_root()? else {
        return Ok(classify_pins(ClassifyPinsInput {
            is_engine: false,
            latest: None,
            fetch_error: false,
            locals: &LocalPins::default(),
            remotes: &RemotePins::unverified(),
            advisory: None,
        }));
    };

    let locals = read_locals(&engine_root);
    let advisory = read_advisory(&engine_root);
    let fetched = fetch_latest_pins(endpoints);
    let latest_owned = fetched.latest.as_ref().ok().cloned();
    let fetch_error = fetched.latest.is_err();
    let remotes = remotes_from_fetch(&fetched);
    Ok(classify_pins(ClassifyPinsInput {
        is_engine: true,
        latest: latest_owned.as_ref(),
        fetch_error,
        locals: &locals,
        remotes: &remotes,
        advisory,
    }))
}

#[cfg(test)]
mod tests;
