//! Collect one complete held-directory sample under the observation's budgets.
use super::buffer::{Reservation, ReservedBytes};
use super::contained::{
    DirectoryVisitError, EntryKind, Identity, MAX_DIRECTORY_ENTRIES, MAX_NAME_BYTES,
    MAX_RELATIVE_BYTES, NativeDirectoryRow, ReadBudget, ReadError,
};
use super::pack::Work;
use super::scope_owner::ScopeRoot;
use std::mem::size_of;

type RowResult<T> = Result<T, DirectoryVisitError<&'static str>>;

pub(crate) struct OwnedDirectoryRow<'a> {
    pub(crate) name: ReservedBytes<'a>,
    pub(crate) kind: EntryKind,
    pub(crate) windows_hidden: bool,
}

pub(crate) struct DirectoryRows<'a> {
    pub(crate) identity: Identity,
    rows: Vec<OwnedDirectoryRow<'a>>,
    _table: Reservation<'a>,
}

impl<'a> DirectoryRows<'a> {
    pub(crate) fn rows(&self) -> &[OwnedDirectoryRow<'a>] {
        &self.rows
    }

    pub(crate) fn capture(
        root: &mut ScopeRoot<'_>,
        relative: &str,
        limit: usize,
        io: &ReadBudget<'a>,
        work: &mut Work<'a>,
    ) -> RowResult<Self> {
        if relative.len() > MAX_RELATIVE_BYTES {
            return Err(ReadError::Path.into());
        }
        let limit = limit.min(MAX_DIRECTORY_ENTRIES);
        let bytes = limit * size_of::<OwnedDirectoryRow<'a>>();
        let table = reserve(work, bytes).map_err(DirectoryVisitError::Visitor)?;
        let _path = work
            .reserve_lookup(relative.len())
            .map_err(DirectoryVisitError::Visitor)?;
        // The readers decode into a bounded stack buffer/borrowed page. Hold room
        // for the existing Windows per-name component validation's temporary string.
        let _name = reserve(work, MAX_NAME_BYTES).map_err(DirectoryVisitError::Visitor)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(limit)
            .map_err(|_| DirectoryVisitError::Visitor("allocation"))?;
        if rows.capacity() != limit {
            return Err(DirectoryVisitError::Visitor("memory-cap"));
        }
        let identity = root.visit_directory(relative, limit, io, |row| match row {
            NativeDirectoryRow::NameBytes(bytes) => work.charge(4 * bytes + 1),
            NativeDirectoryRow::Entry {
                name,
                kind,
                windows_hidden,
            } => insert_row(
                &mut rows,
                limit,
                name.as_bytes(),
                kind,
                windows_hidden,
                work,
            ),
        })?;
        work.charge(0).map_err(DirectoryVisitError::Visitor)?;
        Ok(Self {
            identity,
            rows,
            _table: table,
        })
    }
}

fn insert_row<'a>(
    rows: &mut Vec<OwnedDirectoryRow<'a>>,
    limit: usize,
    name: &[u8],
    kind: EntryKind,
    windows_hidden: bool,
    work: &mut Work<'a>,
) -> Result<(), &'static str> {
    // Both the held-reader count and the preallocated table must admit this row.
    if rows.len() >= limit {
        return Err("directory-entry-cap");
    }
    let comparisons = (usize::BITS - rows.len().leading_zeros()) as usize + 1;
    work.charge(comparisons * (name.len() + 1))?;
    let index = rows
        .binary_search_by(|row| row.name.as_ref().cmp(name))
        .err()
        .ok_or("directory-duplicate-name")?;
    work.charge(name.len() + (rows.len() - index) * size_of::<OwnedDirectoryRow<'a>>())?;
    let reservation = reserve(work, name.len())?;
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(name.len())
        .map_err(|_| "allocation")?;
    if owned.capacity() != name.len() {
        return Err("memory-cap");
    }
    owned.extend_from_slice(name);
    let name = reservation.into_bytes(owned).map_err(|_| "memory-cap")?;
    rows.insert(
        index,
        OwnedDirectoryRow {
            name,
            kind,
            windows_hidden,
        },
    );
    Ok(())
}

fn reserve<'a>(work: &mut Work<'a>, bytes: usize) -> Result<Reservation<'a>, &'static str> {
    if work.read_limit(bytes, 0)? != bytes {
        return Err("memory-cap");
    }
    work.memory_budget()
        .reserve(bytes)
        .map_err(|_| "memory-cap")
}
