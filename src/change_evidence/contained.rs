//! Read bytes from a retained repository root without following redirects.
#[cfg(windows)]
use super::buffer::Reservation;
use super::buffer::{MemoryBudget, ReservedBytes};
#[cfg(any(windows, target_os = "linux"))]
use super::pack::Work;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

pub(super) const MAX_RELATIVE_BYTES: usize = 4_096;
const MAX_COMPONENTS: usize = 64;
const CHUNK_SIZE: usize = 64 * 1_024;
pub(super) const MAX_DIRECTORY_ENTRIES: usize = 4_096;
pub(super) const MAX_NAME_BYTES: usize = 1_024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadError {
    #[error("unsupported repository path")]
    Path,
    #[error("repository input is not a regular file or directory")]
    Kind,
    #[error("unsupported repository link or mount")]
    Redirect,
    #[error("contained repository reads are unavailable on this platform")]
    Unavailable,
    #[error("repository observation interrupted")]
    Interrupted,
    #[error("repository observation reached its time budget")]
    Deadline,
    #[error("repository input changed during the read")]
    Changed,
    #[error("repository input exceeded its byte limit")]
    Bytes,
    #[error("repository directory exceeded its entry limit")]
    Entries,
    #[error("repository observation exhausted its aggregate byte limit")]
    Aggregate,
    #[cfg(any(windows, target_os = "linux"))]
    #[error("repository observation exhausted its work limit")]
    Work,
    #[error("repository observation exhausted its buffer memory limit")]
    Memory,
    #[error("could not allocate a bounded repository input buffer")]
    Allocation,
    #[error("repository input could not be opened or read")]
    Io(#[from] io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    volume: u64,
    file: u128,
}

impl Identity {
    #[cfg(target_os = "linux")]
    pub(super) fn from_linux_file_id(volume: u64, inode: u64) -> Self {
        Self {
            volume,
            file: u128::from(inode),
        }
    }
    #[cfg(windows)]
    pub(super) fn from_windows_file_id(volume: u64, identifier: [u8; 16]) -> Self {
        Self {
            volume,
            file: u128::from_le_bytes(identifier),
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn from_linux_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            volume: metadata.dev(),
            file: u128::from(metadata.ino()),
        }
    }

    pub(super) fn inventory_bytes(self) -> [u8; 24] {
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&self.volume.to_be_bytes());
        bytes[8..].copy_from_slice(&self.file.to_be_bytes());
        bytes
    }
}

pub(crate) struct ReadBudget<'a> {
    remaining: usize,
    aggregate: Option<&'a AtomicUsize>,
    deadline: Instant,
    interrupted: &'a AtomicBool,
}

impl<'a> ReadBudget<'a> {
    pub(crate) fn new(remaining: usize, deadline: Instant, interrupted: &'a AtomicBool) -> Self {
        Self {
            remaining,
            aggregate: None,
            deadline,
            interrupted,
        }
    }

    pub(super) fn new_shared(
        remaining: usize,
        deadline: Instant,
        interrupted: &'a AtomicBool,
        aggregate: &'a AtomicUsize,
    ) -> Self {
        Self {
            aggregate: Some(aggregate),
            ..Self::new(remaining, deadline, interrupted)
        }
    }

    fn reserve_file(&self, size: usize) -> Result<(), ReadError> {
        if size > self.remaining {
            return Err(ReadError::Aggregate);
        }
        if let Some(aggregate) = self.aggregate {
            // A failed or interrupted read retains its full attempted charge.
            aggregate
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    left.checked_sub(size)
                })
                .map_err(|_| ReadError::Aggregate)?;
        }
        Ok(())
    }

    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }

    #[cfg(target_os = "linux")]
    pub(super) fn charge_native_read(&self, bytes: usize) -> Result<(), ReadError> {
        self.checkpoint()?;
        self.reserve_file(bytes)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn native_read_view(&self) -> Self {
        // The native closure and checkout callback are sequential. Both
        // charge the same aggregate and retain the original cutoff.
        Self {
            remaining: self.remaining,
            aggregate: self.aggregate,
            deadline: self.deadline,
            interrupted: self.interrupted,
        }
    }

    pub(crate) fn checkpoint(&self) -> Result<(), ReadError> {
        if self.interrupted.load(Ordering::Acquire) {
            return Err(ReadError::Interrupted);
        }
        if Instant::now() >= self.deadline {
            return Err(ReadError::Deadline);
        }
        Ok(())
    }
}

/// Owns the handles for one repository observation. Open a fresh root for each request.
#[cfg(windows)]
pub(super) struct DatabaseEntryLease {
    _lease: platform::Lease,
    identity: Identity,
}

#[cfg(windows)]
impl DatabaseEntryLease {
    pub(super) fn identity(&self) -> Identity {
        self.identity
    }

    /// Samples the fixed SQLite header while a writer-excluding lease is held.
    pub(super) fn database_header(
        &self,
        budget: &ReadBudget<'_>,
    ) -> Result<([u8; 100], u64), ReadError> {
        use std::os::windows::fs::FileExt;

        budget.checkpoint()?;
        let file = self._lease.file();
        let mut header = [0; 100];
        if file.seek_read(&mut header, 0)? != header.len() {
            return Err(ReadError::Unavailable);
        }
        let len = file.metadata()?.len();
        budget.checkpoint()?;
        Ok((header, len))
    }
}

pub(crate) struct ContainedRoot {
    root: platform::Root,
}

/// A regular-file byte sample and metadata from the same contained handle.
pub(crate) struct RegularFileSample<'a> {
    pub(crate) bytes: ReservedBytes<'a>,
    pub(crate) owner_executable: Option<bool>,
    pub(crate) mtime_ns: Option<i64>,
}

struct RegularFileMetadata {
    owner_executable: Option<bool>,
    mtime_ns: Option<i64>,
}

pub(crate) struct RetainedScopeDirectories {
    #[cfg(any(windows, target_os = "linux"))]
    root: ContainedRoot,
    #[cfg(any(windows, target_os = "linux"))]
    children: platform::ScopeChildren,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct NamedDirectory {
    pub(crate) path: String,
    pub(crate) identity: Identity,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ScopeDirectoryNames {
    pub(crate) worktree: NamedDirectory,
    pub(crate) git: NamedDirectory,
    pub(crate) state: NamedDirectory,
}

/// Windows handles and their metered retained ancestor backing storage.
#[cfg(windows)]
pub(crate) struct MeteredWindowsScopeDirectories<'a> {
    held: RetainedScopeDirectories,
    _ancestors: Reservation<'a>,
}

/// Retained proof that every strict default-walker ancestor input is absent.
#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DefaultAncestorAbsence {
    pub(super) digest: [u8; 32],
    pub(super) directories: usize,
}

/// Windows native names and the reservations that cover their backing storage.
#[cfg(windows)]
pub(crate) struct MeteredWindowsScopeDirectoryNames<'a> {
    names: ScopeDirectoryNames,
    _paths: [Reservation<'a>; 3],
}

#[cfg(windows)]
impl MeteredWindowsScopeDirectoryNames<'_> {
    pub(crate) fn names(&self) -> &ScopeDirectoryNames {
        &self.names
    }
}

#[cfg(windows)]
impl<'a> MeteredWindowsScopeDirectories<'a> {
    pub(crate) fn open(
        path: &Path,
        budget: &ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<Self, ReadError> {
        let owner = platform::Root::open_for_scope(path, budget, work)?;
        let children = owner.root.scope_children_metered(budget, work)?;
        Ok(Self {
            held: RetainedScopeDirectories {
                root: ContainedRoot { root: owner.root },
                children,
            },
            _ancestors: owner.reservation,
        })
    }

    pub(crate) fn root(&self) -> &ContainedRoot {
        &self.held.root
    }

    /// Capture known absence of fixed inherited-policy candidates without opening
    /// policy bytes. A present entry is outside this absence-only proof.
    pub(crate) fn observe_default_ancestor_absence(
        &self,
        budget: &ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<Option<DefaultAncestorAbsence>, ReadError> {
        self.held
            .root
            .root
            .observe_default_ancestor_absence(budget, work)
    }

    /// Sample the actual 0.4.33 built-in Windows global-policy fallback.
    /// Each fixed candidate must be absent. External file contents are never read.
    pub(crate) fn observe_default_global_absence(
        &self,
        allow_contained_cwd: bool,
        budget: &mut ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<Option<[u8; 32]>, ReadError> {
        self.held
            .root
            .root
            .observe_default_global_absence(allow_contained_cwd, budget, work)
    }

    pub(crate) fn list(
        &mut self,
        relative: &str,
        entry_limit: usize,
        budget: &ReadBudget<'_>,
    ) -> Result<Vec<DirectoryEntry>, ReadError> {
        self.held.root.list(relative, entry_limit, budget)
    }

    pub(crate) fn visit_directory<E>(
        &mut self,
        relative: &str,
        entry_limit: usize,
        budget: &ReadBudget<'_>,
        visit: impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
    ) -> Result<Identity, DirectoryVisitError<E>> {
        self.held
            .root
            .visit_directory(relative, entry_limit, budget, visit)
    }

    pub(crate) fn observe(
        &self,
        budget: &ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<MeteredWindowsScopeDirectoryNames<'a>, ReadError> {
        self.held.root.root.with_scope_names(
            &self.held.children,
            budget,
            work,
            |[worktree, git, state], work| {
                let worktree = worktree.freeze(budget, work)?;
                let git = git.freeze(budget, work)?;
                let state = state.freeze(budget, work)?;
                Ok(MeteredWindowsScopeDirectoryNames {
                    names: ScopeDirectoryNames {
                        worktree: worktree.named,
                        git: git.named,
                        state: state.named,
                    },
                    _paths: [worktree.reservation, git.reservation, state.reservation],
                })
            },
        )
    }

    pub(crate) fn confirm_reacquired(
        &self,
        served_root: &Path,
        initial: &MeteredWindowsScopeDirectoryNames<'a>,
        budget: &ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<(), ReadError> {
        let confirm = |directories: &Self, work: &mut Work<'a>| {
            directories.held.root.root.with_scope_names(
                &directories.held.children,
                budget,
                work,
                |actual, work| {
                    for (actual, initial) in actual.into_iter().zip([
                        &initial.names.worktree,
                        &initial.names.git,
                        &initial.names.state,
                    ]) {
                        platform::charge_scope(budget, work, actual.path.len())?;
                        if actual.path != initial.path || actual.identity != initial.identity {
                            return Err(ReadError::Changed);
                        }
                    }
                    Ok(())
                },
            )
        };
        confirm(self, work)?;
        let fresh = Self::open(served_root, budget, work)?;
        confirm(&fresh, work)?;
        confirm(self, work)?;
        platform::charge_scope(budget, work, 0)
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct FixedScopeName {
    pub(crate) bytes: [u8; 4096],
    pub(crate) len: usize,
    pub(crate) identity: Identity,
}
#[cfg(target_os = "linux")]
impl FixedScopeName {
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("validated native name")
    }
}
#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct FixedScopeNames {
    pub(crate) worktree: FixedScopeName,
    pub(crate) git: FixedScopeName,
    pub(crate) state: FixedScopeName,
}
impl RetainedScopeDirectories {
    #[cfg(target_os = "linux")]
    pub(crate) fn root(&self) -> &ContainedRoot {
        &self.root
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn list(
        &mut self,
        relative: &str,
        entry_limit: usize,
        budget: &ReadBudget<'_>,
    ) -> Result<Vec<DirectoryEntry>, ReadError> {
        self.root.list(relative, entry_limit, budget)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn visit_directory<E>(
        &mut self,
        relative: &str,
        entry_limit: usize,
        budget: &ReadBudget<'_>,
        visit: impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
    ) -> Result<Identity, DirectoryVisitError<E>> {
        self.root
            .visit_directory(relative, entry_limit, budget, visit)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn observe_fixed(
        &self,
        budget: &ReadBudget<'_>,
    ) -> Result<FixedScopeNames, ReadError> {
        self.root.root.scope_names_fixed(&self.children, budget)
    }
    pub(crate) fn observe(
        &self,
        budget: &ReadBudget<'_>,
    ) -> Result<ScopeDirectoryNames, ReadError> {
        budget.checkpoint()?;
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.root.root.scope_names(&self.children, budget)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        Err(ReadError::Unavailable)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DirectoryEntry {
    pub(crate) name: String,
    pub(crate) kind: EntryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EntryKind {
    File,
    Directory,
    Redirect,
    Other,
}

pub(crate) enum NativeDirectoryRow<'a> {
    NameBytes(usize),
    Entry {
        name: &'a str,
        kind: EntryKind,
        windows_hidden: bool,
    },
}

#[derive(Debug)]
pub(crate) enum DirectoryVisitError<E> {
    Reader(ReadError),
    Visitor(E),
}

impl<E> From<ReadError> for DirectoryVisitError<E> {
    fn from(error: ReadError) -> Self {
        Self::Reader(error)
    }
}

impl<E> From<io::Error> for DirectoryVisitError<E> {
    fn from(error: io::Error) -> Self {
        Self::Reader(ReadError::Io(error))
    }
}

impl ContainedRoot {
    #[cfg(target_os = "linux")]
    pub(super) fn linux_capture_anchors(&self) -> (&File, &File) {
        self.root.linux_capture_anchors()
    }
    pub(crate) fn open(path: &Path, budget: &ReadBudget<'_>) -> Result<Self, ReadError> {
        budget.checkpoint()?;
        let root = platform::Root::open(path, budget)?;
        Ok(Self { root })
    }

    pub(crate) fn identity(&self) -> Identity {
        self.root.identity()
    }

    /// Observe the exact contained spelling, ordinary kind and native identity.
    pub(crate) fn observe_entry(
        &self,
        relative: &str,
        directory: bool,
        budget: &ReadBudget<'_>,
    ) -> Result<Identity, ReadError> {
        budget.checkpoint()?;
        validate_relative(relative)?;
        let identity = self.root.observe_entry(relative, directory, budget)?;
        budget.checkpoint()?;
        Ok(identity)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn default_exclude_presence(
        &self,
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<(bool, [u8; 32]), ReadError> {
        super::linux_default_policy::observe_worktree_exclude(
            self.root.policy_directory(),
            budget,
            work,
        )
    }

    #[cfg(target_os = "linux")]
    pub(super) fn observe_default_policy_environment<'a>(
        &self,
        worktree: &Path,
        allow_contained_cwd: bool,
        io: &mut ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<Option<(DefaultAncestorAbsence, [u8; 32])>, ReadError> {
        super::linux_default_policy::observe_environment(
            worktree,
            self.root.policy_directory(),
            self.root.policy_procfs(),
            allow_contained_cwd,
            io,
            work,
        )
    }

    #[cfg(windows)]
    pub(crate) fn default_exclude_presence(
        &self,
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<(bool, [u8; 32]), ReadError> {
        self.root
            .observe_fixed_file(&[".git", "info"], "exclude", budget, work)
    }

    #[cfg(windows)]
    pub(super) fn observe_database_entry(
        &self,
        relative: &str,
        budget: &ReadBudget<'_>,
    ) -> Result<Identity, ReadError> {
        budget.checkpoint()?;
        validate_relative(relative)?;
        Ok(self.retain_database_entry(relative, budget)?.identity())
    }

    #[cfg(windows)]
    pub(super) fn retain_database_entry(
        &self,
        relative: &str,
        budget: &ReadBudget<'_>,
    ) -> Result<DatabaseEntryLease, ReadError> {
        budget.checkpoint()?;
        validate_relative(relative)?;
        let (lease, identity) = self.root.retain_database_entry(relative, budget, true)?;
        Ok(DatabaseEntryLease {
            _lease: lease,
            identity,
        })
    }

    #[cfg(windows)]
    pub(super) fn retain_database_read_excluding_writers(
        &self,
        relative: &str,
        budget: &ReadBudget<'_>,
    ) -> Result<DatabaseEntryLease, ReadError> {
        budget.checkpoint()?;
        validate_relative(relative)?;
        let (lease, identity) = self.root.retain_database_entry(relative, budget, false)?;
        Ok(DatabaseEntryLease {
            _lease: lease,
            identity,
        })
    }

    pub(crate) fn retain_scope_directories(
        self,
        budget: &ReadBudget<'_>,
    ) -> Result<RetainedScopeDirectories, ReadError> {
        budget.checkpoint()?;
        #[cfg(any(windows, target_os = "linux"))]
        {
            let children = self.root.scope_children(budget)?;
            Ok(RetainedScopeDirectories {
                root: self,
                children,
            })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        Err(ReadError::Unavailable)
    }

    pub(crate) fn read(
        &self,
        relative: &str,
        file_limit: usize,
        budget: &mut ReadBudget<'_>,
    ) -> Result<Vec<u8>, ReadError> {
        self.read_with(relative, file_limit, budget, |size| {
            Ok((allocate_bytes(size)?, ()))
        })
        .map(|(bytes, (), _)| bytes)
    }

    pub(crate) fn read_reserved<'a>(
        &self,
        relative: &str,
        file_limit: usize,
        budget: &mut ReadBudget<'_>,
        memory: &'a MemoryBudget,
        capacity_limit: usize,
    ) -> Result<ReservedBytes<'a>, ReadError> {
        self.read_reserved_file(relative, file_limit, budget, memory, capacity_limit)
            .map(|sample| sample.bytes)
    }

    pub(crate) fn read_reserved_file<'a>(
        &self,
        relative: &str,
        file_limit: usize,
        budget: &mut ReadBudget<'_>,
        memory: &'a MemoryBudget,
        capacity_limit: usize,
    ) -> Result<RegularFileSample<'a>, ReadError> {
        let (bytes, reservation, metadata) =
            self.read_with(relative, file_limit, budget, |size| {
                if size > capacity_limit {
                    return Err(ReadError::Memory);
                }
                let mut reservation = memory.reserve(size).map_err(|_| ReadError::Memory)?;
                let bytes = allocate_bytes(size)?;
                if bytes.capacity() > capacity_limit {
                    return Err(ReadError::Memory);
                }
                reservation
                    .account_capacity(bytes.capacity())
                    .map_err(|_| ReadError::Memory)?;
                Ok((bytes, reservation))
            })?;
        let bytes = reservation
            .into_bytes(bytes)
            .map_err(|_| ReadError::Memory)?;
        Ok(RegularFileSample {
            bytes,
            owner_executable: metadata.owner_executable,
            mtime_ns: metadata.mtime_ns,
        })
    }

    fn read_with<A>(
        &self,
        relative: &str,
        file_limit: usize,
        budget: &mut ReadBudget<'_>,
        allocate: impl FnOnce(usize) -> Result<(Vec<u8>, A), ReadError>,
    ) -> Result<(Vec<u8>, A, RegularFileMetadata), ReadError> {
        budget.checkpoint()?;
        validate_relative(relative)?;
        let lease = self.root.file(relative, budget)?;
        let result = read_file(lease.file(), file_limit, budget, allocate)?;
        budget.checkpoint()?;
        Ok(result)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn observe_directory_name(
        &self,
        budget: &ReadBudget<'_>,
    ) -> Result<NamedDirectory, ReadError> {
        self.root.observe_directory_name(budget)
    }

    // Windows can reuse this handle's enumeration cursor for the root directory.
    pub(crate) fn list(
        &mut self,
        relative: &str,
        entry_limit: usize,
        budget: &ReadBudget<'_>,
    ) -> Result<Vec<DirectoryEntry>, ReadError> {
        let mut entries = Vec::new();
        self.visit_directory(relative, entry_limit, budget, |row| {
            if let NativeDirectoryRow::Entry { name, kind, .. } = row {
                entries.try_reserve(1).map_err(|_| ReadError::Allocation)?;
                entries.push(DirectoryEntry {
                    name: name.into(),
                    kind,
                });
            }
            Ok(())
        })
        .map_err(|error| match error {
            DirectoryVisitError::Reader(error) | DirectoryVisitError::Visitor(error) => error,
        })?;
        entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        if entries.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(ReadError::Changed);
        }
        budget.checkpoint()?;
        Ok(entries)
    }

    /// Visit bounded native rows on one held directory. Name cost is offered before decoding.
    pub(crate) fn visit_directory<E>(
        &mut self,
        relative: &str,
        entry_limit: usize,
        budget: &ReadBudget<'_>,
        mut visit: impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
    ) -> Result<Identity, DirectoryVisitError<E>> {
        budget.checkpoint()?;
        if !relative.is_empty() {
            validate_relative(relative)?;
        }
        let limit = entry_limit.min(MAX_DIRECTORY_ENTRIES);
        let identity = self
            .root
            .visit_directory(relative, limit, budget, &mut visit)?;
        budget.checkpoint()?;
        Ok(identity)
    }
}

fn forward_entry<E>(
    entries: &mut usize,
    name: &str,
    kind: EntryKind,
    windows_hidden: bool,
    limit: usize,
    visit: &mut impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
) -> Result<(), DirectoryVisitError<E>> {
    if matches!(name, "." | "..") {
        return Ok(());
    }
    if name.len() > MAX_NAME_BYTES || name.contains('/') {
        return Err(ReadError::Path.into());
    }
    validate_relative(name)?;
    if *entries >= limit {
        return Err(ReadError::Entries.into());
    }
    *entries += 1;
    visit(NativeDirectoryRow::Entry {
        name,
        kind,
        windows_hidden,
    })
    .map_err(DirectoryVisitError::Visitor)
}

fn validate_relative(path: &str) -> Result<(), ReadError> {
    if path.is_empty() || path.len() > MAX_RELATIVE_BYTES {
        return Err(ReadError::Path);
    }
    for (position, component) in path.split('/').enumerate() {
        if position >= MAX_COMPONENTS
            || matches!(component, "" | "." | "..")
            || component.contains(['\\', ':', '\0'])
        {
            return Err(ReadError::Path);
        }
        #[cfg(windows)]
        if !platform::admissible_component(component) {
            return Err(ReadError::Path);
        }
    }
    Ok(())
}

fn allocate_bytes(size: usize) -> Result<Vec<u8>, ReadError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| ReadError::Allocation)?;
    Ok(bytes)
}

#[cfg(target_os = "linux")]
pub(super) fn read_linux_discovery_home_file<'a>(
    file: &File,
    budget: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
    limit: usize,
) -> Result<ReservedBytes<'a>, ReadError> {
    let memory = work.memory_budget();
    let capacity = work.read_limit(limit, 0).map_err(|_| ReadError::Work)?;
    let (raw, held, _) = read_file(file, limit, budget, |size| {
        if size > capacity {
            return Err(ReadError::Memory);
        }
        let held = memory.reserve(size).map_err(|_| ReadError::Memory)?;
        let raw = allocate_bytes(size)?;
        if raw.capacity() != size {
            return Err(ReadError::Memory);
        }
        Ok((raw, held))
    })?;
    budget.checkpoint()?;
    work.charge(raw.len()).map_err(|_| ReadError::Work)?;
    held.into_bytes(raw).map_err(|_| ReadError::Memory)
}

fn read_file<A>(
    mut file: &File,
    file_limit: usize,
    budget: &mut ReadBudget<'_>,
    allocate: impl FnOnce(usize) -> Result<(Vec<u8>, A), ReadError>,
) -> Result<(Vec<u8>, A, RegularFileMetadata), ReadError> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(ReadError::Kind);
    }
    let size = usize::try_from(meta.len()).map_err(|_| ReadError::Bytes)?;
    if size > file_limit {
        return Err(ReadError::Bytes);
    }
    budget.reserve_file(size)?;
    let (mut bytes, reservation) = allocate(size)?;
    let mut chunk = [0; CHUNK_SIZE];
    while bytes.len() < size {
        budget.checkpoint()?;
        let capacity = (size - bytes.len()).min(chunk.len());
        let read = match file.read(&mut chunk[..capacity]) {
            Ok(0) => return Err(ReadError::Changed),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        budget.remaining -= read;
        bytes.extend_from_slice(&chunk[..read]);
        #[cfg(all(test, target_os = "linux"))]
        tests::after_chunk(bytes.len());
    }
    let after = file.metadata()?;
    if after.len() != meta.len() {
        return Err(ReadError::Changed);
    }
    #[cfg(target_os = "linux")]
    if !platform::same_read_metadata(&meta, &after) {
        return Err(ReadError::Changed);
    }
    #[cfg(target_os = "linux")]
    let owner_executable = Some(platform::owner_executable(&after));
    #[cfg(not(target_os = "linux"))]
    let owner_executable = None;
    let mtime_ns = crate::index::staleness::extract_mtime_ns(&after);
    if mtime_ns != crate::index::staleness::extract_mtime_ns(&meta) {
        return Err(ReadError::Changed);
    }
    Ok((
        bytes,
        reservation,
        RegularFileMetadata {
            owner_executable,
            mtime_ns,
        },
    ))
}

#[cfg(windows)]
mod platform {
    use super::super::git_config::{MAX_DISCOVERY_HOME_CONFIG_BYTES, admit_discovery_home_config};
    use super::{
        DefaultAncestorAbsence, DirectoryVisitError, EntryKind, File, Identity, MAX_COMPONENTS,
        MAX_NAME_BYTES, NamedDirectory, NativeDirectoryRow, Path, ReadBudget, ReadError,
        Reservation, ReservedBytes, ScopeDirectoryNames, Work, allocate_bytes, forward_entry,
        read_file,
    };
    use crate::state::layout::STATE_DIR;
    use std::ffi::c_void;
    use std::fs::OpenOptions;
    use std::io;
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Component, PathBuf, Prefix};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
        NTSTATUS, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError,
        STATUS_INVALID_PARAMETER, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_REPARSE_POINT_ENCOUNTERED,
        STATUS_REPARSE_POINT_NOT_RESOLVED, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_GENERIC_READ, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_TRAVERSE, FileFullDirectoryInfo, FileFullDirectoryRestartInfo, FileIdInfo,
        FileNormalizedNameInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        GetFinalPathNameByHandleW, OPEN_EXISTING, SYNCHRONIZE, VOLUME_NAME_GUID,
    };

    // Stable NtCreateFile ABI. The WDK bindings are gated by optional
    // windows-sys features, so keep the two small ABI structures here.
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root: HANDLE,
        name: *mut UNICODE_STRING,
        attributes: u32,
        security_descriptor: *mut c_void,
        security_qos: *mut c_void,
    }

    const METADATA_BYTES: usize = 4_096;
    const VOLUME_ROOT_UNITS: usize = 128;
    const SCOPE_NATIVE_NAME_UNITS: usize = 2_048;
    const MAX_SCOPE_NAME_BYTES: usize = 4_096;
    // FileNormalizedNameInfo uses a 4-byte length before the UTF-16 units.
    const MAX_SCOPE_COMPONENT_UNITS: usize = (METADATA_BYTES - size_of::<u32>()) / size_of::<u16>();
    // Slash check, stem extraction/trimming, and character validation.
    const COMPONENT_SCANS: usize = 4;

    #[repr(align(8))]
    struct MetadataBuffer([u8; METADATA_BYTES]);

    fn read_u32(input: &[u8], at: usize) -> Result<u32, ReadError> {
        input
            .get(at..at + 4)
            .and_then(|value| value.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or(ReadError::Unavailable)
    }

    #[derive(Clone, Copy)]
    enum ChildSpelling {
        Exact,
        CurrentDirectoryAsciiAlias,
    }

    impl ChildSpelling {
        fn unit(self, unit: u16) -> u16 {
            if matches!(self, Self::CurrentDirectoryAsciiAlias) && (65..=90).contains(&unit) {
                unit + 32
            } else {
                unit
            }
        }
    }

    fn verify_child_name(
        file: &File,
        expected: &[u16],
        spelling: ChildSpelling,
    ) -> Result<(), ReadError> {
        let mut buffer = MetadataBuffer([0; 4096]);
        // SAFETY: file owns the live handle. The fixed aligned buffer is writable
        // for exactly the supplied length and no data is read until success.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileNormalizedNameInfo,
                buffer.0.as_mut_ptr().cast(),
                buffer.0.len() as u32,
            )
        };
        if success == 0 {
            return Err(ReadError::Unavailable);
        }
        let len = read_u32(&buffer.0, 0)? as usize;
        if len == 0 || !len.is_multiple_of(2) {
            return Err(ReadError::Unavailable);
        }
        let name = buffer.0.get(4..4 + len).ok_or(ReadError::Unavailable)?;
        let last = name
            .as_chunks::<2>()
            .0
            .iter()
            .rev()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|unit| *unit != u16::from(b'\\'));
        if !last
            .map(|unit| spelling.unit(unit))
            .eq(expected.iter().rev().map(|unit| spelling.unit(*unit)))
        {
            return Err(ReadError::Path);
        }
        Ok(())
    }

    fn visit_directory<E>(
        file: &File,
        limit: usize,
        budget: &ReadBudget<'_>,
        visit: &mut impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
    ) -> Result<Identity, DirectoryVisitError<E>> {
        // FILE_FULL_DIR_INFO has a 68-byte fixed prefix before UTF-16 name data.
        const NAME_AT: usize = 68;
        let before = inspect(file, true, budget)?;
        let stamp = file.metadata()?.last_write_time();
        let mut kind = FileFullDirectoryRestartInfo;
        let mut entries = 0;
        let mut decoded = [0; MAX_NAME_BYTES];
        for _ in 0..limit + 4 {
            budget.checkpoint()?;
            let mut page = MetadataBuffer([0; 4096]);
            // SAFETY: file owns the directory handle. The 8-byte aligned page
            // is writable for its bounded length and parsed only on success.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let success = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    kind,
                    page.0.as_mut_ptr().cast(),
                    page.0.len() as u32,
                )
            };
            if success == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    if before != inspect(file, true, budget)?
                        || stamp != file.metadata()?.last_write_time()
                    {
                        return Err(ReadError::Changed.into());
                    }
                    return Ok(before);
                }
                return Err(error.into());
            }
            kind = FileFullDirectoryInfo;
            let mut remaining = page.0.as_slice();
            loop {
                budget.checkpoint()?;
                let next = read_u32(remaining, 0)? as usize;
                let attributes = read_u32(remaining, 56)?;
                let len = read_u32(remaining, 60)? as usize;
                if len == 0 || !len.is_multiple_of(2) || len > 512 {
                    return Err(ReadError::Unavailable.into());
                }
                let end = NAME_AT + len;
                let raw_name = remaining.get(NAME_AT..end).ok_or(ReadError::Unavailable)?;
                visit(NativeDirectoryRow::NameBytes(len)).map_err(DirectoryVisitError::Visitor)?;
                let mut written = 0;
                for character in char::decode_utf16(
                    raw_name
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| u16::from_le_bytes(*pair)),
                ) {
                    let character = character.map_err(|_| ReadError::Path)?;
                    let mut unit = [0; 4];
                    let bytes = character.encode_utf8(&mut unit).as_bytes();
                    let end = written + bytes.len();
                    decoded
                        .get_mut(written..end)
                        .ok_or(ReadError::Path)?
                        .copy_from_slice(bytes);
                    written = end;
                }
                let name = std::str::from_utf8(&decoded[..written]).map_err(|_| ReadError::Path)?;
                let entry = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    EntryKind::Redirect
                } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                };
                forward_entry(
                    &mut entries,
                    name,
                    entry,
                    attributes & FILE_ATTRIBUTE_HIDDEN != 0,
                    limit,
                    visit,
                )?;
                if next == 0 {
                    break;
                }
                if next < end || !next.is_multiple_of(8) {
                    return Err(ReadError::Unavailable.into());
                }
                remaining = remaining.get(next..).ok_or(ReadError::Unavailable)?;
            }
        }
        Err(ReadError::Entries.into())
    }

    #[repr(C)]
    struct IoStatusBlock {
        status_or_pointer: usize,
        information: usize,
    }

    #[link(name = "ntdll")]
    // SAFETY: declared with the native ABI and called only by open_child.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe extern "system" {
        fn NtCreateFile(
            file: *mut HANDLE,
            access: u32,
            attributes: *const ObjectAttributes,
            status: *mut IoStatusBlock,
            allocation: *const i64,
            file_attributes: u32,
            share: u32,
            disposition: u32,
            options: u32,
            ea: *const c_void,
            ea_len: u32,
        ) -> NTSTATUS;
    }

    pub(super) struct Root {
        identity: Identity,
        ancestors: Vec<File>,
    }

    pub(super) struct Lease {
        file: File,
        _ancestors: Vec<File>,
    }

    impl Lease {
        pub(super) fn file(&self) -> &File {
            &self.file
        }
    }

    pub(super) fn admissible_component(component: &str) -> bool {
        let stem = component.split('.').next().unwrap_or_default().trim_end();
        let device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
            .iter()
            .any(|name| stem.eq_ignore_ascii_case(name))
            || stem.split_at_checked(3).is_some_and(|(prefix, suffix)| {
                (prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT"))
                    && matches!(
                        suffix,
                        "1" | "2"
                            | "3"
                            | "4"
                            | "5"
                            | "6"
                            | "7"
                            | "8"
                            | "9"
                            | "\u{00b9}"
                            | "\u{00b2}"
                            | "\u{00b3}"
                    )
            });
        !device
            && !component.ends_with(['.', ' '])
            && !component.chars().any(|c| {
                c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | ':' | '\\')
            })
    }

    fn guid_name<const UNITS: usize>(file: &File) -> Result<String, ReadError> {
        let mut buffer = [0u16; UNITS];
        let units = guid_name_units(file, &mut buffer)?;
        String::from_utf16(units).map_err(|_| ReadError::Path)
    }

    fn guid_name_units<'a>(file: &File, buffer: &'a mut [u16]) -> Result<&'a [u16], ReadError> {
        // SAFETY: the handle is live and the buffer is writable for its declared length.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_GUID,
            )
        } as usize;
        if length == 0 || length >= buffer.len() {
            return Err(ReadError::Unavailable);
        }
        Ok(&buffer[..length])
    }

    fn guid_parts(text: &str) -> Result<(&str, &str), ReadError> {
        let (guid, relative) = text
            .strip_prefix(r"\\?\Volume{")
            .and_then(|suffix| suffix.split_once("}\\"))
            .ok_or(ReadError::Path)?;
        if guid.len() != 36
            || !guid.bytes().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            })
        {
            return Err(ReadError::Path);
        }
        Ok((guid, relative))
    }

    fn verify_volume_root(file: &File) -> Result<(), ReadError> {
        let text = guid_name::<128>(file)?;
        let (_, relative) = guid_parts(&text)?;
        if !relative.is_empty() {
            return Err(ReadError::Path);
        }
        Ok(())
    }

    fn inspect(
        file: &File,
        directory: bool,
        budget: &ReadBudget<'_>,
    ) -> Result<Identity, ReadError> {
        budget.checkpoint()?;
        let meta = file.metadata()?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(ReadError::Redirect);
        }
        if (directory && !meta.is_dir()) || (!directory && !meta.is_file()) {
            return Err(ReadError::Kind);
        }
        let mut id = MaybeUninit::<FILE_ID_INFO>::uninit();
        // SAFETY: file owns the live handle and id is writable for the exact ABI size.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let status = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                id.as_mut_ptr().cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if status == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: a successful FileIdInfo query initialized the structure.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let id = unsafe { id.assume_init() };
        let identity = Identity {
            volume: id.VolumeSerialNumber,
            file: u128::from_le_bytes(id.FileId.Identifier),
        };
        budget.checkpoint()?;
        Ok(identity)
    }

    fn open_child(
        parent: &File,
        child: &str,
        directory: bool,
        budget: &ReadBudget<'_>,
    ) -> Result<(File, Identity), ReadError> {
        if !scope_component(child) {
            return Err(ReadError::Path);
        }
        let before = inspect(parent, true, budget)?;
        let mut wide: Vec<u16> = child.encode_utf16().collect();
        open_child_wide(
            parent,
            before,
            &mut wide,
            directory,
            FILE_SHARE_READ,
            budget,
        )
    }

    // The segment must have no separator, alternate stream or traversal.
    fn scope_component(child: &str) -> bool {
        !matches!(child, "" | "." | "..") && !child.contains('/') && admissible_component(child)
    }

    struct ChildOpen {
        directory: bool,
        access: u32,
        share: u32,
        spelling: ChildSpelling,
    }

    fn open_child_wide(
        parent: &File,
        before: Identity,
        wide: &mut [u16],
        directory: bool,
        share: u32,
        budget: &ReadBudget<'_>,
    ) -> Result<(File, Identity), ReadError> {
        probe_child_wide(
            parent,
            before,
            wide,
            ChildOpen {
                directory,
                access: FILE_GENERIC_READ,
                share,
                spelling: ChildSpelling::Exact,
            },
            budget,
        )?
        .ok_or_else(|| io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32).into())
    }

    fn probe_child_wide(
        parent: &File,
        before: Identity,
        wide: &mut [u16],
        selection: ChildOpen,
        budget: &ReadBudget<'_>,
    ) -> Result<Option<(File, Identity)>, ReadError> {
        const FILE_OPEN: u32 = 1;
        const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
        const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let length = u16::try_from(wide.len().saturating_mul(2)).map_err(|_| ReadError::Path)?;
        let mut name = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: wide.as_mut_ptr(),
        };
        let attrs = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as u32,
            root: parent.as_raw_handle(),
            name: &mut name,
            attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            security_descriptor: null_mut(),
            security_qos: null_mut(),
        };
        let mut handle = null_mut();
        let mut status = IoStatusBlock {
            status_or_pointer: 0,
            information: 0,
        };
        // SAFETY: the parent, UTF-16 name, attributes, and both output buffers
        // remain live for this synchronous call. The name is one component.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let result = unsafe {
            NtCreateFile(
                &mut handle,
                selection.access,
                &attrs,
                &mut status,
                null(),
                0,
                selection.share,
                FILE_OPEN,
                FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                null(),
                0,
            )
        };
        adjudicate_child_open(result, || {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return Err(ReadError::Unavailable);
            }
            // SAFETY: ownership of the successful returned handle transfers to File.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let file = unsafe { File::from_raw_handle(handle) };
            let identity = inspect(&file, selection.directory, budget)?;
            verify_child_name(&file, wide, selection.spelling)?;
            if identity.volume != before.volume || inspect(parent, true, budget)? != before {
                return Err(ReadError::Redirect);
            }
            Ok((file, identity))
        })
    }

    fn probe_fixed_component(
        parent: &File,
        identity: Identity,
        name: &str,
        directory: bool,
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<Option<(File, Identity)>, ReadError> {
        if name.len() > 10 || !name.is_ascii() || !scope_component(name) {
            return Err(ReadError::Path);
        }
        charge_scope(budget, work, 64 + METADATA_BYTES + 4 * name.len())?;
        let _scratch = reserve_scope(work, METADATA_BYTES)?;
        let mut segment = [0_u16; 10];
        for (slot, byte) in segment.iter_mut().zip(name.bytes()) {
            *slot = u16::from(byte);
        }
        let access = FILE_READ_ATTRIBUTES | SYNCHRONIZE | if directory { FILE_TRAVERSE } else { 0 };
        let opened = probe_child_wide(
            parent,
            identity,
            &mut segment[..name.len()],
            ChildOpen {
                directory,
                access,
                share: FILE_SHARE_READ,
                spelling: ChildSpelling::Exact,
            },
            budget,
        );
        charge_scope(budget, work, 0)?;
        if inspect(parent, true, budget)? != identity {
            return Err(ReadError::Changed);
        }
        opened
    }

    fn adjudicate_child_open<T>(
        status: NTSTATUS,
        confirm: impl FnOnce() -> Result<T, ReadError>,
    ) -> Result<Option<T>, ReadError> {
        if status == STATUS_OBJECT_NAME_NOT_FOUND {
            return Ok(None);
        }
        if matches!(
            status,
            STATUS_REPARSE_POINT_ENCOUNTERED | STATUS_REPARSE_POINT_NOT_RESOLVED
        ) {
            return Err(ReadError::Redirect);
        }
        if status == STATUS_INVALID_PARAMETER {
            return Err(ReadError::Unavailable);
        }
        if status < 0 {
            // SAFETY: converts an NTSTATUS value; there are no pointer arguments.
            return Err(io::Error::from_raw_os_error(
                // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
                unsafe { RtlNtStatusToDosError(status) } as i32,
            )
            .into());
        }
        confirm().map(Some)
    }

    #[cfg(test)]
    #[test]
    fn only_initial_child_open_can_prove_absence() {
        let found: Result<Option<()>, _> =
            adjudicate_child_open(STATUS_OBJECT_NAME_NOT_FOUND, || {
                panic!("missing name cannot have a child to inspect")
            });
        assert!(matches!(found, Ok(None)));
        for failed in [0, 1, 2] {
            let phase = |stage| -> Result<(), ReadError> {
                if stage == failed {
                    Err(io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32).into())
                } else {
                    Ok(())
                }
            };
            let later = adjudicate_child_open(0, || {
                phase(0)?; // child metadata
                phase(1)?; // exact child name
                phase(2)?; // retained parent recheck
                Ok(())
            });
            assert!(
                matches!(later, Err(ReadError::Io(error)) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32))
            );
        }
    }

    pub(super) struct ScopeChildren {
        git: (File, Identity),
        state: (File, Identity),
    }

    fn observe_directory(
        handle: &File,
        identity: Identity,
        budget: &ReadBudget<'_>,
    ) -> Result<NamedDirectory, ReadError> {
        if inspect(handle, true, budget)? != identity {
            return Err(ReadError::Changed);
        }
        let path = guid_name::<2048>(handle)?;
        if path.len() > 4096 || !Path::new(&path).is_absolute() {
            return Err(ReadError::Path);
        }
        let (_, relative) = guid_parts(&path)?;
        if !relative.is_empty()
            && relative.split('\\').any(|part| {
                matches!(part, "" | "." | "..") || part.contains('/') || !admissible_component(part)
            })
        {
            return Err(ReadError::Path);
        }
        if inspect(handle, true, budget)? != identity {
            return Err(ReadError::Changed);
        }
        Ok(NamedDirectory { path, identity })
    }

    pub(super) fn charge_scope(
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
        units: usize,
    ) -> Result<(), ReadError> {
        budget.checkpoint()?;
        work.charge(units).map_err(scope_work_error)
    }

    fn scope_work_error(reason: &'static str) -> ReadError {
        match reason {
            "work-cap" => ReadError::Work,
            _ => ReadError::Deadline,
        }
    }

    fn reserve_scope<'a>(work: &mut Work<'a>, bytes: usize) -> Result<Reservation<'a>, ReadError> {
        let available = work.read_limit(bytes, 0).map_err(scope_work_error)?;
        if available < bytes {
            return Err(ReadError::Memory);
        }
        work.memory_budget()
            .reserve(bytes)
            .map_err(|_| ReadError::Memory)
    }

    fn open_scope_drive(
        drive: u8,
        verbatim: bool,
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<File, ReadError> {
        const DRIVE_PATH: &[u8; 8] = b"\\\\?\\X:\\\0";
        const DRIVE_PATH_BYTES: usize = DRIVE_PATH.len() * size_of::<u16>();
        let handle = {
            charge_scope(budget, work, DRIVE_PATH_BYTES + 1)?;
            let _scratch = reserve_scope(work, DRIVE_PATH_BYTES)?;
            let mut path = (*DRIVE_PATH).map(u16::from);
            path[4] = u16::from(drive);
            let path = if verbatim { &path[..] } else { &path[4..] };
            // SAFETY: the drive path has one terminating NUL and remains live for the call.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    null_mut(),
                )
            }
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error().into());
        }
        if handle.is_null() {
            return Err(ReadError::Unavailable);
        }
        // SAFETY: the successful handle is uniquely owned and transfers to File.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let file = unsafe { File::from_raw_handle(handle) };
        // Account for the initialized stack buffers and the native name query.
        let name_bytes = VOLUME_ROOT_UNITS * (size_of::<u16>() + 3);
        charge_scope(budget, work, name_bytes + 1)?;
        let _scratch = reserve_scope(work, name_bytes)?;
        let mut wide = [0; VOLUME_ROOT_UNITS];
        let mut decoded = [0; VOLUME_ROOT_UNITS * 3];
        let units = guid_name_units(&file, &mut wide)?;
        let name = decode_scope_name(units, &mut decoded, budget, work)?;
        charge_scope(budget, work, name.len())?;
        let (_, relative) = guid_parts(name)?;
        if !relative.is_empty() {
            return Err(ReadError::Path);
        }
        Ok(file)
    }

    fn open_scope_child(
        parent: &File,
        child: &str,
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<(File, Identity), ReadError> {
        open_scope_child_with_spelling(parent, child, ChildSpelling::Exact, budget, work)
    }

    fn open_scope_child_with_spelling(
        parent: &File,
        child: &str,
        spelling: ChildSpelling,
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<(File, Identity), ReadError> {
        // Account for validation, encoding and the stack buffers used by relative opens.
        charge_scope(budget, work, COMPONENT_SCANS * child.len())?;
        if !scope_component(child) {
            return Err(ReadError::Path);
        }
        charge_scope(
            budget,
            work,
            child.len() + MAX_SCOPE_COMPONENT_UNITS * size_of::<u16>(),
        )?;
        // The encoded segment and the nested FileNormalizedNameInfo buffer overlap.
        let scratch_bytes =
            MAX_SCOPE_COMPONENT_UNITS * size_of::<u16>() + size_of::<MetadataBuffer>();
        let _scratch = reserve_scope(work, scratch_bytes)?;
        let mut wide = [0; MAX_SCOPE_COMPONENT_UNITS];
        let mut len = 0;
        for unit in child.encode_utf16() {
            *wide.get_mut(len).ok_or(ReadError::Path)? = unit;
            len += 1;
        }
        // An open, three handle inspections, and the bounded exact-name query/comparison.
        charge_scope(
            budget,
            work,
            5 + METADATA_BYTES + (len + 1) * size_of::<u16>(),
        )?;
        let before = inspect(parent, true, budget)?;
        probe_child_wide(
            parent,
            before,
            &mut wide[..len],
            ChildOpen {
                directory: true,
                access: FILE_GENERIC_READ,
                share: FILE_SHARE_READ,
                spelling,
            },
            budget,
        )?
        .ok_or_else(|| io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32).into())
    }

    fn decode_scope_name<'a>(
        units: &[u16],
        decoded: &'a mut [u8],
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<&'a str, ReadError> {
        // UTF-16 input plus at most three UTF-8 bytes per code unit.
        charge_scope(budget, work, units.len() * (size_of::<u16>() + 3))?;
        let mut written = 0;
        for character in char::decode_utf16(units.iter().copied()) {
            let character = character.map_err(|_| ReadError::Path)?;
            let end = written + character.len_utf8();
            character.encode_utf8(decoded.get_mut(written..end).ok_or(ReadError::Path)?);
            written = end;
        }
        charge_scope(budget, work, written)?;
        std::str::from_utf8(&decoded[..written]).map_err(|_| ReadError::Path)
    }

    pub(super) struct MeteredRoot<'a> {
        pub(super) root: Root,
        pub(super) reservation: Reservation<'a>,
    }

    pub(super) struct MeteredDirectory<'a> {
        pub(super) named: NamedDirectory,
        pub(super) reservation: Reservation<'a>,
    }

    pub(super) struct BorrowedDirectory<'a> {
        pub(super) path: &'a str,
        pub(super) identity: Identity,
    }

    impl BorrowedDirectory<'_> {
        pub(super) fn freeze<'a>(
            self,
            budget: &ReadBudget<'_>,
            work: &mut Work<'a>,
        ) -> Result<MeteredDirectory<'a>, ReadError> {
            charge_scope(budget, work, self.path.len())?;
            let mut reservation = reserve_scope(work, self.path.len())?;
            let mut path = String::new();
            path.try_reserve_exact(self.path.len())
                .map_err(|_| ReadError::Allocation)?;
            reservation
                .account_capacity(path.capacity())
                .map_err(|_| ReadError::Memory)?;
            path.push_str(self.path);
            charge_scope(budget, work, 0)?;
            Ok(MeteredDirectory {
                named: NamedDirectory {
                    path,
                    identity: self.identity,
                },
                reservation,
            })
        }
    }

    fn observe_scope_directory<'a>(
        handle: &File,
        identity: Identity,
        decoded: &'a mut [u8; MAX_SCOPE_NAME_BYTES],
        budget: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<BorrowedDirectory<'a>, ReadError> {
        charge_scope(budget, work, 1)?;
        if inspect(handle, true, budget)? != identity {
            return Err(ReadError::Changed);
        }
        let wide_bytes = SCOPE_NATIVE_NAME_UNITS * size_of::<u16>();
        charge_scope(budget, work, wide_bytes + 1)?;
        let _scratch = reserve_scope(work, wide_bytes)?;
        let mut wide = [0; SCOPE_NATIVE_NAME_UNITS];
        let units = guid_name_units(handle, &mut wide)?;
        let path = decode_scope_name(units, decoded, budget, work)?;
        // Check the absolute form, GUID framing, relative delimiters, and each component.
        charge_scope(budget, work, (3 + COMPONENT_SCANS) * path.len())?;
        if !Path::new(path).is_absolute() {
            return Err(ReadError::Path);
        }
        let (_, relative) = guid_parts(path)?;
        if !relative.is_empty() && !relative.split('\\').all(scope_component) {
            return Err(ReadError::Path);
        }
        charge_scope(budget, work, 1)?;
        if inspect(handle, true, budget)? != identity {
            return Err(ReadError::Changed);
        }
        Ok(BorrowedDirectory { path, identity })
    }

    fn is_exact_child(root: &str, child: &str, suffix: &str) -> bool {
        child.strip_prefix(root).is_some_and(|rest| {
            if root.ends_with('\\') {
                rest == suffix
            } else {
                rest.strip_prefix('\\') == Some(suffix)
            }
        })
    }

    impl Root {
        pub(super) fn open(path: &Path, budget: &ReadBudget<'_>) -> Result<Self, ReadError> {
            let mut components = path.components();
            let prefix = match components.next() {
                Some(Component::Prefix(prefix))
                    if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
                {
                    prefix
                }
                _ => return Err(ReadError::Path),
            };
            let Some(Component::RootDir) = components.next() else {
                return Err(ReadError::Path);
            };
            let mut guarded_path = PathBuf::from(prefix.as_os_str());
            guarded_path.push(Component::RootDir.as_os_str());
            let root = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(guarded_path)?;
            verify_volume_root(&root)?;
            let mut identity = inspect(&root, true, budget)?;
            let mut ancestors = vec![root];
            for component in components {
                let Component::Normal(segment) = component else {
                    return Err(ReadError::Path);
                };
                let segment = segment.to_str().ok_or(ReadError::Path)?;
                let parent = ancestors.last().ok_or(ReadError::Unavailable)?;
                let (file, observed) = open_child(parent, segment, true, budget)?;
                identity = observed;
                ancestors.push(file);
            }
            Ok(Self {
                identity,
                ancestors,
            })
        }

        pub(super) fn open_for_scope<'a>(
            path: &Path,
            budget: &ReadBudget<'_>,
            work: &mut Work<'a>,
        ) -> Result<MeteredRoot<'a>, ReadError> {
            Self::open_for_scope_with_spelling(path, ChildSpelling::Exact, budget, work)
        }

        fn open_for_scope_with_spelling<'a>(
            path: &Path,
            spelling: ChildSpelling,
            budget: &ReadBudget<'_>,
            work: &mut Work<'a>,
        ) -> Result<MeteredRoot<'a>, ReadError> {
            charge_scope(budget, work, 0)?;
            let path_bytes = path.as_os_str().len();
            if path_bytes > MAX_SCOPE_NAME_BYTES {
                return Err(ReadError::Path);
            }
            // Count and validate all supplied components before any native open.
            charge_scope(budget, work, path_bytes)?;
            let mut components = path.components();
            let Some(Component::Prefix(prefix)) = components.next() else {
                return Err(ReadError::Path);
            };
            let (drive, verbatim) = match prefix.kind() {
                Prefix::Disk(drive) => (drive, false),
                Prefix::VerbatimDisk(drive) => (drive, true),
                _ => return Err(ReadError::Path),
            };
            if components.next() != Some(Component::RootDir) {
                return Err(ReadError::Path);
            }
            let mut slots = 1;
            for component in components.clone() {
                let Component::Normal(segment) = component else {
                    return Err(ReadError::Path);
                };
                charge_scope(budget, work, segment.len())?;
                let segment = segment.to_str().ok_or(ReadError::Path)?;
                charge_scope(budget, work, (COMPONENT_SCANS + 1) * segment.len())?;
                if slots > MAX_COMPONENTS
                    || !scope_component(segment)
                    || segment.encode_utf16().count() > MAX_SCOPE_COMPONENT_UNITS
                {
                    return Err(ReadError::Path);
                }
                slots += 1;
            }
            let ancestor_bytes = slots * size_of::<File>();
            charge_scope(budget, work, ancestor_bytes)?;
            let mut reservation = reserve_scope(work, ancestor_bytes)?;
            let mut ancestors = Vec::new();
            ancestors
                .try_reserve_exact(slots)
                .map_err(|_| ReadError::Allocation)?;
            reservation
                .account_capacity(ancestors.capacity() * size_of::<File>())
                .map_err(|_| ReadError::Memory)?;
            let root = open_scope_drive(drive, verbatim, budget, work)?;
            charge_scope(budget, work, 1)?;
            let mut identity = inspect(&root, true, budget)?;
            ancestors.push(root);
            // The cloned path iterator's second pass processes the same bounded input.
            charge_scope(budget, work, path_bytes)?;
            for component in components {
                let Component::Normal(segment) = component else {
                    return Err(ReadError::Path);
                };
                charge_scope(budget, work, segment.len())?;
                let segment = segment.to_str().ok_or(ReadError::Path)?;
                let parent = ancestors.last().ok_or(ReadError::Unavailable)?;
                let (file, observed) =
                    open_scope_child_with_spelling(parent, segment, spelling, budget, work)?;
                identity = observed;
                ancestors.push(file);
            }
            charge_scope(budget, work, 0)?;
            Ok(MeteredRoot {
                root: Self {
                    identity,
                    ancestors,
                },
                reservation,
            })
        }

        pub(super) fn observe_default_global_absence<'a>(
            &self,
            allow_contained_cwd: bool,
            budget: &mut ReadBudget<'_>,
            work: &mut Work<'a>,
        ) -> Result<Option<[u8; 32]>, ReadError> {
            // The Windows process environment/current-path APIs are variable-length.
            // Reserve space for the bounded OS strings before taking this sample.
            const OS_STRING_MEMORY: usize = 256 * 1024;
            let _strings = reserve_scope(work, OS_STRING_MEMORY)?;
            charge_scope(budget, work, 0)?;
            for name in ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM", "XDG_CONFIG_HOME"] {
                if std::env::var_os(name).is_some() {
                    return Ok(None);
                }
                charge_scope(budget, work, 1)?;
            }
            let cwd = std::env::current_dir()?;
            if !cwd.is_absolute() {
                return Ok(None);
            }
            let mut matched_cwd = false;
            for ancestor in cwd.ancestors() {
                let current = Self::open_for_scope_with_spelling(
                    ancestor,
                    ChildSpelling::CurrentDirectoryAsciiAlias,
                    budget,
                    work,
                )?;
                if current.root.identity == self.identity {
                    matched_cwd = true;
                    break;
                }
                if !allow_contained_cwd {
                    break;
                }
            }
            if !matched_cwd {
                return Ok(None);
            }
            let drive = cwd.ancestors().last().ok_or(ReadError::Path)?;
            // Match the exact home-selection API called by pinned ignore 0.4.33.
            #[allow(deprecated, reason = "Exact pinned default ignore home selection")]
            let home = std::env::home_dir().ok_or(ReadError::Unavailable)?;
            let home_bytes = home.as_os_str().as_encoded_bytes();
            if home_bytes.len() > MAX_SCOPE_NAME_BYTES {
                return Err(ReadError::Path);
            }
            charge_scope(budget, work, home_bytes.len())?;
            let home_anchor = Self::open_for_scope(&home, budget, work)?;
            let drive_anchor = Self::open_for_scope(drive, budget, work)?;
            if drive_anchor.root.identity.volume != self.identity.volume {
                return Err(ReadError::Changed);
            }
            let mut hash = blake3::Hasher::new();
            hash.update(b"ledgerful:default-ignore-global-policy:v2\0");
            hash.update(&(home_bytes.len() as u64).to_le_bytes());
            hash.update(home_bytes);
            // Only the fixed HOME/.gitconfig file has a content authority;
            // its absence or exact raw digest is part of the repeat sample.
            if let Some((identity, raw)) = home_anchor.root.read_home_git_config(budget, work)? {
                charge_scope(budget, work, 32 + 32 * raw.as_ref().len())?;
                let _parse = reserve_scope(work, 128 * 1024 + 32 * raw.as_ref().len())?;
                if !admit_discovery_home_config(raw.as_ref()) {
                    return Ok(None);
                }
                hash.update(&[1]);
                hash.update(&identity.inventory_bytes());
                hash.update(blake3::hash(raw.as_ref()).as_bytes());
            } else {
                hash.update(&[0]);
            }
            for (root, directories, file) in [
                (&home_anchor.root, [".config", "git"].as_slice(), "config"),
                (&drive_anchor.root, ["etc"].as_slice(), "gitconfig"),
                (&home_anchor.root, [".config", "git"].as_slice(), "ignore"),
            ] {
                let (exists, identities) =
                    root.observe_fixed_file(directories, file, budget, work)?;
                if exists {
                    return Ok(None);
                }
                hash.update(&identities);
            }
            charge_scope(budget, work, 0)?;
            Ok(Some(*hash.finalize().as_bytes()))
        }

        fn read_home_git_config<'a>(
            &self,
            budget: &mut ReadBudget<'_>,
            work: &mut Work<'a>,
        ) -> Result<Option<(Identity, ReservedBytes<'a>)>, ReadError> {
            const LEAF: &[u8; 10] = b".gitconfig";
            let root = self.ancestors.last().ok_or(ReadError::Unavailable)?;
            charge_scope(budget, work, METADATA_BYTES + 128)?;
            let _scratch = reserve_scope(work, METADATA_BYTES)?;
            let mut wide = LEAF.map(u16::from);
            let opened = probe_child_wide(
                root,
                self.identity,
                &mut wide,
                ChildOpen {
                    directory: false,
                    access: FILE_GENERIC_READ,
                    share: FILE_SHARE_READ,
                    spelling: ChildSpelling::Exact,
                },
                budget,
            );
            charge_scope(budget, work, 0)?;
            if inspect(root, true, budget)? != self.identity {
                return Err(ReadError::Changed);
            }
            let Some((file, identity)) = opened? else {
                return Ok(None);
            };
            let (raw, held, _) =
                read_file(&file, MAX_DISCOVERY_HOME_CONFIG_BYTES, budget, |size| {
                    let reservation = reserve_scope(work, size)?;
                    let raw = allocate_bytes(size)?;
                    if raw.capacity() != size {
                        return Err(ReadError::Memory);
                    }
                    Ok((raw, reservation))
                })?;
            charge_scope(budget, work, raw.len())?;
            let raw = held.into_bytes(raw).map_err(|_| ReadError::Memory)?;
            Ok(Some((identity, raw)))
        }

        pub(super) fn observe_fixed_file(
            &self,
            directories: &[&str],
            leaf: &str,
            budget: &ReadBudget<'_>,
            work: &mut Work<'_>,
        ) -> Result<(bool, [u8; 32]), ReadError> {
            if directories.len() > 2 {
                return Err(ReadError::Path);
            }
            let root = self.ancestors.last().ok_or(ReadError::Unavailable)?;
            let mut retained: [Option<File>; 2] = [None, None];
            let mut identity = inspect(root, true, budget)?;
            let mut hash = blake3::Hasher::new();
            hash.update(b"ledgerful:fixed-ignore-path:v1\0");
            hash.update(&identity.inventory_bytes());
            for (index, name) in directories.iter().enumerate() {
                let parent = if index == 0 {
                    root
                } else {
                    retained[index - 1].as_ref().ok_or(ReadError::Unavailable)?
                };
                let Some((file, observed)) =
                    probe_fixed_component(parent, identity, name, true, budget, work)?
                else {
                    return Ok((false, *hash.finalize().as_bytes()));
                };
                retained[index] = Some(file);
                identity = observed;
                hash.update(&identity.inventory_bytes());
            }
            let parent = if directories.is_empty() {
                root
            } else {
                retained[directories.len() - 1]
                    .as_ref()
                    .ok_or(ReadError::Unavailable)?
            };
            let observed = probe_fixed_component(parent, identity, leaf, false, budget, work)?;
            if let Some((_, id)) = observed {
                hash.update(&id.inventory_bytes());
            }
            Ok((observed.is_some(), *hash.finalize().as_bytes()))
        }

        pub(super) fn observe_default_ancestor_absence(
            &self,
            budget: &ReadBudget<'_>,
            work: &mut Work<'_>,
        ) -> Result<Option<DefaultAncestorAbsence>, ReadError> {
            // ignore 0.4.33 builds strict inherited matchers before walking
            // the local root. Require none of these immediate names to exist.
            const CANDIDATES: [(&str, bool); 4] = [
                (".ignore", false),
                (".gitignore", false),
                (".git", true),
                (".jj", true),
            ];
            charge_scope(budget, work, 0)?;
            let (_, ancestors) = self.ancestors.split_last().ok_or(ReadError::Unavailable)?;
            if ancestors.len() > MAX_COMPONENTS {
                return Err(ReadError::Path);
            }
            let mut hash = blake3::Hasher::new();
            hash.update(b"ledgerful:default-ignore-ancestor-absence:v1\0");
            for parent in ancestors.iter().rev() {
                charge_scope(budget, work, 128)?;
                let identity = inspect(parent, true, budget)?;
                hash.update(&identity.inventory_bytes());
                for (name, directory) in CANDIDATES {
                    match probe_fixed_component(parent, identity, name, directory, budget, work) {
                        Ok(None) => {}
                        Ok(Some(_)) => return Ok(None),
                        Err(error) => return Err(error),
                    }
                }
                if inspect(parent, true, budget)? != identity {
                    return Err(ReadError::Changed);
                }
            }
            charge_scope(budget, work, 32)?;
            Ok(Some(DefaultAncestorAbsence {
                digest: *hash.finalize().as_bytes(),
                directories: ancestors.len(),
            }))
        }

        pub(super) fn scope_children_metered(
            &self,
            budget: &ReadBudget<'_>,
            work: &mut Work<'_>,
        ) -> Result<ScopeChildren, ReadError> {
            let root = self.ancestors.last().ok_or(ReadError::Unavailable)?;
            let git = open_scope_child(root, ".git", budget, work)?;
            let state = open_scope_child(root, STATE_DIR, budget, work)?;
            charge_scope(budget, work, 0)?;
            Ok(ScopeChildren { git, state })
        }

        pub(super) fn with_scope_names<'a, T>(
            &self,
            children: &ScopeChildren,
            budget: &ReadBudget<'_>,
            work: &mut Work<'a>,
            use_names: impl FnOnce([BorrowedDirectory<'_>; 3], &mut Work<'a>) -> Result<T, ReadError>,
        ) -> Result<T, ReadError> {
            let root = self.ancestors.last().ok_or(ReadError::Unavailable)?;
            let decoded_bytes = 4 * MAX_SCOPE_NAME_BYTES;
            charge_scope(budget, work, decoded_bytes)?;
            let _scratch = reserve_scope(work, decoded_bytes)?;
            let mut worktree_buffer = [0; MAX_SCOPE_NAME_BYTES];
            let mut git_buffer = [0; MAX_SCOPE_NAME_BYTES];
            let mut state_buffer = [0; MAX_SCOPE_NAME_BYTES];
            let mut check_buffer = [0; MAX_SCOPE_NAME_BYTES];
            let worktree =
                observe_scope_directory(root, self.identity, &mut worktree_buffer, budget, work)?;
            let git = observe_scope_directory(
                &children.git.0,
                children.git.1,
                &mut git_buffer,
                budget,
                work,
            )?;
            let state = observe_scope_directory(
                &children.state.0,
                children.state.1,
                &mut state_buffer,
                budget,
                work,
            )?;
            let check =
                observe_scope_directory(root, self.identity, &mut check_buffer, budget, work)?;
            charge_scope(
                budget,
                work,
                git.path.len() + state.path.len() + check.path.len(),
            )?;
            if !is_exact_child(worktree.path, git.path, ".git")
                || !is_exact_child(worktree.path, state.path, STATE_DIR)
                || check.path != worktree.path
            {
                return Err(ReadError::Changed);
            }
            use_names([worktree, git, state], work)
        }

        pub(super) fn identity(&self) -> Identity {
            self.identity
        }

        pub(super) fn scope_children(
            &self,
            budget: &ReadBudget<'_>,
        ) -> Result<ScopeChildren, ReadError> {
            let root = self.ancestors.last().ok_or(ReadError::Unavailable)?;
            let git = open_child(root, ".git", true, budget)?;
            let state = open_child(root, STATE_DIR, true, budget)?;
            Ok(ScopeChildren { git, state })
        }

        pub(super) fn scope_names(
            &self,
            children: &ScopeChildren,
            budget: &ReadBudget<'_>,
        ) -> Result<ScopeDirectoryNames, ReadError> {
            let root = self.ancestors.last().ok_or(ReadError::Unavailable)?;
            let worktree = observe_directory(root, self.identity, budget)?;
            let git = observe_directory(&children.git.0, children.git.1, budget)?;
            let state = observe_directory(&children.state.0, children.state.1, budget)?;
            let root_path = Path::new(&worktree.path);
            if root_path.join(".git").as_os_str() != Path::new(&git.path).as_os_str()
                || root_path.join(STATE_DIR).as_os_str() != Path::new(&state.path).as_os_str()
                || observe_directory(root, self.identity, budget)? != worktree
            {
                return Err(ReadError::Changed);
            }
            Ok(ScopeDirectoryNames {
                worktree,
                git,
                state,
            })
        }

        pub(super) fn file(
            &self,
            relative: &str,
            budget: &ReadBudget<'_>,
        ) -> Result<Lease, ReadError> {
            self.descendant(relative, false, budget)
        }

        pub(super) fn observe_entry(
            &self,
            relative: &str,
            directory: bool,
            budget: &ReadBudget<'_>,
        ) -> Result<Identity, ReadError> {
            // Descendant opens verify each component's native spelling and ordinary kind.
            let lease = self.descendant(relative, directory, budget)?;
            inspect(lease.file(), directory, budget)
        }

        pub(super) fn visit_directory<E>(
            &mut self,
            relative: &str,
            limit: usize,
            budget: &ReadBudget<'_>,
            visit: &mut impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
        ) -> Result<Identity, DirectoryVisitError<E>> {
            if relative.is_empty() {
                let root = self.ancestors.last().ok_or(ReadError::Unavailable)?;
                return visit_directory(root, limit, budget, visit);
            }
            let lease = self.descendant(relative, true, budget)?;
            visit_directory(lease.file(), limit, budget, visit)
        }

        pub(super) fn retain_database_entry(
            &self,
            relative: &str,
            budget: &ReadBudget<'_>,
            allow_writers: bool,
        ) -> Result<(Lease, Identity), ReadError> {
            let share = if allow_writers {
                FILE_SHARE_READ | FILE_SHARE_WRITE
            } else {
                FILE_SHARE_READ
            };
            let lease = self.descendant_with_sharing(relative, false, share, budget)?;
            let identity = inspect(lease.file(), false, budget)?;
            let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
            // SAFETY: the lease owns a live regular-file handle, and the OS
            // receives a writable output slot of the documented type.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let status = unsafe {
                GetFileInformationByHandle(lease.file().as_raw_handle(), info.as_mut_ptr())
            };
            if status == 0 {
                return Err(io::Error::last_os_error().into());
            }
            // SAFETY: success initialized BY_HANDLE_FILE_INFORMATION.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            if unsafe { info.assume_init() }.nNumberOfLinks != 1 {
                return Err(ReadError::Redirect);
            }
            budget.checkpoint()?;
            Ok((lease, identity))
        }

        fn descendant(
            &self,
            relative: &str,
            final_directory: bool,
            budget: &ReadBudget<'_>,
        ) -> Result<Lease, ReadError> {
            self.descendant_with_sharing(relative, final_directory, FILE_SHARE_READ, budget)
        }

        fn descendant_with_sharing(
            &self,
            relative: &str,
            final_directory: bool,
            final_share: u32,
            budget: &ReadBudget<'_>,
        ) -> Result<Lease, ReadError> {
            let root = self.ancestors.last().ok_or(ReadError::Unavailable)?;
            let mut components = relative.split('/').peekable();
            let mut ancestors = Vec::new();
            while let Some(component) = components.next() {
                let intermediate = components.peek().is_some();
                let parent = ancestors.last().unwrap_or(root);
                let (handle, _) = if intermediate || final_share == FILE_SHARE_READ {
                    open_child(parent, component, intermediate || final_directory, budget)?
                } else {
                    if !scope_component(component) {
                        return Err(ReadError::Path);
                    }
                    let previous = inspect(parent, true, budget)?;
                    let mut name: Vec<u16> = component.encode_utf16().collect();
                    open_child_wide(
                        parent,
                        previous,
                        &mut name,
                        final_directory,
                        final_share,
                        budget,
                    )?
                };
                if intermediate {
                    ancestors.push(handle);
                } else {
                    return Ok(Lease {
                        file: handle,
                        _ancestors: ancestors,
                    });
                }
            }
            Err(ReadError::Path)
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::super::linux_proc_fds::LinuxProcFds;
    use super::{
        DirectoryVisitError, EntryKind, File, Identity, NamedDirectory, NativeDirectoryRow, Path,
        ReadBudget, ReadError, ScopeDirectoryNames, forward_entry,
    };
    use crate::state::layout::STATE_DIR;
    use nix::errno::Errno;
    use nix::fcntl::{AtFlags, OFlag, OpenHow, ResolveFlag, open, openat, openat2};
    use nix::libc::{DT_DIR, DT_LNK, DT_REG, SYS_getdents64, readlinkat, syscall};
    use nix::sys::stat::{Mode, fstatat};
    use std::ffi::CStr;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;

    pub(super) struct Root {
        directory: File,
        process_fds: LinuxProcFds,
        identity: Identity,
    }

    pub(super) struct Lease(File);

    pub(super) struct ScopeChildren {
        git: (File, Identity),
        state: (File, Identity),
    }

    const MAX_DIRECTORY_NAME_BYTES: usize = 4_096;

    // The procfs descriptor was authenticated for the actual process at root open.
    fn observe_directory_name(
        process_fds: &File,
        directory: &File,
        identity: Identity,
        budget: &ReadBudget<'_>,
    ) -> Result<NamedDirectory, ReadError> {
        with_native_name(process_fds, directory, identity, true, budget, |path| {
            Ok(NamedDirectory {
                path: path.into(),
                identity,
            })
        })
    }

    fn with_native_name<T>(
        process_fds: &File,
        entry: &File,
        identity: Identity,
        directory: bool,
        budget: &ReadBudget<'_>,
        use_name: impl FnOnce(&str) -> Result<T, ReadError>,
    ) -> Result<T, ReadError> {
        budget.checkpoint()?;
        let raw_fd = u32::try_from(entry.as_raw_fd()).map_err(|_| ReadError::Unavailable)?;
        let mut digits = [0u8; 11];
        let mut pos = 10;
        let mut value = raw_fd;
        loop {
            pos -= 1;
            digits[pos] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        let descriptor =
            CStr::from_bytes_with_nul(&digits[pos..]).map_err(|_| ReadError::Unavailable)?;
        let verify_binding = || -> Result<(), ReadError> {
            budget.checkpoint()?;
            let owned = entry.metadata().map_err(|_| ReadError::Unavailable)?;
            if owned.is_symlink() {
                return Err(ReadError::Redirect);
            }
            if (directory && !owned.is_dir()) || (!directory && !owned.is_file()) {
                return Err(ReadError::Kind);
            }
            if owned.nlink() == 0
                || owned.dev() != identity.volume
                || u128::from(owned.ino()) != identity.file
            {
                return Err(ReadError::Unavailable);
            }
            // Stat the referent of the exact procfs component without opening it.
            budget.checkpoint()?;
            let target = fstatat(process_fds, descriptor, AtFlags::empty())
                .map_err(|_| ReadError::Unavailable)?;
            if target.st_nlink == 0
                || u128::from(target.st_dev) != u128::from(identity.volume)
                || u128::from(target.st_ino) != identity.file
            {
                return Err(ReadError::Unavailable);
            }
            Ok(())
        };
        verify_binding()?;

        let mut bytes = [0u8; MAX_DIRECTORY_NAME_BYTES];
        // SAFETY: both Files remain borrowed and alive, the NUL-terminated name
        // is exactly this live entry's numeric descriptor, and bytes is a
        // writable stack buffer of exactly the passed capacity. readlinkat does
        // not retain pointers. Only a checked returned byte span is used below.
        budget.checkpoint()?;
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let count = unsafe {
            readlinkat(
                process_fds.as_raw_fd(),
                descriptor.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        let count = usize::try_from(count).map_err(|_| ReadError::Unavailable)?;
        if count == 0 || count >= bytes.len() {
            return Err(ReadError::Unavailable);
        }
        let path = std::str::from_utf8(&bytes[..count]).map_err(|_| ReadError::Unavailable)?;
        if !Path::new(path).is_absolute() || path.ends_with(" (deleted)") {
            return Err(ReadError::Unavailable);
        }
        verify_binding()?;
        budget.checkpoint()?;
        use_name(path)
    }

    pub(super) fn same_read_metadata(
        before: &std::fs::Metadata,
        after: &std::fs::Metadata,
    ) -> bool {
        let observation = |meta: &std::fs::Metadata| {
            (
                meta.dev(),
                meta.ino(),
                meta.len(),
                meta.mode(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec(),
            )
        };
        observation(before) == observation(after)
    }

    pub(super) fn owner_executable(metadata: &std::fs::Metadata) -> bool {
        // Git's Unix index comparison uses the owner execute bit for regular files.
        metadata.mode() & 0o100 != 0
    }

    impl Lease {
        pub(super) fn file(&self) -> &File {
            &self.0
        }
    }

    fn failure(error: Errno) -> ReadError {
        match error {
            Errno::ELOOP | Errno::EXDEV => ReadError::Redirect,
            Errno::ENOTDIR => ReadError::Kind,
            Errno::ENOSYS => ReadError::Unavailable,
            _ => io::Error::from_raw_os_error(error as i32).into(),
        }
    }

    fn visit_directory<E>(
        file: &File,
        limit: usize,
        budget: &ReadBudget<'_>,
        visit: &mut impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
    ) -> Result<Identity, DirectoryVisitError<E>> {
        // Linux getdents64 dirent layout is ino64, off64, u16 length, u8 type, name[].
        const NAME_AT: usize = 19;
        let before = file.metadata()?;
        let mut entries = 0;
        let mut page = [0u8; 4096];
        loop {
            budget.checkpoint()?;
            // SAFETY: file owns the open directory descriptor. The kernel may
            // write to page through its pointer for at most page.len() bytes.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let result = unsafe {
                syscall(
                    SYS_getdents64,
                    file.as_raw_fd(),
                    page.as_mut_ptr(),
                    page.len(),
                )
            };
            if result < 0 {
                let error = Errno::last();
                if error == Errno::EINTR {
                    continue;
                }
                return Err(failure(error).into());
            }
            if result == 0 {
                if !same_read_metadata(&before, &file.metadata()?) {
                    return Err(ReadError::Changed.into());
                }
                return Ok(Identity {
                    volume: before.dev(),
                    file: u128::from(before.ino()),
                });
            }
            let mut rows = page.get(..result as usize).ok_or(ReadError::Unavailable)?;
            while !rows.is_empty() {
                budget.checkpoint()?;
                let header = rows.get(..NAME_AT).ok_or(ReadError::Unavailable)?;
                let len = usize::from(u16::from_ne_bytes([header[16], header[17]]));
                if len <= NAME_AT || !len.is_multiple_of(8) {
                    return Err(ReadError::Unavailable.into());
                }
                let (row, rest) = rows.split_at_checked(len).ok_or(ReadError::Unavailable)?;
                rows = rest;
                let name = &row[NAME_AT..];
                visit(NativeDirectoryRow::NameBytes(name.len()))
                    .map_err(DirectoryVisitError::Visitor)?;
                let end = name
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or(ReadError::Unavailable)?;
                let name = std::str::from_utf8(&name[..end]).map_err(|_| ReadError::Path)?;
                let kind = match header[18] {
                    DT_REG => EntryKind::File,
                    DT_DIR => EntryKind::Directory,
                    DT_LNK => EntryKind::Redirect,
                    _ => EntryKind::Other,
                };
                forward_entry(&mut entries, name, kind, false, limit, visit)?;
            }
        }
    }

    fn fixed_name(
        process_fds: &File,
        directory: &File,
        identity: Identity,
        budget: &ReadBudget<'_>,
    ) -> Result<super::FixedScopeName, ReadError> {
        with_native_name(process_fds, directory, identity, true, budget, |path| {
            let mut bytes = [0u8; 4096];
            bytes[..path.len()].copy_from_slice(path.as_bytes());
            Ok(super::FixedScopeName {
                bytes,
                len: path.len(),
                identity,
            })
        })
    }

    impl Root {
        pub(super) fn linux_capture_anchors(&self) -> (&File, &File) {
            (&self.directory, self.process_fds.directory())
        }
        pub(super) fn scope_names_fixed(
            &self,
            children: &ScopeChildren,
            budget: &ReadBudget<'_>,
        ) -> Result<super::FixedScopeNames, ReadError> {
            let worktree = fixed_name(
                self.process_fds.directory(),
                &self.directory,
                self.identity,
                budget,
            )?;
            let git = fixed_name(
                self.process_fds.directory(),
                &children.git.0,
                children.git.1,
                budget,
            )?;
            let state = fixed_name(
                self.process_fds.directory(),
                &children.state.0,
                children.state.1,
                budget,
            )?;
            let root = Path::new(worktree.as_str());
            if Path::new(git.as_str()).strip_prefix(root).ok() != Some(Path::new(".git"))
                || Path::new(state.as_str()).strip_prefix(root).ok() != Some(Path::new(STATE_DIR))
                || fixed_name(
                    self.process_fds.directory(),
                    &self.directory,
                    self.identity,
                    budget,
                )? != worktree
            {
                return Err(ReadError::Changed);
            }
            Ok(super::FixedScopeNames {
                worktree,
                git,
                state,
            })
        }
        pub(super) fn open(path: &Path, budget: &ReadBudget<'_>) -> Result<Self, ReadError> {
            let mut components = path.components();
            if !matches!(components.next(), Some(Component::RootDir)) {
                return Err(ReadError::Path);
            }
            let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
            budget.checkpoint()?;
            let mut directory = open(Path::new("/"), flags, Mode::empty()).map_err(failure)?;
            for component in components {
                budget.checkpoint()?;
                let Component::Normal(name) = component else {
                    return Err(ReadError::Path);
                };
                directory = openat(&directory, name, flags, Mode::empty()).map_err(failure)?;
            }
            let directory = File::from(directory);
            budget.checkpoint()?;
            let meta = directory.metadata()?;
            let identity = Identity {
                volume: meta.dev(),
                file: u128::from(meta.ino()),
            };
            budget.checkpoint()?;
            let process_fds = LinuxProcFds::open(budget)?;
            Ok(Self {
                directory,
                process_fds,
                identity,
            })
        }

        pub(super) fn identity(&self) -> Identity {
            self.identity
        }

        pub(super) fn policy_directory(&self) -> &File {
            &self.directory
        }

        pub(super) fn policy_procfs(&self) -> &LinuxProcFds {
            &self.process_fds
        }

        pub(super) fn observe_directory_name(
            &self,
            budget: &ReadBudget<'_>,
        ) -> Result<NamedDirectory, ReadError> {
            observe_directory_name(
                self.process_fds.directory(),
                &self.directory,
                self.identity,
                budget,
            )
        }

        fn open_directory(&self, name: &str, budget: &ReadBudget<'_>) -> Result<File, ReadError> {
            budget.checkpoint()?;
            let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY;
            let how = OpenHow::new().flags(flags).resolve(
                ResolveFlag::RESOLVE_BENEATH
                    | ResolveFlag::RESOLVE_NO_SYMLINKS
                    | ResolveFlag::RESOLVE_NO_XDEV,
            );
            openat2(&self.directory, name, how)
                .map(File::from)
                .map_err(failure)
        }

        fn scope_child(
            &self,
            name: &str,
            budget: &ReadBudget<'_>,
        ) -> Result<(File, Identity), ReadError> {
            let file = self.open_directory(name, budget)?;
            budget.checkpoint()?;
            let meta = file.metadata()?;
            if meta.dev() != self.identity.volume {
                return Err(ReadError::Redirect);
            }
            Ok((
                file,
                Identity {
                    volume: meta.dev(),
                    file: u128::from(meta.ino()),
                },
            ))
        }

        pub(super) fn scope_children(
            &self,
            budget: &ReadBudget<'_>,
        ) -> Result<ScopeChildren, ReadError> {
            let git = self.scope_child(".git", budget)?;
            let state = self.scope_child(STATE_DIR, budget)?;
            Ok(ScopeChildren { git, state })
        }

        pub(super) fn scope_names(
            &self,
            children: &ScopeChildren,
            budget: &ReadBudget<'_>,
        ) -> Result<ScopeDirectoryNames, ReadError> {
            let worktree = self.observe_directory_name(budget)?;
            let git = observe_directory_name(
                self.process_fds.directory(),
                &children.git.0,
                children.git.1,
                budget,
            )?;
            let state = observe_directory_name(
                self.process_fds.directory(),
                &children.state.0,
                children.state.1,
                budget,
            )?;
            let root_path = Path::new(&worktree.path);
            if Path::new(&git.path).strip_prefix(root_path).ok() != Some(Path::new(".git"))
                || Path::new(&state.path).strip_prefix(root_path).ok() != Some(Path::new(STATE_DIR))
                || self.observe_directory_name(budget)? != worktree
            {
                return Err(ReadError::Changed);
            }
            Ok(ScopeDirectoryNames {
                worktree,
                git,
                state,
            })
        }

        pub(super) fn visit_directory<E>(
            &mut self,
            relative: &str,
            limit: usize,
            budget: &ReadBudget<'_>,
            visit: &mut impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
        ) -> Result<Identity, DirectoryVisitError<E>> {
            let name = if relative.is_empty() { "." } else { relative };
            let file = self.open_directory(name, budget)?;
            visit_directory(&file, limit, budget, visit)
        }

        pub(super) fn file(
            &self,
            relative: &str,
            budget: &ReadBudget<'_>,
        ) -> Result<Lease, ReadError> {
            budget.checkpoint()?;
            let flags = OFlag::O_PATH | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
            let how = OpenHow::new().flags(flags).resolve(
                ResolveFlag::RESOLVE_BENEATH
                    | ResolveFlag::RESOLVE_NO_SYMLINKS
                    | ResolveFlag::RESOLVE_NO_XDEV,
            );
            let observed = File::from(openat2(&self.directory, relative, how).map_err(failure)?);
            let meta = observed.metadata()?;
            if meta.is_symlink() {
                return Err(ReadError::Redirect);
            }
            if !meta.is_file() {
                return Err(ReadError::Kind);
            }
            budget.checkpoint()?;
            self.process_fds
                .reopen_regular(&observed, budget)
                .map(Lease)
        }

        pub(super) fn observe_entry(
            &self,
            relative: &str,
            directory: bool,
            budget: &ReadBudget<'_>,
        ) -> Result<Identity, ReadError> {
            let flags = OFlag::O_PATH | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
            let how = OpenHow::new().flags(flags).resolve(
                ResolveFlag::RESOLVE_BENEATH
                    | ResolveFlag::RESOLVE_NO_SYMLINKS
                    | ResolveFlag::RESOLVE_NO_XDEV,
            );
            let entry = File::from(openat2(&self.directory, relative, how).map_err(failure)?);
            let meta = entry.metadata()?;
            let identity = Identity {
                volume: meta.dev(),
                file: u128::from(meta.ino()),
            };
            if identity.volume != self.identity.volume {
                return Err(ReadError::Redirect);
            }
            with_native_name(
                self.process_fds.directory(),
                &self.directory,
                self.identity,
                true,
                budget,
                |root| {
                    with_native_name(
                        self.process_fds.directory(),
                        &entry,
                        identity,
                        directory,
                        budget,
                        |name| {
                            let name_relative = if root == "/" {
                                name.strip_prefix('/')
                            } else {
                                name.strip_prefix(root)
                                    .and_then(|tail| tail.strip_prefix('/'))
                            };
                            if name_relative != Some(relative) {
                                return Err(ReadError::Path);
                            }
                            Ok(identity)
                        },
                    )
                },
            )
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::{
        DirectoryVisitError, File, Identity, NativeDirectoryRow, Path, ReadBudget, ReadError,
    };
    pub(super) enum Root {}
    pub(super) struct Lease(File);

    impl Lease {
        pub(super) fn file(&self) -> &File {
            &self.0
        }
    }

    impl Root {
        pub(super) fn open(_: &Path, _: &ReadBudget<'_>) -> Result<Self, ReadError> {
            Err(ReadError::Unavailable)
        }
        pub(super) fn identity(&self) -> Identity {
            match *self {}
        }
        pub(super) fn file(&self, _: &str, _: &ReadBudget<'_>) -> Result<Lease, ReadError> {
            Err(ReadError::Unavailable)
        }
        pub(super) fn observe_entry(
            &self,
            _: &str,
            _: bool,
            _: &ReadBudget<'_>,
        ) -> Result<Identity, ReadError> {
            Err(ReadError::Unavailable)
        }
        pub(super) fn visit_directory<E>(
            &mut self,
            _: &str,
            _: usize,
            _: &ReadBudget<'_>,
            _: &mut impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
        ) -> Result<Identity, DirectoryVisitError<E>> {
            Err(ReadError::Unavailable.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[cfg(any(windows, target_os = "linux"))]
    fn fixture_path(fixture: &tempfile::TempDir) -> std::path::PathBuf {
        #[cfg(windows)]
        let path = dunce::canonicalize(fixture.path()).expect("canonicalize private fixture");
        #[cfg(target_os = "linux")]
        let path = fixture.path().to_path_buf();
        path
    }

    #[cfg(target_os = "linux")]
    thread_local! {
        static MUTATE_AFTER_FIRST_CHUNK: std::cell::RefCell<Option<std::path::PathBuf>> =
            const { std::cell::RefCell::new(None) };
    }

    #[cfg(target_os = "linux")]
    pub(super) fn after_chunk(read: usize) {
        use std::fs::FileTimes;
        use std::os::unix::fs::{FileExt, MetadataExt};
        if read != CHUNK_SIZE {
            return;
        }
        MUTATE_AFTER_FIRST_CHUNK.with(|mutation| {
            if let Some(path) = mutation.borrow_mut().take() {
                let writer = File::options().write(true).open(path).unwrap();
                let before = writer.metadata().unwrap();
                std::thread::sleep(Duration::from_millis(20));
                writer.write_all_at(b"mutated", CHUNK_SIZE as u64).unwrap();
                writer
                    .set_times(FileTimes::new().set_modified(before.modified().unwrap()))
                    .unwrap();
                let after = writer.metadata().unwrap();
                assert_eq!(before.len(), after.len());
                assert_eq!(before.modified().unwrap(), after.modified().unwrap());
                assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
                assert_ne!(
                    (before.ctime(), before.ctime_nsec()),
                    (after.ctime(), after.ctime_nsec())
                );
            }
        });
    }

    #[test]
    fn relative_paths_have_unambiguous_components() {
        for path in [
            "",
            "/absolute",
            "../outside",
            "src/../outside",
            "src//name",
            "src/./name",
            "src/",
            r"src\name",
            "src/a:stream",
        ] {
            assert!(
                matches!(validate_relative(path), Err(ReadError::Path)),
                "{path}"
            );
        }
        assert!(validate_relative("src/a.rs").is_ok());
        assert!(validate_relative(".git/HEAD").is_ok());
    }

    #[test]
    fn expired_budget_rejects_before_path_resolution() {
        let interrupted = AtomicBool::new(false);
        let budget = ReadBudget::new(16, Instant::now(), &interrupted);
        assert!(matches!(
            ContainedRoot::open(Path::new("relative"), &budget),
            Err(ReadError::Deadline)
        ));
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn lists_exact_names_with_a_limit_and_detects_inventory_change() {
        let fixture = tempfile::tempdir().unwrap();
        let fixture_root = fixture_path(&fixture);
        let dir = fixture_root.join("pack");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("Exact.idx"), b"index").unwrap();
        for i in 0..90 {
            std::fs::write(dir.join(format!("pack-{i:040x}.pack")), b"scratch").unwrap();
        }
        #[cfg(target_os = "linux")]
        std::os::unix::fs::symlink(&fixture_root, dir.join("redirect")).unwrap();
        let interrupted = AtomicBool::new(false);
        let mut budget =
            ReadBudget::new(16, Instant::now() + Duration::from_secs(10), &interrupted);
        let mut root = ContainedRoot::open(&fixture_root, &budget).unwrap();
        let entries = root.list("pack", 128, &budget).unwrap();
        assert_eq!(
            entries[0],
            DirectoryEntry {
                name: "Exact.idx".into(),
                kind: EntryKind::File
            }
        );
        assert!(matches!(
            root.list("pack", 4, &budget),
            Err(ReadError::Entries)
        ));
        assert!(matches!(
            root.list("pack/Exact.idx", 128, &budget),
            Err(ReadError::Kind)
        ));
        assert_eq!(
            root.read("pack/Exact.idx", 16, &mut budget).unwrap(),
            b"index"
        );
        #[cfg(windows)]
        assert!(matches!(
            root.read("pack/exact.idx", 16, &mut budget),
            Err(ReadError::Path)
        ));
        #[cfg(target_os = "linux")]
        {
            assert_eq!(entries.last().unwrap().kind, EntryKind::Redirect);
            assert!(matches!(
                root.list("pack/redirect", 128, &budget),
                Err(ReadError::Redirect | ReadError::Kind)
            ));
        }
        std::fs::write(dir.join("next.idx"), b"scratch").unwrap();
        assert_ne!(root.list("pack", 128, &budget).unwrap(), entries);
        assert_eq!(
            root.list("", 128, &budget).unwrap(),
            [DirectoryEntry {
                name: "pack".into(),
                kind: EntryKind::Directory
            }]
        );
        interrupted.store(true, Ordering::Release);
        assert!(matches!(
            root.list("pack", 128, &budget),
            Err(ReadError::Interrupted)
        ));
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn reads_exact_bytes_and_enforces_limits_and_interrupt() {
        let fixture = tempfile::tempdir().unwrap();
        let fixture_root = fixture_path(&fixture);
        std::fs::create_dir(fixture_root.join("src")).unwrap();
        let raw = b"\xef\xbb\xbf// raw\r\n";
        std::fs::write(fixture_root.join("src/input.rs"), raw).unwrap();
        let interrupted = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut budget = ReadBudget::new(raw.len(), deadline, &interrupted);
        let root = ContainedRoot::open(&fixture_root, &budget).unwrap();
        assert_eq!(
            root.read("src/input.rs", raw.len(), &mut budget).unwrap(),
            raw
        );
        assert!(matches!(
            root.read("src/input.rs", raw.len(), &mut budget),
            Err(ReadError::Aggregate)
        ));
        let mut budget = ReadBudget::new(raw.len(), deadline, &interrupted);
        assert!(matches!(
            root.read("src/input.rs", raw.len() - 1, &mut budget),
            Err(ReadError::Bytes)
        ));
        assert!(matches!(
            root.read("src", raw.len(), &mut budget),
            Err(ReadError::Kind)
        ));
        interrupted.store(true, Ordering::Release);
        assert!(matches!(
            root.read("src/input.rs", raw.len(), &mut budget),
            Err(ReadError::Interrupted)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refuses_same_inode_mutation_during_a_multi_chunk_read() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("file");
        let original = vec![b'a'; 2 * CHUNK_SIZE];
        std::fs::write(&path, &original).unwrap();
        let interrupted = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut budget = ReadBudget::new(original.len(), deadline, &interrupted);
        let root = ContainedRoot::open(fixture.path(), &budget).unwrap();
        assert_eq!(
            root.read("file", original.len(), &mut budget).unwrap(),
            original
        );
        assert_eq!(budget.remaining, 0);
        MUTATE_AFTER_FIRST_CHUNK.with(|mutation| *mutation.borrow_mut() = Some(path));
        let mut budget = ReadBudget::new(original.len(), deadline, &interrupted);
        assert!(matches!(
            root.read("file", original.len(), &mut budget),
            Err(ReadError::Changed)
        ));
        assert_eq!(budget.remaining, 0);
        MUTATE_AFTER_FIRST_CHUNK.with(|mutation| assert!(mutation.borrow().is_none()));
    }

    #[cfg(windows)]
    #[test]
    fn holds_worktree_identity_and_rejects_device_names() {
        let fixture = tempfile::tempdir().unwrap();
        let fixture_root = fixture_path(&fixture);
        let path = fixture_root.join("scope");
        std::fs::create_dir(&path).unwrap();
        let interrupted = AtomicBool::new(false);
        let mut budget =
            ReadBudget::new(16, Instant::now() + Duration::from_secs(10), &interrupted);
        let root = ContainedRoot::open(&path, &budget).unwrap();
        let duplicate = ContainedRoot::open(&path, &budget).unwrap();
        assert_eq!(root.identity(), duplicate.identity());
        let target = fixture_root.join("moved");
        assert!(std::fs::rename(&path, &target).is_err());
        for path in [
            "CON",
            "AUX.txt",
            "src/COM\u{00b9}",
            "com\u{00b2}.log",
            "COM\u{00b3}",
            "LPT\u{00b9}.txt",
            "src/lpt\u{00b2}",
            "LPT\u{00b3}",
            "NUL .txt",
            "trailing.",
        ] {
            assert!(
                matches!(root.read(path, 16, &mut budget), Err(ReadError::Path)),
                "{path}"
            );
        }
        drop(duplicate);
        drop(root);
        std::fs::rename(path, target).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_root_does_not_follow_replacement_or_new_symlink() {
        use nix::sys::stat::Mode;
        use nix::unistd::mkfifo;
        use std::os::unix::fs::symlink;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("scope");
        let outside = fixture.path().join("outside");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(path.join("file"), "retained").unwrap();
        std::fs::write(outside.join("file"), "outside").unwrap();
        mkfifo(path.join("fifo").as_path(), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let interrupted = AtomicBool::new(false);
        let mut budget =
            ReadBudget::new(128, Instant::now() + Duration::from_secs(10), &interrupted);
        let mut root = ContainedRoot::open(&path, &budget).unwrap();
        std::fs::rename(&path, fixture.path().join("moved")).unwrap();
        symlink(&outside, &path).unwrap();
        symlink(outside.join("file"), fixture.path().join("moved/redirect")).unwrap();
        assert_eq!(root.read("file", 32, &mut budget).unwrap(), b"retained");
        let entries = root.list("", 4, &budget).unwrap();
        assert_eq!(
            entries[0],
            DirectoryEntry {
                name: "fifo".into(),
                kind: EntryKind::Other
            }
        );
        assert!(!entries.iter().any(|entry| entry.name == "outside"));
        assert!(matches!(
            root.read("redirect", 32, &mut budget),
            Err(ReadError::Redirect)
        ));
        assert!(matches!(
            root.read("fifo", 32, &mut budget),
            Err(ReadError::Kind)
        ));
    }
}
