use super::helpers::{parse_category_from_message, risk_from_category};
use crate::commands::helpers::get_layout;
use crate::commands::hook_sidecar::{PendingHookTx, hash_message, write_pending_sidecar};
use crate::config::model::Config;
use crate::ledger::crypto::{LedgerSignInput, sign_ledger_entry_v2};
use crate::ledger::types::{ChangeType, EntryType};
use crate::ledger::{Category, TransactionManager, TransactionRequest};
use crate::state::storage::StorageManager;
use miette::Result;

pub(super) struct SilentRecordArgs<'a> {
    pub config: &'a Config,
    pub entity: &'a str,
    pub what: &'a str,
    pub why: &'a str,
    pub risk: &'a str,
    pub related: Vec<String>,
    pub related_files: &'a str,
    pub raw_commit_msg: &'a str,
    pub snapshot_id: Option<i64>,
    /// When true, summary is already `[SKIPPED]`-prefixed; observed=false under enforce.
    pub skipped: bool,
}

/// Risk for durable `[SKIPPED]` coverage rows under enforce.
///
/// Non-TRIVIAL so post-commit promote sets `verification_status = Unverified`
/// (phase0: SKIPPED is never Verified and never silent None-as-green).
pub const SKIPPED_COVERAGE_RISK: &str = "MEDIUM";

/// Prefix for durable skip coverage summaries.
pub const SKIPPED_SUMMARY_PREFIX: &str = "[SKIPPED]";

/// Build a durable SKIPPED summary line from a commit subject.
pub fn skipped_coverage_summary(subject_line: &str) -> String {
    format!("{SKIPPED_SUMMARY_PREFIX} {subject_line}")
}

pub(crate) struct RecordEnforceSkippedArgs<'a> {
    pub config: &'a Config,
    pub entity: &'a str,
    pub related_files: &'a str,
    pub raw_commit_msg: &'a str,
    pub why: &'a str,
    pub snapshot_id: Option<i64>,
}

/// Write a durable PENDING + `[SKIPPED]` sidecar under enforce.
///
/// Shared by adaptive trivial bypass and TUI Skip so both paths produce the
/// same coverage model (CHORE category, MEDIUM risk → Unverified on promote).
pub(crate) fn record_enforce_skipped(args: RecordEnforceSkippedArgs<'_>) -> Result<()> {
    let subject = args
        .raw_commit_msg
        .lines()
        .next()
        .unwrap_or("skipped")
        .trim();
    silently_record_ledger(SilentRecordArgs {
        config: args.config,
        entity: args.entity,
        what: &skipped_coverage_summary(subject),
        why: args.why,
        risk: SKIPPED_COVERAGE_RISK,
        related: Vec::new(),
        related_files: args.related_files,
        raw_commit_msg: args.raw_commit_msg,
        snapshot_id: args.snapshot_id,
        skipped: true,
    })
}

pub(super) struct LinkPendingArgs<'a> {
    pub config: &'a Config,
    pub tx_id: &'a str,
    pub related_files: &'a str,
    pub raw_commit_msg: &'a str,
    pub snapshot_id: Option<i64>,
}

/// Link an existing PENDING TX to this commit via sidecar (no `start_change`, no LLM).
pub(super) fn link_pending_provenance(args: LinkPendingArgs<'_>) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::init_with_layout(&layout)?;
    let tx_mgr = TransactionManager::new(
        &mut storage,
        layout.root.clone().into(),
        args.config.clone(),
    );

    let tx = tx_mgr
        .get_transaction(args.tx_id)
        .map_err(|e| miette::miette!("Failed to load pending TX {}: {}", args.tx_id, e))?
        .ok_or_else(|| miette::miette!("Pending TX {} not found for LinkPending", args.tx_id))?;

    if !tx.status.eq_ignore_ascii_case("PENDING") {
        return Err(miette::miette!(
            "TX {} is not PENDING (status={}); cannot LinkPending",
            args.tx_id,
            tx.status
        ));
    }

    let subject = args
        .raw_commit_msg
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let body = args
        .raw_commit_msg
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    // Summary/reason from pending planned_action / commit subject (B1).
    let summary = tx
        .planned_action
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            if subject.is_empty() {
                format!("Link pending TX {}", args.tx_id)
            } else {
                subject.clone()
            }
        });
    let reason = if !body.is_empty() {
        body
    } else if !subject.is_empty() {
        subject
    } else {
        summary.clone()
    };
    // MUST use pending TX category (and derived risk/entry_type). Promote
    // verifies signatures against `tx.category`, not a re-parsed message
    // category — agent planned_action rarely has a conventional prefix.
    let category = tx.category;
    let risk = risk_from_category(category).to_string();

    let entity_normalized = tx_mgr
        .entity_normalized(&tx.entity)
        .unwrap_or_else(|_| tx.entity_normalized.clone());

    write_signed_sidecar_for_tx(WriteSidecarArgs {
        config: args.config,
        layout: &layout,
        tx_id: args.tx_id.to_string(),
        entity: &tx.entity,
        entity_normalized: &entity_normalized,
        category,
        what: &summary,
        why: &reason,
        risk: &risk,
        related: Vec::new(),
        related_files: args.related_files,
        raw_commit_msg: args.raw_commit_msg,
        snapshot_id: args.snapshot_id,
        skipped: false,
        observe_warned: false,
    })
}

struct WriteSidecarArgs<'a> {
    config: &'a Config,
    layout: &'a crate::state::layout::Layout,
    tx_id: String,
    entity: &'a str,
    entity_normalized: &'a str,
    category: Category,
    what: &'a str,
    why: &'a str,
    risk: &'a str,
    related: Vec<String>,
    related_files: &'a str,
    raw_commit_msg: &'a str,
    snapshot_id: Option<i64>,
    skipped: bool,
    observe_warned: bool,
}

/// Shared post-`start_change` body: sign + write `pending_hook_tx` sidecar.
///
/// Used by silent hook path (after `start_change`) and LinkPending (existing TX).
fn write_signed_sidecar_for_tx(args: WriteSidecarArgs<'_>) -> Result<()> {
    let committed_at = chrono::Utc::now().to_rfc3339();

    let tickets = args.related.join(", ");
    let combined_related = if tickets.is_empty() {
        args.related_files.to_string()
    } else {
        format!("{} | {}", tickets, args.related_files)
    };

    // Match commit_change basis: author from git, origin LOCAL, entry_type from category.
    let author = {
        let policy = args.config.verify.effective_process_policy();
        let read = |key: &str| -> Option<String> {
            crate::git::git_command_with_policy(&policy)
                .ok()?
                .args(["config", key])
                .current_dir(args.layout.root.as_std_path())
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if s.is_empty() { None } else { Some(s) }
                    } else {
                        None
                    }
                })
        };
        read("user.name")
            .or_else(|| read("user.email"))
            .unwrap_or_else(|| "unknown".to_string())
    };
    let entry_type = if args.category == Category::Architecture {
        EntryType::Architecture
    } else {
        EntryType::Implementation
    };
    let sign_input = LedgerSignInput::for_new_commit(
        &args.tx_id,
        args.category,
        args.what,
        args.why,
        &committed_at,
        args.entity,
        args.entity_normalized,
        ChangeType::Modify,
        entry_type,
        &author,
        Some(args.risk),
        false,
        Some(&combined_related),
        "LOCAL",
    );
    let sign_result = sign_ledger_entry_v2(&sign_input);
    let (signature, pub_key) = match sign_result {
        Ok(keys) => keys,
        Err(e) => {
            if args.config.intent.require_signing {
                return Err(miette::miette!(
                    "Signing failed and require_signing is true: {}",
                    e
                ));
            } else {
                tracing::warn!(
                    "Ledger entry signing failed (continuing as require_signing=false): {}",
                    e
                );
                (None, None)
            }
        }
    };

    // SKIPPED under enforce: observed false/None. Observe soft-skip does not reach here.
    let observed = if args.skipped {
        if args.config.gate.is_observe() {
            Some(true)
        } else {
            None
        }
    } else if args.observe_warned {
        Some(true)
    } else {
        None
    };

    let pending = PendingHookTx {
        tx_id: args.tx_id,
        commit_msg_hash: hash_message(&crate::util::text::clean_commit_msg(args.raw_commit_msg)),
        summary: args.what.to_string(),
        reason: args.why.to_string(),
        committed_at: Some(committed_at),
        risk: Some(args.risk.to_string()),
        related_tickets: Some(combined_related),
        signature,
        public_key: pub_key,
        snapshot_id: args.snapshot_id,
        observed,
        promote_failed: None,
        promote_error: None,
    };

    let sidecar_path = args.layout.state_subdir().join("pending_hook_tx");
    write_pending_sidecar(sidecar_path.as_std_path(), &pending)?;

    Ok(())
}

pub(super) fn silently_record_ledger(args: SilentRecordArgs<'_>) -> Result<()> {
    let layout = get_layout()?;
    let category = if args.skipped {
        Category::Chore
    } else {
        parse_category_from_message(args.what)
    };
    let mut storage = StorageManager::init_with_layout(&layout)?;
    let mut tx_mgr = TransactionManager::new(
        &mut storage,
        layout.root.clone().into(),
        args.config.clone(),
    );

    // B6: silent hook path tags source HOOK (agent CLI stays default "CLI").
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category,
            entity: args.entity.to_string(),
            planned_action: Some(args.what.to_string()),
            source: Some("HOOK".into()),
            ..Default::default()
        })
        .map_err(|e| miette::miette!("{}", e))?;

    let observe_warned = tx_mgr.observe_warned();
    let entity_normalized = tx_mgr
        .entity_normalized(args.entity)
        .unwrap_or_else(|_| args.entity.to_string());

    write_signed_sidecar_for_tx(WriteSidecarArgs {
        config: args.config,
        layout: &layout,
        tx_id,
        entity: args.entity,
        entity_normalized: &entity_normalized,
        category,
        what: args.what,
        why: args.why,
        risk: args.risk,
        related: args.related,
        related_files: args.related_files,
        raw_commit_msg: args.raw_commit_msg,
        snapshot_id: args.snapshot_id,
        skipped: args.skipped,
        observe_warned,
    })
}
