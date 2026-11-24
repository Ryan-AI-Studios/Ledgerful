//! Isolated Linux scope owner; no config, origin, common-dir or guard claim.
use super::buffer::Reservation;
use super::contained::{
    ContainedRoot, DirectoryEntry, DirectoryVisitError, FixedScopeNames, Identity,
    NativeDirectoryRow, ReadBudget, ReadError, RetainedScopeDirectories,
};
use super::linux_proc_fds::PROC_ACQUIRE_WORK;
use super::pack::Work;
use std::path::{Component, Path};
const NAME_CAP: usize = 4096;
const MAX_COMPONENTS: usize = 64;
// Four names each perform five explicit fallible native operations; this is not a syscall count.
const NAME_NATIVE_CALLS: usize = 4 * 5;
// Filesystem root, at most 64 openat steps, root metadata, authenticated proc and two children.
const ACQUIRE_NATIVE_CALLS: usize = 1 + MAX_COMPONENTS + 1 + PROC_ACQUIRE_WORK + 2 * 2;
const OBSERVE_WORK: usize = NAME_CAP * 8 + NAME_NATIVE_CALLS;
const SCRATCH_BYTES: usize = NAME_CAP * 2 + 128;
pub(crate) struct MeteredLinuxScopeDirectories<'a> {
    owner: RetainedScopeDirectories,
    names: FixedScopeNames,
    _names_memory: Reservation<'a>,
}
impl<'a> MeteredLinuxScopeDirectories<'a> {
    pub(crate) fn open_metered_linux(
        path: &Path,
        io: &ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<Self, &'static str> {
        work.charge(0)?;
        let raw_len = path.as_os_str().len();
        if raw_len > NAME_CAP {
            return Err("scope-path-cap");
        }
        work.charge(
            raw_len
                .checked_mul(4)
                .and_then(|n| n.checked_add(1))
                .ok_or("work-cap")?,
        )?;
        let _raw = path.to_str().ok_or("scope-path")?;
        let mut count = 0;
        for c in path.components() {
            if let Component::Normal(v) = c {
                count += 1;
                if v.len() >= 1024 {
                    return Err("scope-component-cap");
                }
            }
        }
        if count > MAX_COMPONENTS {
            return Err("scope-component-cap");
        }
        // Nix 0.31.3 keeps paths shorter than 1024 bytes on stack.
        work.charge(ACQUIRE_NATIVE_CALLS + OBSERVE_WORK)?;
        let retained = std::mem::size_of::<FixedScopeNames>();
        let peak = retained.checked_add(SCRATCH_BYTES).ok_or("memory-cap")?;
        if work.read_limit(peak, 0)? != peak {
            return Err("memory-cap");
        }
        let memory = work.memory_budget();
        let held = memory.reserve(retained).map_err(|_| "memory-cap")?;
        let temporary = memory.reserve(SCRATCH_BYTES).map_err(|_| "memory-cap")?;
        let root = ContainedRoot::open(path, io).map_err(|_| "scope-unavailable")?;
        let owner = root
            .retain_scope_directories(io)
            .map_err(|_| "scope-unavailable")?;
        let names = owner.observe_fixed(io).map_err(|_| "scope-unavailable")?;
        drop(temporary);
        work.charge(0)?;
        Ok(Self {
            owner,
            names,
            _names_memory: held,
        })
    }
    pub(crate) fn observe(&self) -> &FixedScopeNames {
        &self.names
    }
    pub(crate) fn root(&self) -> &ContainedRoot {
        self.owner.root()
    }
    pub(crate) fn list(
        &mut self,
        relative: &str,
        entry_limit: usize,
        io: &ReadBudget<'_>,
    ) -> Result<Vec<DirectoryEntry>, ReadError> {
        self.owner.list(relative, entry_limit, io)
    }
    pub(crate) fn visit_directory<E>(
        &mut self,
        relative: &str,
        entry_limit: usize,
        io: &ReadBudget<'_>,
        visit: impl FnMut(NativeDirectoryRow<'_>) -> Result<(), E>,
    ) -> Result<Identity, DirectoryVisitError<E>> {
        self.owner.visit_directory(relative, entry_limit, io, visit)
    }
    pub(crate) fn confirm_reacquired(
        &self,
        path: &Path,
        io: &ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> Result<(), &'static str> {
        let fresh = Self::open_metered_linux(path, io, work)?;
        if fresh.names != self.names {
            return Err("scope-changed");
        }
        work.charge(OBSERVE_WORK)?;
        let peak = std::mem::size_of::<FixedScopeNames>() + SCRATCH_BYTES;
        if work.read_limit(peak, 0)? != peak {
            return Err("memory-cap");
        }
        let _temp = work
            .memory_budget()
            .reserve(peak)
            .map_err(|_| "memory-cap")?;
        if self
            .owner
            .observe_fixed(io)
            .map_err(|_| "scope-unavailable")?
            != self.names
        {
            return Err("scope-changed");
        }
        work.charge(0)
    }
}
