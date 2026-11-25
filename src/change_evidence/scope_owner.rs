//! One root owner with scoped platform variants; mutable roots never escape.
use super::buffer::Reservation;
use super::contained::{
    ContainedRoot, DirectoryEntry, DirectoryVisitError, Identity, NativeDirectoryRow, ReadBudget,
    ReadError,
};
#[cfg(windows)]
use super::contained::{MeteredWindowsScopeDirectories, MeteredWindowsScopeDirectoryNames};
#[cfg(target_os = "linux")]
use super::linux_scope::MeteredLinuxScopeDirectories;
use super::pack::Work;
use super::scope::CanonicalScopePaths;
use std::{ops::Deref, path::Path};
pub(super) struct ScopedSlot<'a, T> {
    item: Vec<T>,
    _storage: Reservation<'a>,
}
impl<'a, T> ScopedSlot<'a, T> {
    fn allocate(work: &mut Work<'a>) -> Result<Self, &'static str> {
        let bytes = std::mem::size_of::<T>();
        work.charge(bytes)?;
        if work.read_limit(bytes, 0)? != bytes {
            return Err("memory-cap");
        }
        let storage = work
            .memory_budget()
            .reserve(bytes)
            .map_err(|_| "memory-cap")?;
        let mut item = Vec::new();
        item.try_reserve_exact(1).map_err(|_| "allocation")?;
        if item.capacity() != 1 {
            return Err("memory-cap");
        }
        Ok(Self {
            item,
            _storage: storage,
        })
    }
    fn put(&mut self, value: T, moved_inline_credit: usize) -> Result<(), &'static str> {
        self.item.push(value);
        let remaining = std::mem::size_of::<T>()
            .checked_sub(moved_inline_credit)
            .ok_or("memory-cap")?;
        self._storage
            .account_capacity(remaining)
            .map_err(|_| "memory-cap")
    }
    fn get(&self) -> &T {
        &self.item[0]
    }
    fn get_mut(&mut self) -> &mut T {
        &mut self.item[0]
    }
}
#[cfg(windows)]
pub(super) struct WindowsScope<'a> {
    owner: MeteredWindowsScopeDirectories<'a>,
    names: MeteredWindowsScopeDirectoryNames<'a>,
}
pub(super) enum ScopeRoot<'a> {
    Plain(ContainedRoot),
    #[cfg(windows)]
    Windows(ScopedSlot<'a, WindowsScope<'a>>),
    #[cfg(target_os = "linux")]
    Linux(ScopedSlot<'a, MeteredLinuxScopeDirectories<'a>>),
    #[cfg(not(any(windows, target_os = "linux")))]
    Unsupported(std::marker::PhantomData<&'a ()>),
}
impl Deref for ScopeRoot<'_> {
    type Target = ContainedRoot;
    fn deref(&self) -> &ContainedRoot {
        match self {
            Self::Plain(root) => root,
            #[cfg(windows)]
            Self::Windows(slot) => slot.get().owner.root(),
            #[cfg(target_os = "linux")]
            Self::Linux(slot) => slot.get().root(),
            #[cfg(not(any(windows, target_os = "linux")))]
            Self::Unsupported(_) => unreachable!(),
        }
    }
}
impl<'a> ScopeRoot<'a> {
    pub(super) fn open_scoped(
        path: &Path,
        io: &ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<Self, &'static str> {
        #[cfg(windows)]
        {
            let mut slot = ScopedSlot::allocate(work)?;
            let owner = MeteredWindowsScopeDirectories::open(path, io, work).map_err(read_error)?;
            let names = owner.observe(io, work).map_err(read_error)?;
            slot.put(WindowsScope { owner, names }, 0)?;
            Ok(Self::Windows(slot))
        }
        #[cfg(target_os = "linux")]
        {
            let mut slot = ScopedSlot::allocate(work)?;
            let owner = MeteredLinuxScopeDirectories::open_metered_linux(path, io, work)?;
            // The owner's fixed inline names have their own reservation. The slot
            // and original owner overlap during construction; after the move,
            // credit those bytes to avoid charging the same fixed names twice.
            slot.put(
                owner,
                std::mem::size_of::<super::contained::FixedScopeNames>(),
            )?;
            Ok(Self::Linux(slot))
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (path, io, work);
            Err("scope-unavailable")
        }
    }
    pub(super) fn list(
        &mut self,
        p: &str,
        n: usize,
        io: &ReadBudget<'_>,
    ) -> Result<Vec<DirectoryEntry>, ReadError> {
        match self {
            Self::Plain(root) => root.list(p, n, io),
            #[cfg(windows)]
            Self::Windows(slot) => slot.get_mut().owner.list(p, n, io),
            #[cfg(target_os = "linux")]
            Self::Linux(slot) => slot.get_mut().list(p, n, io),
            #[cfg(not(any(windows, target_os = "linux")))]
            Self::Unsupported(_) => Err(ReadError::Unavailable),
        }
    }
    pub(super) fn visit_directory<E>(
        &mut self,
        p: &str,
        n: usize,
        io: &ReadBudget<'_>,
        visit: impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
    ) -> Result<Identity, DirectoryVisitError<E>> {
        match self {
            Self::Plain(root) => root.visit_directory(p, n, io, visit),
            #[cfg(windows)]
            Self::Windows(slot) => slot.get_mut().owner.visit_directory(p, n, io, visit),
            #[cfg(target_os = "linux")]
            Self::Linux(slot) => slot.get_mut().visit_directory(p, n, io, visit),
            #[cfg(not(any(windows, target_os = "linux")))]
            Self::Unsupported(_) => Err(DirectoryVisitError::Reader(ReadError::Unavailable)),
        }
    }
    pub(super) fn confirm_reacquired(
        &self,
        p: &Path,
        io: &ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<(), &'static str> {
        match self {
            Self::Plain(_) => Err("scope-owner-unavailable"),
            #[cfg(windows)]
            Self::Windows(slot) => slot
                .get()
                .owner
                .confirm_reacquired(p, &slot.get().names, io, work)
                .map_err(read_error),
            #[cfg(target_os = "linux")]
            Self::Linux(slot) => slot.get().confirm_reacquired(p, io, work),
            #[cfg(not(any(windows, target_os = "linux")))]
            Self::Unsupported(_) => Err("scope-unavailable"),
        }
    }
    pub(super) fn confirm_same_scope(
        &self,
        other: &Self,
        work: &mut Work<'a>,
    ) -> Result<(), &'static str> {
        // Each of the three retained names has at most 4096 bytes on either side.
        work.charge(6 * 4096 + 1)?;
        let matches = match (self, other) {
            #[cfg(windows)]
            (Self::Windows(first), Self::Windows(second)) => {
                first.get().names.names() == second.get().names.names()
            }
            #[cfg(target_os = "linux")]
            (Self::Linux(first), Self::Linux(second)) => {
                first.get().observe() == second.get().observe()
            }
            _ => return Err("scope-owner-unavailable"),
        };
        if !matches {
            return Err("scope-changed");
        }
        work.charge(0)
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(super) fn observe_default_policy_environment(
        &self,
        #[cfg(target_os = "linux")] layout_root: &Path,
        allow_contained_cwd: bool,
        io: &mut ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<(super::contained::DefaultAncestorAbsence, [u8; 32]), &'static str> {
        match self {
            #[cfg(windows)]
            Self::Windows(slot) => {
                let owner = &slot.get().owner;
                let ancestry = owner
                    .observe_default_ancestor_absence(io, work)
                    .map_err(read_error)?
                    .ok_or("discovery-ancestor-policy-unavailable")?;
                let global = owner
                    .observe_default_global_absence(allow_contained_cwd, io, work)
                    .map_err(read_error)?
                    .ok_or("discovery-global-policy-unavailable")?;
                Ok((ancestry, global))
            }
            #[cfg(target_os = "linux")]
            Self::Linux(slot) => {
                let owner = slot.get();
                let canonical = owner.observe().worktree.as_str();
                work.charge(canonical.len() + layout_root.as_os_str().len())?;
                if layout_root.as_os_str().as_encoded_bytes() != canonical.as_bytes() {
                    return Err("discovery-layout-path-unavailable");
                }
                owner
                    .root()
                    .observe_default_policy_environment(
                        Path::new(canonical),
                        allow_contained_cwd,
                        io,
                        work,
                    )
                    .map_err(read_error)?
                    .ok_or("discovery-ambient-policy-unavailable")
            }
            _ => Err("discovery-scope-unavailable"),
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn linux_state_identity(&self) -> Option<Identity> {
        match self {
            Self::Linux(slot) => Some(slot.get().observe().state.identity),
            _ => None,
        }
    }
    pub(super) fn canonical_paths(
        &self,
        work: &mut Work<'_>,
    ) -> Result<CanonicalScopePaths<'_>, &'static str> {
        // Each native path is bounded by 4096 bytes; charge before UTF-8 validation.
        work.charge(3 * 4096 + 1)?;
        match self {
            Self::Plain(_) => Err("scope-owner-unavailable"),
            #[cfg(windows)]
            Self::Windows(slot) => {
                let n = slot.get().names.names();
                Ok(CanonicalScopePaths {
                    worktree: &n.worktree.path,
                    git: &n.git.path,
                    common: &n.git.path,
                    state: &n.state.path,
                })
            }
            #[cfg(target_os = "linux")]
            Self::Linux(slot) => {
                let n = slot.get().observe();
                let git = n.git.as_str();
                Ok(CanonicalScopePaths {
                    worktree: n.worktree.as_str(),
                    git,
                    common: git,
                    state: n.state.as_str(),
                })
            }
            #[cfg(not(any(windows, target_os = "linux")))]
            Self::Unsupported(_) => Err("scope-unavailable"),
        }
    }
}
#[cfg(any(windows, target_os = "linux"))]
fn read_error(e: ReadError) -> &'static str {
    match e {
        ReadError::Work => "work-cap",
        other => super::git::read_error(other),
    }
}
