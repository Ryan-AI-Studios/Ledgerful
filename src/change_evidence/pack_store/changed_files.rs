//! Exact path changes between two admitted immutable Git trees.
use super::super::tree_index::{
    TreeFile, TreeIndexWalk, TreePaths, copy_path, diff_path_cmp, is_evidence_path,
};
use super::{MAX_TRACKED_BLOB_BYTES, Observation, PackResult, Reservation, ReservedBytes, Work};
use gix::{ObjectId, objs::Kind};
use std::cmp::Ordering;

#[cfg(all(test, any(windows, target_os = "linux")))]
mod tests;

const MAX_ROWS: usize = 100;
const MAX_PATH_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct ChangedFileLimits {
    pub(crate) rows: usize,
    pub(crate) path_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChangedFileKind {
    Added,
    Deleted,
    Modified,
    Rewrite,
}

pub(crate) struct ChangedFile<'a> {
    pub(crate) kind: ChangedFileKind,
    path: ReservedBytes<'a>,
    old_path: Option<ReservedBytes<'a>>,
    pub(crate) additions: Option<u64>,
    pub(crate) deletions: Option<u64>,
    pub(crate) is_binary: Option<bool>,
}

impl ChangedFile<'_> {
    pub(crate) fn path(&self) -> &str {
        admitted_path(&self.path)
    }

    pub(crate) fn old_path(&self) -> Option<&str> {
        self.old_path.as_ref().map(admitted_path)
    }
}

fn admitted_path<'a>(path: &'a ReservedBytes<'_>) -> &'a str {
    std::str::from_utf8(path.as_ref()).expect("only admitted paths are projected")
}

fn projectable_path(path: &[u8]) -> bool {
    std::str::from_utf8(path).is_ok_and(is_evidence_path)
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ChangedFileCompleteness {
    Complete,
    Partial {
        projection_limited: bool,
        paths_omitted: bool,
        rewrites_unresolved: bool,
    },
}

pub(crate) struct ChangedFiles<'a> {
    files: Vec<ChangedFile<'a>>,
    pub(crate) completeness: ChangedFileCompleteness,
    pub(crate) total: Option<usize>,
    pub(crate) omitted: Option<usize>,
    pub(super) head_has_unsupported_checkout_path: bool,
    _table: Reservation<'a>,
}

impl<'a> ChangedFiles<'a> {
    pub(crate) fn files(&self) -> &[ChangedFile<'a>] {
        &self.files
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChangedFileInput {
    Revision,
    ParentTree,
    HeadTree,
    ParentFile,
    HeadFile,
    PackInventory,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ChangedFilesError {
    Budget(&'static str),
    Withheld(ChangedFileInput, &'static str),
}

type DiffResult<T> = Result<T, ChangedFilesError>;

#[derive(Clone, Copy)]
enum Change<'tree> {
    Added(TreeFile<'tree>),
    Deleted(TreeFile<'tree>),
    Modified(TreeFile<'tree>, TreeFile<'tree>),
    Rewrite(TreeFile<'tree>, TreeFile<'tree>),
}

impl<'a> Observation<'a> {
    pub(crate) fn observe_changed_files(
        &mut self,
        head: ObjectId,
        parent: ObjectId,
        limits: ChangedFileLimits,
    ) -> DiffResult<ChangedFiles<'a>> {
        let pair = checked(
            ChangedFileInput::Revision,
            self.observe_commit_pair(head, parent),
        )?;
        let old = checked(
            ChangedFileInput::ParentTree,
            self.walk_verified_tree(pair.parent_tree, TreePaths::ChangedFiles),
        )?;
        let new = checked(
            ChangedFileInput::HeadTree,
            self.walk_verified_tree(pair.head_tree, TreePaths::ChangedFiles),
        )?;
        let (mut changes, _table) =
            work_result(reserve_table(old.files() + new.files(), &mut self.work))?;
        let rewrites_unresolved = work_result(collect(&old, &new, &mut changes, &mut self.work))?;
        work_result(self.work.charge(changes.len()))?;
        for change in changes.iter().flatten() {
            self.verify_changed_objects(*change)?;
        }
        checked(
            ChangedFileInput::PackInventory,
            self.confirm_pack_inventory(),
        )?;
        work_result(project(
            &changes,
            rewrites_unresolved,
            new.has_unsupported_checkout_path(),
            limits,
            &mut self.work,
        ))
    }

    fn verify_changed_objects(&mut self, change: Change<'_>) -> DiffResult<()> {
        if let Change::Deleted(old) | Change::Modified(old, _) | Change::Rewrite(old, _) = change {
            checked(
                ChangedFileInput::ParentFile,
                self.object(old.id, Kind::Blob, MAX_TRACKED_BLOB_BYTES),
            )?;
        }
        match change {
            Change::Added(new) => {
                checked(
                    ChangedFileInput::HeadFile,
                    self.object(new.id, Kind::Blob, MAX_TRACKED_BLOB_BYTES),
                )?;
            }
            Change::Modified(old, new) if old.id != new.id => {
                checked(
                    ChangedFileInput::HeadFile,
                    self.object(new.id, Kind::Blob, MAX_TRACKED_BLOB_BYTES),
                )?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn checked<T>(input: ChangedFileInput, result: PackResult<T>) -> DiffResult<T> {
    result.map_err(|reason| match reason {
        "deadline"
        | "work-cap"
        | "memory-cap"
        | "aggregate-cap"
        | "pack-read-cap"
        | "allocation"
        | "allocation-cap"
        | "tree-walk-entry-cap"
        | "tree-depth-cap"
        | "tree-path-cap"
        | "tree-byte-cap"
        | "tree-entry-cap"
        | "declared-size-cap"
        | "stored-bytes-cap"
        | "pack-cap"
        | "index-entry-cap" => ChangedFilesError::Budget(reason),
        _ => ChangedFilesError::Withheld(input, reason),
    })
}

fn work_result<T>(result: PackResult<T>) -> DiffResult<T> {
    result.map_err(ChangedFilesError::Budget)
}

fn reserve_table<'a, T>(
    count: usize,
    work: &mut Work<'a>,
) -> PackResult<(Vec<T>, Reservation<'a>)> {
    let size = count * size_of::<T>();
    work.charge(size)?;
    if work.read_limit(size, 0)? != size {
        return Err("memory-cap");
    }
    let held = work
        .memory_budget()
        .reserve(size)
        .map_err(|_| "memory-cap")?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(count).map_err(|_| "allocation")?;
    if rows.capacity() != count {
        return Err("memory-cap");
    }
    Ok((rows, held))
}

fn collect<'a, 'tree>(
    old: &'tree TreeIndexWalk<'a>,
    new: &'tree TreeIndexWalk<'a>,
    changes: &mut Vec<Option<Change<'tree>>>,
    work: &mut Work<'a>,
) -> PackResult<bool> {
    let mut left = old.files_in_path_order(work)?.peekable();
    let mut right = new.files_in_path_order(work)?.peekable();
    loop {
        work.charge(1)?;
        let order = match (left.peek(), right.peek()) {
            (Some(a), Some(b)) => {
                work.charge(2 * (a.path.len().min(b.path.len()) + 1))?;
                diff_path_cmp(a.path, b.path)
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => break,
        };
        let change = match order {
            Ordering::Less => left.next().map(Change::Deleted),
            Ordering::Greater => right.next().map(Change::Added),
            Ordering::Equal => left.next().zip(right.next()).and_then(|(a, b)| {
                (a.id != b.id || a.executable != b.executable).then_some(Change::Modified(a, b))
            }),
        };
        if let Some(change) = change {
            changes.push(Some(change));
        }
    }
    match_exact_rewrites(old, new, changes, work)?;
    work.charge(2 * changes.len())?;
    Ok(changes
        .iter()
        .any(|change| matches!(change, Some(Change::Added(_))))
        && changes
            .iter()
            .any(|change| matches!(change, Some(Change::Deleted(_)))))
}

fn match_exact_rewrites<'a, 'tree>(
    old: &TreeIndexWalk<'a>,
    new: &TreeIndexWalk<'a>,
    changes: &mut [Option<Change<'tree>>],
    work: &mut Work<'a>,
) -> PackResult<()> {
    work.charge(changes.len())?;
    if !changes
        .iter()
        .any(|change| matches!(change, Some(Change::Deleted(_))))
    {
        return Ok(());
    }
    for index in 0..changes.len() {
        work.charge(1)?;
        let Some(Change::Added(destination)) = changes[index] else {
            continue;
        };
        if destination.id == ObjectId::empty_blob(destination.id.kind()) {
            continue;
        }
        work.charge(changes.len() * (size_of::<ObjectId>() + 1))?;
        let Some(source_index) = changes.iter().position(|change| {
            matches!(change, Some(Change::Deleted(source)) if source.id == destination.id && source.executable == destination.executable)
        }) else {
            continue;
        };
        if !unique_id(old, destination.id, work)? || !unique_id(new, destination.id, work)? {
            continue;
        }
        if let Some(Change::Deleted(source)) = changes[source_index].take() {
            changes[index] = Some(Change::Rewrite(source, destination));
        }
    }
    Ok(())
}

fn unique_id(walk: &TreeIndexWalk<'_>, id: ObjectId, work: &mut Work<'_>) -> PackResult<bool> {
    work.charge(walk.directory_count() + walk.files() * (size_of::<ObjectId>() + 1))?;
    let count = walk
        .tracked_files()
        .filter(|(_, candidate, _)| *candidate == id)
        .take(2)
        .count();
    Ok(count == 1)
}

fn project<'a>(
    changes: &[Option<Change<'_>>],
    rewrites_unresolved: bool,
    head_has_unsupported_checkout_path: bool,
    limits: ChangedFileLimits,
    work: &mut Work<'a>,
) -> PackResult<ChangedFiles<'a>> {
    work.charge(2 * changes.len())?;
    let count = changes.iter().flatten().count();
    let row_limit = limits.rows.min(MAX_ROWS).min(count);
    let mut path_bytes = limits.path_bytes.min(MAX_PATH_BYTES);
    let (mut files, table) = reserve_table(row_limit, work)?;
    let mut projection_limited = false;
    let mut paths_omitted = false;
    for change in changes.iter().flatten() {
        work.charge(1)?;
        let (kind, file, prior) = match *change {
            Change::Added(file) => (ChangedFileKind::Added, file, None),
            Change::Deleted(file) => (ChangedFileKind::Deleted, file, None),
            Change::Modified(source, file) => (ChangedFileKind::Modified, file, Some(source)),
            Change::Rewrite(source, file) => (ChangedFileKind::Rewrite, file, Some(source)),
        };
        let path = file.path;
        let old = prior
            .filter(|_| kind == ChangedFileKind::Rewrite)
            .map(|f| f.path);
        let bytes = path.len() + old.map_or(0, <[u8]>::len);
        work.charge(bytes)?;
        if file.checkout_ambiguous
            || prior.is_some_and(|f| f.checkout_ambiguous)
            || !projectable_path(path)
            || old.is_some_and(|path| !projectable_path(path))
        {
            paths_omitted = true;
            continue;
        }
        if projection_limited || files.len() == row_limit {
            projection_limited = true;
            continue;
        }
        let Some(remaining) = path_bytes.checked_sub(bytes) else {
            projection_limited = true;
            continue;
        };
        path_bytes = remaining;
        files.push(ChangedFile {
            kind,
            path: copy_path(&[], path, work)?,
            old_path: old.map(|path| copy_path(&[], path, work)).transpose()?,
            additions: None,
            deletions: None,
            is_binary: None,
        });
    }
    let completeness = if projection_limited || paths_omitted || rewrites_unresolved {
        ChangedFileCompleteness::Partial {
            projection_limited,
            paths_omitted,
            rewrites_unresolved,
        }
    } else {
        ChangedFileCompleteness::Complete
    };
    let (total, omitted) = if rewrites_unresolved {
        (None, None)
    } else {
        (Some(count), Some(count - files.len()))
    };
    work.charge(0)?;
    Ok(ChangedFiles {
        files,
        completeness,
        total,
        omitted,
        head_has_unsupported_checkout_path,
        _table: table,
    })
}
