//! Private retained SQLite writer before public verify qualification.
#[path = "sqlite_capture_open.rs"]
pub(super) mod sqlite_open;
pub(crate) use sqlite_open::CaptureDbUnavailable;
#[cfg(any(windows, target_os = "linux"))]
pub(super) mod baseline_store;

#[cfg(target_os = "linux")]
use super::stock_vfs::{self, Scope as StockScope};
use super::{Observation, paired_checkout::PairedHeadProof};
#[cfg(windows)]
use crate::change_evidence::contained::DatabaseEntryLease;
use crate::change_evidence::contained::{ContainedRoot, Identity, ReadBudget, ReadError};
use crate::change_evidence::scope::ScopeGuard;
use crate::commands::verify::AttemptPayload;
#[cfg(any(windows, target_os = "linux"))]
use crate::state::storage::change_evidence::EvidenceStorageError;
use crate::state::storage::change_evidence::resolve_optional_qualified_tx;
#[cfg(target_os = "linux")]
use crate::state::storage::verification::TransactionStage;
use crate::state::{
    layout::{Layout, StateDirOrigin},
    migrations::get_migrations_count,
    storage::{
        StorageManager,
        verification::{
            AttemptIdentity, CaptureUnavailable, PlanningCaptureCommit, PreparedExecutedAttempt,
            VerificationCaptureCommit,
        },
    },
};
use crate::verify::engine::CompletedExecution;
use gix::ObjectId;
use rusqlite::Connection;
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub(super) const DB: &str = ".ledgerful/state/ledger.db";
pub(super) const WAL: &str = ".ledgerful/state/ledger.db-wal";
pub(super) const SHM: &str = ".ledgerful/state/ledger.db-shm";
pub(super) const JOURNAL: &str = ".ledgerful/state/ledger.db-journal";

/// Use the qualified CLI VFS with the selected state directory as its anchor.
/// Architecture's calling checkout can be elsewhere, such as a linked worktree.
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
pub(super) fn read_architecture<T>(
    layout: &Layout,
    inspect: impl FnOnce(Connection, Instant) -> (Connection, T),
) -> Option<T> {
    use super::capture_budget::CaptureBudget;
    use crate::change_evidence::pack::Work;
    use crate::state::layout::STATE_DIR;

    let state_dir = layout.state_dir.as_std_path();
    if state_dir.file_name()? != STATE_DIR {
        return None;
    }
    let state_anchor = state_dir.parent()?;
    let budget = CaptureBudget::new();
    let phase = budget.phase()?;
    let io = ReadBudget::new_shared(
        phase.remaining_read_bytes(),
        phase.deadline,
        phase.interrupted,
        phase.aggregate,
    );
    let mut work = Work::new(
        phase.deadline,
        phase.interrupted,
        512 * 1024 * 1024,
        phase.memory,
    );
    let checkout_path = layout.root.as_std_path();
    let checkout = ContainedRoot::open(checkout_path, &io).ok()?;
    let state_root = ContainedRoot::open(state_anchor, &io).ok()?;
    let expected = state_root.observe_entry(STATE_DIR, true, &io).ok()?;
    let (root, proc_fd) = state_root.linux_capture_anchors();
    let state = layout.state_subdir();
    let mut scope = StockScope::cold(root, proc_fd, state.as_std_path(), &io, &mut work, expected)?;
    let observed = stock_vfs::writer::with_preflight_reader(
        &mut scope,
        state.join("ledger.db").as_std_path(),
        &io,
        &mut work,
        |reader| inspect(reader, phase.deadline),
    )?;
    let state_matches =
        ContainedRoot::open(state_anchor, &io).ok()?.identity() == state_root.identity();
    let checkout_matches =
        ContainedRoot::open(checkout_path, &io).ok()?.identity() == checkout.identity();
    (state_matches && checkout_matches).then_some(observed)
}

/// Sealed native check: storage can invoke it but cannot access its SQL connection.
#[cfg(target_os = "linux")]
pub(crate) struct NativeCaptureRecheck<'a> {
    connection: &'a Connection,
}
#[cfg(target_os = "linux")]
impl NativeCaptureRecheck<'_> {
    pub(crate) fn admit(&mut self) -> bool {
        stock_vfs::capture_admit(self.connection)
    }

    pub(crate) fn check(&mut self) -> bool {
        stock_vfs::capture_check(self.connection)
    }
}

pub(crate) struct QualifiedSqliteConnection(Connection);

impl QualifiedSqliteConnection {
    pub(crate) fn into_inner(self) -> Connection {
        self.0
    }
}

struct OpenCaptureDb<'a> {
    // Drop SQLite before the retained scope and Linux watches on early discard.
    connection: QualifiedSqliteConnection,
    observation: Observation<'a>,
    #[cfg(windows)]
    database_lease: DatabaseEntryLease,
    #[cfg(target_os = "linux")]
    linux_guard: StockScope<'a>,
    identity: Identity,
    guard: ScopeGuard,
    head: ObjectId,
}

pub(super) struct PrequalifiedPlanningStore<'memory> {
    #[cfg(windows)]
    _bridge: DatabaseEntryLease,
    #[cfg(target_os = "linux")]
    native: StockScope<'memory>,
    #[cfg(not(target_os = "linux"))]
    _phase: std::marker::PhantomData<&'memory ()>,
    identity: Identity,
    scope: Option<Uuid>,
}

#[cfg(any(windows, target_os = "linux"))]
pub(super) fn prequalify_planning_store<'memory>(
    observation: &mut Observation<'memory>,
    layout: &Layout,
    origin: StateDirOrigin,
    proof: &PairedHeadProof,
) -> Result<PrequalifiedPlanningStore<'memory>, CaptureUnavailable> {
    with_checkpointed_reader(
        observation,
        layout,
        origin,
        proof.head(),
        proof.guard(),
        None,
        |_, _| Ok(()),
    )
    .map(|(store, ())| store)
}

/// The reader and namespace are held through the caller's bounded SQL evaluation.
#[cfg(windows)]
pub(super) fn with_checkpointed_reader<'memory, T>(
    observation: &mut Observation<'memory>,
    layout: &Layout,
    origin: StateDirOrigin,
    head: ObjectId,
    guard: &ScopeGuard,
    deadline: Option<Instant>,
    inspect: impl FnOnce(&rusqlite::Transaction<'_>, Option<Uuid>) -> Result<T, CaptureUnavailable>,
) -> Result<(PrequalifiedPlanningStore<'memory>, T), CaptureUnavailable> {
    use super::capture_session::classify_db;
    require_guard(observation, layout, origin, head, guard).map_err(classify_db)?;
    let bridge = observation
        .root
        .retain_database_entry(DB, &observation.io)
        .map_err(|error| classify_db(main_error(error)))?;
    let exclusive_read = observation
        .root
        .retain_database_read_excluding_writers(DB, &observation.io)
        .map_err(|_| CaptureUnavailable::PlanningStorageUnavailable)?;
    let expected = bridge.identity();
    if exclusive_read.identity() != expected {
        return Err(CaptureUnavailable::ScopeUnavailable);
    }
    require_absent_companions(observation)?;
    let db_path = layout.state_subdir().join("ledger.db");
    let mut reader =
        sqlite_open::open_immutable_planning_reader(db_path.as_std_path()).map_err(classify_db)?;
    if opened_identity(&reader).ok() != Some(expected)
        || observed_db(observation).ok() != Some(expected)
    {
        return Err(CaptureUnavailable::ScopeUnavailable);
    }
    let (scope, result) = inspect_checkpointed_sql(&mut reader, guard, deadline, inspect)?;
    drop(reader);
    require_absent_companions(observation)?;
    require_guard(observation, layout, origin, head, guard).map_err(classify_db)?;
    drop(exclusive_read);
    Ok((
        PrequalifiedPlanningStore {
            _bridge: bridge,
            _phase: std::marker::PhantomData,
            identity: expected,
            scope,
        },
        result,
    ))
}

#[cfg(target_os = "linux")]
pub(super) fn with_checkpointed_reader<'memory, T>(
    observation: &mut Observation<'memory>,
    layout: &Layout,
    origin: StateDirOrigin,
    head: ObjectId,
    guard: &ScopeGuard,
    deadline: Option<Instant>,
    inspect: impl FnOnce(&rusqlite::Transaction<'_>, Option<Uuid>) -> Result<T, CaptureUnavailable>,
) -> Result<(PrequalifiedPlanningStore<'memory>, T), CaptureUnavailable> {
    use super::capture_session::classify_db;
    require_guard(observation, layout, origin, head, guard).map_err(classify_db)?;
    let expected_ledger = observation
        .root
        .linux_state_identity()
        .ok_or(CaptureUnavailable::ScopeUnavailable)?;
    let (root, proc_fd) = observation.root.linux_capture_anchors();
    let state = layout.state_subdir();
    let mut native = StockScope::cold(
        root,
        proc_fd,
        state.as_std_path(),
        &observation.io,
        &mut observation.work,
        expected_ledger,
    )
    .ok_or(CaptureUnavailable::PlanningStorageUnavailable)?;
    let identity = native
        .main_identity()
        .ok_or(CaptureUnavailable::ScopeUnavailable)?;
    let (scope, result) = stock_vfs::writer::open_preflight(
        &mut native,
        state.join("ledger.db").as_std_path(),
        &observation.io,
        &mut observation.work,
        |reader| inspect_checkpointed_sql(reader, guard, deadline, inspect),
    )
    .ok_or(CaptureUnavailable::PlanningStorageUnavailable)??;
    require_guard(observation, layout, origin, head, guard).map_err(classify_db)?;
    stock_vfs::with_scope(&mut native, &observation.io, &mut observation.work, || ())
        .ok_or(CaptureUnavailable::ScopeUnavailable)?;
    Ok((
        PrequalifiedPlanningStore {
            native,
            identity,
            scope,
        },
        result,
    ))
}

/// Evaluate scope and optional planning inputs within one already qualified snapshot.
#[cfg(any(windows, target_os = "linux"))]
fn inspect_checkpointed_sql<T>(
    reader: &mut Connection,
    guard: &ScopeGuard,
    deadline: Option<Instant>,
    inspect: impl FnOnce(&rusqlite::Transaction<'_>, Option<Uuid>) -> Result<T, CaptureUnavailable>,
) -> Result<(Option<Uuid>, T), CaptureUnavailable> {
    if let Some(deadline) = deadline {
        reader
            .progress_handler(1_000, Some(move || Instant::now() >= deadline))
            .map_err(|_| CaptureUnavailable::PlanningStorageUnavailable)?;
    }
    let tx = reader
        .unchecked_transaction()
        .map_err(|_| CaptureUnavailable::StorageUnavailable)?;
    let scope =
        resolve_optional_qualified_tx(&tx, guard.as_bytes()).map_err(|error| match error {
            EvidenceStorageError::Uninitialized | EvidenceStorageError::SchemaIncompatible => {
                CaptureUnavailable::PlanningStorageUnavailable
            }
            _ => CaptureUnavailable::ScopeUnavailable,
        })?;
    let result = inspect(&tx, scope)?;
    tx.commit()
        .map_err(|_| CaptureUnavailable::StorageUnavailable)?;
    Ok((scope, result))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(super) fn prequalify_planning_store<'memory>(
    _: &mut Observation<'memory>,
    _: &Layout,
    _: StateDirOrigin,
    _: &PairedHeadProof,
) -> Result<PrequalifiedPlanningStore<'memory>, CaptureUnavailable> {
    Err(CaptureUnavailable::PlatformUnavailable)
}

#[cfg(windows)]
fn require_absent_companions(observation: &Observation<'_>) -> Result<(), CaptureUnavailable> {
    for path in [WAL, SHM, JOURNAL] {
        if !matches!(
            observe_file(&observation.root, path, &observation.io),
            Err(ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ) {
            return Err(CaptureUnavailable::PlanningStorageUnavailable);
        }
    }
    Ok(())
}

pub(crate) struct BeforeExecution<'a>(OpenCaptureDb<'a>);
pub(crate) struct AfterExecution<'a>(OpenCaptureDb<'a>);

impl<'a> BeforeExecution<'a> {
    pub(crate) fn open(
        observation: Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        proof: &PairedHeadProof,
    ) -> Result<Self, CaptureDbUnavailable> {
        Self::open_bound(
            observation,
            layout,
            origin,
            proof.head(),
            proof.guard(),
            None,
        )
    }

    pub(super) fn open_planning(
        observation: Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        proof: &PairedHeadProof,
        prequalified: PrequalifiedPlanningStore<'a>,
    ) -> Result<(Self, Option<Uuid>), CaptureUnavailable> {
        use super::capture_session::classify_db;

        let initial_scope = prequalified.scope;
        #[cfg(not(target_os = "linux"))]
        let mut opened = Self::open_bound(
            observation,
            layout,
            origin,
            proof.head(),
            proof.guard(),
            Some(prequalified.identity),
        )
        .map_err(classify_db)?;
        #[cfg(target_os = "linux")]
        let mut opened = Self::open_prequalified(
            observation,
            layout,
            origin,
            proof.head(),
            proof.guard(),
            prequalified,
        )
        .map_err(classify_db)?;
        let scope = opened.scope_binding()?;
        if initial_scope != scope {
            return Err(CaptureUnavailable::ScopeUnavailable);
        }
        Ok((opened, scope))
    }

    #[cfg(not(target_os = "linux"))]
    fn open_bound(
        mut observation: Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        head: ObjectId,
        guard: &ScopeGuard,
        preflight_identity: Option<Identity>,
    ) -> Result<Self, CaptureDbUnavailable> {
        if !cfg!(any(windows, target_os = "linux")) {
            return Err(CaptureDbUnavailable::UnsupportedIdentity);
        }
        require_guard(&mut observation, layout, origin, head, guard)?;
        #[cfg(windows)]
        let database_lease = observation
            .root
            .retain_database_entry(DB, &observation.io)
            .map_err(main_error)?;
        #[cfg(windows)]
        let expected = database_lease.identity();
        #[cfg(not(any(windows, target_os = "linux")))]
        let expected = observed_db(&observation)?;
        let bound = preflight_identity.unwrap_or(expected);
        if expected != bound {
            return Err(CaptureDbUnavailable::Identity);
        }
        confirm_companions(&observation.root, &observation.io)?;
        let db_path = layout.state_subdir().join("ledger.db");
        let connection = sqlite_open::open_existing(db_path.as_std_path())?;
        #[cfg(windows)]
        if opened_identity(&connection)? != bound {
            return Err(CaptureDbUnavailable::Identity);
        }
        if observed_db(&observation)? != bound {
            return Err(CaptureDbUnavailable::Identity);
        }
        require_guard(&mut observation, layout, origin, head, guard)?;
        confirm_companions(&observation.root, &observation.io)?;
        let version =
            i64::try_from(get_migrations_count()).map_err(|_| CaptureDbUnavailable::Schema)?;
        sqlite_open::qualify_schema(&connection, version)?;
        require_guard(&mut observation, layout, origin, head, guard)?;
        #[cfg(windows)]
        if opened_identity(&connection)? != bound {
            return Err(CaptureDbUnavailable::Identity);
        }
        if observed_db(&observation)? != bound {
            return Err(CaptureDbUnavailable::Identity);
        }
        confirm_companions(&observation.root, &observation.io)?;
        let opened = OpenCaptureDb {
            observation,
            #[cfg(windows)]
            database_lease,
            connection: QualifiedSqliteConnection(connection),
            identity: expected,
            guard: *guard,
            head,
        };
        Ok(Self(opened))
    }

    #[cfg(target_os = "linux")]
    fn open_bound(
        mut observation: Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        head: ObjectId,
        guard: &ScopeGuard,
        _: Option<Identity>,
    ) -> Result<Self, CaptureDbUnavailable> {
        let deadline = observation.io.deadline();
        let (qualified, _) = with_checkpointed_reader(
            &mut observation,
            layout,
            origin,
            head,
            guard,
            Some(deadline),
            |_, _| Ok(()),
        )
        .map_err(|error| match error {
            CaptureUnavailable::RevisionChanged => CaptureDbUnavailable::RevisionChanged,
            CaptureUnavailable::ScopeUnavailable => CaptureDbUnavailable::Scope,
            _ => CaptureDbUnavailable::Sqlite,
        })?;
        Self::open_prequalified(observation, layout, origin, head, guard, qualified)
    }

    #[cfg(target_os = "linux")]
    fn open_prequalified(
        mut observation: Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        head: ObjectId,
        guard: &ScopeGuard,
        qualified: PrequalifiedPlanningStore<'a>,
    ) -> Result<Self, CaptureDbUnavailable> {
        require_guard(&mut observation, layout, origin, head, guard)?;
        let PrequalifiedPlanningStore {
            mut native,
            identity,
            scope,
        } = qualified;
        let version =
            i64::try_from(get_migrations_count()).map_err(|_| CaptureDbUnavailable::Schema)?;
        let connection = stock_vfs::writer::open_authorized(
            &mut native,
            layout.state_subdir().join("ledger.db").as_std_path(),
            &observation.io,
            &mut observation.work,
            |connection| {
                sqlite_open::qualify_schema(connection, version).is_ok()
                    && resolve_initial_scope(connection, guard).is_ok_and(|seen| seen == scope)
            },
        )
        .ok_or(CaptureDbUnavailable::Sqlite)?;
        require_guard(&mut observation, layout, origin, head, guard)?;
        stock_vfs::with_scope(&mut native, &observation.io, &mut observation.work, || {
            stock_vfs::capture_check(&connection)
        })
        .filter(|&good| good)
        .ok_or(CaptureDbUnavailable::Identity)?;
        Ok(Self(OpenCaptureDb {
            observation,
            linux_guard: native,
            connection: QualifiedSqliteConnection(connection),
            identity,
            guard: *guard,
            head,
        }))
    }

    /// Snapshot the initial stored owner using the same held connection.
    pub(crate) fn scope_binding(&mut self) -> Result<Option<Uuid>, CaptureUnavailable> {
        self.0
            .observation
            .io
            .checkpoint()
            .map_err(|_| CaptureUnavailable::CaptureBudget)?;
        let store = &mut self.0;
        #[cfg(target_os = "linux")]
        let owner = stock_vfs::with_scope(
            &mut store.linux_guard,
            &store.observation.io,
            &mut store.observation.work,
            || resolve_initial_scope(&store.connection.0, &store.guard),
        )
        .ok_or(CaptureUnavailable::ScopeUnavailable)??;
        #[cfg(not(target_os = "linux"))]
        let owner = resolve_initial_scope(&store.connection.0, &store.guard)?;
        store
            .observation
            .io
            .checkpoint()
            .map_err(|_| CaptureUnavailable::CaptureBudget)?;
        Ok(owner)
    }

    pub(crate) fn confirm_before_execution(
        mut self,
        phase_observation: &mut Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        proof: &PairedHeadProof,
    ) -> Result<AfterExecution<'a>, CaptureDbUnavailable> {
        self.0.recheck(phase_observation, layout, origin, proof)?;
        Ok(AfterExecution(self.0))
    }

    pub(crate) fn confirm_after_planning(
        mut self,
        mut phase_observation: Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        proof: &PairedHeadProof,
    ) -> Result<QualifiedCaptureStorage<'a>, CaptureDbUnavailable> {
        self.0
            .recheck(&mut phase_observation, layout, origin, proof)?;
        Ok(self.0.into_qualified(layout, phase_observation))
    }
}

fn resolve_initial_scope(
    connection: &Connection,
    guard: &ScopeGuard,
) -> Result<Option<Uuid>, CaptureUnavailable> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| CaptureUnavailable::StorageUnavailable)?;
    let owner = resolve_optional_qualified_tx(&tx, guard.as_bytes())
        .map_err(|_| CaptureUnavailable::ScopeUnavailable)?;
    tx.commit()
        .map_err(|_| CaptureUnavailable::StorageUnavailable)?;
    Ok(owner)
}

impl<'a> AfterExecution<'a> {
    pub(crate) fn confirm_after_termination(
        mut self,
        mut phase_observation: Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        proof: &PairedHeadProof,
    ) -> Result<QualifiedCaptureStorage<'a>, CaptureDbUnavailable> {
        self.0
            .recheck(&mut phase_observation, layout, origin, proof)?;
        Ok(self.0.into_qualified(layout, phase_observation))
    }
}

/// The SQLite connection and the original native directory/file owner cannot
/// be separated. A write consumes this wrapper and runs synchronously before
/// any of its owners can drop.
pub(crate) struct QualifiedCaptureStorage<'a> {
    storage: StorageManager,
    owner: Observation<'a>,
    #[cfg(windows)]
    _database_lease: DatabaseEntryLease,
    #[cfg(target_os = "linux")]
    linux_guard: StockScope<'a>,
    #[cfg(target_os = "linux")]
    phase_work: super::super::pack::Work<'a>,
    #[cfg(target_os = "linux")]
    phase_io: ReadBudget<'a>,
    identity: Identity,
    guard: ScopeGuard,
    head: ObjectId,
    deadline: Instant,
}

impl QualifiedCaptureStorage<'_> {
    pub(crate) fn write_executed(
        mut self,
        execution: &CompletedExecution<'_>,
        attempt: PreparedExecutedAttempt,
    ) -> Result<VerificationCaptureCommit, CaptureUnavailable> {
        self.confirm_for_write(attempt.identity())?;
        #[cfg(target_os = "linux")]
        {
            let mut recheck = NativeCaptureRecheck {
                connection: self.storage.get_connection(),
            };
            let mut observed = None;
            let confirmed = stock_vfs::with_scope(
                &mut self.linux_guard,
                &self.phase_io,
                &mut self.phase_work,
                || {
                    let outcome = self.storage.persist_verification_with_capture(
                        execution,
                        attempt,
                        &mut recheck,
                    );
                    observed = Some(outcome);
                    outcome
                },
            );
            match (confirmed, observed) {
                (Some(outcome), _) => Ok(outcome),
                (
                    None,
                    Some(
                        outcome @ (VerificationCaptureCommit::RolledBack(_)
                        | VerificationCaptureCommit::Unconfirmed(_)),
                    ),
                ) => Ok(outcome),
                // The writer ran and the boundary failed afterwards. The
                // canonical transaction may already be durable, so the engine
                // must not invoke its legacy writer for this same run.
                (None, Some(_)) => Ok(VerificationCaptureCommit::Unconfirmed(
                    TransactionStage::Commit,
                )),
                (None, None) => Err(CaptureUnavailable::ScopeUnavailable),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(self
                .storage
                .persist_verification_with_capture(execution, attempt))
        }
    }

    pub(crate) fn write_planning(
        mut self,
        identity: AttemptIdentity,
        payload: AttemptPayload,
    ) -> PlanningCaptureCommit {
        if let Err(reason) = self.confirm_for_write(&identity) {
            return PlanningCaptureCommit::Unavailable(reason);
        }
        #[cfg(target_os = "linux")]
        {
            let mut recheck = NativeCaptureRecheck {
                connection: self.storage.get_connection(),
            };
            let mut observed = None;
            let confirmed = stock_vfs::with_scope(
                &mut self.linux_guard,
                &self.phase_io,
                &mut self.phase_work,
                || {
                    let outcome =
                        self.storage
                            .persist_planning_attempt(identity, payload, &mut recheck);
                    observed = Some(outcome);
                    outcome
                },
            );
            match (confirmed, observed) {
                (Some(outcome), _) => outcome,
                (None, Some(outcome @ PlanningCaptureCommit::Unconfirmed(_))) => outcome,
                (None, Some(_)) => PlanningCaptureCommit::Unconfirmed(TransactionStage::Commit),
                (None, None) => {
                    PlanningCaptureCommit::Unavailable(CaptureUnavailable::ScopeUnavailable)
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.storage.persist_planning_attempt(identity, payload)
        }
    }

    fn confirm_for_write(&mut self, attempt: &AttemptIdentity) -> Result<(), CaptureUnavailable> {
        if &attempt.scope_guard != self.guard.as_bytes() || attempt.head != self.head.to_string() {
            return Err(CaptureUnavailable::ScopeUnavailable);
        }
        // Sample companion names once more immediately before SQLite work.
        // The original main-file and ancestor leases remain held while SQLite
        // owns the transaction. This does not bind SQLite's auxiliary handles.
        let interrupted = AtomicBool::new(false);
        let now = Instant::now();
        if now >= self.deadline {
            return Err(CaptureUnavailable::CaptureBudget);
        }
        let budget = ReadBudget::new(
            0,
            self.deadline.min(now + Duration::from_millis(100)),
            &interrupted,
        );
        #[cfg(windows)]
        if opened_identity(self.storage.get_connection()).ok() != Some(self.identity) {
            return Err(CaptureUnavailable::ScopeUnavailable);
        }
        #[cfg(target_os = "linux")]
        if stock_vfs::with_scope(
            &mut self.linux_guard,
            &self.phase_io,
            &mut self.phase_work,
            || stock_vfs::capture_check(self.storage.get_connection()),
        ) != Some(true)
        {
            self.phase_io
                .checkpoint()
                .map_err(|_| CaptureUnavailable::CaptureBudget)?;
            return Err(CaptureUnavailable::ScopeUnavailable);
        }
        if observe_file(&self.owner.root, DB, &budget).ok() != Some(self.identity)
            || confirm_companions(&self.owner.root, &budget).is_err()
        {
            return Err(CaptureUnavailable::ScopeUnavailable);
        }
        budget
            .checkpoint()
            .map_err(|_| CaptureUnavailable::CaptureBudget)?;
        Ok(())
    }
}

impl<'a> OpenCaptureDb<'a> {
    fn into_qualified(
        self,
        layout: &Layout,
        phase: Observation<'a>,
    ) -> QualifiedCaptureStorage<'a> {
        let deadline = phase.io.deadline();
        QualifiedCaptureStorage {
            storage: StorageManager::from_capture_connection(self.connection, layout),
            owner: self.observation,
            #[cfg(windows)]
            _database_lease: self.database_lease,
            #[cfg(target_os = "linux")]
            linux_guard: self.linux_guard,
            #[cfg(target_os = "linux")]
            phase_work: phase.work,
            #[cfg(target_os = "linux")]
            phase_io: phase.io,
            identity: self.identity,
            guard: self.guard,
            head: self.head,
            deadline,
        }
    }

    fn recheck(
        &mut self,
        phase_observation: &mut Observation<'a>,
        layout: &Layout,
        origin: StateDirOrigin,
        proof: &PairedHeadProof,
    ) -> Result<(), CaptureDbUnavailable> {
        if proof.head() != self.head || proof.guard() != &self.guard {
            return Err(CaptureDbUnavailable::Scope);
        }
        // Keep the original directory handles alive while each phase can use
        // a newly budgeted observation. Step runtime does not drain that phase's
        // observation deadline.
        if phase_observation.root.identity() != self.observation.root.identity() {
            return Err(CaptureDbUnavailable::Scope);
        }
        require_guard(phase_observation, layout, origin, self.head, &self.guard)?;
        #[cfg(windows)]
        if opened_identity(&self.connection.0)? != self.identity {
            return Err(CaptureDbUnavailable::Identity);
        }
        #[cfg(target_os = "linux")]
        if stock_vfs::with_scope(
            &mut self.linux_guard,
            &phase_observation.io,
            &mut phase_observation.work,
            || stock_vfs::capture_check(&self.connection.0),
        ) != Some(true)
        {
            return Err(CaptureDbUnavailable::Identity);
        }
        if observed_db(phase_observation)? != self.identity {
            return Err(CaptureDbUnavailable::Identity);
        }
        confirm_companions(&phase_observation.root, &phase_observation.io)
    }
}

fn require_guard(
    observation: &mut Observation<'_>,
    layout: &Layout,
    origin: StateDirOrigin,
    expected_head: ObjectId,
    expected: &ScopeGuard,
) -> Result<(), CaptureDbUnavailable> {
    let actual = observation
        .confirm_scoped_guard(layout, origin, expected_head)
        .map_err(|reason| match reason {
            "head-mismatch" | "refs-changed" => CaptureDbUnavailable::RevisionChanged,
            _ => CaptureDbUnavailable::Scope,
        })?;
    if &actual != expected {
        return Err(CaptureDbUnavailable::Scope);
    }
    Ok(())
}

fn observe_file(
    root: &ContainedRoot,
    relative: &str,
    budget: &ReadBudget<'_>,
) -> Result<Identity, ReadError> {
    #[cfg(windows)]
    {
        root.observe_database_entry(relative, budget)
    }
    #[cfg(not(windows))]
    {
        root.observe_entry(relative, false, budget)
    }
}

fn main_error(error: ReadError) -> CaptureDbUnavailable {
    match error {
        ReadError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            CaptureDbUnavailable::Missing
        }
        _ => CaptureDbUnavailable::Identity,
    }
}

fn observed_db(observation: &Observation<'_>) -> Result<Identity, CaptureDbUnavailable> {
    observe_file(&observation.root, DB, &observation.io).map_err(main_error)
}

fn confirm_companions(
    root: &ContainedRoot,
    budget: &ReadBudget<'_>,
) -> Result<(), CaptureDbUnavailable> {
    for path in [WAL, SHM, JOURNAL] {
        match observe_file(root, path, budget) {
            Err(ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) if path != JOURNAL => {}
            _ => return Err(CaptureDbUnavailable::Identity),
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn opened_identity(connection: &Connection) -> Result<Identity, CaptureDbUnavailable> {
    let (volume, file) = sqlite_open::windows_main_identity(connection)?;
    Ok(Identity::from_windows_file_id(volume, file))
}
