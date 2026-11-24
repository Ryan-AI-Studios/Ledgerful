//! One bounded inventory of the visible worktree namespace.
use super::buffer::{Reservation, ReservedBytes};
use super::contained::{
    DirectoryVisitError, EntryKind, Identity, MAX_RELATIVE_BYTES, ReadBudget, ReadError,
};
use super::directory_rows::{DirectoryRows, OwnedDirectoryRow};
use super::git::{optional, read_error};
use super::inventory_transcript::{InventoryTranscript, RowRole};
use super::pack::Work;
use super::root_ignore::MAX_IGNORE_FILE_BYTES;
use super::root_ignore_matcher::RootIgnoreMatcher;
use super::scope_owner::ScopeRoot;
use super::tree_index::{ExpectedEntry, TreeIndexWalk, folded_cmp};
use crate::state::layout::STATE_DIR;
use std::mem::size_of;

const MAX_ENTRIES: usize = 4_096;
const MAX_DEPTH: usize = 32;
const METADATA_DIRECTORIES: [&str; 2] = [".git", STATE_DIR];
const METADATA_ROLES: [RowRole; 2] = [RowRole::GitDirectory, RowRole::StateDirectory];
const IGNORE_NAME: &[u8] = b".gitignore";

#[derive(Clone, Copy)]
pub(crate) struct InventoryLimits {
    pub(crate) entries: usize,
    pub(crate) directories: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum InventoryError {
    Budget(&'static str),
    Changed,
    Unsupported(&'static str),
    TrackedNamespace,
}

/// Completed counts for one observation only.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct InventoryPass {
    pub(crate) directories_visited: usize,
    pub(crate) directories_pruned: usize,
    pub(crate) entries_observed: usize,
    pub(crate) untracked_files: usize,
}

type InventoryResult<T> = Result<T, InventoryError>;

struct Rules<'a> {
    raw: ReservedBytes<'a>,
    matcher: RootIgnoreMatcher<'a>,
    identity: Identity,
}

struct Directory<'a> {
    path: ReservedBytes<'a>,
    parent: Option<usize>,
    depth: usize,
    identity: Identity,
    pruned: bool,
    rules: Option<Rules<'a>>,
}

pub(super) struct InventoryRead<'observation, 'a> {
    pub(super) root: &'observation mut ScopeRoot<'a>,
    pub(super) io: &'observation mut ReadBudget<'a>,
    pub(super) work: &'observation mut Work<'a>,
    pub(super) root_matcher: &'observation RootIgnoreMatcher<'a>,
    pub(super) ignore_case: Option<bool>,
}

impl<'a> InventoryRead<'_, 'a> {
    pub(super) fn capture(
        &mut self,
        tracked: &TreeIndexWalk<'a>,
        limits: InventoryLimits,
        mut transcript: Option<&mut InventoryTranscript>,
    ) -> InventoryResult<InventoryPass> {
        if limits.entries > MAX_ENTRIES || !(1..=MAX_ENTRIES + 1).contains(&limits.directories) {
            return Err(InventoryError::Budget("inventory-limits"));
        }
        let metadata = [
            self.observe_entry(".git", true)?,
            self.observe_entry(STATE_DIR, true)?,
        ];
        let _table = reserve(self.work, limits.directories * size_of::<Directory<'a>>())?;
        let mut directories = Vec::new();
        directories
            .try_reserve_exact(limits.directories)
            .map_err(|_| InventoryError::Budget("allocation"))?;
        if directories.capacity() != limits.directories {
            return Err(InventoryError::Budget("memory-cap"));
        }
        directories.push(Directory {
            path: own(self.work, b"")?,
            parent: None,
            depth: 0,
            identity: self.root.identity(),
            pruned: false,
            rules: None,
        });

        let mut pass = InventoryPass::default();
        let mut current_path = [0; MAX_RELATIVE_BYTES];
        let mut child_path = [0; MAX_RELATIVE_BYTES];
        let mut cursor = 0;
        while cursor < directories.len() {
            self.work.charge(1).map_err(reason)?;
            if directories[cursor].pruned {
                pass.directories_pruned += 1;
            } else {
                let path = joined(
                    self.work,
                    b"",
                    directories[cursor].path.as_ref(),
                    &mut current_path,
                )?;
                self.scan_directory(
                    Visit {
                        path,
                        cursor,
                        limits,
                        tracked,
                        metadata: &metadata,
                    },
                    &mut directories,
                    &mut pass,
                    &mut child_path,
                    transcript.as_deref_mut(),
                )?;
            }
            cursor += 1;
        }
        self.confirm_directories(&directories, &mut child_path)?;
        for (name, identity) in METADATA_DIRECTORIES.into_iter().zip(metadata) {
            if self.observe_entry(name, true)? != identity {
                return Err(InventoryError::Changed);
            }
        }
        self.work.charge(0).map_err(reason)?;
        Ok(pass)
    }

    fn scan_directory(
        &mut self,
        visit: Visit<'_, 'a>,
        directories: &mut Vec<Directory<'a>>,
        pass: &mut InventoryPass,
        child_path: &mut [u8; MAX_RELATIVE_BYTES],
        mut transcript: Option<&mut InventoryTranscript>,
    ) -> InventoryResult<()> {
        let scope = &directories[visit.cursor];
        let identity = scope.identity;
        let depth = scope.depth;
        if !visit.path.is_empty() {
            if self.observe_entry(visit.path, true)? != identity {
                return Err(InventoryError::Changed);
            }
            let raw = self.read_rules(visit.path, child_path)?;
            if self.observe_entry(visit.path, true)? != identity {
                return Err(InventoryError::Changed);
            }
            if let Some((raw, rule_identity)) = raw {
                let matcher = RootIgnoreMatcher::compile(
                    [None, Some(raw.as_ref())],
                    self.ignore_case,
                    self.work,
                )
                .map_err(|error| reason(error.reason()))?;
                directories[visit.cursor].rules = Some(Rules {
                    raw,
                    matcher,
                    identity: rule_identity,
                });
            }
        }
        let remaining = visit.limits.entries - pass.entries_observed;
        let rows = DirectoryRows::capture(self.root, visit.path, remaining, self.io, self.work)
            .map_err(row_error)?;
        if rows.identity != identity {
            return Err(InventoryError::Changed);
        }
        verify_unaliased_rows(rows.rows(), self.work)?;
        if let Some(transcript) = transcript.as_deref_mut() {
            transcript.directory(visit.path.as_bytes(), identity, self.work)?;
            if !visit.path.is_empty() {
                let rules = directories[visit.cursor].rules.as_ref();
                transcript.policy(
                    visit.path.as_bytes(),
                    IGNORE_NAME,
                    rules.map(|rules| rules.raw.as_ref()),
                    rules.map(|rules| rules.identity),
                    self.work,
                )?;
            }
        }
        let mut expected_seen = 0;
        let mut reserved_seen = 0;
        for row in rows.rows() {
            self.work.charge(1).map_err(reason)?;
            let child = joined(
                self.work,
                visit.path.as_bytes(),
                row.name.as_ref(),
                child_path,
            )?;
            let is_dir = match row.kind {
                EntryKind::Directory => true,
                EntryKind::File => false,
                _ => return Err(InventoryError::Unsupported("inventory-kind")),
            };
            let identity = self.observe_entry(child, is_dir)?;
            if let Some(index) = METADATA_DIRECTORIES
                .iter()
                .position(|name| name.as_bytes().eq_ignore_ascii_case(row.name.as_ref()))
            {
                if visit.path.is_empty()
                    && is_dir
                    && child == METADATA_DIRECTORIES[index]
                    && identity == visit.metadata[index]
                {
                    reserved_seen += 1;
                    if let Some(transcript) = transcript.as_deref_mut() {
                        transcript.row(
                            row.name.as_ref(),
                            METADATA_ROLES[index],
                            identity,
                            self.work,
                        )?;
                    }
                    continue;
                }
                return Err(InventoryError::Unsupported("inventory-metadata-origin"));
            }
            let expected_entry = visit
                .tracked
                .expected_entry(child.as_bytes(), self.work)
                .map_err(reason)?;
            if let Some(expected) = expected_entry {
                if is_dir != (expected == ExpectedEntry::Directory) {
                    return Err(InventoryError::TrackedNamespace);
                }
                expected_seen += 1;
            }
            let role = if is_dir {
                if directories.len() >= visit.limits.directories || depth == MAX_DEPTH {
                    return Err(InventoryError::Budget("inventory-directory-cap"));
                }
                let pruned = self.ignored(child, true, directories, visit.cursor)?;
                directories.push(Directory {
                    path: own(self.work, child.as_bytes())?,
                    parent: Some(visit.cursor),
                    depth: depth + 1,
                    identity,
                    pruned,
                    rules: None,
                });
                match (pruned, expected_entry.is_some()) {
                    (false, true) => RowRole::TrackedDirectory,
                    (false, false) => RowRole::VisibleUntrackedDirectory,
                    (true, true) => RowRole::PrunedTrackedDirectory,
                    (true, false) => RowRole::PrunedUntrackedDirectory,
                }
            } else if expected_entry.is_some() {
                RowRole::TrackedFile
            } else if self.ignored(child, false, directories, visit.cursor)? {
                RowRole::IgnoredUntrackedFile
            } else {
                pass.untracked_files += 1;
                RowRole::VisibleUntrackedFile
            };
            if let Some(transcript) = transcript.as_deref_mut() {
                transcript.row(row.name.as_ref(), role, identity, self.work)?;
            }
        }
        let count = rows.rows().len();
        drop(rows);
        if visit.path.is_empty() && reserved_seen != METADATA_DIRECTORIES.len() {
            return Err(InventoryError::Changed);
        }
        let expected = visit
            .tracked
            .direct_child_count(visit.path.as_bytes(), self.work)
            .map_err(reason)?;
        if expected_seen != expected {
            return Err(InventoryError::TrackedNamespace);
        }
        if let Some(transcript) = transcript {
            transcript.directory_end(count, self.work)?;
        }
        pass.directories_visited += 1;
        pass.entries_observed += count;
        Ok(())
    }

    fn ignored(
        &mut self,
        path: &str,
        directory: bool,
        frames: &[Directory<'a>],
        mut current: usize,
    ) -> InventoryResult<bool> {
        while let Some(parent) = frames[current].parent {
            self.work.charge(1).map_err(reason)?;
            let frame = &frames[current];
            if let Some(policy) = &frame.rules {
                let suffix = &path.as_bytes()[frame.path.as_ref().len() + 1..];
                if let Some(ignored) = policy
                    .matcher
                    .leaf(suffix, directory, self.work)
                    .map_err(|error| reason(error.reason()))?
                {
                    return Ok(ignored);
                }
            }
            current = parent;
        }
        self.root_matcher
            .leaf(path.as_bytes(), directory, self.work)
            .map(|decision| decision == Some(true))
            .map_err(|error| reason(error.reason()))
    }

    fn observe_entry(&mut self, path: &str, directory: bool) -> InventoryResult<Identity> {
        let _lookup = self.work.reserve_lookup(path.len()).map_err(reason)?;
        self.root
            .observe_entry(path, directory, self.io)
            .map_err(|error| reason(read_error(error)))
    }

    fn read_rules(
        &mut self,
        directory: &str,
        path_store: &mut [u8; MAX_RELATIVE_BYTES],
    ) -> InventoryResult<Option<(ReservedBytes<'a>, Identity)>> {
        let path = joined(self.work, directory.as_bytes(), IGNORE_NAME, path_store)?;
        let raw = {
            let _lookup = self.work.reserve_lookup(path.len()).map_err(reason)?;
            optional(self.root, path, MAX_IGNORE_FILE_BYTES, self.io, self.work).map_err(reason)?
        };
        raw.map(|raw| {
            self.observe_entry(path, false)
                .map(|identity| (raw, identity))
        })
        .transpose()
    }

    fn confirm_directories(
        &mut self,
        directories: &[Directory<'a>],
        path_store: &mut [u8; MAX_RELATIVE_BYTES],
    ) -> InventoryResult<()> {
        for frame in &directories[1..] {
            let path =
                std::str::from_utf8(frame.path.as_ref()).map_err(|_| reason("inventory-path"))?;
            self.work.charge(path.len()).map_err(reason)?;
            if self.observe_entry(path, true)? != frame.identity {
                return Err(InventoryError::Changed);
            }
            if !frame.pruned {
                let current = self.read_rules(path, path_store)?;
                let original = frame
                    .rules
                    .as_ref()
                    .map(|rules| (rules.raw.as_ref(), rules.identity));
                self.work
                    .charge(original.map_or(0, |(raw, _)| raw.len()))
                    .map_err(reason)?;
                if current
                    .as_ref()
                    .map(|(raw, identity)| (raw.as_ref(), *identity))
                    != original
                    || self.observe_entry(path, true)? != frame.identity
                {
                    return Err(InventoryError::Changed);
                }
            }
        }
        Ok(())
    }
}

struct Visit<'a, 'work> {
    path: &'a str,
    cursor: usize,
    limits: InventoryLimits,
    tracked: &'a TreeIndexWalk<'work>,
    metadata: &'a [Identity; 2],
}

fn joined<'buffer>(
    work: &mut Work<'_>,
    parent: &[u8],
    name: &[u8],
    buffer: &'buffer mut [u8; MAX_RELATIVE_BYTES],
) -> InventoryResult<&'buffer str> {
    let prefix = parent.len() + usize::from(!parent.is_empty());
    let end = prefix + name.len();
    if end > buffer.len() {
        return Err(InventoryError::Budget("inventory-path-cap"));
    }
    work.charge(2 * end).map_err(reason)?;
    if !parent.is_empty() {
        buffer[..parent.len()].copy_from_slice(parent);
        buffer[parent.len()] = b'/';
    }
    buffer[prefix..end].copy_from_slice(name);
    std::str::from_utf8(&buffer[..end]).map_err(|_| reason("inventory-path"))
}

pub(super) fn verify_unaliased_rows(
    rows: &[OwnedDirectoryRow<'_>],
    work: &mut Work<'_>,
) -> InventoryResult<()> {
    let _table = reserve(work, rows.len() * size_of::<&[u8]>())?;
    let mut ordered: Vec<&[u8]> = Vec::new();
    ordered
        .try_reserve_exact(rows.len())
        .map_err(|_| reason("allocation"))?;
    if ordered.capacity() != rows.len() {
        return Err(reason("memory-cap"));
    }
    for row in rows {
        let name = row.name.as_ref();
        let comparisons = (usize::BITS - ordered.len().leading_zeros()) as usize + 1;
        work.charge(comparisons * (name.len() + 1))
            .map_err(reason)?;
        let index = ordered
            .binary_search_by(|other| folded_cmp(other, name))
            .err()
            .ok_or(InventoryError::Unsupported("inventory-case-alias"))?;
        work.charge((ordered.len() - index) * size_of::<&[u8]>())
            .map_err(reason)?;
        ordered.insert(index, name);
    }
    Ok(())
}

fn own<'a>(work: &mut Work<'a>, path: &[u8]) -> InventoryResult<ReservedBytes<'a>> {
    work.charge(path.len()).map_err(reason)?;
    let reservation = reserve(work, path.len())?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(path.len())
        .map_err(|_| reason("allocation"))?;
    if bytes.capacity() != path.len() {
        return Err(reason("memory-cap"));
    }
    bytes.extend_from_slice(path);
    reservation
        .into_bytes(bytes)
        .map_err(|_| reason("memory-cap"))
}

fn reserve<'a>(work: &mut Work<'a>, bytes: usize) -> InventoryResult<Reservation<'a>> {
    if work.read_limit(bytes, 0).map_err(reason)? != bytes {
        return Err(reason("memory-cap"));
    }
    work.memory_budget()
        .reserve(bytes)
        .map_err(|_| reason("memory-cap"))
}

fn row_error(error: DirectoryVisitError<&'static str>) -> InventoryError {
    match error {
        DirectoryVisitError::Reader(ReadError::Entries) => {
            InventoryError::Budget("inventory-entry-cap")
        }
        DirectoryVisitError::Reader(error) => reason(read_error(error)),
        DirectoryVisitError::Visitor(error) => reason(error),
    }
}

pub(super) fn reason(reason: &'static str) -> InventoryError {
    match reason {
        "input-changed" | "root-ignore-changed" | "git-config-changed" | "staged-index-changed" => {
            InventoryError::Changed
        }
        "work-cap"
        | "memory-cap"
        | "aggregate-cap"
        | "allocation"
        | "deadline"
        | "stored-bytes-cap"
        | "ignore-input-cap"
        | "ignore-rule-cap"
        | "ignore-rule-byte-cap"
        | "ignore-candidate-cap" => InventoryError::Budget(reason),
        "staged-tree-mismatch" => InventoryError::TrackedNamespace,
        _ => InventoryError::Unsupported(reason),
    }
}
