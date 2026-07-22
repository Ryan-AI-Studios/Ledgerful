//! Shared pending-hook sidecar type and GC policy.
//!
//! Used by commit-msg, post-commit, status, doctor, verify, and recovery.
//! Binding is a **message-hash heuristic** (SHA-256 of cleaned commit message),
//! not a git object id — amend/reword can re-match hashes.
//!
//! GC eligibility (0074 / 0035 shared policy):
//! - `false` if `promote_failed` (orphan needs recovery)
//! - `false` if matches HEAD message-hash (orphan needs recovery)
//! - `false` if matches active COMMIT_EDITMSG (in-flight amend)
//! - `true` only for true-stale (neither HEAD nor editmsg)
//!
//! Unparseable sidecars remain removable (cannot recover).

use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Pending ledger transaction written by the commit-msg hook and promoted
/// (or retained as a promote-fail orphan) by the post-commit hook.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PendingHookTx {
    pub tx_id: String,
    pub commit_msg_hash: String,
    pub summary: String,
    pub reason: String,
    pub committed_at: Option<String>,
    pub risk: Option<String>,
    pub related_tickets: Option<String>,
    pub signature: Option<String>,
    pub public_key: Option<String>,
    pub snapshot_id: Option<i64>,
    /// Carried from the commit-msg hook through the pending sidecar so the
    /// post-commit hook can record whether the commit happened under observe
    /// mode. Stored as unsigned ledger metadata.
    pub observed: Option<bool>,
    /// Set when post-commit promote failed under enforce (or was retained under
    /// observe). GC-ineligible until recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promote_failed: Option<bool>,
    /// Human-readable promote error when `promote_failed` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promote_error: Option<String>,
}

impl PendingHookTx {
    /// Whether this sidecar is a promote-failure orphan that must not be GC'd.
    pub fn is_promote_failed(&self) -> bool {
        self.promote_failed.unwrap_or(false)
    }
}

/// SHA-256 hex of a cleaned commit message body (HEAD-bind heuristic).
pub fn hash_message(msg: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(msg.as_bytes());
    hex::encode(hasher.finalize())
}

/// Read and parse the pending sidecar, if present.
pub fn read_pending_sidecar(path: &Path) -> Result<Option<PendingHookTx>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).into_diagnostic()?;
    let pending: PendingHookTx = serde_json::from_str(&content).into_diagnostic()?;
    Ok(Some(pending))
}

/// Write the pending sidecar atomically enough for local hook use.
pub fn write_pending_sidecar(path: &Path, pending: &PendingHookTx) -> Result<()> {
    let content = serde_json::to_string(pending).into_diagnostic()?;
    fs::write(path, content).into_diagnostic()?;
    Ok(())
}

/// Mark a sidecar as promote-failed and persist it (never delete on this path).
pub fn mark_promote_failed(path: &Path, pending: &mut PendingHookTx, error: &str) -> Result<()> {
    pending.promote_failed = Some(true);
    pending.promote_error = Some(error.to_string());
    write_pending_sidecar(path, pending)
}

/// Inputs for the shared GC eligibility decision.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcContext<'a> {
    /// SHA-256 of cleaned HEAD commit message, if available.
    pub head_msg_hash: Option<&'a str>,
    /// SHA-256 of cleaned active COMMIT_EDITMSG, if available.
    pub editmsg_hash: Option<&'a str>,
}

/// Shared GC policy: only true-stale sidecars are eligible for removal.
///
/// Returns `false` (keep) when:
/// - `promote_failed` is set
/// - sidecar hash matches HEAD message-hash
/// - sidecar hash matches active COMMIT_EDITMSG
///
/// Returns `true` only when the sidecar is neither HEAD-matching nor
/// editmsg-matching and is not a promote-fail orphan.
pub fn is_gc_eligible(sidecar: &PendingHookTx, ctx: &GcContext<'_>) -> bool {
    if sidecar.is_promote_failed() {
        return false;
    }
    if let Some(head) = ctx.head_msg_hash
        && sidecar.commit_msg_hash == head
    {
        return false;
    }
    if let Some(edit) = ctx.editmsg_hash
        && sidecar.commit_msg_hash == edit
    {
        return false;
    }
    true
}

/// Load HEAD commit message hash from a repo root (best-effort).
pub fn head_message_hash(repo_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head_msg = String::from_utf8_lossy(&output.stdout).to_string();
    let cleaned = crate::util::text::clean_commit_msg(&head_msg);
    Some(hash_message(&cleaned))
}

/// Load COMMIT_EDITMSG hash from a repo root (best-effort).
pub fn editmsg_hash(repo_root: &Path) -> Option<String> {
    let editmsg_path = repo_root.join(".git").join("COMMIT_EDITMSG");
    if !editmsg_path.exists() {
        return None;
    }
    let edit_msg = fs::read_to_string(&editmsg_path).ok()?;
    let cleaned = crate::util::text::clean_commit_msg(&edit_msg);
    Some(hash_message(&cleaned))
}

/// Recovery hint printed on CRITICAL promote-orphan surfaces.
pub const RECOVER_HINT: &str = "ledgerful ledger recover-orphan --promote  OR  ledgerful ledger recover-orphan --abandon --reason \"...\"";

/// Stable doctor / status code: promote-fail orphan retained.
pub const CODE_PROMOTE_ORPHAN: &str = "PROMOTE_ORPHAN";
/// Stable doctor / status code: HEAD uncovered via promote-fail or HEAD-matching
/// pending sidecar (message-hash heuristic). **Not** a full material-HEAD-without-row scan.
pub const CODE_HEAD_UNCOVERED: &str = "HEAD_UNCOVERED";
/// Stable doctor code: intent.required=never while gate=enforce.
pub const CODE_INTENT_NEVER_UNDER_ENFORCE: &str = "INTENT_NEVER_UNDER_ENFORCE";
/// Stable doctor code: legacy Verified without bound verification run.
pub const CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY: &str = "PHANTOM_PROMOTED_WITHOUT_VERIFY";

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(hash: &str, promote_failed: bool) -> PendingHookTx {
        PendingHookTx {
            tx_id: "tx-1".to_string(),
            commit_msg_hash: hash.to_string(),
            summary: "s".to_string(),
            reason: "r".to_string(),
            committed_at: None,
            risk: None,
            related_tickets: None,
            signature: None,
            public_key: None,
            snapshot_id: None,
            observed: None,
            promote_failed: if promote_failed { Some(true) } else { None },
            promote_error: if promote_failed {
                Some("err".to_string())
            } else {
                None
            },
        }
    }

    #[test]
    fn gc_rejects_promote_failed() {
        let s = sample("aaa", true);
        let ctx = GcContext {
            head_msg_hash: Some("bbb"),
            editmsg_hash: Some("ccc"),
        };
        assert!(!is_gc_eligible(&s, &ctx));
    }

    #[test]
    fn gc_rejects_head_match() {
        let s = sample("headhash", false);
        let ctx = GcContext {
            head_msg_hash: Some("headhash"),
            editmsg_hash: None,
        };
        assert!(!is_gc_eligible(&s, &ctx));
    }

    #[test]
    fn gc_rejects_editmsg_match() {
        let s = sample("edithash", false);
        let ctx = GcContext {
            head_msg_hash: Some("other"),
            editmsg_hash: Some("edithash"),
        };
        assert!(!is_gc_eligible(&s, &ctx));
    }

    #[test]
    fn gc_allows_true_stale() {
        let s = sample("stale", false);
        let ctx = GcContext {
            head_msg_hash: Some("head"),
            editmsg_hash: Some("edit"),
        };
        assert!(is_gc_eligible(&s, &ctx));
    }

    #[test]
    fn gc_allows_stale_with_no_context() {
        let s = sample("stale", false);
        let ctx = GcContext::default();
        assert!(is_gc_eligible(&s, &ctx));
    }

    #[test]
    fn serde_defaults_missing_promote_fields() {
        let json = r#"{"tx_id":"t","commit_msg_hash":"h","summary":"s","reason":"r"}"#;
        let p: PendingHookTx = serde_json::from_str(json).unwrap();
        assert!(!p.is_promote_failed());
        assert!(p.promote_error.is_none());
    }

    #[test]
    fn hash_message_is_stable() {
        assert_eq!(hash_message("hello"), hash_message("hello"));
        assert_ne!(hash_message("hello"), hash_message("world"));
    }
}
