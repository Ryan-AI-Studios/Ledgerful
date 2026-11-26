//! Contained checkout observations for the bounded architecture proposal.
use super::super::baseline_discovery::{self, ProposalDiscoveryBudget, RetainedDiscovery};
use super::capture_budget::CapturePhase;
use super::index_inputs::{MatchedIndexInputs, ProposalSourceBudget};
use super::paired_checkout::CheckoutLimits;
use super::{ContainedRoot, InventoryLimits, Observation, ReadBudget, ReadError};
use crate::change_evidence::contained::Identity;
use crate::change_evidence::scope::ScopeGuard;
use crate::commands::architecture::proposal::AMBIENT_GIT_CONFIG_OVERRIDE;
use crate::commands::baseline::extract::capture_snapshot::CaptureSnapshot;
use crate::state::layout::{Layout, STATE_DIR, StateDirOrigin};
use gix::ObjectId;
use std::cell::Cell;
use std::path::Path;

type Result<T> = std::result::Result<T, &'static str>;
const WORK_UNITS: usize = 512 * 1024 * 1024;
const MAX_WALK_ENTRIES: usize = 20_000;
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONFIG_BYTES: usize = 64 * 1024;

pub(crate) struct ProposalBudget {
    entries: Cell<usize>,
    bytes: Cell<usize>,
}

impl ProposalBudget {
    pub(crate) fn new() -> Self {
        Self {
            entries: Cell::new(MAX_WALK_ENTRIES),
            bytes: Cell::new(MAX_SOURCE_BYTES),
        }
    }
}

pub(crate) fn discover_layout(cwd: &Path, phase: CapturePhase<'_>) -> Result<(Layout, Identity)> {
    let current = std::env::current_dir().map_err(|_| "UNSUPPORTED_REPO")?;
    let explicit = cwd != Path::new(".");
    let cwd = if explicit { cwd } else { &current };
    if cwd.as_os_str().len() > 4_096 || cwd.components().take(65).count() > 64 {
        return Err("LIMIT_EXCEEDED");
    }
    // The contained routine checks every ancestor with exact spelling and no
    // links/reparse points, including a Windows 8.3 alias retained in CWD.
    let io = ReadBudget::new(0, phase.deadline, phase.interrupted);
    let cwd_identity = ContainedRoot::open(cwd, &io)
        .map_err(|_| "STORAGE_UNAVAILABLE")?
        .identity();
    if explicit
        && ContainedRoot::open(&current, &io)
            .map_err(|_| "CONTEXT_CHANGED")?
            .identity()
            != cwd_identity
    {
        return Err("CONTEXT_CHANGED");
    }
    for path in cwd.ancestors() {
        let root = ContainedRoot::open(path, &io).map_err(|_| "STORAGE_UNAVAILABLE")?;
        match root.observe_entry(".git", true, &io) {
            Err(ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("UNSUPPORTED_REPO"),
            Ok(_) => {
                let utf8 = camino::Utf8Path::from_path(path).ok_or("UNSUPPORTED_REPO")?;
                return Ok((
                    Layout {
                        root: utf8.to_owned(),
                        state_dir: utf8.join(STATE_DIR),
                    },
                    cwd_identity,
                ));
            }
        }
    }
    Err("UNSUPPORTED_REPO")
}

pub(crate) struct ProposalCheckout<'a> {
    observation: Observation<'a>,
    guard: ScopeGuard,
    scope_digest: [u8; 32],
    expected_head: ObjectId,
    architecture: (Identity, Vec<u8>),
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ProposalInputs {
    discovery: RetainedDiscovery,
    indexed: MatchedIndexInputs,
    tree: ObjectId,
}

impl ProposalInputs {
    pub(crate) fn config_digest(&self) -> blake3::Hash {
        self.discovery.config_digest
    }

    pub(crate) fn stale_threshold_days(&self) -> u64 {
        self.discovery.stale_threshold_days
    }
}

impl<'a> ProposalCheckout<'a> {
    pub(crate) fn open(
        layout: &Layout,
        expected_head: ObjectId,
        phase: CapturePhase<'a>,
        config_path: &str,
    ) -> Result<Self> {
        let origin = StateDirOrigin::RepositoryLocal;
        let mut observation = Observation::open_scoped_metered(
            layout,
            origin,
            CheckoutLimits {
                inventory: InventoryLimits {
                    entries: 0,
                    directories: 0,
                },
                read_bytes: phase.remaining_read_bytes(),
                work_units: WORK_UNITS,
            },
            phase,
        )
        .map_err(|error| limit_or(error, "STORAGE_UNAVAILABLE"))?;
        observation.allow_regular_pack_index_cache = true;
        observation
            .observe_local_git_config_with(true)
            .map_err(|error| limit_or(error, "UNSUPPORTED_REPO"))?;
        let guard = observation
            .confirm_scoped_guard(layout, origin, expected_head)
            .map_err(|error| match error {
                "head-mismatch" => "HEAD_MISMATCH",
                _ => limit_or(error, "UNSUPPORTED_REPO"),
            })?;
        let scope_digest = native_scope_digest(&observation, &guard)?;
        let architecture = read_architecture(&mut observation, config_path)?;
        Ok(Self {
            observation,
            guard,
            scope_digest,
            expected_head,
            architecture,
        })
    }

    pub(crate) fn observe_inputs(
        &mut self,
        snapshot: &CaptureSnapshot,
        budget: &'a ProposalBudget,
        layout: &Layout,
        from: &str,
        to: &str,
    ) -> Result<ProposalInputs> {
        #[cfg(windows)]
        let _ = layout;
        if snapshot.generation.head_hash.as_deref() != Some(&self.expected_head.to_string()) {
            return Err("HEAD_MISMATCH");
        }
        let mut endpoints = (false, false);
        for input in snapshot.witnesses() {
            endpoints.0 |= input.path == from;
            endpoints.1 |= input.path == to;
        }
        if endpoints != (true, true) {
            return Err("ENDPOINT_UNKNOWN");
        }
        let observation = &mut self.observation;
        let tree = observation
            .commit_facts(self.expected_head)
            .map_err(|error| limit_or(error, "UNSUPPORTED_REPO"))?
            .tree;
        let discovery = baseline_discovery::observe_with(
            &mut observation.root,
            &mut observation.io,
            &mut observation.work,
            #[cfg(target_os = "linux")]
            layout.root.as_std_path(),
            snapshot,
            Some(ProposalDiscoveryBudget {
                entries: &budget.entries,
            }),
        )
        .map_err(|error| match error {
            "discovery-config-unavailable" | "discovery-config-shape" => "INVALID_CONTEXT_CONFIG",
            AMBIENT_GIT_CONFIG_OVERRIDE => AMBIENT_GIT_CONFIG_OVERRIDE,
            _ => limit_or(error, "DISCOVERY_FAILED"),
        })?;
        discovery
            .membership
            .require_exact()
            .map_err(|_| "CONTENT_DRIFT")?;
        let indexed = observation
            .match_index_inputs_with(
                self.expected_head,
                tree,
                snapshot,
                Some(ProposalSourceBudget {
                    entries: &budget.entries,
                    bytes: &budget.bytes,
                }),
            )
            .map_err(|error| match error {
                "index-input-path-not-in-head" | "worktree-file-missing" => "ENDPOINT_UNKNOWN",
                "index-generation-mismatch" | "index-parser-version-mismatch" => {
                    "FRESHNESS_INDETERMINATE"
                }
                "input-changed" | "staged-index-changed" => "CONTEXT_CHANGED",
                _ => limit_or(error, "CONTENT_DRIFT"),
            })?;
        observation
            .confirm_staged_index()
            .map_err(context_changed)?;
        observation
            .confirm_pack_inventory()
            .map_err(context_changed)?;
        Ok(ProposalInputs {
            discovery,
            indexed,
            tree,
        })
    }

    pub(crate) fn confirm(
        mut self,
        mut second: Self,
        layout: &Layout,
        config_path: &str,
    ) -> Result<([u8; 32], Vec<u8>)> {
        let origin = StateDirOrigin::RepositoryLocal;
        if self.guard != second.guard
            || self.scope_digest != second.scope_digest
            || self.architecture != second.architecture
        {
            return Err("CONTEXT_CHANGED");
        }
        self.observation
            .root
            .confirm_same_scope(&second.observation.root, &mut self.observation.work)
            .map_err(context_changed)?;
        self.observation
            .confirm_local_git_config()
            .map_err(context_changed)?;
        self.observation
            .confirm_staged_index()
            .map_err(context_changed)?;
        if second
            .observation
            .confirm_scoped_guard(layout, origin, self.expected_head)
            .map_err(context_changed)?
            != self.guard
            || read_architecture(&mut second.observation, config_path).map_err(context_changed)?
                != self.architecture
        {
            return Err("CONTEXT_CHANGED");
        }
        Ok((self.scope_digest, self.architecture.1))
    }
}

fn native_scope_digest(observation: &Observation<'_>, guard: &ScopeGuard) -> Result<[u8; 32]> {
    #[cfg(windows)]
    let platform = b"windows-volume-file-id";
    #[cfg(target_os = "linux")]
    let platform = b"linux-device-inode";
    let root = &observation.root;
    let native_directory = |path| {
        root.observe_entry(path, true, &observation.io)
            .map_err(|error| limit_or(super::git_read_error(error), "STORAGE_UNAVAILABLE"))
    };
    let mut hash = blake3::Hasher::new_derive_key("ledgerful architecture proposal checkout v1");
    hash.update(platform);
    hash.update(guard.as_bytes());
    for directory in [
        root.identity(),
        native_directory(".git")?,
        native_directory(STATE_DIR)?,
    ] {
        hash.update(&directory.inventory_bytes());
    }
    Ok(*hash.finalize().as_bytes())
}

fn read_architecture(observation: &mut Observation<'_>, path: &str) -> Result<(Identity, Vec<u8>)> {
    let before = observation
        .root
        .observe_entry(path, false, &observation.io)
        .map_err(|error| limit_or(super::git_read_error(error), "INVALID_CONTEXT_CONFIG"))?;
    let bytes = observation
        .root
        .read(path, MAX_CONFIG_BYTES, &mut observation.io)
        .map_err(|error| limit_or(super::git_read_error(error), "INVALID_CONTEXT_CONFIG"))?;
    if observation
        .root
        .observe_entry(path, false, &observation.io)
        .map_err(|error| context_changed(super::git_read_error(error)))?
        != before
    {
        return Err("CONTEXT_CHANGED");
    }
    Ok((before, bytes))
}

pub(crate) fn context_changed(error: &'static str) -> &'static str {
    limit_or(error, "CONTEXT_CHANGED")
}

fn limit_or(error: &'static str, fallback: &'static str) -> &'static str {
    if error == "deadline"
        || error == "allocation"
        || error == "LIMIT_EXCEEDED"
        || error.ends_with("-cap")
    {
        "LIMIT_EXCEEDED"
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::super::charge_proposal_limit;
    use super::*;

    #[test]
    fn architecture_proposal_repeated_walk_and_source_charges_share_exact_caps() {
        let budget = ProposalBudget::new();
        let file = MAX_SOURCE_BYTES / 4;
        assert_eq!(
            charge_proposal_limit(&budget.bytes, 2 * file, "proposal-source-cap"),
            Ok(())
        );
        assert_eq!(
            charge_proposal_limit(&budget.bytes, 2 * file, "proposal-source-cap"),
            Ok(())
        );
        assert_eq!(budget.bytes.get(), 0);
        assert_eq!(
            charge_proposal_limit(&budget.bytes, 1, "proposal-source-cap"),
            Err("proposal-source-cap")
        );

        let initial_walk = MAX_WALK_ENTRIES / 2;
        assert_eq!(
            charge_proposal_limit(&budget.entries, initial_walk, "discovery-entry-cap"),
            Ok(())
        );
        assert_eq!(
            charge_proposal_limit(&budget.entries, initial_walk, "tree-walk-entry-cap"),
            Ok(())
        );
        assert_eq!(budget.entries.get(), 0);
        assert_eq!(
            charge_proposal_limit(&budget.entries, 1, "tree-walk-entry-cap"),
            Err("tree-walk-entry-cap")
        );
    }
}
