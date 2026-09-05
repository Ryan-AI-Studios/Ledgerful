//! Commit-msg hook execute: never-gate → GC → staged → SoT → intent → record.

use super::gc::{GcArgs, GcOutcome, gc_pending_sidecar};
use super::intent::{IntentArgs, IntentOutcome, capture_intent};
use super::record::{LinkPendingArgs, link_pending_provenance};
use super::sot::{ProvenanceSotClass, resolve_provenance_sot};
use super::staged::{canonical_entity, capture_staged_snapshot, get_staged_files};
use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::commands::hook_sidecar::CODE_INTENT_NEVER_UNDER_ENFORCE;
use miette::{IntoDiagnostic, Result};
use std::fs;
use std::path::Path;

pub fn execute_hook_commit_msg(msg_file: &Path) -> Result<()> {
    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;

    // 1. intent.required=never under enforce → hard-fail + doctor CRITICAL code
    // MUST run before GC so disabled-intent repos do not roll back sidecars.
    if config.intent.required == "never" {
        if config.gate.is_enforce() {
            return Err(miette::miette!(
                "[{}] intent.required=never is not allowed under gate mode enforce. \
                 Set intent.required to \"always\" or switch gate mode to observe. \
                 Doctor will flag this as CRITICAL.",
                CODE_INTENT_NEVER_UNDER_ENFORCE
            ));
        }
        return Ok(());
    }

    let repo_root = layout.root.as_std_path();

    match gc_pending_sidecar(GcArgs {
        layout: &layout,
        config: &config,
    })? {
        GcOutcome::EarlyReturnOk => return Ok(()),
        GcOutcome::Proceed => {}
    }

    // 2. Read git staged files (snapshot deferred until after provenance classify — B7).
    let staged_files = get_staged_files(repo_root);
    if staged_files.is_empty() {
        return Ok(()); // Nothing staged, nothing to record
    }
    let entity = canonical_entity(&staged_files);
    let related_files = staged_files.join(", ");

    // 3. Read current commit message
    if !msg_file.exists() {
        return Err(miette::miette!(
            "Commit message file does not exist at '{}'",
            msg_file.display()
        ));
    }
    let raw_commit_msg = fs::read_to_string(msg_file)
        .into_diagnostic()?
        .trim()
        .to_string();

    // 3b. Provenance SoT classifier — after raw msg, BEFORE adaptive / conventional / LLM (B1).
    match resolve_provenance_sot(&raw_commit_msg, &layout, &config)? {
        ProvenanceSotClass::AlreadyCommitted { tx_id } => {
            tracing::info!(
                target: "cli_summary",
                "[Ledgerful] Provenance SoT: ledger TX {} already committed; skipping intent draft.",
                tx_id
            );
            return Ok(());
        }
        ProvenanceSotClass::LinkPending { tx_id } => {
            // Current commit snapshot (not the agent's start-time snapshot).
            let snapshot_capture = capture_staged_snapshot(&layout, repo_root);
            link_pending_provenance(LinkPendingArgs {
                config: &config,
                tx_id: &tx_id,
                related_files: &related_files,
                raw_commit_msg: &raw_commit_msg,
                snapshot_id: snapshot_capture.as_ref().map(|s| s.snapshot_id),
            })?;
            tracing::info!(
                target: "cli_summary",
                "[Ledgerful] Provenance SoT: linking open ledger TX {}; skipping intent draft.",
                tx_id
            );
            return Ok(());
        }
        ProvenanceSotClass::AmbiguousMulti => {
            tracing::warn!(
                target: "cli_summary",
                "[Ledgerful] Multiple open pending transactions found; falling back to hook intent capture."
            );
            // Fall through to HookFallback (adaptive / conventional / LLM).
        }
        ProvenanceSotClass::Fallback => {
            // HookFallback — existing path below.
        }
    }

    // Capture snapshot for diff stats (best-effort). After classify so
    // AlreadyCommitted skips wasted snapshot work (B7).
    let snapshot_capture = capture_staged_snapshot(&layout, repo_root);

    match capture_intent(IntentArgs {
        config: &config,
        layout: &layout,
        repo_root,
        msg_file,
        entity: &entity,
        related_files: &related_files,
        staged_files: &staged_files,
        raw_commit_msg: &raw_commit_msg,
        snapshot_id: snapshot_capture.as_ref().map(|s| s.snapshot_id),
    })? {
        IntentOutcome::Done => Ok(()),
        IntentOutcome::Abort => {
            eprintln!("[Ledgerful] Transaction aborted. Commit blocked.");
            std::process::exit(1);
        }
    }
}
