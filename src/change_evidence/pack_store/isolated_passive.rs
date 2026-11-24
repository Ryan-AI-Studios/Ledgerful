//! Short SQLite observations over the independently held canonical inputs.

use super::{
    isolated_fd::{GuardError, guard_work_error},
    isolated_readonly_inputs::{InputRole, IsolatedReadInputs, with_isolated_readonly_inputs},
    isolated_readonly_pager::{
        InputError, PagerError, PagerSource, Role, with_readonly_connection,
    },
    linux_capture::LinuxReadScope,
    passive_db::PassiveReadError,
    published_snapshot::{
        SnapshotError, SnapshotValidator, ValidatedSnapshot, WAL_HEADER, verify_empty,
    },
};
use crate::change_evidence::{
    buffer::ReservedBytes,
    contained::ReadBudget,
    http_worker::{WorkerContext, WorkerError},
    native_heap::NativeHeapTicket,
    pack::Work,
};
use rusqlite::Connection;

const PAGE_BYTES: u32 = 4096;
const MAX_WAL_PREFIX: u64 = 4 * 1024 * 1024;
const MAX_DATABASE_EXTENT: u64 = 64 * 1024 * 1024;
pub(super) const SNAPSHOTS_PER_REQUEST: usize = 2;
const SCOPE_WORK_PER_SNAPSHOT: usize = 2 * 1024 * 1024;

pub(super) fn storage_work_units(read_bytes: usize) -> usize {
    // A backed pager read charges two output passes and a helper copy.
    // Four units per admitted native byte also cover per-read bookkeeping
    // and the ordinary WAL certification and first private recovery pass.
    let backed_reads = 4 * read_bytes.min(MAX_DATABASE_EXTENT as usize);
    // Retained WAL reads consume no further shared I/O bytes. Allow another
    // WAL traversal in SQL and precharge initializing its retained buffer.
    let wal_and_scope = 3 * MAX_WAL_PREFIX as usize + SCOPE_WORK_PER_SNAPSHOT;
    backed_reads + SNAPSHOTS_PER_REQUEST * wal_and_scope
}

pub(super) fn observe<'memory, T>(
    scope: &LinuxReadScope<'_>,
    io: &ReadBudget<'memory>,
    work: &mut Work<'memory>,
    heap: &NativeHeapTicket<'memory>,
    worker: &WorkerContext,
    query: impl FnOnce(&Connection) -> Result<T, PassiveReadError>,
) -> Result<T, PassiveReadError> {
    let result = with_isolated_readonly_inputs(scope, io, work, |work, inputs| {
        let checked = certify(inputs, work)?;
        let mut view = CertifiedSource {
            checked,
            inputs,
            work,
        };
        let result = heap
            .activate(|| with_readonly_connection(&mut view, Some(worker), query))
            .ok_or(GuardError::Unavailable)?;
        if heap.refusal().is_some() {
            return Err(GuardError::Budget);
        }
        view.inputs.check_stable(view.work)?;
        Ok(result.map_err(pager_error))
    });
    result.map_err(guard_error)?
}

struct CertifiedSnapshot<'memory> {
    metadata: ValidatedSnapshot,
    wal: Option<ReservedBytes<'memory>>,
}

fn certify<'memory>(
    inputs: &mut IsolatedReadInputs<'_, 'memory>,
    work: &mut Work<'memory>,
) -> Result<CertifiedSnapshot<'memory>, GuardError> {
    let mut index = [0; 96];
    inputs.read_exact_at(InputRole::Shm, 0, &mut index, work)?;
    let main_len = inputs.len(InputRole::Main);
    let wal_len = inputs.len(InputRole::Wal);
    let mut main = [0; 100];
    inputs.read_exact_at(InputRole::Main, 0, &mut main, work)?;
    if wal_len == 0 {
        let metadata = verify_empty(
            &index,
            &main,
            main_len,
            wal_len,
            PAGE_BYTES,
            MAX_DATABASE_EXTENT,
        )
        .map_err(snapshot_error)?;
        return Ok(CertifiedSnapshot {
            metadata,
            wal: None,
        });
    }
    if &main[..16] != b"SQLite format 3\0"
        || main[18..20] != [2, 2]
        || u32::from(u16::from_be_bytes([main[16], main[17]])) != PAGE_BYTES
        || main_len < u64::from(PAGE_BYTES)
        || !main_len.is_multiple_of(u64::from(PAGE_BYTES))
    {
        return Err(GuardError::Unavailable);
    }
    let mut header = [0; WAL_HEADER];
    inputs.read_exact_at(InputRole::Wal, 0, &mut header, work)?;
    let mut verifier = SnapshotValidator::begin(
        &index,
        &header,
        wal_len,
        PAGE_BYTES,
        MAX_WAL_PREFIX,
        MAX_DATABASE_EXTENT,
    )
    .map_err(snapshot_error)?;
    let plan = verifier.read_plan();
    let bytes = usize::try_from(plan.frames)
        .ok()
        .and_then(|frames| frames.checked_mul(plan.frame_bytes))
        .and_then(|frames| frames.checked_add(WAL_HEADER))
        .ok_or(GuardError::Budget)?;
    if work.read_limit(bytes, 0).map_err(guard_work_error)? != bytes {
        return Err(GuardError::Budget);
    }
    let mut reservation = work
        .memory_budget()
        .reserve(bytes)
        .map_err(|_| GuardError::Budget)?;
    let mut wal = Vec::new();
    wal.try_reserve_exact(bytes)
        .map_err(|_| GuardError::Budget)?;
    reservation
        .account_capacity(wal.capacity())
        .map_err(|_| GuardError::Budget)?;
    work.charge(bytes).map_err(guard_work_error)?;
    wal.resize(bytes, 0);
    wal[..WAL_HEADER].copy_from_slice(&header);
    let mut offset = WAL_HEADER as u64;
    for frame in wal[WAL_HEADER..].chunks_exact_mut(plan.frame_bytes) {
        inputs.read_exact_at(InputRole::Wal, offset, frame, work)?;
        work.charge(plan.frame_bytes.div_ceil(8))
            .map_err(guard_work_error)?;
        verifier.push_frame(frame).map_err(snapshot_error)?;
        offset += plan.frame_bytes as u64;
    }
    let metadata = verifier.finish().map_err(snapshot_error)?;
    let wal = reservation
        .into_bytes(wal)
        .map_err(|_| GuardError::Budget)?;
    Ok(CertifiedSnapshot {
        metadata,
        wal: Some(wal),
    })
}

struct CertifiedSource<'a, 'io, 'memory> {
    checked: CertifiedSnapshot<'memory>,
    inputs: &'a mut IsolatedReadInputs<'io, 'memory>,
    work: &'a mut Work<'memory>,
}

impl PagerSource for CertifiedSource<'_, '_, '_> {
    fn len(&self, role: Role) -> Option<u64> {
        Some(match role {
            Role::Main => self
                .inputs
                .len(InputRole::Main)
                .min(self.checked.metadata.database_bytes()),
            Role::Wal => self.checked.metadata.wal_bytes(),
        })
    }

    fn charge(&mut self, units: usize) -> Result<(), InputError> {
        self.work.charge(units).map_err(|_| InputError::Budget)
    }

    fn read(&mut self, role: Role, offset: u64, out: &mut [u8]) -> Result<(), InputError> {
        if role == Role::Wal {
            let start = usize::try_from(offset).map_err(|_| InputError::Unavailable)?;
            let end = start
                .checked_add(out.len())
                .ok_or(InputError::Unavailable)?;
            let wal = self.checked.wal.as_ref().ok_or(InputError::Unavailable)?;
            let exact = wal
                .as_ref()
                .get(start..end)
                .ok_or(InputError::Unavailable)?;
            out.copy_from_slice(exact);
            return Ok(());
        }
        self.inputs
            .read_exact_at(InputRole::Main, offset, out, self.work)
            .map_err(|error| match error {
                GuardError::Budget | GuardError::Deadline => InputError::Budget,
                _ => InputError::Unavailable,
            })
    }
}

fn snapshot_error(error: SnapshotError) -> GuardError {
    match error {
        SnapshotError::PrefixTooLarge | SnapshotError::DatabaseTooLarge => GuardError::Budget,
        _ => GuardError::Unavailable,
    }
}

pub(super) fn guard_error(error: GuardError) -> PassiveReadError {
    match error {
        GuardError::Unavailable => PassiveReadError::ScopeUnavailable,
        GuardError::Uninitialized => PassiveReadError::Uninitialized,
        GuardError::Busy => PassiveReadError::StorageBusy,
        GuardError::Deadline | GuardError::Budget => PassiveReadError::RequestBudget,
    }
}

fn pager_error(error: PagerError<PassiveReadError>) -> PassiveReadError {
    match error {
        PagerError::Input(InputError::Budget) | PagerError::Worker(WorkerError::Cancelled) => {
            PassiveReadError::RequestBudget
        }
        PagerError::Input(InputError::Unavailable) => PassiveReadError::ScopeUnavailable,
        PagerError::Sqlite(error) => {
            crate::state::storage::change_evidence::sql_error(error).into()
        }
        PagerError::Worker(_) | PagerError::Retirement => PassiveReadError::Internal,
        PagerError::Query(error) => error,
    }
}
