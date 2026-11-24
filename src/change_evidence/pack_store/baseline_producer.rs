//! Authorize explicit baseline publication from an exact index and checkout.

#[cfg(any(windows, target_os = "linux"))]
use super::{Observation, capture_budget::CaptureBudget, resolve_head};
use crate::state::layout::{Layout, StateDirOrigin};
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct BaselineFacts {
    pub(crate) head: Option<String>,
    pub(crate) repo_scope: Option<Uuid>,
    pub(crate) base: Option<String>,
    pub(crate) baseline_digest: Option<String>,
}

pub(crate) fn attest(
    layout: &Layout,
    origin: StateDirOrigin,
    name: &str,
    facts: &mut BaselineFacts,
) -> Result<DateTime<Utc>, Vec<String>> {
    #[cfg(any(windows, target_os = "linux"))]
    return platform::attest(layout, origin, name, facts);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (layout, origin, name, facts);
        Err(vec!["platform-unavailable".into()])
    }
}

pub(crate) enum PreparationVerdict {
    Prepared,
    Incomparable(Vec<String>),
    Unavailable(Vec<String>),
}

pub(crate) fn prepare(
    layout: &Layout,
    origin: StateDirOrigin,
    name: &str,
    facts: &mut BaselineFacts,
) -> Result<(DateTime<Utc>, PreparationVerdict), Vec<String>> {
    #[cfg(any(windows, target_os = "linux"))]
    return platform::prepare(layout, origin, name, facts);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (layout, origin, name, facts);
        Err(vec!["platform-unavailable".into()])
    }
}

#[cfg(any(windows, target_os = "linux"))]
mod platform {
    use super::*;
    use crate::change_evidence::pack_store::{
        capture_budget::CapturePhase,
        capture_db::{
            CaptureDbUnavailable,
            baseline_store::{BaselineCheckout, BaselineStore, LoadError, PreparedInputs},
        },
        changed_files::ChangedFileLimits,
        paired_checkout::{
            CheckoutInput, CheckoutLimits, CheckoutPairError, CheckoutRequest,
            FirstDefaultIndexedHeadPass, HeadCheckoutRequest, observe_prepare_pair,
        },
    };
    use crate::change_evidence::{
        baseline_discovery, buffer::Reservation, inventory::InventoryLimits,
    };
    use crate::commands::baseline::{
        FileEdgeSet, canonical_set_json, compare_edge_sets,
        extract::capture_snapshot::CaptureSnapshotError,
    };
    use crate::state::storage::{
        baselines::compute_payload_digest,
        change_evidence::{
            EvidenceStorageError, ExactPreparedKey, sql_error,
            writes::{
                AttestationInput, ScopeExpectation, WriteError,
                prepared::{
                    MAX_COMPARE_EDGES, PreparationRefusal, PreparedComparison, PreparedEvidence,
                    PreparedGit, PreparedIndexMarker,
                },
            },
        },
    };

    pub(super) fn attest(
        layout: &Layout,
        origin: StateDirOrigin,
        name: &str,
        facts: &mut BaselineFacts,
    ) -> Result<DateTime<Utc>, Vec<String>> {
        let budget = CaptureBudget::new();
        let output = attest_in_phase(layout, origin, name, facts, &budget);
        if output.is_err() && budget.phase().is_none() {
            return Err(reason("capture-budget"));
        }
        output
    }

    pub(super) fn prepare(
        layout: &Layout,
        origin: StateDirOrigin,
        name: &str,
        facts: &mut BaselineFacts,
    ) -> Result<(DateTime<Utc>, PreparationVerdict), Vec<String>> {
        let budget = CaptureBudget::new();
        let output = prepare_in_phase(layout, origin, name, facts, &budget);
        if output.is_err() && budget.phase().is_none() {
            return Err(reason("capture-budget"));
        }
        output
    }

    fn prepare_in_phase(
        layout: &Layout,
        origin: StateDirOrigin,
        name: &str,
        facts: &mut BaselineFacts,
        budget: &CaptureBudget,
    ) -> Result<(DateTime<Utc>, PreparationVerdict), Vec<String>> {
        let phase = budget.phase().ok_or_else(|| reason("capture-budget"))?;
        let limits = checkout_limits(phase);
        let (mut owner, head, guard) = current_owner(layout, origin, limits, phase)?;
        let base = owner
            .require_direct_parent(head)
            .map_err(|_| reason("pair-unavailable"))?;
        let commits = owner
            .observe_commit_pair(head, base)
            .map_err(|_| reason("pair-unavailable"))?;
        owner
            .visit_tree(commits.parent_tree, |_| Ok(()))
            .map_err(|_| reason("pair-unavailable"))?;
        owner
            .visit_tree(commits.head_tree, |_| Ok(()))
            .map_err(|_| reason("pair-unavailable"))?;
        let head_text = head.to_string();
        let base_text = base.to_string();
        facts.head = Some(head_text.clone());
        facts.base = Some(base_text.clone());
        let mut store = BaselineStore::open(owner, layout, origin, head, &guard, phase)
            .map_err(database_reason)?;
        let PreparedInputs {
            scope,
            baseline,
            snapshot,
            expectation,
        } = store
            .load_prepared_inputs(name, &head_text, &base_text)
            .map_err(load_reason)?;
        facts.repo_scope = Some(scope);
        facts.baseline_digest = Some(baseline.digest().to_owned());

        let (projection, unsupported_path) = {
            let owner = store.observation();
            match owner.observe_changed_files(
                head,
                base,
                ChangedFileLimits {
                    rows: 100,
                    path_bytes: 100 * 1024,
                },
            ) {
                Ok(files) => (
                    PreparedGit::from_files(&files).map_err(write_reason)?,
                    files.head_has_unsupported_checkout_path,
                ),
                Err(_) => (PreparedGit::Unavailable, false),
            }
        };
        let observed = Utc::now();
        let mut held_comparison = None;
        let (generation, comparison, marker, project_config) = if unsupported_path {
            // This proof covers immutable Git objects only. The strict checkout
            // proof cannot admit the HEAD, so no comparison or index marker is kept.
            let owner = store.observation();
            let (_, _, config) = baseline_discovery::read_project_config(
                &owner.root,
                &mut owner.io,
                &mut owner.work,
            )
            .map_err(native_reason)?;
            (
                snapshot.generation,
                PreparedComparison::Unavailable {
                    reason_code: PreparationRefusal::UnsupportedTreePath,
                },
                None,
                config,
            )
        } else {
            let proof = observe_prepare_pair(
                CheckoutRequest {
                    layout,
                    origin,
                    head,
                    parent: base,
                },
                &snapshot,
                limits,
                phase,
            )
            .map_err(pair_reason)?;
            if proof.guard() != &guard {
                return Err(reason("scope-unavailable"));
            }
            let config = proof.config_digest();
            if let Some(refusal) = proof.input_unavailable() {
                let code = PreparationRefusal::from_index_reason(refusal);
                (
                    snapshot.generation,
                    PreparedComparison::Unavailable { reason_code: code },
                    None,
                    config,
                )
            } else {
                let (generation, current) = snapshot
                    .finish_prepared(&proof, observed)
                    .map_err(snapshot_reason)?;
                let owner = store.observation();
                held_comparison = Some(reserve_comparison(owner, baseline.set(), &current)?);
                let comparison = PreparedComparison::from_delta(
                    baseline.set(),
                    &current,
                    compare_edge_sets(baseline.set(), &current),
                )
                .map_err(write_reason)?;
                let marker = PreparedIndexMarker::from_generation(
                    &generation,
                    proof.stale_threshold_days(),
                    proof.discovery_fingerprint(),
                )
                .map_err(write_reason)?;
                (generation, comparison, Some(marker), config)
            }
        };
        let verdict = match &comparison {
            PreparedComparison::Comparable(_) => PreparationVerdict::Prepared,
            PreparedComparison::Incomparable(value) => {
                let mut codes = value
                    .incomparable_reasons
                    .iter()
                    .map(|r| r.token().to_owned())
                    .collect::<Vec<_>>();
                codes.extend(value.current_input_reasons.iter().cloned());
                PreparationVerdict::Incomparable(codes)
            }
            PreparedComparison::Unavailable { reason_code } => {
                PreparationVerdict::Unavailable(reason(reason_code.code()))
            }
        };
        let key = ExactPreparedKey::new(&head_text, &base_text, name, baseline.digest())
            .map_err(storage_reason)?;
        let body = PreparedEvidence {
            git: projection,
            comparison,
            index_marker: marker,
        }
        .encode(&key)
        .map_err(write_reason)?;
        drop(held_comparison);
        let checkout = BaselineCheckout {
            layout,
            origin,
            guard: &guard,
            project_config,
        };
        store.confirm(checkout).map_err(database_reason)?;
        let committed = store
            .commit_preparation(
                expectation,
                &baseline,
                &generation,
                &body,
                observed,
                checkout,
            )
            .map_err(write_reason)?;
        facts.repo_scope = Some(committed.scope);
        Ok((observed, verdict))
    }

    fn reserve_comparison<'a>(
        owner: &mut Observation<'a>,
        baseline: &FileEdgeSet,
        current: &FileEdgeSet,
    ) -> Result<Reservation<'a>, Vec<String>> {
        let count = baseline.edges.len() + current.edges.len();
        if count > MAX_COMPARE_EDGES {
            return Err(reason("capture-budget"));
        }
        let paths = baseline
            .edges
            .iter()
            .chain(&current.edges)
            .map(|edge| edge.src_path.len() + edge.dst_path.len())
            .sum::<usize>();
        let temporary = 6 * paths + 512 * count + 64 * 1024;
        owner
            .work
            .charge(16 * (paths + count))
            .map_err(native_reason)?;
        if owner.work.read_limit(temporary, 0).map_err(native_reason)? != temporary {
            return Err(reason("capture-budget"));
        }
        owner
            .work
            .memory_budget()
            .reserve(temporary)
            .map_err(|_| reason("capture-budget"))
    }

    fn checkout_limits(phase: CapturePhase<'_>) -> CheckoutLimits {
        CheckoutLimits {
            inventory: InventoryLimits {
                entries: 4_096,
                directories: 4_096,
            },
            read_bytes: phase.remaining_read_bytes(),
            work_units: 512 * 1024 * 1024,
        }
    }

    fn current_owner<'a>(
        layout: &Layout,
        origin: StateDirOrigin,
        limits: CheckoutLimits,
        phase: CapturePhase<'a>,
    ) -> Result<
        (
            Observation<'a>,
            gix::ObjectId,
            crate::change_evidence::scope::ScopeGuard,
        ),
        Vec<String>,
    > {
        let mut owner = Observation::open_scoped_metered(layout, origin, limits, phase)
            .map_err(native_reason)?;
        owner.observe_local_git_config().map_err(native_reason)?;
        let (head, refs) =
            resolve_head(&owner.root, &mut owner.io, &mut owner.work).map_err(native_reason)?;
        refs.confirm(&owner.root, &mut owner.io, &mut owner.work)
            .map_err(native_reason)?;
        drop(refs);
        let guard = owner
            .confirm_scoped_guard(layout, origin, head)
            .map_err(native_reason)?;
        Ok((owner, head, guard))
    }

    fn attest_in_phase(
        layout: &Layout,
        origin: StateDirOrigin,
        name: &str,
        facts: &mut BaselineFacts,
        budget: &CaptureBudget,
    ) -> Result<DateTime<Utc>, Vec<String>> {
        let phase = budget.phase().ok_or_else(|| reason("capture-budget"))?;
        let limits = checkout_limits(phase);
        let (owner, head, guard) = current_owner(layout, origin, limits, phase)?;
        facts.base = Some(head.to_string());

        let mut store = BaselineStore::open(owner, layout, origin, head, &guard, phase)
            .map_err(database_reason)?;
        let (scope, baseline, snapshot) = store.load(name).map_err(load_reason)?;
        facts.repo_scope = scope;
        facts.baseline_digest = Some(baseline.digest().to_owned());
        let base = facts.base.as_deref().expect("exact head already observed");
        let saved = &baseline.set().identity;
        if saved.index_head_hash.as_deref() != Some(base) {
            return Err(reason("baseline-revision-mismatch"));
        }
        if !saved.input_completeness.is_complete() {
            return Err(reason("baseline-incomplete"));
        }
        let proof = FirstDefaultIndexedHeadPass::begin_metered(
            HeadCheckoutRequest {
                layout,
                origin,
                head,
            },
            &snapshot,
            limits,
            phase,
        )
        .and_then(FirstDefaultIndexedHeadPass::compare_next)
        .map_err(pair_reason)?;
        if proof.guard() != &guard {
            return Err(reason("scope-unavailable"));
        }
        let observed = Utc::now();
        let (generation, reproduced) = snapshot
            .finish_proven(&proof, observed)
            .map_err(snapshot_reason)?;
        let completeness = &reproduced.identity.input_completeness;
        if !completeness.is_complete() {
            return Err(completeness.reasons.clone());
        }
        // The strictly loaded stored canonical body is bounded. Check typed equality
        // before serialization so the reproduced body has that same bound.
        if baseline.set() != &reproduced {
            return Err(reason("baseline-digest-mismatch"));
        }
        let (identity, payload) =
            canonical_set_json(&reproduced).map_err(|_| reason("baseline-input-unavailable"))?;
        if compute_payload_digest(&identity, &payload) != baseline.digest() {
            return Err(reason("baseline-digest-mismatch"));
        }
        drop((identity, payload));

        let key =
            ExactPreparedKey::new(base, base, name, baseline.digest()).map_err(storage_reason)?;
        let checkout = BaselineCheckout {
            layout,
            origin,
            guard: proof.guard(),
            project_config: proof.config_digest(),
        };
        store.confirm(checkout).map_err(database_reason)?;
        let committed = store
            .commit(
                scope.map_or(ScopeExpectation::AuthorizeFirst, ScopeExpectation::Existing),
                &key,
                &baseline,
                AttestationInput {
                    reproduced: &reproduced,
                    generation: &generation,
                    observed,
                },
                checkout,
            )
            .map_err(write_reason)?;
        facts.repo_scope = Some(committed.scope);
        Ok(committed.observed)
    }

    fn reason(code: &'static str) -> Vec<String> {
        vec![code.to_owned()]
    }

    fn native_reason(reason_code: &'static str) -> Vec<String> {
        match reason_code {
            "deadline" | "interrupted" | "read-budget" | "work-cap" | "memory-cap" => {
                reason("capture-budget")
            }
            "head-mismatch" | "refs-changed" => reason("revision-changed"),
            _ => reason("scope-unavailable"),
        }
    }

    fn pair_reason(error: CheckoutPairError) -> Vec<String> {
        match error {
            CheckoutPairError::Deadline
            | CheckoutPairError::Interrupted
            | CheckoutPairError::Budget(_) => reason("capture-budget"),
            CheckoutPairError::Changed(CheckoutInput::Refs | CheckoutInput::Revision)
            | CheckoutPairError::Input(CheckoutInput::Refs, "head-mismatch") => {
                reason("revision-changed")
            }
            CheckoutPairError::Input(CheckoutInput::Scope, _)
            | CheckoutPairError::Changed(CheckoutInput::Scope) => reason("scope-unavailable"),
            CheckoutPairError::Input(CheckoutInput::IndexedInputs, code) => match code {
                "index-generation-mismatch"
                | "index-parser-version-mismatch"
                | "index-input-stamp-mismatch"
                | "index-input-stamp-missing"
                | "index-input-hash-mismatch"
                | "unsupported-input-encoding"
                | "worktree-blob-mismatch" => reason(code),
                _ => reason("index-input-unavailable"),
            },
            CheckoutPairError::Input(CheckoutInput::DefaultDiscovery, _)
            | CheckoutPairError::Changed(CheckoutInput::DefaultDiscovery) => {
                reason("index-discovery-unavailable")
            }
            CheckoutPairError::Tracked(
                crate::change_evidence::tracked_attributes::TrackedAttributeError::Tracked(
                    "worktree-blob-mismatch",
                ),
            ) => reason("worktree-blob-mismatch"),
            CheckoutPairError::VisibleUntracked => reason("worktree-untracked"),
            _ => reason("checkout-unavailable"),
        }
    }

    fn database_reason(error: CaptureDbUnavailable) -> Vec<String> {
        use CaptureDbUnavailable::*;
        match error {
            UnsupportedIdentity => reason("platform-unavailable"),
            Scope | Identity => reason("scope-unavailable"),
            RevisionChanged => reason("revision-changed"),
            Missing => reason("storage-uninitialized"),
            Schema | ForeignKeys => reason("storage-schema-unavailable"),
            Sqlite => reason("storage-unavailable"),
        }
    }

    fn load_reason(error: LoadError) -> Vec<String> {
        match error {
            LoadError::Scope(error) | LoadError::Baseline(error) => write_reason(error),
            LoadError::Snapshot(error) => snapshot_reason(error),
        }
    }

    fn snapshot_reason(error: CaptureSnapshotError) -> Vec<String> {
        match error {
            CaptureSnapshotError::Sql(error) => storage_reason(sql_error(error)),
            CaptureSnapshotError::Limit(_) | CaptureSnapshotError::Allocation => {
                reason("capture-budget")
            }
            CaptureSnapshotError::Corrupt(_) | CaptureSnapshotError::Graph(_) => {
                reason("index-snapshot-unavailable")
            }
        }
    }

    fn storage_reason(error: EvidenceStorageError) -> Vec<String> {
        use EvidenceStorageError::*;
        match error {
            Uninitialized => reason("storage-uninitialized"),
            SchemaIncompatible => reason("storage-schema-unavailable"),
            ScopeUnavailable | ForeignScope => reason("scope-unavailable"),
            StorageBusy => reason("storage-busy"),
            PayloadOverBudget => reason("capture-budget"),
            Corrupt | InvalidInput | StorageUnavailable => reason("storage-unavailable"),
        }
    }

    fn write_reason(error: WriteError) -> Vec<String> {
        use WriteError::*;
        match error {
            Storage(error) => storage_reason(error),
            ScopeUnavailable => reason("scope-unavailable"),
            PayloadOverBudget | Capacity => reason("capture-budget"),
            WriteConflict => reason("storage-conflict"),
            BaselineMissing => reason("baseline-missing"),
            BaselineChanged => reason("baseline-changed"),
            BaselineInvalid => reason("baseline-invalid"),
            IndexChanged => reason("index-generation-changed"),
            RevisionChanged => reason("revision-changed"),
            AttestationMissing => reason("attestation-missing"),
            Corrupt | InvalidInput => reason("storage-unavailable"),
        }
    }
}
