//! Bounded worklist and path lookup for one tree-to-staged comparison.
use super::buffer::{Reservation, ReservedBytes};
use super::pack::Work;
use gix::ObjectId;
use gix::index::entry::Mode;
use gix::objs::tree::{EntryKind, EntryRef};
use gix::validate::path::{component, component::Options};
use std::cmp::Ordering;
use std::mem::size_of;

type TreeIndexResult<T> = Result<T, &'static str>;
const MAX_NODES: usize = 4_096;
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 4_094;
const MAX_COMPONENT_BYTES: usize = 255;

pub(crate) fn is_evidence_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && path
            .split('/')
            .all(|entry| is_evidence_component(entry.as_bytes()))
}

fn is_evidence_component(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_COMPONENT_BYTES
        && !name.eq_ignore_ascii_case(b".ledgerful")
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(byte))
        && component(name.into(), None, Options::default()).is_ok()
}

struct WalkEntry<'a> {
    path: ReservedBytes<'a>,
    depth: usize,
    kind: EntryKind,
    id: ObjectId,
    checkout_ambiguous: bool,
}

pub(super) struct PendingTree<'a> {
    path: ReservedBytes<'a>,
    depth: usize,
    pub(super) id: ObjectId,
    checkout_ambiguous: bool,
}

#[derive(Clone, Copy)]
pub(super) struct TreeFile<'a> {
    pub(super) path: &'a [u8],
    pub(super) id: ObjectId,
    pub(super) executable: bool,
    pub(super) checkout_ambiguous: bool,
}

pub(super) struct TreeIndexWalk<'a> {
    entries: Vec<WalkEntry<'a>>,
    by_path: Vec<usize>,
    files: usize,
    limit: usize,
    paths: TreePaths,
    unsupported_checkout_path: bool,
    _tables: Reservation<'a>,
}

pub(super) enum TreePaths {
    Strict,
    ChangedFiles,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ExpectedEntry {
    Directory,
    File,
}

impl<'a> TreeIndexWalk<'a> {
    pub(super) fn new(paths: TreePaths, work: &mut Work<'a>) -> TreeIndexResult<Self> {
        Self::with_limit(paths, MAX_NODES, work)
    }

    pub(super) fn with_limit(
        paths: TreePaths,
        limit: usize,
        work: &mut Work<'a>,
    ) -> TreeIndexResult<Self> {
        let bytes = limit * (size_of::<WalkEntry<'a>>() + size_of::<usize>());
        let tables = reserve(work, bytes)?;
        let mut entries = Vec::new();
        let mut by_path = Vec::new();
        entries.try_reserve_exact(limit).map_err(|_| "allocation")?;
        by_path.try_reserve_exact(limit).map_err(|_| "allocation")?;
        if entries.capacity() != limit || by_path.capacity() != limit {
            return Err("memory-cap");
        }
        Ok(Self {
            entries,
            by_path,
            files: 0,
            limit,
            paths,
            unsupported_checkout_path: false,
            _tables: tables,
        })
    }

    pub(super) fn files(&self) -> usize {
        self.files
    }

    pub(super) fn has_unsupported_checkout_path(&self) -> bool {
        self.unsupported_checkout_path
    }

    pub(super) fn directory_count(&self) -> usize {
        self.entries.len() - self.files
    }

    pub(super) fn expected_entry(
        &self,
        path: &[u8],
        work: &mut Work<'a>,
    ) -> TreeIndexResult<Option<ExpectedEntry>> {
        let Ok(index) = self.find_path(path, work)? else {
            return Ok(None);
        };
        let entry = &self.entries[self.by_path[index]];
        work.charge(path.len())?;
        if entry.path.as_ref() != path {
            return Err("inventory-tracked-alias");
        }
        Ok(Some(if entry.kind == EntryKind::Tree {
            ExpectedEntry::Directory
        } else {
            ExpectedEntry::File
        }))
    }

    pub(super) fn direct_child_count(
        &self,
        parent: &[u8],
        work: &mut Work<'a>,
    ) -> TreeIndexResult<usize> {
        let prefix = parent
            .iter()
            .copied()
            .chain((!parent.is_empty()).then_some(b'/'));
        let prefix_len = parent.len() + usize::from(!parent.is_empty());
        let comparisons = (usize::BITS - self.by_path.len().leading_zeros()) as usize + 1;
        work.charge(comparisons * (prefix_len + 1))?;
        let start = self.by_path.partition_point(|&index| {
            self.entries[index]
                .path
                .as_ref()
                .iter()
                .map(u8::to_ascii_lowercase)
                .cmp(prefix.clone().map(|byte| byte.to_ascii_lowercase()))
                .is_lt()
        });
        let mut count = 0;
        for index in &self.by_path[start..] {
            let path = self.entries[*index].path.as_ref();
            work.charge(prefix_len + 1)?;
            if !path.iter().copied().take(prefix_len).eq(prefix.clone()) {
                break;
            }
            let tail = &path[prefix_len..];
            work.charge(tail.len())?;
            count += usize::from(!tail.contains(&b'/'));
        }
        Ok(count)
    }

    pub(super) fn tracked_directories<'tree>(
        &'tree self,
        work: &mut Work<'a>,
    ) -> TreeIndexResult<impl Iterator<Item = &'tree [u8]> + use<'a, 'tree>> {
        work.charge(self.entries.len())?;
        Ok(self.by_path.iter().filter_map(|&index| {
            let entry = &self.entries[index];
            (entry.kind == EntryKind::Tree).then_some(entry.path.as_ref())
        }))
    }

    pub(super) fn tracked_files(&self) -> impl Iterator<Item = (&[u8], ObjectId, bool)> {
        self.entries
            .iter()
            .filter(|entry| entry.kind != EntryKind::Tree)
            .map(|entry| {
                (
                    entry.path.as_ref(),
                    entry.id,
                    entry.kind == EntryKind::BlobExecutable,
                )
            })
    }

    pub(super) fn files_in_path_order<'tree>(
        &'tree self,
        work: &mut Work<'a>,
    ) -> TreeIndexResult<impl Iterator<Item = TreeFile<'tree>> + use<'a, 'tree>> {
        work.charge(self.by_path.len())?;
        Ok(self.by_path.iter().filter_map(|&index| {
            let entry = &self.entries[index];
            (entry.kind != EntryKind::Tree).then_some(TreeFile {
                path: entry.path.as_ref(),
                id: entry.id,
                executable: entry.kind == EntryKind::BlobExecutable,
                checkout_ambiguous: entry.checkout_ambiguous,
            })
        }))
    }

    pub(super) fn push(
        &mut self,
        parent: Option<&PendingTree<'_>>,
        entry: EntryRef<'_>,
        work: &mut Work<'a>,
    ) -> TreeIndexResult<()> {
        let prefix = parent.map_or(&[][..], |tree| tree.path.as_ref());
        let depth = parent.map_or(1, |tree| tree.depth + 1);
        if self.entries.len() == self.limit {
            return Err("tree-walk-entry-cap");
        }
        if depth > MAX_DEPTH {
            return Err("tree-depth-cap");
        }
        let name: &[u8] = entry.filename.as_ref();
        match self.paths {
            TreePaths::Strict => {
                if !is_evidence_component(name) {
                    return Err("unsupported-tree-path");
                }
                if path_len(prefix, name) > MAX_PATH_BYTES {
                    return Err("tree-path-cap");
                }
            }
            // Raw components never reach a filesystem lookup in the diff. Keep the
            // hierarchy unambiguous while retaining names that cannot be projected.
            TreePaths::ChangedFiles if name.contains(&b'/') => return Err("invalid-tree-entry"),
            TreePaths::ChangedFiles => {
                work.charge(name.len())?;
                self.unsupported_checkout_path |=
                    !is_evidence_component(name) || path_len(prefix, name) > MAX_PATH_BYTES;
            }
        }
        let path = copy_path(prefix, name, work)?;
        let mut checkout_ambiguous = parent.is_some_and(|tree| tree.checkout_ambiguous);
        let slot = match self.paths {
            TreePaths::Strict => self.find_path(path.as_ref(), work)?,
            TreePaths::ChangedFiles => self.search_path(path.as_ref(), diff_path_cmp, 2, work)?,
        }
        .err()
        .ok_or("tree-path-alias")?;
        if matches!(self.paths, TreePaths::ChangedFiles) {
            // A directory's siblings are all discovered before its children, so
            // descendants inherit any ambiguity already found for their parent.
            for neighbor in [slot.checked_sub(1), Some(slot)].into_iter().flatten() {
                if let Some(&index) = self.by_path.get(neighbor) {
                    work.charge(path.as_ref().len() + 1)?;
                    let other = &mut self.entries[index];
                    if folded_cmp(other.path.as_ref(), path.as_ref()).is_eq() {
                        other.checkout_ambiguous = true;
                        checkout_ambiguous = true;
                    }
                }
            }
            self.unsupported_checkout_path |= checkout_ambiguous;
        }
        work.charge((self.by_path.len() - slot) * size_of::<usize>())?;
        let kind = entry.mode.kind();
        self.files += usize::from(kind != EntryKind::Tree);
        self.by_path.insert(slot, self.entries.len());
        self.entries.push(WalkEntry {
            path,
            depth,
            kind,
            id: entry.oid.to_owned(),
            checkout_ambiguous,
        });
        Ok(())
    }

    pub(super) fn next_tree(
        &self,
        cursor: &mut usize,
        work: &mut Work<'a>,
    ) -> TreeIndexResult<Option<PendingTree<'a>>> {
        while let Some(entry) = self.entries.get(*cursor) {
            *cursor += 1;
            work.charge(1)?;
            if entry.kind == EntryKind::Tree {
                let path = copy_path(&[], entry.path.as_ref(), work)?;
                return Ok(Some(PendingTree {
                    path,
                    depth: entry.depth,
                    id: entry.id,
                    checkout_ambiguous: entry.checkout_ambiguous,
                }));
            }
        }
        Ok(None)
    }

    pub(super) fn compare_file(
        &self,
        path: &[u8],
        mode: Mode,
        id: ObjectId,
        work: &mut Work<'a>,
    ) -> TreeIndexResult<()> {
        let slot = self
            .find_path(path, work)?
            .map_err(|_| "staged-tree-mismatch")?;
        let entry = &self.entries[self.by_path[slot]];
        let expected_mode = match entry.kind {
            EntryKind::Blob => Mode::FILE,
            EntryKind::BlobExecutable => Mode::FILE_EXECUTABLE,
            _ => return Err("staged-tree-mismatch"),
        };
        work.charge(path.len())?;
        if entry.path.as_ref() != path || entry.id != id || expected_mode != mode {
            return Err("staged-tree-mismatch");
        }
        Ok(())
    }

    pub(super) fn exact_file(
        &self,
        path: &[u8],
        work: &mut Work<'a>,
    ) -> TreeIndexResult<Option<TreeFile<'_>>> {
        let Ok(slot) = self.find_path(path, work)? else {
            return Ok(None);
        };
        let entry = &self.entries[self.by_path[slot]];
        work.charge(path.len())?;
        if entry.path.as_ref() != path
            || !matches!(entry.kind, EntryKind::Blob | EntryKind::BlobExecutable)
        {
            return Ok(None);
        }
        Ok(Some(TreeFile {
            path: entry.path.as_ref(),
            id: entry.id,
            executable: entry.kind == EntryKind::BlobExecutable,
            checkout_ambiguous: entry.checkout_ambiguous,
        }))
    }

    fn find_path(&self, path: &[u8], work: &mut Work<'_>) -> TreeIndexResult<Result<usize, usize>> {
        self.search_path(path, folded_cmp, 1, work)
    }

    fn search_path(
        &self,
        path: &[u8],
        compare: fn(&[u8], &[u8]) -> Ordering,
        passes: usize,
        work: &mut Work<'_>,
    ) -> TreeIndexResult<Result<usize, usize>> {
        let comparisons = (usize::BITS - self.by_path.len().leading_zeros()) as usize + 1;
        work.charge(passes * comparisons * (path.len() + 1))?;
        Ok(self
            .by_path
            .binary_search_by(|index| compare(self.entries[*index].path.as_ref(), path)))
    }
}

pub(super) fn diff_path_cmp(left: &[u8], right: &[u8]) -> Ordering {
    folded_cmp(left, right).then_with(|| left.cmp(right))
}

pub(super) fn folded_cmp(left: &[u8], right: &[u8]) -> Ordering {
    left.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(right.iter().map(u8::to_ascii_lowercase))
}

fn reserve<'a>(work: &mut Work<'a>, bytes: usize) -> TreeIndexResult<Reservation<'a>> {
    if work.read_limit(bytes, 0)? != bytes {
        return Err("memory-cap");
    }
    work.memory_budget()
        .reserve(bytes)
        .map_err(|_| "memory-cap")
}

pub(super) fn copy_path<'a>(
    prefix: &[u8],
    name: &[u8],
    work: &mut Work<'a>,
) -> TreeIndexResult<ReservedBytes<'a>> {
    let bytes = path_len(prefix, name);
    work.charge(bytes)?;
    let reservation = reserve(work, bytes)?;
    let mut path = Vec::new();
    path.try_reserve_exact(bytes).map_err(|_| "allocation")?;
    if path.capacity() != bytes {
        return Err("memory-cap");
    }
    path.extend_from_slice(prefix);
    if !prefix.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    reservation.into_bytes(path).map_err(|_| "memory-cap")
}

fn path_len(prefix: &[u8], name: &[u8]) -> usize {
    prefix.len() + name.len() + usize::from(!prefix.is_empty())
}
