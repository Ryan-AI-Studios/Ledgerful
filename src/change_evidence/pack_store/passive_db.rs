//! Scoped transport input from the existing-only SQLite lane.
use super::{
    CommitPair, Observation,
    capture_budget::CapturePhase,
    guard_scope,
    paired_checkout::{
        CheckoutInput, CheckoutLimits, CheckoutPairError, CheckoutRequest, observe_paired_checkout,
    },
    qualify_layout,
};
use crate::change_evidence::{
    buffer::MemoryBudget,
    http_worker::WorkerContext,
    inventory::{InventoryError, InventoryLimits},
    scope::ScopeGuard,
    tracked_attributes::TrackedAttributeError,
};
use crate::state::layout::{Layout, StateDirOrigin};
use crate::state::storage::change_evidence::{
    attempts::AttemptHistory,
    http::{BaselineVerdict, CandidatePage, SelectionMetadata},
    writes::prepared::{PreparedGit, PreparedRecheck, PreparedRecord},
};
use gix::ObjectId;
use std::sync::atomic::AtomicUsize;
use uuid::Uuid;

#[cfg(windows)]
use super::capture_db::{
    DB, JOURNAL, SHM, WAL, opened_identity,
    sqlite_open::{cold_sqlite_header, open_immutable_planning_reader},
};
#[cfg(windows)]
use crate::change_evidence::contained::{DatabaseEntryLease, ReadError};
#[cfg(windows)]
use crate::state::storage::change_evidence::EvidenceReader;
#[cfg(any(windows, target_os = "linux"))]
use crate::state::storage::change_evidence::{
    EvidenceStorageError, ExactPreparedKey,
    attempts::attempt_history_in_tx,
    http::{candidates_in_tx, pair_git_key, selection_in_tx},
    resolve_qualified_tx,
    writes::{
        ScopeExpectation, WriteError, WriteScope,
        prepared::{read_prepared_in_tx, recheck_prepared_in_tx},
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PassiveReadError {
    PairNotFound,
    Uninitialized,
    SchemaIncompatible,
    ScopeUnavailable,
    StorageBusy,
    RequestBudget,
    Internal,
}

#[cfg(any(windows, target_os = "linux"))]
impl From<EvidenceStorageError> for PassiveReadError {
    fn from(error: EvidenceStorageError) -> Self {
        match error {
            EvidenceStorageError::ForeignScope => Self::PairNotFound,
            EvidenceStorageError::Uninitialized => Self::Uninitialized,
            EvidenceStorageError::SchemaIncompatible => Self::SchemaIncompatible,
            EvidenceStorageError::ScopeUnavailable | EvidenceStorageError::StorageUnavailable => {
                Self::ScopeUnavailable
            }
            EvidenceStorageError::StorageBusy => Self::StorageBusy,
            EvidenceStorageError::PayloadOverBudget => Self::RequestBudget,
            EvidenceStorageError::Corrupt | EvidenceStorageError::InvalidInput => Self::Internal,
        }
    }
}

pub(crate) struct ScopedExactPair {
    guard: ScopeGuard,
    pair: CommitPair,
}

impl ScopedExactPair {
    pub(crate) fn guard(&self) -> &ScopeGuard {
        &self.guard
    }

    pub(crate) fn commits(&self) -> &CommitPair {
        &self.pair
    }
}

pub(crate) struct PassiveSelection<'a> {
    pub(crate) scope: Uuid,
    pub(crate) proof: &'a ScopedExactPair,
    pub(crate) baseline: Option<(&'a str, &'a str)>,
    pub(crate) earlier: Option<&'a PreparedRecord>,
}

pub(crate) struct PassiveRead {
    pub(crate) scope: Uuid,
    pub(crate) history: Option<Result<AttemptHistory, PassiveReadError>>,
    pub(crate) candidates: Option<Result<CandidatePage, PassiveReadError>>,
    pub(crate) selection: Option<Result<SelectionMetadata, PassiveReadError>>,
    pub(crate) prepared: Result<Option<PreparedRecord>, PassiveReadError>,
    pub(crate) pair_git: Result<Option<PreparedGit>, PassiveReadError>,
    pub(crate) prepared_recheck: Option<Result<PreparedRecheck, PassiveReadError>>,
}

#[cfg(any(windows, target_os = "linux"))]
fn prepared_error(error: WriteError) -> PassiveReadError {
    match error {
        WriteError::Storage(error) => error.into(),
        WriteError::ScopeUnavailable => PassiveReadError::ScopeUnavailable,
        WriteError::PayloadOverBudget => PassiveReadError::RequestBudget,
        _ => PassiveReadError::Internal,
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn read_passive_snapshot(
    connection: &rusqlite::Connection,
    guard: &ScopeGuard,
    selected: Option<PassiveSelection<'_>>,
) -> Result<PassiveRead, EvidenceStorageError> {
    let tx = connection
        .unchecked_transaction()
        .map_err(crate::state::storage::change_evidence::sql_error)?;
    let scope = resolve_qualified_tx(&tx, guard.as_bytes())?;
    let mut read = PassiveRead {
        scope,
        history: None,
        candidates: None,
        selection: None,
        prepared: Ok(None),
        pair_git: Ok(None),
        prepared_recheck: None,
    };
    if let Some(selected) = selected {
        if selected.scope != scope {
            return Err(EvidenceStorageError::ForeignScope);
        }
        let proof = selected.proof.commits();
        let head = proof.head.to_string();
        let base = proof.parent.to_string();
        let ownership = WriteScope {
            guard: guard.as_bytes(),
            expected: ScopeExpectation::Existing(scope),
        };
        let key = selected
            .baseline
            .map(|(name, digest)| ExactPreparedKey::new(&head, &base, name, digest))
            .transpose()?;
        if let Some(earlier) = selected.earlier {
            let key = key.as_ref().ok_or(EvidenceStorageError::InvalidInput)?;
            read.prepared_recheck =
                Some(recheck_prepared_in_tx(&tx, ownership, key, earlier).map_err(prepared_error));
        } else {
            read.history = Some(
                attempt_history_in_tx(&tx, guard.as_bytes(), scope, proof)
                    .map_err(PassiveReadError::from),
            );
            if let Some(key) = &key {
                read.selection = Some(
                    selection_in_tx(&tx, guard.as_bytes(), scope, key)
                        .map_err(PassiveReadError::from),
                );
                read.prepared = read_prepared_in_tx(&tx, ownership, key).map_err(prepared_error);
                if let Ok(Some(record)) = &read.prepared
                    && record.captured_at.is_some()
                {
                    read.prepared_recheck = Some(
                        recheck_prepared_in_tx(&tx, ownership, key, record).map_err(prepared_error),
                    );
                }
            }
            if !matches!(&read.prepared, Ok(Some(_))) {
                read.pair_git = (|| {
                    let Some((name, digest)) =
                        pair_git_key(&tx, scope, &head, &base).map_err(PassiveReadError::from)?
                    else {
                        return Ok(None);
                    };
                    let key = ExactPreparedKey::new(&head, &base, &name, &digest)
                        .map_err(PassiveReadError::from)?;
                    read_prepared_in_tx(&tx, ownership, &key)
                        .map(|row| row.map(|row| row.evidence.git))
                        .map_err(prepared_error)
                })();
            }
            read.candidates = Some(
                candidates_in_tx(&tx, guard.as_bytes(), scope, &head, &base)
                    .map_err(PassiveReadError::from),
            );
        }
    }
    tx.commit()
        .map_err(crate::state::storage::change_evidence::sql_error)?;
    Ok(read)
}

#[cfg(windows)]
fn require_absent(observation: &Observation<'_>, paths: &[&str]) -> Result<(), PassiveReadError> {
    for path in paths {
        match observation
            .root
            .observe_database_entry(path, &observation.io)
        {
            Err(ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(PassiveReadError::ScopeUnavailable),
        }
    }
    Ok(())
}

#[cfg(windows)]
enum SidecarLease {
    Live {
        _wal: DatabaseEntryLease,
        _shm: DatabaseEntryLease,
    },
    Cold {
        _exclusive: DatabaseEntryLease,
    },
}

impl Observation<'_> {
    fn passive_scope(
        &mut self,
        layout: &Layout,
        origin: StateDirOrigin,
    ) -> Result<ScopeGuard, PassiveReadError> {
        let check = || {
            self.io
                .checkpoint()
                .map_err(|_| PassiveReadError::RequestBudget)
        };
        check()?;
        let result = (|| {
            qualify_layout(layout, origin, &mut self.work)?;
            self.root
                .confirm_reacquired(layout.root.as_std_path(), &self.io, &mut self.work)?;
            guard_scope(&self.root, &mut self.work)
        })();
        self.io
            .checkpoint()
            .map_err(|_| PassiveReadError::RequestBudget)?;
        result.map_err(|_| PassiveReadError::ScopeUnavailable)
    }

    /// This seals the historical pair to the same admitted repository as its Git
    /// object proof. It does not require the requested commit to be current HEAD.
    pub(crate) fn observe_scoped_commit_pair(
        &mut self,
        layout: &Layout,
        origin: StateDirOrigin,
        head: ObjectId,
        parent: ObjectId,
    ) -> Result<ScopedExactPair, PassiveReadError> {
        let guard = self.passive_scope(layout, origin)?;
        let pair = self.observe_commit_pair(head, parent);
        if self.passive_scope(layout, origin)? != guard {
            return Err(PassiveReadError::ScopeUnavailable);
        }
        let pair = pair.map_err(|reason| match reason {
            "deadline" | "work-cap" | "memory-cap" | "pack-read-cap" | "inventory-entry-cap" => {
                PassiveReadError::RequestBudget
            }
            _ => PassiveReadError::PairNotFound,
        })?;
        Ok(ScopedExactPair { guard, pair })
    }

    /// With no selection, read capability only. With a selection, verify the
    /// scoped identity inside the same snapshot as the bounded attempt history.
    #[cfg(windows)]
    pub(crate) fn read_passive_evidence(
        &mut self,
        layout: &Layout,
        origin: StateDirOrigin,
        worker: &WorkerContext,
        selected: Option<PassiveSelection<'_>>,
    ) -> Result<PassiveRead, PassiveReadError> {
        let guard = self.passive_scope(layout, origin)?;
        if selected
            .as_ref()
            .is_some_and(|selected| selected.proof.guard != guard)
        {
            return Err(PassiveReadError::PairNotFound);
        }
        // A read-only SQLite main connection may still create WAL companions.
        // Retain *both* already existing files without delete sharing until SQLite
        // closes. Admission refuses a missing or redirected companion before SQL.
        let main = self
            .root
            .retain_database_entry(DB, &self.io)
            .map_err(|error| match error {
                ReadError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    PassiveReadError::Uninitialized
                }
                _ => PassiveReadError::ScopeUnavailable,
            })?;
        let path = layout.state_subdir().join("ledger.db");
        let lease = match self.root.retain_database_entry(WAL, &self.io) {
            Ok(wal) => {
                let shm = self
                    .root
                    .retain_database_entry(SHM, &self.io)
                    .map_err(|_| PassiveReadError::ScopeUnavailable)?;
                require_absent(self, &[JOURNAL])?;
                SidecarLease::Live {
                    _wal: wal,
                    _shm: shm,
                }
            }
            Err(ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                let exclusive = self
                    .root
                    .retain_database_read_excluding_writers(DB, &self.io)
                    .map_err(|_| PassiveReadError::ScopeUnavailable)?;
                if exclusive.identity() != main.identity() {
                    return Err(PassiveReadError::ScopeUnavailable);
                }
                require_absent(self, &[WAL, SHM, JOURNAL])?;
                let (header, bytes) = exclusive
                    .database_header(&self.io)
                    .map_err(|_| PassiveReadError::ScopeUnavailable)?;
                if !cold_sqlite_header(&header, bytes) {
                    return Err(PassiveReadError::ScopeUnavailable);
                }
                SidecarLease::Cold {
                    _exclusive: exclusive,
                }
            }
            _ => return Err(PassiveReadError::ScopeUnavailable),
        };
        let mut reader = match lease {
            SidecarLease::Live { .. } => EvidenceReader::open_existing(path.as_std_path())?,
            SidecarLease::Cold { .. } => EvidenceReader::from_immutable_connection(
                open_immutable_planning_reader(path.as_std_path())
                    .map_err(|_| PassiveReadError::ScopeUnavailable)?,
            )?,
        };
        let connection = reader.connection_mut();
        if opened_identity(connection).ok() != Some(main.identity()) {
            return Err(PassiveReadError::ScopeUnavailable);
        }
        let registered = worker.register(connection).map_err(|error| match error {
            crate::change_evidence::http_worker::WorkerError::Cancelled => {
                PassiveReadError::RequestBudget
            }
            _ => PassiveReadError::Internal,
        })?;
        let result = read_passive_snapshot(registered.connection(), &guard, selected);
        drop(registered);
        drop(reader);
        if matches!(lease, SidecarLease::Cold { .. }) {
            require_absent(self, &[WAL, SHM, JOURNAL])?;
        }
        if self.passive_scope(layout, origin)? != guard {
            return Err(PassiveReadError::ScopeUnavailable);
        }
        result.map_err(PassiveReadError::from)
    }

    #[cfg(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu",
        feature = "web"
    ))]
    pub(crate) fn read_passive_evidence(
        &mut self,
        layout: &Layout,
        origin: StateDirOrigin,
        worker: &WorkerContext,
        selected: Option<PassiveSelection<'_>>,
    ) -> Result<PassiveRead, PassiveReadError> {
        use super::isolated_passive::{SNAPSHOTS_PER_REQUEST, observe};
        use super::linux_capture::LinuxReadScope;
        let bytes = crate::change_evidence::passive_startup::admitted_heap_bytes()
            .ok_or(PassiveReadError::ScopeUnavailable)?;
        let guard = self.passive_scope(layout, origin)?;
        if selected.as_ref().is_some_and(|s| s.proof.guard != guard) {
            return Err(PassiveReadError::PairNotFound);
        }
        let storage = self
            .passive_storage
            .as_mut()
            .ok_or(PassiveReadError::ScopeUnavailable)?;
        // Allocation work is cumulative across the two snapshots. Live
        // allocation stays bounded by one slice and the shared memory budget.
        let request_credit = bytes
            .checked_mul(SNAPSHOTS_PER_REQUEST)
            .ok_or(PassiveReadError::RequestBudget)?;
        let remaining = storage.heap_credit.unwrap_or(request_credit);
        let slice = bytes.min(remaining);
        let fresh_heap = storage.heap.is_none();
        if fresh_heap {
            if slice == 0 {
                return Err(PassiveReadError::RequestBudget);
            }
            storage.heap = Some(
                storage
                    .work
                    .reserve_native_heap(slice)
                    .map_err(|_| PassiveReadError::RequestBudget)?,
            );
        }
        let (root, proc_fd) = self.root.linux_capture_anchors();
        let scope =
            LinuxReadScope::arm(root, proc_fd, &mut storage.work, &self.io).map_err(|reason| {
                match reason {
                    "deadline" | "work-cap" | "memory-cap" => PassiveReadError::RequestBudget,
                    _ => PassiveReadError::ScopeUnavailable,
                }
            })?;
        let result = observe(
            &scope,
            &self.io,
            &mut storage.work,
            storage
                .heap
                .as_ref()
                .ok_or(PassiveReadError::ScopeUnavailable)?,
            worker,
            |connection| {
                read_passive_snapshot(connection, &guard, selected).map_err(PassiveReadError::from)
            },
        )?;
        // Success includes checked close and helper join. A ticket held
        // after any earlier failure retains its original lifetime.
        if fresh_heap && let Some(heap) = storage.heap.take() {
            let spent = slice - heap.remaining_credit();
            storage.heap_credit = Some(remaining - spent);
        }
        if self.passive_scope(layout, origin)? != guard {
            return Err(PassiveReadError::ScopeUnavailable);
        }
        Ok(result)
    }

    #[cfg(not(any(
        windows,
        all(
            target_os = "linux",
            target_arch = "x86_64",
            target_env = "gnu",
            feature = "web"
        )
    )))]
    pub(crate) fn read_passive_evidence(
        &mut self,
        _: &Layout,
        _: StateDirOrigin,
        _: &WorkerContext,
        _: Option<PassiveSelection<'_>>,
    ) -> Result<PassiveRead, PassiveReadError> {
        Err(PassiveReadError::ScopeUnavailable)
    }
}

fn checkout_verdict(error: CheckoutPairError) -> BaselineVerdict {
    match error {
        CheckoutPairError::Changed(CheckoutInput::Refs)
        | CheckoutPairError::Input(CheckoutInput::Revision, "head-mismatch") => {
            BaselineVerdict::HeadMoved
        }
        CheckoutPairError::VisibleUntracked
        | CheckoutPairError::Inventory(InventoryError::TrackedNamespace)
        | CheckoutPairError::Tracked(TrackedAttributeError::Tracked("worktree-blob-mismatch")) => {
            BaselineVerdict::DirtyWorktree
        }
        _ => BaselineVerdict::LiveProofUnavailable,
    }
}

pub(crate) fn prove_live_checkout(
    layout: &Layout,
    origin: StateDirOrigin,
    worker: &WorkerContext,
    memory: &MemoryBudget,
    read_bytes: &AtomicUsize,
    pair: &ScopedExactPair,
) -> Result<(), BaselineVerdict> {
    let checkout = observe_paired_checkout(
        CheckoutRequest {
            layout,
            origin,
            head: pair.commits().head,
            parent: pair.commits().parent,
        },
        CheckoutLimits {
            inventory: InventoryLimits {
                entries: 4096,
                directories: 4097,
            },
            read_bytes: 64 * 1024 * 1024,
            work_units: 2_000_000,
        },
        CapturePhase {
            deadline: worker.deadline(),
            interrupted: worker.interrupted(),
            aggregate: read_bytes,
            memory,
        },
    )
    .map_err(checkout_verdict)?;
    if checkout.guard() == pair.guard() && checkout.commits() == pair.commits() {
        Ok(())
    } else {
        Err(BaselineVerdict::LiveProofUnavailable)
    }
}
