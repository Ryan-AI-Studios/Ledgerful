//! Keep contained Git object reads and retained packs in one observation.
pub(crate) mod baseline_producer;
pub(crate) mod capture_budget;
pub(crate) mod capture_db;
pub(crate) mod capture_session;
pub(crate) mod changed_files;
mod index_inputs;
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
mod isolated_fd;
#[cfg(target_os = "linux")]
#[path = "pack_store/linux_capture.rs"]
mod linux_capture;
pub(crate) mod paired_checkout;
pub(crate) mod passive_db;
#[cfg(target_os = "linux")]
#[path = "pack_store/stock_vfs.rs"]
pub(crate) mod stock_vfs;

#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
mod isolated_passive;
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
mod isolated_readonly_inputs;
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
mod isolated_readonly_pager;
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
mod published_snapshot;

#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
pub(super) fn register_isolated_passive_pager() -> rusqlite::Result<()> {
    isolated_readonly_pager::register_private_pager()
}

use super::buffer::{MemoryBudget, Reservation, ReservedBytes};
use super::contained::{ContainedRoot, DirectoryVisitError, EntryKind, ReadBudget, ReadError};
use super::decode::{DecodeError, decode_index};
use super::directory_rows::DirectoryRows;
use super::filemode::FileModePolicy;
use super::git::{
    CommitFacts, MAX_COMMIT_BYTES, MAX_TREE_BYTES, decode_commit_facts, exact_oid,
    read_error as git_read_error, read_loose, resolve_head, visit_tree_entries,
};
use super::git_config::{
    LocalGitConfigError, LocalGitPolicy, MAX_LOCAL_GIT_CONFIG_BYTES, admit_local_git_config,
};
use super::inventory::{
    InventoryError, InventoryLimits, InventoryPass, InventoryRead, reason as inventory_error,
};
use super::inventory_transcript::{InventoryTranscript, ObservedInventory};
use super::nested_ignore::{PathIgnoreDecision, PathPolicy};
use super::pack::{MAX_INDEX_BYTES, MAX_PACK_BYTES, Pack, Work};
use super::root_ignore::RootIgnoreSnapshot;
use super::root_ignore_matcher::RootIgnoreMatcher;
use super::scope::ScopeGuard;
use super::scope_owner::ScopeRoot;
use super::tracked_attributes::{
    AttributeRead, ObservedTrackedAttributes, TrackedAttributeError, TrackedAttributeTranscript,
    TrackedAttributes,
};
use super::tree_index::{TreeIndexWalk, TreePaths};
use crate::state::layout::{Layout, STATE_DIR, StateDirOrigin};
use gix::ObjectId;
use gix::objs::{Kind, tree::EntryRef};
use std::mem::size_of;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

type PackResult<T> = Result<T, &'static str>;
const INVENTORY_LIMIT: usize = 64;
const PAIR_LIMIT: usize = 16;
const PACK_DIRECTORY: &str = ".git/objects/pack";
const PACK_PATH_PREFIX_LEN: usize = PACK_DIRECTORY.len() + 1;
const MAX_PACK_PATH: usize = PACK_PATH_PREFIX_LEN + "pack-".len() + 40 + ".pack".len();
const LOCAL_GIT_CONFIG_PATH: &str = ".git/config";
const STAGED_INDEX_PATH: &str = ".git/index";
const MAX_STAGED_INDEX_BYTES: usize = 1024 * 1024;
const MAX_STAGED_INDEX_ENTRIES: usize = 4_096;
const MAX_TRACKED_BLOB_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct ObservedStagedIndex<'a> {
    raw: ReservedBytes<'a>,
    state: gix::index::State,
    _decoded: Reservation<'a>,
}

pub(crate) struct StagedEntry<'a> {
    pub(crate) path: &'a [u8],
    pub(crate) mode: gix::index::entry::Mode,
    pub(crate) id: ObjectId,
}

impl ObservedStagedIndex<'_> {
    pub(crate) fn entries(&self) -> impl ExactSizeIterator<Item = StagedEntry<'_>> {
        self.state.entries().iter().map(|entry| StagedEntry {
            path: entry.path(&self.state).as_ref(),
            mode: entry.mode,
            id: entry.id,
        })
    }
}

struct ObservedLocalGitConfig<'a> {
    raw: ReservedBytes<'a>,
    policy: LocalGitPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommitPair {
    pub(crate) head: ObjectId,
    pub(crate) parent: ObjectId,
    pub(crate) head_tree: ObjectId,
    pub(crate) parent_tree: ObjectId,
}

/// A stored commit pair bracketed by matching observations of current HEAD and refs.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CurrentCommitPair(CommitPair);

impl CurrentCommitPair {
    pub(crate) fn commits(&self) -> &CommitPair {
        &self.0
    }
}

#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
struct PassiveStorageBudget<'a> {
    work: Work<'a>,
    heap: Option<super::native_heap::NativeHeapTicket<'a>>,
    heap_credit: Option<usize>,
}

pub(crate) struct Observation<'a> {
    #[cfg(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu",
        feature = "web"
    ))]
    passive_storage: Option<PassiveStorageBudget<'a>>,
    root: ScopeRoot<'a>,
    io: ReadBudget<'a>,
    work: Work<'a>,
    packs: Option<PackResult<PackStore<'a>>>,
    git_config: Option<PackResult<ObservedLocalGitConfig<'a>>>,
    root_ignore: Option<PackResult<RootIgnoreSnapshot<'a>>>,
    compiled_root_ignore: Option<RootIgnoreMatcher<'a>>,
    staged_index: Option<PackResult<ObservedStagedIndex<'a>>>,
}

impl<'a> Observation<'a> {
    pub(crate) fn open(
        path: &Path,
        deadline: Instant,
        interrupted: &'a AtomicBool,
        read_bytes: usize,
        work_units: usize,
        memory: &'a MemoryBudget,
    ) -> PackResult<Self> {
        let io = ReadBudget::new(read_bytes, deadline, interrupted);
        let root = ContainedRoot::open(path, &io).map_err(read_error)?;
        Ok(Self {
            root: ScopeRoot::Plain(root),
            io,
            work: Work::new(deadline, interrupted, work_units, memory),
            packs: None,
            git_config: None,
            root_ignore: None,
            compiled_root_ignore: None,
            staged_index: None,
            #[cfg(all(
                target_os = "linux",
                target_arch = "x86_64",
                target_env = "gnu",
                feature = "web"
            ))]
            passive_storage: None,
        })
    }

    /// Private scoped construction; origin must accompany the actual served layout.
    pub(crate) fn open_scoped(
        layout: &Layout,
        origin: StateDirOrigin,
        deadline: Instant,
        interrupted: &'a AtomicBool,
        read_bytes: usize,
        work_units: usize,
        memory: &'a MemoryBudget,
    ) -> PackResult<Self> {
        Self::open_scoped_io(
            layout,
            origin,
            deadline,
            interrupted,
            work_units,
            memory,
            ReadBudget::new(read_bytes, deadline, interrupted),
        )
    }

    #[cfg(feature = "web")]
    pub(crate) fn open_scoped_passive(
        layout: &Layout,
        origin: StateDirOrigin,
        deadline: Instant,
        interrupted: &'a AtomicBool,
        read_bytes: &'a std::sync::atomic::AtomicUsize,
        work_units: usize,
        memory: &'a MemoryBudget,
    ) -> PackResult<Self> {
        let read_limit = read_bytes.load(std::sync::atomic::Ordering::Relaxed);
        let observation = Self::open_scoped_io(
            layout,
            origin,
            deadline,
            interrupted,
            work_units,
            memory,
            ReadBudget::new_shared(read_limit, deadline, interrupted, read_bytes),
        )?;
        #[cfg(all(
            target_os = "linux",
            target_arch = "x86_64",
            target_env = "gnu",
            feature = "web"
        ))]
        let observation = Self {
            passive_storage: Some(PassiveStorageBudget {
                work: Work::new(
                    deadline,
                    interrupted,
                    isolated_passive::storage_work_units(read_limit),
                    memory,
                ),
                heap: None,
                heap_credit: None,
            }),
            ..observation
        };
        Ok(observation)
    }

    pub(super) fn open_scoped_metered(
        layout: &Layout,
        origin: StateDirOrigin,
        limits: paired_checkout::CheckoutLimits,
        phase: capture_budget::CapturePhase<'a>,
    ) -> PackResult<Self> {
        let io = ReadBudget::new_shared(
            limits.read_bytes,
            phase.deadline,
            phase.interrupted,
            phase.aggregate,
        );
        Self::open_scoped_io(
            layout,
            origin,
            phase.deadline,
            phase.interrupted,
            limits.work_units,
            phase.memory,
            io,
        )
    }

    fn open_scoped_io(
        layout: &Layout,
        origin: StateDirOrigin,
        deadline: Instant,
        interrupted: &'a AtomicBool,
        work_units: usize,
        memory: &'a MemoryBudget,
        io: ReadBudget<'a>,
    ) -> PackResult<Self> {
        let mut work = Work::new(deadline, interrupted, work_units, memory);
        qualify_layout(layout, origin, &mut work)?;
        let root = ScopeRoot::open_scoped(layout.root.as_std_path(), &io, &mut work)?;
        Ok(Self {
            root,
            io,
            work,
            packs: None,
            git_config: None,
            root_ignore: None,
            compiled_root_ignore: None,
            staged_index: None,
            #[cfg(all(
                target_os = "linux",
                target_arch = "x86_64",
                target_env = "gnu",
                feature = "web"
            ))]
            passive_storage: None,
        })
    }

    /// Reacquire the scope and confirm the exact HEAD from the caller’s checkout proof.
    pub(crate) fn confirm_scoped_guard(
        &mut self,
        layout: &Layout,
        origin: StateDirOrigin,
        expected_head: ObjectId,
    ) -> PackResult<ScopeGuard> {
        qualify_layout(layout, origin, &mut self.work)?;
        self.observe_local_git_config()?;
        let (head, refs) = resolve_head(&self.root, &mut self.io, &mut self.work)?;
        if head != expected_head {
            return Err("head-mismatch");
        }
        self.confirm_local_git_config()?;
        self.root
            .confirm_reacquired(layout.root.as_std_path(), &self.io, &mut self.work)?;
        refs.confirm(&self.root, &mut self.io, &mut self.work)?;
        self.confirm_local_git_config()?;
        guard_scope(&self.root, &mut self.work)
    }

    pub(crate) fn observe_local_git_config(&mut self) -> PackResult<&LocalGitPolicy> {
        self.work.charge(0)?;
        self.git_config
            .get_or_insert_with(|| {
                let raw = read_local_git_config(&self.root, &mut self.io, &mut self.work)?;
                let policy = admit_local_git_config(raw.as_ref()).map_err(git_config_error)?;
                self.work.charge(0)?;
                Ok(ObservedLocalGitConfig { raw, policy })
            })
            .as_ref()
            .map(|snapshot| &snapshot.policy)
            .map_err(|error| *error)
    }

    pub(crate) fn confirm_local_git_config(&mut self) -> PackResult<()> {
        let captured = self
            .git_config
            .as_ref()
            .ok_or("git-config-unobserved")?
            .as_ref()
            .map_err(|error| *error)?;
        let result =
            read_local_git_config(&self.root, &mut self.io, &mut self.work).and_then(|raw| {
                if raw.as_ref() != captured.raw.as_ref() {
                    return Err("git-config-changed");
                }
                self.work.charge(0)
            });
        if let Err(error) = result {
            self.git_config = Some(Err(error));
        }
        result
    }

    pub(crate) fn observe_root_ignore_rules(&mut self) -> PackResult<&RootIgnoreSnapshot<'a>> {
        let ignore_case = self.observe_local_git_config()?.ignore_case;
        self.root_ignore
            .get_or_insert_with(|| {
                RootIgnoreSnapshot::capture(&self.root, &mut self.io, &mut self.work, ignore_case)
            })
            .as_ref()
            .map_err(|error| *error)
    }

    pub(crate) fn confirm_root_ignore_rules(&mut self) -> PackResult<()> {
        let snapshot = self
            .root_ignore
            .as_ref()
            .ok_or("root-ignore-unobserved")?
            .as_ref()
            .map_err(|error| *error)?;
        let result = snapshot
            .confirm(&self.root, &mut self.io, &mut self.work)
            .and_then(|()| self.confirm_local_git_config());
        if let Err(error) = result {
            self.root_ignore = Some(Err(error));
            self.compiled_root_ignore = None;
        }
        result
    }

    /// Return one root-policy decision only, bracketed by exact observations of its inputs.
    pub(crate) fn root_ignore_decision(
        &mut self,
        path: &[u8],
        is_dir: bool,
    ) -> PackResult<Option<bool>> {
        self.prepare_root_ignore()?;
        let decision = self
            .compiled_root_ignore
            .as_ref()
            .ok_or("root-ignore-unobserved")?
            .leaf(path, is_dir, &mut self.work)
            .map_err(|error| error.reason())?;
        self.confirm_root_ignore_rules()?;
        Ok(decision)
    }

    /// Decide a contained leaf or an observed ignored ancestor for one selected path.
    pub(crate) fn nested_ignore_decision(
        &mut self,
        path: &str,
        is_dir: bool,
    ) -> PackResult<PathIgnoreDecision> {
        let ignore_case = self.prepare_root_ignore()?;
        let root_matcher = self
            .compiled_root_ignore
            .as_ref()
            .ok_or("root-ignore-unobserved")?;
        let decision = PathPolicy {
            root: &self.root,
            io: &mut self.io,
            work: &mut self.work,
            root_matcher,
            ignore_case,
        }
        .decide(path, is_dir)?;
        self.confirm_root_ignore_rules()?;
        Ok(decision)
    }

    /// Observe the visible namespace once against the exact matched tree and local ignore policy.
    pub(crate) fn observe_worktree_inventory(
        &mut self,
        tree: ObjectId,
        limits: InventoryLimits,
    ) -> Result<InventoryPass, InventoryError> {
        self.collect_worktree_inventory(tree, limits, None)
    }

    pub(crate) fn observe_worktree_inventory_witness(
        &mut self,
        tree: ObjectId,
        limits: InventoryLimits,
    ) -> Result<ObservedInventory, InventoryError> {
        let mut transcript = InventoryTranscript::new(&mut self.work)?;
        let counts = self.collect_worktree_inventory(tree, limits, Some(&mut transcript))?;
        transcript.complete(counts, &mut self.work)
    }

    fn collect_worktree_inventory(
        &mut self,
        tree: ObjectId,
        limits: InventoryLimits,
        mut transcript: Option<&mut InventoryTranscript>,
    ) -> Result<InventoryPass, InventoryError> {
        let ignore_case = self.prepare_root_ignore().map_err(inventory_error)?;
        let walk = self.match_staged_tree(tree).map_err(inventory_error)?;
        self.confirm_staged_index().map_err(inventory_error)?;
        if let Some(transcript) = transcript.as_deref_mut() {
            let config = self
                .git_config
                .as_ref()
                .ok_or_else(|| inventory_error("git-config-unobserved"))?
                .as_ref()
                .map_err(|error| inventory_error(error))?;
            let rules = self
                .root_ignore
                .as_ref()
                .ok_or_else(|| inventory_error("root-ignore-unobserved"))?
                .as_ref()
                .map_err(|error| inventory_error(error))?;
            transcript.start(tree, config.raw.as_ref(), ignore_case, &mut self.work)?;
            for (path, raw) in rules.inputs() {
                transcript.policy(b"", path.as_bytes(), raw, None, &mut self.work)?;
            }
        }
        let root_matcher = self
            .compiled_root_ignore
            .as_ref()
            .ok_or_else(|| inventory_error("root-ignore-unobserved"))?;
        let pass = InventoryRead {
            root: &mut self.root,
            io: &mut self.io,
            work: &mut self.work,
            root_matcher,
            ignore_case,
        }
        .capture(&walk, limits, transcript)?;
        drop(walk);
        self.confirm_staged_index().map_err(inventory_error)?;
        self.confirm_root_ignore_rules().map_err(inventory_error)?;
        Ok(pass)
    }

    fn prepare_root_ignore(&mut self) -> PackResult<Option<bool>> {
        let ignore_case = self.observe_root_ignore_rules()?.ignore_case();
        self.confirm_root_ignore_rules()?;
        if self.compiled_root_ignore.is_none() {
            let source = self
                .root_ignore
                .as_ref()
                .ok_or("root-ignore-unobserved")?
                .as_ref()
                .map_err(|error| *error)?;
            let matcher =
                RootIgnoreMatcher::compile(source.raw_inputs(), ignore_case, &mut self.work)
                    .map_err(|error| error.reason())?;
            self.compiled_root_ignore = Some(matcher);
        }
        Ok(ignore_case)
    }

    pub(crate) fn observe_staged_index(&mut self) -> PackResult<&ObservedStagedIndex<'a>> {
        self.work.charge(0)?;
        self.staged_index
            .get_or_insert_with(|| {
                let raw = read_staged_index(&self.root, &mut self.io, &mut self.work)?;
                decode_staged_index(raw, &mut self.work)
            })
            .as_ref()
            .map_err(|error| *error)
    }

    pub(crate) fn confirm_staged_index(&mut self) -> PackResult<()> {
        let captured = self
            .staged_index
            .as_ref()
            .ok_or("staged-index-unobserved")?
            .as_ref()
            .map_err(|error| *error)?;
        let result = read_staged_index(&self.root, &mut self.io, &mut self.work).and_then(|raw| {
            if raw.as_ref() != captured.raw.as_ref() {
                return Err("staged-index-changed");
            }
            self.work.charge(0)
        });
        if let Err(error) = result {
            self.staged_index = Some(Err(error));
        }
        result
    }

    /// Check the one-parent relationship for two selected stored commits.
    pub(crate) fn observe_commit_pair(
        &mut self,
        head: ObjectId,
        requested_parent: ObjectId,
    ) -> PackResult<CommitPair> {
        let head_commit = self.commit_facts(head)?;
        let parent = head_commit.only_parent.ok_or("non-single-parent")?;
        if parent != requested_parent {
            return Err("commit-parent-mismatch");
        }
        let parent_commit = self.commit_facts(parent)?;
        Ok(CommitPair {
            head,
            parent,
            head_tree: head_commit.tree,
            parent_tree: parent_commit.tree,
        })
    }

    pub(crate) fn observe_current_commit_pair(
        &mut self,
        requested_head: ObjectId,
        requested_parent: ObjectId,
    ) -> PackResult<CurrentCommitPair> {
        let (head, refs) = resolve_head(&self.root, &mut self.io, &mut self.work)?;
        if head != requested_head {
            return Err("head-mismatch");
        }
        let pair = self.observe_commit_pair(head, requested_parent)?;
        refs.confirm(&self.root, &mut self.io, &mut self.work)?;
        Ok(CurrentCommitPair(pair))
    }

    pub(super) fn require_direct_parent(&mut self, head: ObjectId) -> PackResult<ObjectId> {
        self.commit_facts(head)?
            .only_parent
            .ok_or("non-single-parent")
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn evidence_index_config(&mut self) -> PackResult<(String, u64)> {
        let days = super::baseline_discovery::read_project_config_days(
            &self.root,
            &mut self.io,
            &mut self.work,
        )?;
        let fingerprint =
            crate::state::storage::change_evidence::writes::prepared::effective_config_fingerprint(
                days,
            );
        Ok((fingerprint, days))
    }

    fn commit_facts(&mut self, id: ObjectId) -> PackResult<CommitFacts> {
        let raw = self.object(id, Kind::Commit, MAX_COMMIT_BYTES)?;
        decode_commit_facts(raw.as_ref(), &mut self.work)
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn evidence_discovery_fingerprint(&mut self, layout: &Layout) -> PackResult<String> {
        #[cfg(windows)]
        let _ = layout;
        let witness = super::baseline_discovery::replay_fingerprint(
            &mut self.root,
            &mut self.io,
            &mut self.work,
            #[cfg(target_os = "linux")]
            layout.root.as_std_path(),
        )?;
        Ok(
            crate::state::storage::change_evidence::writes::prepared::format_discovery_fingerprint(
                witness,
            ),
        )
    }

    /// Inspect one tree. Entry names remain raw and must be qualified before use as paths.
    pub(crate) fn visit_tree(
        &mut self,
        id: ObjectId,
        mut visit: impl FnMut(EntryRef<'_>) -> PackResult<()>,
    ) -> PackResult<usize> {
        self.visit_tree_with_work(id, |entry, _| visit(entry))
    }

    fn visit_tree_with_work(
        &mut self,
        id: ObjectId,
        visit: impl FnMut(EntryRef<'_>, &mut Work<'a>) -> PackResult<()>,
    ) -> PackResult<usize> {
        let raw = self.object(id, Kind::Tree, MAX_TREE_BYTES)?;
        visit_tree_entries(raw.as_ref(), &mut self.work, visit)
    }

    /// Compare one tree hierarchy with the exact staged snapshot and recheck the index.
    pub(crate) fn compare_tree_to_staged_index(&mut self, tree: ObjectId) -> PackResult<usize> {
        let walk = self.match_staged_tree(tree)?;
        let count = walk.files();
        drop(walk);
        self.confirm_staged_index()?;
        Ok(count)
    }

    /// Sample only the raw bytes of every tracked file in a matched tree and index.
    pub(crate) fn compare_tracked_worktree_bytes(&mut self, tree: ObjectId) -> PackResult<usize> {
        self.compare_tracked_worktree(tree, FileModePolicy::Ignore)
    }

    /// Sample the tracked bytes and apply the explicitly admitted local filemode policy.
    pub(crate) fn compare_tracked_worktree_modes_and_bytes(
        &mut self,
        tree: ObjectId,
    ) -> PackResult<usize> {
        let policy = FileModePolicy::from_local(self.observe_local_git_config()?.file_mode)?;
        self.confirm_local_git_config()?;
        let count = self.compare_tracked_worktree(tree, policy)?;
        self.confirm_local_git_config()?;
        Ok(count)
    }

    /// Bracket the exact tracked sample with admitted local attribute observations.
    pub(crate) fn compare_tracked_worktree_with_attributes(
        &mut self,
        tree: ObjectId,
    ) -> Result<usize, TrackedAttributeError> {
        self.collect_tracked_worktree_attributes(tree, None)
    }

    pub(crate) fn observe_tracked_attribute_witness(
        &mut self,
        tree: ObjectId,
    ) -> Result<ObservedTrackedAttributes, TrackedAttributeError> {
        let mut transcript = TrackedAttributeTranscript::new(&mut self.work)?;
        let count = self.collect_tracked_worktree_attributes(tree, Some(&mut transcript))?;
        transcript.complete(count, &mut self.work)
    }

    fn collect_tracked_worktree_attributes(
        &mut self,
        tree: ObjectId,
        transcript: Option<&mut TrackedAttributeTranscript>,
    ) -> Result<usize, TrackedAttributeError> {
        let tracked = TrackedAttributeError::Tracked;
        let file_mode = self.observe_local_git_config().map_err(tracked)?.file_mode;
        let policy = FileModePolicy::from_local(file_mode).map_err(tracked)?;
        self.confirm_local_git_config().map_err(tracked)?;
        let walk = self.match_staged_tree(tree).map_err(tracked)?;
        self.confirm_staged_index().map_err(tracked)?;
        let attributes = TrackedAttributes::capture(
            &walk,
            &mut AttributeRead {
                root: &self.root,
                io: &mut self.io,
                work: &mut self.work,
            },
        )?;
        let count = self
            .compare_matched_tracked_worktree(&walk, policy, Some(&attributes))
            .map_err(tracked)?;
        attributes.confirm(&mut AttributeRead {
            root: &self.root,
            io: &mut self.io,
            work: &mut self.work,
        })?;
        if let Some(transcript) = transcript {
            transcript.start(tree, policy, &mut self.work)?;
            attributes.write_witness(transcript, &mut self.work)?;
        }
        drop(attributes);
        drop(walk);
        self.confirm_staged_index().map_err(tracked)?;
        self.confirm_local_git_config().map_err(tracked)?;
        Ok(count)
    }

    fn compare_tracked_worktree(
        &mut self,
        tree: ObjectId,
        policy: FileModePolicy,
    ) -> PackResult<usize> {
        let walk = self.match_staged_tree(tree)?;
        self.confirm_staged_index()?;
        let count = self.compare_matched_tracked_worktree(&walk, policy, None)?;
        drop(walk);
        self.confirm_staged_index()?;
        Ok(count)
    }

    fn compare_matched_tracked_worktree(
        &mut self,
        walk: &TreeIndexWalk<'a>,
        policy: FileModePolicy,
        attributes: Option<&TrackedAttributes<'a, '_>>,
    ) -> PackResult<usize> {
        for (raw_path, id, executable) in walk.tracked_files() {
            self.work.charge(raw_path.len())?;
            let path = std::str::from_utf8(raw_path).map_err(|_| "unsupported-tracked-path")?;
            let captured_rules = match attributes {
                Some(attributes) => attributes.captured_tracked_rule(path, &mut self.work)?,
                None => None,
            };
            self.compare_tracked_blob(path, id, executable, policy, captured_rules)?;
        }
        Ok(walk.files())
    }

    fn compare_tracked_blob(
        &mut self,
        path: &str,
        id: ObjectId,
        executable: bool,
        policy: FileModePolicy,
        captured_rules: Option<&[u8]>,
    ) -> PackResult<()> {
        self.match_tracked_blob(path, id, executable, policy, captured_rules)?;
        Ok(())
    }

    fn match_tracked_blob(
        &mut self,
        path: &str,
        id: ObjectId,
        executable: bool,
        policy: FileModePolicy,
        captured_rules: Option<&[u8]>,
    ) -> PackResult<(ReservedBytes<'a>, Option<i64>)> {
        let blob = self.object(id, Kind::Blob, MAX_TRACKED_BLOB_BYTES)?;
        let expected = blob.as_ref();
        if expected.len() > MAX_TRACKED_BLOB_BYTES {
            return Err("tracked-blob-cap");
        }
        if let Some(rules) = captured_rules {
            self.work.charge(rules.len())?;
            if rules != expected {
                return Err("worktree-blob-mismatch");
            }
        }
        self.work.charge(1)?;
        let capacity = self.work.read_limit(expected.len(), 0)?;
        let actual = self
            .root
            .read_reserved_file(
                path,
                expected.len(),
                &mut self.io,
                self.work.memory_budget(),
                capacity,
            )
            .map_err(|error| match error {
                ReadError::Bytes => "worktree-blob-mismatch",
                ReadError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    "worktree-file-missing"
                }
                error => git_read_error(error),
            })?;
        self.work.charge(actual.bytes.as_ref().len())?;
        if actual.bytes.as_ref() != expected {
            return Err("worktree-blob-mismatch");
        }
        policy.verify(executable, actual.owner_executable)?;
        self.work.charge(0)?;
        Ok((blob, actual.mtime_ns))
    }

    fn match_staged_tree(&mut self, tree: ObjectId) -> PackResult<TreeIndexWalk<'a>> {
        self.observe_staged_index()?;
        let walk = self.walk_verified_tree(tree, TreePaths::Strict)?;
        let staged = self
            .staged_index
            .as_ref()
            .ok_or("staged-index-unobserved")?
            .as_ref()
            .map_err(|error| *error)?;
        let entries = staged.entries();
        let count = entries.len();
        if count != walk.files() {
            return Err("staged-tree-mismatch");
        }
        for entry in entries {
            walk.compare_file(entry.path, entry.mode, entry.id, &mut self.work)?;
        }
        Ok(walk)
    }

    fn walk_verified_tree(
        &mut self,
        tree: ObjectId,
        paths: TreePaths,
    ) -> PackResult<TreeIndexWalk<'a>> {
        let mut walk = TreeIndexWalk::new(paths, &mut self.work)?;
        self.visit_tree_with_work(tree, |entry, work| walk.push(None, entry, work))?;
        let mut cursor = 0;
        while let Some(dir) = walk.next_tree(&mut cursor, &mut self.work)? {
            let count = self
                .visit_tree_with_work(dir.id, |entry, work| walk.push(Some(&dir), entry, work))?;
            if count == 0 {
                return Err("empty-subtree");
            }
        }

        Ok(walk)
    }

    /// The loose path uses the supplied cap. Packed decoding keeps its own cap.
    pub(crate) fn object(
        &mut self,
        id: ObjectId,
        kind: Kind,
        loose_byte_limit: usize,
    ) -> PackResult<ReservedBytes<'a>> {
        match read_loose(
            &self.root,
            id,
            kind,
            loose_byte_limit,
            &mut self.work,
            &mut self.io,
        ) {
            Err("missing-loose-object") => {}
            result => return result,
        }
        let packs = self.packs.get_or_insert_with(|| {
            PackStore::observe(&mut self.root, &mut self.io, &mut self.work)
        });
        packs
            .as_ref()
            .map_err(|error| *error)?
            .object(id, kind, &mut self.work)
    }

    pub(crate) fn confirm_pack_inventory(&mut self) -> PackResult<()> {
        match &self.packs {
            Some(Ok(packs)) => packs.confirm(&mut self.root, &self.io, &mut self.work),
            Some(Err(error)) => Err(error),
            None => self.io.checkpoint().map_err(read_error),
        }
    }
}

fn read_staged_index<'a>(
    root: &ContainedRoot,
    io: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
) -> PackResult<ReservedBytes<'a>> {
    let capacity = work.read_limit(MAX_STAGED_INDEX_BYTES, 0)?;
    let raw = root
        .read_reserved(
            STAGED_INDEX_PATH,
            MAX_STAGED_INDEX_BYTES,
            io,
            work.memory_budget(),
            capacity,
        )
        .map_err(git_read_error)?;
    work.charge(raw.as_ref().len())?;
    Ok(raw)
}

fn decode_staged_index<'a>(
    raw: ReservedBytes<'a>,
    work: &mut Work<'a>,
) -> PackResult<ObservedStagedIndex<'a>> {
    const HEADER_AND_CHECKSUM_BYTES: usize = 12 + 20;
    const MIN_ENTRY_BYTES: usize = 64;
    let len = raw.as_ref().len();
    let possible_entries = len.saturating_sub(HEADER_AND_CHECKSUM_BYTES) / MIN_ENTRY_BYTES;
    let entry_capacity = possible_entries.clamp(4, MAX_STAGED_INDEX_ENTRIES);
    // V2/V3 store complete pathname bytes. Pinned gix reserves entry and path
    // vectors once for this admitted layout, with minimum nonzero Vec capacities.
    let decoded_bytes = entry_capacity * std::mem::size_of::<gix::index::Entry>() + len.max(8);
    // Account for a sanitized envelope, possible copied error paths, and fixed
    // decoder bookkeeping. This is an allowance, not an allocator measurement.
    let temporary_bytes = 2 * len + 4096;
    let peak = decoded_bytes + temporary_bytes;
    if work.read_limit(peak, 0)? != peak {
        return Err("memory-cap");
    }
    let memory = work.memory_budget();
    let _temporary = memory.reserve(temporary_bytes).map_err(|_| "memory-cap")?;
    let decoded = memory.reserve(decoded_bytes).map_err(|_| "memory-cap")?;
    let state = decode_index(
        raw.as_ref(),
        MAX_STAGED_INDEX_BYTES,
        MAX_STAGED_INDEX_ENTRIES,
    )
    .map_err(staged_index_error)?;
    work.charge(0)?;
    Ok(ObservedStagedIndex {
        raw,
        state,
        _decoded: decoded,
    })
}

fn staged_index_error(error: DecodeError) -> &'static str {
    match error {
        DecodeError::Bytes => "staged-index-byte-cap",
        DecodeError::Entries => "staged-index-entry-cap",
        DecodeError::IndexChecksum => "staged-index-checksum",
        DecodeError::IndexUnsupported => "staged-index-unsupported",
        DecodeError::Allocation => "allocation",
        _ => "staged-index-invalid",
    }
}

fn read_local_git_config<'a>(
    root: &ContainedRoot,
    io: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
) -> PackResult<ReservedBytes<'a>> {
    let capacity = work.read_limit(MAX_LOCAL_GIT_CONFIG_BYTES, 0)?;
    let raw = root
        .read_reserved(
            LOCAL_GIT_CONFIG_PATH,
            MAX_LOCAL_GIT_CONFIG_BYTES,
            io,
            work.memory_budget(),
            capacity,
        )
        .map_err(git_read_error)?;
    work.charge(raw.as_ref().len())?;
    Ok(raw)
}

fn git_config_error(error: LocalGitConfigError) -> &'static str {
    match error {
        LocalGitConfigError::Bytes => "git-config-byte-cap",
        LocalGitConfigError::Invalid => "git-config-invalid",
        LocalGitConfigError::Duplicate => "git-config-duplicate",
        LocalGitConfigError::Unsupported => "git-config-unsupported",
    }
}

struct PackStore<'a> {
    names: DirectoryRows<'a>,
    packs: Vec<Pack<'a>>,
    _pack_table: Reservation<'a>,
}

struct PackPair<'rows> {
    id: ObjectId,
    index_name: Option<&'rows str>,
    data_name: Option<&'rows str>,
}

fn read_error(error: ReadError) -> &'static str {
    match error {
        ReadError::Redirect => "redirect",
        ReadError::Kind => "nonregular",
        ReadError::Changed => "inventory-changed",
        ReadError::Entries => "inventory-entry-cap",
        ReadError::Path => "unsafe-path",
        ReadError::Bytes | ReadError::Aggregate => "pack-read-cap",
        ReadError::Memory => "memory-cap",
        ReadError::Deadline | ReadError::Interrupted => "deadline",
        _ => "inventory-unavailable",
    }
}

fn row_error(error: DirectoryVisitError<&'static str>) -> &'static str {
    match error {
        DirectoryVisitError::Reader(error) => read_error(error),
        DirectoryVisitError::Visitor("directory-duplicate-name") => "inventory-changed",
        DirectoryVisitError::Visitor("directory-entry-cap") => "inventory-entry-cap",
        DirectoryVisitError::Visitor("allocation") => "inventory-unavailable",
        DirectoryVisitError::Visitor(reason) => reason,
    }
}

fn pack_table<'a, T>(count: usize, work: &mut Work<'a>) -> PackResult<(Vec<T>, Reservation<'a>)> {
    let bytes = count.checked_mul(size_of::<T>()).ok_or("memory-cap")?;
    work.charge(bytes)?;
    if work.read_limit(bytes, 0)? != bytes {
        return Err("memory-cap");
    }
    let reservation = work
        .memory_budget()
        .reserve(bytes)
        .map_err(|_| "memory-cap")?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(count).map_err(|_| "allocation")?;
    if rows.capacity() != count {
        return Err("memory-cap");
    }
    Ok((rows, reservation))
}

fn read_pack_file<'a>(
    root: &ContainedRoot,
    name: &str,
    limit: usize,
    io: &mut ReadBudget<'a>,
    work: &mut Work<'a>,
) -> PackResult<ReservedBytes<'a>> {
    let end = PACK_PATH_PREFIX_LEN + name.len();
    let _lookup = work.reserve_lookup(end)?;
    let mut path = [0; MAX_PACK_PATH];
    path[..PACK_DIRECTORY.len()].copy_from_slice(PACK_DIRECTORY.as_bytes());
    path[PACK_DIRECTORY.len()] = b'/';
    path.get_mut(PACK_PATH_PREFIX_LEN..end)
        .ok_or("unsafe-path")?
        .copy_from_slice(name.as_bytes());
    let path = std::str::from_utf8(&path[..end]).map_err(|_| "unsafe-path")?;
    let cap = work.read_limit(usize::MAX, 0)?;
    root.read_reserved(path, limit, io, work.memory_budget(), cap)
        .map_err(read_error)
}

impl<'a> PackStore<'a> {
    fn observe(
        root: &mut ScopeRoot<'_>,
        io: &mut ReadBudget<'a>,
        work: &mut Work<'a>,
    ) -> PackResult<Self> {
        let names = DirectoryRows::capture(root, PACK_DIRECTORY, INVENTORY_LIMIT, io, work)
            .map_err(row_error)?;
        let (mut pairs, pair_table) = pack_table::<PackPair<'_>>(names.rows().len(), work)?;
        for entry in names.rows() {
            let bytes = entry.name.as_ref();
            work.charge(4 * bytes.len() + 1)?;
            match entry.kind {
                EntryKind::File => (),
                EntryKind::Redirect => return Err("redirect"),
                _ => return Err("nonregular"),
            }
            let name = std::str::from_utf8(bytes).map_err(|_| "unsafe-path")?;
            let (stem, extension) = name
                .strip_prefix("pack-")
                .and_then(|name| name.split_once('.'))
                .ok_or("unsupported-pack-inventory")?;
            let id = exact_oid(stem.as_bytes())?;
            if pairs.last().is_none_or(|pair| pair.id != id) {
                // Exact lowercase hex names are sorted by ObjectId in DirectoryRows.
                pairs.push(PackPair {
                    id,
                    index_name: None,
                    data_name: None,
                });
            }
            let pair = pairs.last_mut().expect("the current pack pair is present");
            match extension {
                "idx" => pair.index_name = Some(name),
                "pack" => pair.data_name = Some(name),
                "rev" | "keep" | "bitmap" => (),
                _ => return Err("unsupported-pack-inventory"),
            }
        }
        if pairs.len() > PAIR_LIMIT {
            return Err("pack-pair-cap");
        }
        let (mut packs, pack_table) = pack_table(pairs.len(), work)?;
        for pair in pairs {
            let index_name = pair.index_name.ok_or("pack-pair-missing")?;
            let data_name = pair.data_name.ok_or("pack-pair-missing")?;
            let index = read_pack_file(root, index_name, MAX_INDEX_BYTES, io, work)?;
            let data = read_pack_file(root, data_name, MAX_PACK_BYTES, io, work)?;
            packs.push(Pack::admit(data, &index, pair.id, work)?);
        }
        drop(pair_table);
        let store = Self {
            names,
            packs,
            _pack_table: pack_table,
        };
        store.confirm(root, io, work)?;
        Ok(store)
    }

    // A fresh directory sample complements the initially verified pack bytes.
    fn confirm(
        &self,
        root: &mut ScopeRoot<'_>,
        io: &ReadBudget<'a>,
        work: &mut Work<'a>,
    ) -> PackResult<()> {
        let current = DirectoryRows::capture(root, PACK_DIRECTORY, INVENTORY_LIMIT, io, work)
            .map_err(row_error)?;
        if current.identity != self.names.identity
            || current.rows().len() != self.names.rows().len()
        {
            return Err("inventory-changed");
        }
        for (original, row) in self.names.rows().iter().zip(current.rows()) {
            work.charge(original.name.as_ref().len() + row.name.as_ref().len() + 1)?;
            if original.kind != row.kind || original.name.as_ref() != row.name.as_ref() {
                return Err("inventory-changed");
            }
        }
        work.charge(0)
    }

    fn object(
        &self,
        id: ObjectId,
        kind: Kind,
        work: &mut Work<'a>,
    ) -> PackResult<ReservedBytes<'a>> {
        let mut matches = self.packs.iter().filter(|pack| pack.contains(id));
        let pack = matches.next().ok_or("missing-object")?;
        if matches.next().is_some() {
            return Err("ambiguous-packed-object");
        }
        pack.object(id, kind, 16, work)
    }
}

fn guard_scope<'a>(root: &ScopeRoot<'a>, work: &mut Work<'a>) -> PackResult<ScopeGuard> {
    let paths = root.canonical_paths(work)?;
    let length = paths
        .worktree
        .len()
        .checked_add(paths.git.len())
        .and_then(|n| n.checked_add(paths.state.len()))
        .ok_or("scope-path-cap")?;
    let _joins = work.reserve_lookup(length)?;
    let guard =
        ScopeGuard::for_conventional_layout(Some(paths), false).ok_or("scope-unavailable")?;
    work.charge(0)?;
    Ok(guard)
}

fn qualify_layout(layout: &Layout, origin: StateDirOrigin, work: &mut Work<'_>) -> PackResult<()> {
    work.charge(0)?;
    let root = layout.root.as_std_path();
    let state = layout.state_dir.as_std_path();
    let len = root
        .as_os_str()
        .len()
        .checked_add(state.as_os_str().len())
        .ok_or("scope-path-cap")?;
    if len > 8192 {
        return Err("scope-path-cap");
    }
    work.charge(
        len.checked_mul(4)
            .and_then(|n| n.checked_add(1))
            .ok_or("work-cap")?,
    )?;
    if origin != StateDirOrigin::RepositoryLocal
        || state.strip_prefix(root).ok() != Some(Path::new(STATE_DIR))
    {
        return Err("scope-origin-unavailable");
    }
    Ok(())
}
