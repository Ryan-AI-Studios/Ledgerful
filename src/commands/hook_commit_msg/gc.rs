//! Pending-sidecar GC for the commit-msg hook.
//!
//! Amend-reuse and observe-orphan must not fall through to `start_change`
//! (duplicate TXs / clobbered sidecars). [`GcOutcome::EarlyReturnOk`] is
//! that contract; a `Result<()>` helper cannot express it.

use crate::commands::hook_sidecar::{
    CODE_PROMOTE_ORPHAN, GcContext, PendingHookTx, RECOVER_HINT, editmsg_hash, head_message_hash,
    is_gc_eligible,
};
use crate::config::model::Config;
use crate::ledger::TransactionManager;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::Result;
use std::fs;

/// Result of sidecar GC. `Ok(())` on the helper is not enough: amend-reuse
/// and observe-orphan are successful early returns from execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GcOutcome {
    /// No sidecar, or a true-stale sidecar was rolled back. Continue execute.
    Proceed,
    /// Amend-reuse or observe-orphan retained. Caller must `return Ok(())`.
    EarlyReturnOk,
}

pub(super) struct GcArgs<'a> {
    pub layout: &'a Layout,
    pub config: &'a Config,
}

pub(super) fn gc_pending_sidecar(args: GcArgs<'_>) -> Result<GcOutcome> {
    let layout = args.layout;
    let config = args.config;
    let repo_root = layout.root.as_std_path();

    // Proactive GC: clean up true-stale sidecars only (shared policy with 0035/0074).
    // Promote-failed and HEAD-matching orphans are GC-ineligible.
    let sidecar_path = layout.state_subdir().join("pending_hook_tx");
    if sidecar_path.exists() {
        match fs::read_to_string(&sidecar_path) {
            Ok(content) => match serde_json::from_str::<PendingHookTx>(&content) {
                Ok(pending) => {
                    let head_hash = head_message_hash(repo_root);
                    let edit_hash = editmsg_hash(repo_root);
                    let ctx = GcContext {
                        head_msg_hash: head_hash.as_deref(),
                        editmsg_hash: edit_hash.as_deref(),
                    };

                    let matches_editmsg = edit_hash
                        .as_deref()
                        .is_some_and(|h| h == pending.commit_msg_hash);
                    let matches_head = head_hash
                        .as_deref()
                        .is_some_and(|h| h == pending.commit_msg_hash);
                    let promote_failed = pending.is_promote_failed();

                    if matches_editmsg && !promote_failed {
                        // Sidecar matches the active commit-msg (amend/re-run). Keep it.
                        return Ok(GcOutcome::EarlyReturnOk);
                    }

                    if promote_failed || matches_head {
                        // Orphan: previous commit succeeded but promote failed/skipped.
                        let detail = if promote_failed {
                            pending.promote_error.as_deref().unwrap_or("promote failed")
                        } else {
                            "HEAD-matching pending without successful promote"
                        };
                        if config.gate.is_enforce() {
                            return Err(miette::miette!(
                                "[{}] Promote orphan retained (tx {}): {}. \
                                 Next commit blocked under enforce until recovery. \
                                 Recover with: {}",
                                CODE_PROMOTE_ORPHAN,
                                pending.tx_id,
                                detail,
                                RECOVER_HINT
                            ));
                        }
                        // Single emission via cli_summary (0093 DoD-9); warn! → stderr.
                        tracing::warn!(
                            target: "cli_summary",
                            "[Ledgerful] WARNING [{}]: promote orphan retained (tx {}): {}. \
                             Recover with: {}",
                            CODE_PROMOTE_ORPHAN,
                            pending.tx_id,
                            detail,
                            RECOVER_HINT
                        );
                        // Observe: do not GC; continue so the new commit can proceed with a banner.
                        // Still block writing a second concurrent sidecar — hard-fail only under enforce.
                        // Under observe we return Ok early so we don't clobber the orphan.
                        return Ok(GcOutcome::EarlyReturnOk);
                    }

                    if is_gc_eligible(&pending, &ctx) {
                        tracing::warn!(
                            "Found stale pending sidecar (does not match HEAD). Rolling back pending transaction and cleaning up."
                        );

                        match StorageManager::init_with_layout(layout) {
                            Ok(mut storage) => {
                                let mut tx_mgr = TransactionManager::new(
                                    &mut storage,
                                    layout.root.clone().into(),
                                    config.clone(),
                                );
                                if let Err(e) = tx_mgr.rollback_change(
                                    pending.tx_id.clone(),
                                    "Stale sidecar cleaned up by commit-msg hook".to_string(),
                                ) {
                                    return Err(miette::miette!(
                                        "Failed to rollback stale pending transaction {}: {}",
                                        pending.tx_id,
                                        e
                                    ));
                                }
                            }
                            Err(e) => {
                                return Err(miette::miette!(
                                    "Failed to initialize storage for sidecar rollback: {}",
                                    e
                                ));
                            }
                        }

                        if let Err(e) = fs::remove_file(&sidecar_path) {
                            tracing::warn!("Failed to remove stale sidecar file: {}", e);
                        }
                    }
                }
                Err(e) => {
                    // Unparseable sidecars are still removable (cannot recover).
                    tracing::warn!("Failed to parse pending hook sidecar for GC: {}", e);
                    if let Err(e) = fs::remove_file(&sidecar_path) {
                        tracing::warn!("Failed to remove unparseable sidecar file: {}", e);
                    }
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read pending hook sidecar for GC: {}", e);
                if let Err(e) = fs::remove_file(&sidecar_path) {
                    tracing::warn!("Failed to remove unreadable sidecar file: {}", e);
                }
            }
        }
    }

    Ok(GcOutcome::Proceed)
}
