use super::{
    Observation,
    capture_budget::{CaptureBudget, CapturePhase},
    capture_db::{
        AfterExecution, BeforeExecution, CaptureDbUnavailable, QualifiedCaptureStorage,
        prequalify_planning_store,
    },
    paired_checkout::{
        CheckoutInput, CheckoutLimits, CheckoutPairError, HeadCheckoutRequest, PairedHeadProof,
        observe_capture_head,
    },
    resolve_head,
};
use crate::change_evidence::inventory::InventoryLimits;
use crate::state::{
    layout::{Layout, StateDirOrigin},
    storage::verification::{AttemptIdentity, CaptureUnavailable},
};
use chrono::{DateTime, Utc};
use gix::ObjectId;
use std::path::Path;
use uuid::Uuid;

type Result<T> = std::result::Result<T, CaptureUnavailable>;

struct CaptureFacts {
    head: ObjectId,
    guard: [u8; 32],
    scope: Option<Uuid>,
    attempt_id: Uuid,
    started: DateTime<Utc>,
    origin: StateDirOrigin,
}

pub(crate) struct CaptureSession<'run> {
    facts: CaptureFacts,
    db: BeforeExecution<'run>,
    budget: &'run CaptureBudget,
}

pub(crate) struct ExecutingCapture<'run> {
    facts: CaptureFacts,
    db: AfterExecution<'run>,
    budget: &'run CaptureBudget,
}

impl<'run> CaptureSession<'run> {
    pub(crate) fn start(
        layout: &Layout,
        origin: StateDirOrigin,
        cwd: &Path,
        budget: &'run CaptureBudget,
    ) -> Result<Self> {
        Self::start_inner(layout, origin, cwd, budget, false)
    }

    pub(crate) fn start_planning(
        layout: &Layout,
        origin: StateDirOrigin,
        cwd: &Path,
        budget: &'run CaptureBudget,
    ) -> Result<Self> {
        Self::start_inner(layout, origin, cwd, budget, true)
    }

    fn start_inner(
        layout: &Layout,
        origin: StateDirOrigin,
        cwd: &Path,
        budget: &'run CaptureBudget,
        planning_only: bool,
    ) -> Result<Self> {
        if !cfg!(any(windows, target_os = "linux")) {
            return Err(CaptureUnavailable::PlatformUnavailable);
        }
        if origin != StateDirOrigin::RepositoryLocal || cwd != layout.root.as_std_path() {
            return Err(CaptureUnavailable::ScopeUnavailable);
        }
        let started = Utc::now();
        let phase = active_phase(budget)?;
        let limits = limits(phase);
        let mut observer = Observation::open_scoped_metered(layout, origin, limits, phase)
            .map_err(|_| classify_observation(budget))?;
        let (head, refs) = resolve_head(&observer.root, &mut observer.io, &mut observer.work)
            .map_err(|_| classify_observation(budget))?;
        refs.confirm(&observer.root, &mut observer.io, &mut observer.work)
            .map_err(|_| classify_observation(budget))?;
        drop(refs);
        let proof = observe_capture_head(
            HeadCheckoutRequest {
                layout,
                origin,
                head,
            },
            limits,
            phase,
        )
        .map_err(classify_proof)?;
        let (db, scope) = if planning_only {
            let bridge = prequalify_planning_store(&mut observer, layout, origin, &proof)?;
            BeforeExecution::open_planning(observer, layout, origin, &proof, bridge)?
        } else {
            let mut db =
                BeforeExecution::open(observer, layout, origin, &proof).map_err(classify_db)?;
            let scope = db.scope_binding()?;
            (db, scope)
        };
        Ok(Self {
            facts: CaptureFacts {
                head,
                guard: *proof.guard().as_bytes(),
                scope,
                attempt_id: Uuid::new_v4(),
                started,
                origin,
            },
            db,
            budget,
        })
    }

    pub(crate) fn before_execution(self, layout: &Layout) -> Result<ExecutingCapture<'run>> {
        let (mut observer, proof) = sample(layout, &self.facts, self.budget)?;
        let db = self
            .db
            .confirm_before_execution(&mut observer, layout, self.facts.origin, &proof)
            .map_err(classify_db)?;
        Ok(ExecutingCapture {
            facts: self.facts,
            db,
            budget: self.budget,
        })
    }

    pub(crate) fn finish_planning(
        self,
        layout: &Layout,
    ) -> Result<(AttemptIdentity, QualifiedCaptureStorage<'run>)> {
        let (observer, proof) = sample(layout, &self.facts, self.budget)?;
        let db = self
            .db
            .confirm_after_planning(observer, layout, self.facts.origin, &proof)
            .map_err(classify_db)?;
        Ok((self.facts.finish(), db))
    }
}

impl<'run> ExecutingCapture<'run> {
    pub(crate) fn after_termination(
        self,
        layout: &Layout,
    ) -> Result<(AttemptIdentity, QualifiedCaptureStorage<'run>)> {
        let (observer, proof) = sample(layout, &self.facts, self.budget)?;
        let db = self
            .db
            .confirm_after_termination(observer, layout, self.facts.origin, &proof)
            .map_err(classify_db)?;
        Ok((self.facts.finish(), db))
    }
}

impl CaptureFacts {
    fn finish(self) -> AttemptIdentity {
        AttemptIdentity {
            repo_scope: self.scope,
            authorize_first_scope: self.scope.is_none(),
            scope_guard: self.guard,
            attempt_id: self.attempt_id,
            head: self.head.to_string(),
            observed_from: self.started,
            observed_to: Utc::now(),
        }
    }
}

/// Scope-only authority for dirty planning. This never creates a capture proof.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn with_bound_fast_mapping<T>(
    layout: &Layout,
    origin: StateDirOrigin,
    expected_head: &str,
    budget: &CaptureBudget,
    inspect: impl FnOnce(&rusqlite::Transaction<'_>) -> Option<T>,
) -> Option<T> {
    let head = gix::ObjectId::from_hex(expected_head.as_bytes()).ok()?;
    let phase = active_phase(budget).ok()?;
    let mut observation =
        Observation::open_scoped_metered(layout, origin, limits(phase), phase).ok()?;
    let guard = observation
        .confirm_scoped_guard(layout, origin, head)
        .ok()?;
    let (_, result) = super::capture_db::with_checkpointed_reader(
        &mut observation,
        layout,
        origin,
        head,
        &guard,
        Some(phase.deadline),
        |tx, scope| {
            if scope.is_none() {
                return Err(CaptureUnavailable::ScopeUnavailable);
            }
            inspect(tx).ok_or(CaptureUnavailable::PlanningStorageUnavailable)
        },
    )
    .ok()?;
    Some(result)
}

fn active_phase(budget: &CaptureBudget) -> Result<CapturePhase<'_>> {
    budget.phase().ok_or_else(|| {
        if budget.is_invalidated() {
            CaptureUnavailable::ScopeUnavailable
        } else {
            CaptureUnavailable::CaptureBudget
        }
    })
}

fn limits(phase: CapturePhase<'_>) -> CheckoutLimits {
    CheckoutLimits {
        inventory: InventoryLimits {
            entries: 4096,
            directories: 4096,
        },
        read_bytes: phase.remaining_read_bytes(),
        work_units: 512 * 1024 * 1024,
    }
}

fn sample<'run>(
    layout: &Layout,
    facts: &CaptureFacts,
    budget: &'run CaptureBudget,
) -> Result<(Observation<'run>, PairedHeadProof)> {
    if std::env::current_dir().ok().as_deref() != Some(layout.root.as_std_path()) {
        return Err(CaptureUnavailable::ScopeUnavailable);
    }
    let phase = active_phase(budget)?;
    let limits = limits(phase);
    let request = HeadCheckoutRequest {
        layout,
        origin: facts.origin,
        head: facts.head,
    };
    let proof = observe_capture_head(request, limits, phase).map_err(classify_proof)?;
    if proof.guard().as_bytes() != &facts.guard {
        return Err(CaptureUnavailable::ScopeUnavailable);
    }
    let observer = Observation::open_scoped_metered(layout, facts.origin, limits, phase)
        .map_err(|_| classify_observation(budget))?;
    Ok((observer, proof))
}

fn classify_observation(budget: &CaptureBudget) -> CaptureUnavailable {
    active_phase(budget)
        .err()
        .unwrap_or(CaptureUnavailable::CheckoutUnavailable)
}

fn classify_proof(error: CheckoutPairError) -> CaptureUnavailable {
    match error {
        CheckoutPairError::Deadline | CheckoutPairError::Budget(_) => {
            CaptureUnavailable::CaptureBudget
        }
        CheckoutPairError::Interrupted => CaptureUnavailable::ScopeUnavailable,
        CheckoutPairError::Changed(CheckoutInput::Refs | CheckoutInput::Revision)
        | CheckoutPairError::Input(CheckoutInput::Refs, "head-mismatch") => {
            CaptureUnavailable::RevisionChanged
        }
        _ => CaptureUnavailable::CheckoutUnavailable,
    }
}

pub(super) fn classify_db(error: CaptureDbUnavailable) -> CaptureUnavailable {
    match error {
        CaptureDbUnavailable::RevisionChanged => CaptureUnavailable::RevisionChanged,
        CaptureDbUnavailable::UnsupportedIdentity => CaptureUnavailable::PlatformUnavailable,
        CaptureDbUnavailable::Scope | CaptureDbUnavailable::Identity => {
            CaptureUnavailable::ScopeUnavailable
        }
        _ => CaptureUnavailable::StorageUnavailable,
    }
}
