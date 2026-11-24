//! Bounded retained discovery over the exact default-ignore absence subset.
use super::baseline_discovery_policy::{
    DiscoveryEntry, MembershipError, RuleFrame, classify_selected, match_paths,
};
use super::buffer::{Reservation, ReservedBytes};
use super::config::decode_index_stale_threshold_days;
use super::contained::{
    DefaultAncestorAbsence, DirectoryVisitError, EntryKind, Identity, MAX_RELATIVE_BYTES,
    ReadBudget, ReadError,
};
use super::directory_rows::DirectoryRows;
use super::git::read_error;
use super::inventory::{InventoryError, verify_unaliased_rows};
use super::pack::Work;
use super::root_ignore::MAX_IGNORE_FILE_BYTES;
use super::scope_owner::ScopeRoot;
use super::witness_stream::WitnessStream;
use crate::commands::baseline::extract::capture_snapshot::CaptureSnapshot;
use std::mem::size_of;

type Result<T> = std::result::Result<T, &'static str>;

const MAX_ENTRIES: usize = 4_096;
const MAX_DIR_ROWS: usize = 512;
const MAX_DEPTH: usize = 32;
const CONFIG_BYTES: usize = 16 * 1024;
const CONFIG_LINE_BYTES: usize = 1_024;
const CONFIG_LINES: usize = 384;
const EXCLUDE_PATH: &str = ".git/info/exclude";
const CONFIG_PATH: &str = ".ledgerful/config.toml";

/// A finished physical discovery pass and exact SQL path-set match.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct RetainedDiscovery {
    pub(super) membership: RetainedMembership,
    pub(super) stale_threshold_days: u64,
    pub(super) config_digest: blake3::Hash,
    pub(super) witness: [u8; 32],
    ancestor: DefaultAncestorAbsence,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum RetainedMembership {
    Exact(usize),
    UndiscoveredIndexRow,
    UnindexedDiscoveredPath,
}

impl RetainedMembership {
    pub(super) fn require_exact(&self) -> Result<usize> {
        match self {
            Self::Exact(count) => Ok(*count),
            Self::UndiscoveredIndexRow => Err("discovery-index-row-not-found"),
            Self::UnindexedDiscoveredPath => Err("discovery-unindexed-path"),
        }
    }
}

struct ScopePolicy<'a, 'dir> {
    dot_ignore: RuleFrame<'a, 'dir>,
    gitignore: RuleFrame<'a, 'dir>,
    parent: Option<&'dir ScopePolicy<'a, 'dir>>,
}

impl<'a> ScopePolicy<'a, '_> {
    fn decide(&self, path: &str, directory: bool, work: &mut Work<'a>) -> Result<Option<bool>> {
        let bytes = path.as_bytes();
        // .ignore is a separate higher-priority lane across all applicable frames.
        for use_dot_ignore in [true, false] {
            let mut current = Some(self);
            while let Some(scope) = current {
                let rule = if use_dot_ignore {
                    &scope.dot_ignore
                } else {
                    &scope.gitignore
                };
                if let Some(result) = rule.decide(bytes, directory, work).map_err(membership)? {
                    return Ok(Some(result));
                }
                current = scope.parent;
            }
        }
        Ok(None)
    }
}

struct SelectedPaths<'a> {
    paths: Vec<ReservedBytes<'a>>,
    _table: Reservation<'a>,
}

struct DiscoverySample<'a> {
    selected: Option<SelectedPaths<'a>>,
    stale_threshold_days: u64,
    config_digest: blake3::Hash,
    witness: [u8; 32],
    ancestor: DefaultAncestorAbsence,
}

struct DiscoveryWalk<'a, 'observation> {
    root: &'observation mut ScopeRoot<'a>,
    io: &'observation mut ReadBudget<'a>,
    work: &'observation mut Work<'a>,
    remaining: usize,
    selected: Option<SelectedPaths<'a>>,
    transcript: WitnessStream,
    root_exclude: RuleFrame<'a, 'static>,
}

impl<'a, 'observation> DiscoveryWalk<'a, 'observation> {
    fn walk(
        &mut self,
        dir: &str,
        expected_directory: Identity,
        parent: Option<&ScopePolicy<'a, '_>>,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err("discovery-directory-depth-cap");
        }
        let rows = self.rows(dir, self.remaining.min(MAX_DIR_ROWS))?;
        verify_unaliased_rows(rows.rows(), self.work).map_err(|error| match error {
            InventoryError::Budget(reason) => reason,
            _ => "discovery-alias",
        })?;
        if rows.identity != expected_directory {
            return Err("discovery-directory-changed");
        }
        self.remaining -= rows.rows().len();
        let ignore = self.local_rules(dir, &rows, ".ignore")?;
        let gitignore = self.local_rules(dir, &rows, ".gitignore")?;
        self.transcript.record(
            1,
            &[dir.as_bytes(), &expected_directory.inventory_bytes()],
            self.work,
        )?;
        self.record_rules(2, ignore.as_ref())?;
        self.record_rules(3, gitignore.as_ref())?;
        let scope = ScopePolicy {
            dot_ignore: RuleFrame::compile(dir.as_bytes(), raw(&ignore), self.work)
                .map_err(membership)?,
            gitignore: RuleFrame::compile(dir.as_bytes(), raw(&gitignore), self.work)
                .map_err(membership)?,
            parent,
        };
        let mut reserved_seen = 0;
        for row in rows.rows() {
            let kind = match row.kind {
                EntryKind::Directory => true,
                EntryKind::File => false,
                _ => return Err("discovery-unsupported-entry-kind"),
            };
            let filename = std::str::from_utf8(row.name.as_ref()).map_err(|_| "discovery-path")?;
            let mut storage = [0_u8; MAX_RELATIVE_BYTES];
            let path = join(dir, filename, &mut storage, self.work)?;
            let selected = match scope.decide(path, kind, self.work)? {
                Some(value) => Some(value),
                None => self
                    .root_exclude
                    .decide(path.as_bytes(), kind, self.work)
                    .map_err(membership)?,
            };
            let role = classify_selected(path, kind, row.windows_hidden, selected, self.work)
                .map_err(membership)?;
            let special = [".git", ".ledgerful", ".jj"]
                .into_iter()
                .find(|name| filename.eq_ignore_ascii_case(name));
            if let Some(special) = special
                && (special == ".jj"
                    || !dir.is_empty()
                    || filename != special
                    || !kind
                    || role != DiscoveryEntry::Prune)
            {
                return Err("discovery-metadata-or-nested-repo");
            }
            if special.is_some() {
                reserved_seen += 1;
            }
            let _lookup = self.work.reserve_lookup(path.len())?;
            let observed = self
                .root
                .observe_entry(path, kind, self.io)
                .map_err(native)?;
            let observation = [u8::from(kind), u8::from(row.windows_hidden), role_tag(role)];
            self.transcript.record(
                4,
                &[path.as_bytes(), &observed.inventory_bytes(), &observation],
                self.work,
            )?;
            match role {
                DiscoveryEntry::Directory => self.walk(path, observed, Some(&scope), depth + 1)?,
                DiscoveryEntry::CodeFile => self.add_path(path)?,
                DiscoveryEntry::Prune | DiscoveryEntry::OtherFile => {}
            }
        }
        if dir.is_empty() && reserved_seen != 2 {
            return Err("discovery-metadata-or-nested-repo");
        }
        let close = self.rows(dir, rows.rows().len())?;
        if !same_rows(&rows, &close, self.work)?
            || !same_optional(
                &ignore,
                &self.local_rules(dir, &close, ".ignore")?,
                self.work,
            )?
            || !same_optional(
                &gitignore,
                &self.local_rules(dir, &close, ".gitignore")?,
                self.work,
            )?
        {
            return Err("discovery-directory-changed");
        }
        self.transcript.record(5, &[dir.as_bytes()], self.work)
    }

    fn rows(&mut self, dir: &str, limit: usize) -> Result<DirectoryRows<'a>> {
        DirectoryRows::capture(self.root, dir, limit, self.io, self.work).map_err(|error| {
            match error {
                DirectoryVisitError::Reader(error) => native(error),
                DirectoryVisitError::Visitor(error) => error,
            }
        })
    }

    fn local_rules(
        &mut self,
        dir: &str,
        rows: &DirectoryRows<'a>,
        filename: &str,
    ) -> Result<Option<ReservedBytes<'a>>> {
        for row in rows.rows() {
            self.work.charge(row.name.as_ref().len() + 1)?;
            if row.name.as_ref().eq_ignore_ascii_case(filename.as_bytes()) {
                if row.name.as_ref() != filename.as_bytes() || row.kind != EntryKind::File {
                    return Err("discovery-policy-path-unqualified");
                }
                let mut storage = [0_u8; MAX_RELATIVE_BYTES];
                let path = join(dir, filename, &mut storage, self.work)?;
                return read(self.root, self.io, self.work, path, MAX_IGNORE_FILE_BYTES).map(Some);
            }
        }
        // Absence comes from the complete bounded native directory sample.
        Ok(None)
    }

    fn record_rules(&mut self, tag: u8, policy: Option<&ReservedBytes<'a>>) -> Result<()> {
        self.transcript.record(
            tag,
            &[
                &[u8::from(policy.is_some())],
                policy.map_or(&[], AsRef::as_ref),
            ],
            self.work,
        )
    }

    fn add_path(&mut self, path: &str) -> Result<()> {
        let Some(selection) = &mut self.selected else {
            return Ok(());
        };
        let insertion = search(
            &selection.paths,
            path.as_bytes(),
            |path| path.as_ref(),
            self.work,
        )?;
        let Err(slot) = insertion else {
            return Err("discovery-alias");
        };
        if selection.paths.len() == MAX_ENTRIES {
            return Err("discovery-entry-cap");
        }
        self.work
            .charge(path.len() + (selection.paths.len() - slot) * size_of::<ReservedBytes<'_>>())?;
        let owned = reserve(self.work, path.len())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(path.len())
            .map_err(|_| "allocation")?;
        if bytes.capacity() != path.len() {
            return Err("memory-cap");
        }
        bytes.extend_from_slice(path.as_bytes());
        let retained = owned.into_bytes(bytes).map_err(|_| "memory-cap")?;
        selection.paths.insert(slot, retained);
        Ok(())
    }
}

pub(super) fn observe<'a>(
    root: &mut ScopeRoot<'a>,
    io: &mut ReadBudget<'a>,
    work: &mut Work<'a>,
    #[cfg(target_os = "linux")] layout_root: &std::path::Path,
    snapshot: &CaptureSnapshot,
) -> Result<RetainedDiscovery> {
    let sampled = sample(
        root,
        io,
        work,
        #[cfg(target_os = "linux")]
        layout_root,
        true,
    )?;
    let wanted = sorted_sql_paths(snapshot, work)?;
    let selected = sampled
        .selected
        .as_ref()
        .expect("capture requested exact paths");
    let membership = match match_paths(
        selected
            .paths
            .iter()
            .map(|path| std::str::from_utf8(path.as_ref()).expect("admitted UTF-8 path")),
        wanted.0.iter().copied(),
        work,
    ) {
        Ok(files) => RetainedMembership::Exact(files),
        Err(MembershipError::UndiscoveredIndexRow) => RetainedMembership::UndiscoveredIndexRow,
        Err(MembershipError::UnindexedDiscoveredPath) => {
            RetainedMembership::UnindexedDiscoveredPath
        }
        Err(error) => return Err(membership(error)),
    };
    Ok(RetainedDiscovery {
        membership,
        stale_threshold_days: sampled.stale_threshold_days,
        config_digest: sampled.config_digest,
        witness: sampled.witness,
        ancestor: sampled.ancestor,
    })
}

/// Reproduce the same admitted native discovery transcript without SQL or owned path copies.
pub(super) fn replay_fingerprint<'a>(
    root: &mut ScopeRoot<'a>,
    io: &mut ReadBudget<'a>,
    work: &mut Work<'a>,
    #[cfg(target_os = "linux")] layout_root: &std::path::Path,
) -> Result<[u8; 32]> {
    let sampled = sample(
        root,
        io,
        work,
        #[cfg(target_os = "linux")]
        layout_root,
        false,
    )?;
    Ok(sampled.witness)
}

fn sample<'a>(
    root: &mut ScopeRoot<'a>,
    io: &mut ReadBudget<'a>,
    work: &mut Work<'a>,
    #[cfg(target_os = "linux")] layout_root: &std::path::Path,
    collect_selected: bool,
) -> Result<DiscoverySample<'a>> {
    let env = root.observe_default_policy_environment(
        #[cfg(target_os = "linux")]
        layout_root,
        io,
        work,
    )?;
    let stamp = root.default_exclude_presence(io, work).map_err(native)?;
    let exclude = if stamp.0 {
        Some(read(root, io, work, EXCLUDE_PATH, MAX_IGNORE_FILE_BYTES)?)
    } else {
        None
    };
    let (config, threshold, config_digest) = read_project_config(root, io, work)?;
    let root_identity = root.identity();
    let mut transcript = WitnessStream::new(b"ledgerful:retained-default-discovery:v1\0", work)?;
    transcript.record(
        10,
        &[&stamp.1, &env.1, config.as_ref(), &threshold.to_be_bytes()],
        work,
    )?;
    let selected = if collect_selected {
        let table = reserve(work, MAX_ENTRIES * size_of::<ReservedBytes<'a>>())?;
        Some(SelectedPaths {
            paths: allocate(MAX_ENTRIES, work)?,
            _table: table,
        })
    } else {
        None
    };
    let mut walk = DiscoveryWalk {
        root,
        io,
        remaining: MAX_ENTRIES,
        selected,
        transcript,
        root_exclude: RuleFrame::compile(b"", raw(&exclude), work).map_err(membership)?,
        work,
    };
    walk.record_rules(11, exclude.as_ref())?;
    walk.walk("", root_identity, None, 0)?;
    if walk
        .root
        .default_exclude_presence(walk.io, walk.work)
        .map_err(native)?
        != stamp
        || !same_optional(
            &exclude,
            &if stamp.0 {
                Some(read(
                    walk.root,
                    walk.io,
                    walk.work,
                    EXCLUDE_PATH,
                    MAX_IGNORE_FILE_BYTES,
                )?)
            } else {
                None
            },
            walk.work,
        )?
    {
        return Err("discovery-policy-changed");
    }
    let after = read(walk.root, walk.io, walk.work, CONFIG_PATH, CONFIG_BYTES)?;
    walk.work.charge(config.as_ref().len())?;
    if after.as_ref() != config.as_ref()
        || walk.root.observe_default_policy_environment(
            #[cfg(target_os = "linux")]
            layout_root,
            walk.io,
            walk.work,
        )? != env
    {
        return Err("discovery-policy-changed");
    }
    let witness = walk.transcript.finish(walk.work)?;
    Ok(DiscoverySample {
        selected: walk.selected,
        stale_threshold_days: threshold,
        config_digest,
        witness,
        ancestor: env.0,
    })
}

pub(super) fn read_project_config_days<'a>(
    root: &ScopeRoot<'a>,
    io: &mut ReadBudget<'a>,
    work: &mut Work<'a>,
) -> Result<u64> {
    let bytes = read(root, io, work, CONFIG_PATH, CONFIG_BYTES)?;
    supported_config(bytes.as_ref(), work)
}

pub(super) fn read_project_config<'a>(
    root: &ScopeRoot<'a>,
    io: &mut ReadBudget<'a>,
    work: &mut Work<'a>,
) -> Result<(ReservedBytes<'a>, u64, blake3::Hash)> {
    let config = read(root, io, work, CONFIG_PATH, CONFIG_BYTES)?;
    let threshold = supported_config(config.as_ref(), work)?;
    work.charge(config.as_ref().len())?;
    let digest = blake3::hash(config.as_ref());
    Ok((config, threshold, digest))
}

pub(super) fn confirm_project_config<'a>(
    root: &ScopeRoot<'a>,
    io: &mut ReadBudget<'a>,
    work: &mut Work<'a>,
    expected: blake3::Hash,
) -> Result<()> {
    let bytes = read(root, io, work, CONFIG_PATH, CONFIG_BYTES)?;
    work.charge(bytes.as_ref().len())?;
    if blake3::hash(bytes.as_ref()) != expected {
        return Err("discovery-policy-changed");
    }
    work.charge(0)
}

fn sorted_sql_paths<'a, 'sql>(
    snapshot: &'sql CaptureSnapshot,
    work: &mut Work<'a>,
) -> Result<(Vec<&'sql str>, Reservation<'a>)> {
    let witness = snapshot.witnesses();
    let count = witness.len();
    if count > MAX_ENTRIES {
        return Err("discovery-entry-cap");
    }
    let held = reserve(work, count * size_of::<&str>())?;
    let mut paths = allocate(count, work)?;
    for witness in witness {
        let Err(slot) = search(
            &paths,
            witness.path.as_bytes(),
            |path: &&str| path.as_bytes(),
            work,
        )?
        else {
            return Err("discovery-index-alias");
        };
        work.charge((paths.len() - slot) * size_of::<&str>())?;
        paths.insert(slot, witness.path);
    }
    Ok((paths, held))
}

fn supported_config(bytes: &[u8], work: &mut Work<'_>) -> Result<u64> {
    work.charge(64 * bytes.len() + 1)?;
    let text = std::str::from_utf8(bytes).map_err(|_| "discovery-config-unavailable")?;
    // Bound the input to ordinary line-oriented TOML before handing ownership,
    // default construction and validation to the same loader as legacy.
    let mut arrays = 0_usize;
    for (index, line) in text.lines().enumerate() {
        if index >= CONFIG_LINES || line.len() > CONFIG_LINE_BYTES {
            return Err("discovery-config-shape");
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.contains("\"\"\"") || line.contains("'''") {
            return Err("discovery-config-shape");
        }
        let mut quoted = None;
        let mut escape = false;
        let mut dotted = 0;
        for byte in line.bytes() {
            if escape {
                escape = false;
                continue;
            }
            if let Some(mark) = quoted {
                if byte == mark {
                    quoted = None;
                } else if byte == b'\\' && mark == b'"' {
                    escape = true;
                }
                continue;
            }
            match byte {
                b'#' => break,
                b'"' | b'\'' => quoted = Some(byte),
                b'{' | b'}' => return Err("discovery-config-shape"),
                b'[' => {
                    arrays += 1;
                    if arrays > 2 {
                        return Err("discovery-config-shape");
                    }
                }
                b']' => arrays = arrays.checked_sub(1).ok_or("discovery-config-shape")?,
                b'.' => {
                    dotted += 1;
                    if dotted > 8 {
                        return Err("discovery-config-shape");
                    }
                }
                _ => {}
            }
        }
        if quoted.is_some() {
            return Err("discovery-config-shape");
        }
    }
    if arrays != 0 {
        return Err("discovery-config-shape");
    }
    let _parser = reserve(work, 512 * 1024 + 128 * bytes.len())?;
    decode_index_stale_threshold_days(bytes, CONFIG_BYTES)
        .map_err(|_| "discovery-config-unavailable")
}

fn raw<'a>(value: &'a Option<ReservedBytes<'_>>) -> Option<&'a [u8]> {
    value.as_ref().map(AsRef::as_ref)
}

fn same_optional(
    first: &Option<ReservedBytes<'_>>,
    second: &Option<ReservedBytes<'_>>,
    work: &mut Work<'_>,
) -> Result<bool> {
    work.charge(raw(first).map_or(0, <[u8]>::len) + 1)?;
    Ok(raw(first) == raw(second))
}

fn same_rows(
    first: &DirectoryRows<'_>,
    second: &DirectoryRows<'_>,
    work: &mut Work<'_>,
) -> Result<bool> {
    if first.identity != second.identity || first.rows().len() != second.rows().len() {
        return Ok(false);
    }
    for (a, b) in first.rows().iter().zip(second.rows()) {
        work.charge(a.name.as_ref().len() + 1)?;
        if a.name.as_ref() != b.name.as_ref()
            || a.kind != b.kind
            || a.windows_hidden != b.windows_hidden
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read<'a>(
    root: &ScopeRoot<'a>,
    io: &mut ReadBudget<'a>,
    work: &mut Work<'a>,
    path: &str,
    limit: usize,
) -> Result<ReservedBytes<'a>> {
    let _lookup = work.reserve_lookup(path.len())?;
    let cap = work.read_limit(limit, 0)?;
    root.read_reserved(path, limit, io, work.memory_budget(), cap)
        .map_err(native)
}

fn search<T>(
    rows: &[T],
    path: &[u8],
    key: impl Fn(&T) -> &[u8],
    work: &mut Work<'_>,
) -> Result<std::result::Result<usize, usize>> {
    if path.len() > MAX_RELATIVE_BYTES {
        return Err("discovery-path");
    }
    let comparisons = (usize::BITS - rows.len().leading_zeros()) as usize + 1;
    work.charge(comparisons * (path.len() + 1))?;
    Ok(rows.binary_search_by(|row| key(row).cmp(path)))
}

fn join<'a>(
    dir: &str,
    name: &str,
    storage: &'a mut [u8; MAX_RELATIVE_BYTES],
    work: &mut Work<'_>,
) -> Result<&'a str> {
    let slash = usize::from(!dir.is_empty());
    let total = dir.len() + slash + name.len();
    work.charge(2 * total + 1)?;
    if total > storage.len() {
        return Err("discovery-path");
    }
    storage[..dir.len()].copy_from_slice(dir.as_bytes());
    if slash != 0 {
        storage[dir.len()] = b'/';
    }
    storage[dir.len() + slash..total].copy_from_slice(name.as_bytes());
    std::str::from_utf8(&storage[..total]).map_err(|_| "discovery-path")
}

fn allocate<T>(count: usize, work: &mut Work<'_>) -> Result<Vec<T>> {
    work.charge(count * size_of::<T>() + 1)?;
    let mut output = Vec::new();
    output.try_reserve_exact(count).map_err(|_| "allocation")?;
    if output.capacity() != count {
        return Err("memory-cap");
    }
    Ok(output)
}

fn reserve<'a>(work: &mut Work<'a>, bytes: usize) -> Result<Reservation<'a>> {
    if work.read_limit(bytes, 0)? != bytes {
        return Err("memory-cap");
    }
    work.memory_budget()
        .reserve(bytes)
        .map_err(|_| "memory-cap")
}

fn membership(error: MembershipError) -> &'static str {
    match error {
        MembershipError::Policy(error) => error.reason(),
        MembershipError::Budget(reason) => reason,
        MembershipError::Unqualified => "discovery-unqualified",
        MembershipError::UndiscoveredIndexRow => "discovery-index-row-not-found",
        MembershipError::UnindexedDiscoveredPath => "discovery-unindexed-path",
    }
}

fn native(error: ReadError) -> &'static str {
    match error {
        ReadError::Work => "work-cap",
        other => read_error(other),
    }
}

fn role_tag(role: DiscoveryEntry) -> u8 {
    match role {
        DiscoveryEntry::Prune => 0,
        DiscoveryEntry::Directory => 1,
        DiscoveryEntry::CodeFile => 2,
        DiscoveryEntry::OtherFile => 3,
    }
}
