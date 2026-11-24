//! One retained existing store for the explicit baseline producer protocol.

#[cfg(windows)]
use super::opened_identity;
use super::{
    BeforeExecution, CaptureDbUnavailable, DB, OpenCaptureDb, confirm_companions, observe_file,
    observed_db, require_guard, resolve_optional_qualified_tx,
};
use crate::change_evidence::{
    baseline_discovery,
    pack_store::{Observation, capture_budget::CapturePhase},
    scope::ScopeGuard,
};
use crate::commands::baseline::extract::capture_snapshot::{
    CaptureSnapshot, CaptureSnapshotError, CaptureSnapshotLimits, IndexGeneration,
    extract_capture_snapshot,
};
use crate::state::{
    layout::{Layout, StateDirOrigin},
    storage::change_evidence::writes::{
        AttestationInput, Attested, PreparedBody, PreparedCommit, PreparedExpectation,
        PreparedWrite, ScopeExpectation, VerifiedBaseline, WriteError, WriteScope,
        commit_attestation_guarded, commit_prepared_guarded, read_baseline,
        read_prepared_expectation, require_prepared_attestation,
    },
    storage::change_evidence::{EvidenceStorageError, ExactPreparedKey, sql_error},
};
use chrono::{DateTime, Utc};
use gix::ObjectId;
use rusqlite::Connection;
use std::time::Instant;
use uuid::Uuid;

const SNAPSHOT_LIMITS: CaptureSnapshotLimits = CaptureSnapshotLimits {
    active_files: 4_096,
    scanned_files: 16_384,
    scanned_edges: 65_536,
    scanned_metadata: 128,
    text_bytes: 32 * 1024 * 1024,
};

#[derive(Clone, Copy)]
pub(in crate::change_evidence::pack_store) struct BaselineCheckout<'a> {
    pub(in crate::change_evidence::pack_store) layout: &'a Layout,
    pub(in crate::change_evidence::pack_store) origin: StateDirOrigin,
    pub(in crate::change_evidence::pack_store) guard: &'a ScopeGuard,
    pub(in crate::change_evidence::pack_store) project_config: blake3::Hash,
}

impl BaselineCheckout<'_> {
    fn confirm(
        self,
        observation: &mut Observation<'_>,
        head: ObjectId,
    ) -> Result<(), CaptureDbUnavailable> {
        require_guard(observation, self.layout, self.origin, head, self.guard)?;
        baseline_discovery::confirm_project_config(
            &observation.root,
            &mut observation.io,
            &mut observation.work,
            self.project_config,
        )
        .map_err(|_| CaptureDbUnavailable::Scope)
    }
}

pub(in crate::change_evidence::pack_store) struct BaselineStore<'a>(OpenCaptureDb<'a>);

#[derive(Debug)]
pub(in crate::change_evidence::pack_store) enum LoadError {
    Scope(WriteError),
    Baseline(WriteError),
    Snapshot(CaptureSnapshotError),
}

pub(in crate::change_evidence::pack_store) struct PreparedInputs {
    pub(in crate::change_evidence::pack_store) scope: Uuid,
    pub(in crate::change_evidence::pack_store) baseline: VerifiedBaseline,
    pub(in crate::change_evidence::pack_store) snapshot: CaptureSnapshot,
    pub(in crate::change_evidence::pack_store) expectation: PreparedExpectation,
}

struct BeforeCommit<'a, 'memory> {
    checkout: BaselineCheckout<'a>,
    observation: &'a mut Observation<'memory>,
    identity: super::Identity,
    head: ObjectId,
}

impl BeforeCommit<'_, '_> {
    fn confirm(&mut self, tx: &Connection) -> Result<(), WriteError> {
        let observation = &mut self.observation;
        let budget_failure = || WriteError::Storage(EvidenceStorageError::StorageUnavailable);
        observation.io.checkpoint().map_err(|_| budget_failure())?;
        #[cfg(windows)]
        if opened_identity(tx).ok() != Some(self.identity) {
            return Err(WriteError::ScopeUnavailable);
        }
        #[cfg(target_os = "linux")]
        if !super::stock_vfs::capture_check(tx) {
            return Err(WriteError::ScopeUnavailable);
        }
        if observe_file(&observation.root, DB, &observation.io).ok() != Some(self.identity)
            || confirm_companions(&observation.root, &observation.io).is_err()
        {
            return Err(WriteError::ScopeUnavailable);
        }
        // The SQLite write lock may have waited. Recheck the same retained
        // checkout after acquisition and before publishing any result.
        self.checkout
            .confirm(observation, self.head)
            .map_err(|error| match error {
                CaptureDbUnavailable::RevisionChanged => WriteError::RevisionChanged,
                _ => WriteError::ScopeUnavailable,
            })?;
        observation.io.checkpoint().map_err(|_| budget_failure())
    }
}

impl<'a> BaselineStore<'a> {
    pub(in crate::change_evidence::pack_store) fn open(
        observation: Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        head: ObjectId,
        guard: &ScopeGuard,
        phase: CapturePhase<'a>,
    ) -> Result<Self, CaptureDbUnavailable> {
        let BeforeExecution(mut store) =
            BeforeExecution::open_bound(observation, layout, origin, head, guard, None)?;
        let deadline = phase.deadline;
        store
            .connection
            .0
            .progress_handler(1_000, Some(move || Instant::now() >= deadline))
            .map_err(|_| CaptureDbUnavailable::Sqlite)?;
        Ok(Self(store))
    }

    pub(in crate::change_evidence::pack_store) fn load(
        &mut self,
        name: &str,
    ) -> Result<(Option<Uuid>, VerifiedBaseline, CaptureSnapshot), LoadError> {
        self.read_snapshot(|connection, guard| {
            let tx = connection
                .unchecked_transaction()
                .map_err(|error| LoadError::Scope(sql_error(error).into()))?;
            let scope = resolve_optional_qualified_tx(&tx, guard)
                .map_err(|error| LoadError::Scope(error.into()))?;
            let baseline = read_baseline(&tx, guard, name).map_err(LoadError::Baseline)?;
            let snapshot =
                extract_capture_snapshot(&tx, SNAPSHOT_LIMITS).map_err(LoadError::Snapshot)?;
            tx.commit()
                .map_err(|error| LoadError::Scope(sql_error(error).into()))?;
            Ok((scope, baseline, snapshot))
        })
    }

    pub(in crate::change_evidence::pack_store) fn load_prepared_inputs(
        &mut self,
        name: &str,
        head: &str,
        base: &str,
    ) -> Result<PreparedInputs, LoadError> {
        self.read_snapshot(|connection, guard| {
            let tx = connection
                .unchecked_transaction()
                .map_err(|error| LoadError::Scope(sql_error(error).into()))?;
            let scope = resolve_optional_qualified_tx(&tx, guard)
                .map_err(|error| LoadError::Scope(error.into()))?
                .ok_or(LoadError::Baseline(WriteError::AttestationMissing))?;
            let baseline = read_baseline(&tx, guard, name).map_err(LoadError::Baseline)?;
            let key = ExactPreparedKey::new(head, base, name, baseline.digest())
                .map_err(|error| LoadError::Baseline(error.into()))?;
            require_prepared_attestation(&tx, scope, &key, &baseline)
                .map_err(LoadError::Baseline)?;
            let expectation = read_prepared_expectation(
                &tx,
                WriteScope {
                    guard,
                    expected: ScopeExpectation::Existing(scope),
                },
                key,
            )
            .map_err(LoadError::Baseline)?;
            let snapshot =
                extract_capture_snapshot(&tx, SNAPSHOT_LIMITS).map_err(LoadError::Snapshot)?;
            tx.commit()
                .map_err(|error| LoadError::Scope(sql_error(error).into()))?;
            Ok(PreparedInputs {
                scope,
                baseline,
                snapshot,
                expectation,
            })
        })
    }

    fn read_snapshot<T>(
        &mut self,
        read: impl FnOnce(&Connection, &[u8; 32]) -> Result<T, LoadError>,
    ) -> Result<T, LoadError> {
        let store = &mut self.0;
        #[cfg(target_os = "linux")]
        return super::stock_vfs::with_scope(
            &mut store.linux_guard,
            &store.observation.io,
            &mut store.observation.work,
            || read(&store.connection.0, store.guard.as_bytes()),
        )
        .ok_or(LoadError::Scope(WriteError::ScopeUnavailable))?;
        #[cfg(not(target_os = "linux"))]
        read(&store.connection.0, store.guard.as_bytes())
    }

    pub(in crate::change_evidence::pack_store) fn observation(&mut self) -> &mut Observation<'a> {
        &mut self.0.observation
    }

    pub(in crate::change_evidence::pack_store) fn confirm(
        &mut self,
        checkout: BaselineCheckout<'_>,
    ) -> Result<(), CaptureDbUnavailable> {
        let store = &mut self.0;
        if &store.guard != checkout.guard {
            return Err(CaptureDbUnavailable::Scope);
        }
        let observation = &mut store.observation;
        checkout.confirm(observation, store.head)?;
        #[cfg(windows)]
        if opened_identity(&store.connection.0)? != store.identity {
            return Err(CaptureDbUnavailable::Identity);
        }
        #[cfg(target_os = "linux")]
        if super::stock_vfs::with_scope(
            &mut store.linux_guard,
            &observation.io,
            &mut observation.work,
            || super::stock_vfs::capture_check(&store.connection.0),
        ) != Some(true)
        {
            return Err(CaptureDbUnavailable::Identity);
        }
        if observed_db(observation)? != store.identity {
            return Err(CaptureDbUnavailable::Identity);
        }
        confirm_companions(&observation.root, &observation.io)
    }

    pub(in crate::change_evidence::pack_store) fn commit(
        &mut self,
        expected: ScopeExpectation,
        key: &ExactPreparedKey<'_>,
        baseline: &VerifiedBaseline,
        input: AttestationInput<'_>,
        checkout: BaselineCheckout<'_>,
    ) -> Result<Attested, WriteError> {
        let guard = self.0.guard;
        let ownership = WriteScope {
            guard: guard.as_bytes(),
            expected,
        };
        self.with_writer(checkout, |conn, recheck| {
            commit_attestation_guarded(
                conn,
                ownership,
                key,
                baseline,
                input,
                #[cfg(target_os = "linux")]
                admit_native_writer,
                |tx| recheck.confirm(tx),
            )
        })
    }

    pub(in crate::change_evidence::pack_store) fn commit_preparation(
        &mut self,
        expectation: PreparedExpectation,
        baseline: &VerifiedBaseline,
        index: &IndexGeneration,
        body: &PreparedBody,
        observed: DateTime<Utc>,
        checkout: BaselineCheckout<'_>,
    ) -> Result<PreparedCommit, WriteError> {
        self.with_writer(checkout, |conn, recheck| {
            commit_prepared_guarded(
                conn,
                expectation,
                PreparedWrite {
                    baseline,
                    index,
                    body,
                    observed,
                },
                #[cfg(target_os = "linux")]
                admit_native_writer,
                |tx| recheck.confirm(tx),
            )
        })
    }

    fn with_writer<T>(
        &mut self,
        checkout: BaselineCheckout<'_>,
        write: impl FnOnce(&mut Connection, &mut BeforeCommit<'_, 'a>) -> Result<T, WriteError>,
    ) -> Result<T, WriteError> {
        let store = &mut self.0;
        if &store.guard != checkout.guard {
            return Err(WriteError::ScopeUnavailable);
        }
        store
            .observation
            .io
            .checkpoint()
            .map_err(|_| WriteError::Storage(EvidenceStorageError::StorageUnavailable))?;
        let mut recheck = BeforeCommit {
            checkout,
            observation: &mut store.observation,
            identity: store.identity,
            head: store.head,
        };
        #[cfg(target_os = "linux")]
        {
            // Debit both paths from the original Work before creating the native
            // child allowance. The checkout callback still uses the parent Work.
            let mut work = recheck
                .observation
                .work
                .lend_native(16 * 1024 * 1024)
                .map_err(|_| WriteError::Storage(EvidenceStorageError::PayloadOverBudget))?;
            let io = recheck.observation.io.native_read_view();
            super::stock_vfs::with_scope(&mut store.linux_guard, &io, &mut work, || {
                write(&mut store.connection.0, &mut recheck)
            })
            .ok_or(WriteError::ScopeUnavailable)?
        }
        #[cfg(not(target_os = "linux"))]
        write(&mut store.connection.0, &mut recheck)
    }
}

#[cfg(test)]
mod lock_wait_tests;

#[cfg(target_os = "linux")]
fn admit_native_writer(conn: &Connection) -> Result<(), WriteError> {
    super::stock_vfs::capture_admit(conn)
        .then_some(())
        .ok_or(WriteError::ScopeUnavailable)
}
