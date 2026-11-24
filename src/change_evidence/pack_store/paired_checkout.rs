//! Compare two independent native checkout observations on one request budget.
#[cfg(any(windows, target_os = "linux"))]
use super::super::baseline_discovery::{self, RetainedDiscovery};
use super::super::git::{ObservedRefs, resolve_paired_head};
use super::super::tracked_attributes::ObservedTrackedAttributes;
use super::capture_budget::CapturePhase;
use super::index_inputs::MatchedIndexInputs;
use super::{
    CommitPair, InventoryError, InventoryLimits, Layout, MemoryBudget, Observation,
    ObservedInventory, PackResult, ReservedBytes, ScopeGuard, ScopeRoot, StateDirOrigin,
    TrackedAttributeError, Work, guard_scope,
};
use crate::commands::baseline::extract::capture_snapshot::CaptureSnapshot;
use gix::ObjectId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(crate) struct CheckoutRequest<'a> {
    pub(crate) layout: &'a Layout,
    pub(crate) origin: StateDirOrigin,
    pub(crate) head: ObjectId,
    pub(crate) parent: ObjectId,
}

#[derive(Clone, Copy)]
pub(crate) struct HeadCheckoutRequest<'a> {
    pub(crate) layout: &'a Layout,
    pub(crate) origin: StateDirOrigin,
    pub(crate) head: ObjectId,
}

#[derive(Clone, Copy)]
pub(crate) struct CheckoutLimits {
    pub(crate) inventory: InventoryLimits,
    pub(crate) read_bytes: usize,
    pub(crate) work_units: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckoutInput {
    Scope,
    Revision,
    ParentTree,
    Refs,
    GitConfig,
    StagedIndex,
    IndexedInputs,
    #[cfg(any(windows, target_os = "linux"))]
    DefaultDiscovery,
    Packs,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CheckoutPairError {
    Deadline,
    Interrupted,
    Budget(&'static str),
    Input(CheckoutInput, &'static str),
    Inventory(InventoryError),
    Tracked(TrackedAttributeError),
    Changed(CheckoutInput),
    InventoryChanged,
    TrackedAttributesChanged,
    VisibleUntracked,
}

type PairResult<T> = Result<T, CheckoutPairError>;

/// The two bounded samples agreed. This does not carry an opaque stored scope ID.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PairedCheckoutProof {
    guard: ScopeGuard,
    commits: CommitPair,
}

impl PairedCheckoutProof {
    pub(crate) fn guard(&self) -> &ScopeGuard {
        &self.guard
    }

    pub(crate) fn commits(&self) -> &CommitPair {
        &self.commits
    }
}

/// A paired sample of one exact HEAD. This carries no parent or selectable pair.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PairedHeadProof {
    guard: ScopeGuard,
    head: HeadFacts,
}

impl PairedHeadProof {
    pub(crate) fn guard(&self) -> &ScopeGuard {
        &self.guard
    }

    pub(crate) fn head(&self) -> ObjectId {
        self.head.id
    }

    pub(crate) fn tree(&self) -> ObjectId {
        self.head.tree
    }
}

#[derive(Debug, Eq, PartialEq)]
struct HeadFacts {
    id: ObjectId,
    tree: ObjectId,
}

trait RevisionRequirement: Copy {
    type Facts: Eq;

    fn verify(self, head: ObjectId, observation: &mut Observation<'_>) -> PairResult<Self::Facts>;
    fn head_tree(facts: &Self::Facts) -> ObjectId;

    fn differing_input(_: &Self::Facts, _: &Self::Facts) -> CheckoutInput {
        CheckoutInput::Revision
    }
}

#[derive(Clone, Copy)]
struct ExactHead;

impl RevisionRequirement for ExactHead {
    type Facts = HeadFacts;

    fn verify(self, head: ObjectId, observation: &mut Observation<'_>) -> PairResult<HeadFacts> {
        let commit = checked(CheckoutInput::Revision, observation.commit_facts(head))?;
        Ok(HeadFacts {
            id: head,
            tree: commit.tree,
        })
    }

    fn head_tree(facts: &HeadFacts) -> ObjectId {
        facts.tree
    }
}

#[derive(Clone, Copy)]
struct IndexedHead<'a>(&'a CaptureSnapshot);

#[derive(Debug, Eq, PartialEq)]
struct IndexedHeadFacts {
    head: HeadFacts,
    inputs: MatchedIndexInputs,
}

impl RevisionRequirement for IndexedHead<'_> {
    type Facts = IndexedHeadFacts;

    fn verify(self, head: ObjectId, observation: &mut Observation<'_>) -> PairResult<Self::Facts> {
        let head = ExactHead.verify(head, observation)?;
        let inputs = checked(
            CheckoutInput::IndexedInputs,
            observation.match_index_inputs(head.id, head.tree, self.0),
        )?;
        Ok(IndexedHeadFacts { head, inputs })
    }

    fn head_tree(facts: &Self::Facts) -> ObjectId {
        facts.head.tree
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Clone, Copy)]
struct DefaultIndexedHead<'a> {
    snapshot: &'a CaptureSnapshot,
    #[cfg(target_os = "linux")]
    layout_root: &'a std::path::Path,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Eq, PartialEq)]
struct DefaultIndexedHeadFacts {
    indexed: IndexedHeadFacts,
    discovery: RetainedDiscovery,
}

#[cfg(any(windows, target_os = "linux"))]
impl RevisionRequirement for DefaultIndexedHead<'_> {
    type Facts = DefaultIndexedHeadFacts;

    fn verify(self, head: ObjectId, observation: &mut Observation<'_>) -> PairResult<Self::Facts> {
        let indexed = IndexedHead(self.snapshot).verify(head, observation)?;
        let discovery = checked(
            CheckoutInput::DefaultDiscovery,
            baseline_discovery::observe(
                &mut observation.root,
                &mut observation.io,
                &mut observation.work,
                #[cfg(target_os = "linux")]
                self.layout_root,
                self.snapshot,
            ),
        )?;
        checked(
            CheckoutInput::DefaultDiscovery,
            discovery.membership.require_exact(),
        )?;
        Ok(DefaultIndexedHeadFacts { indexed, discovery })
    }

    fn head_tree(facts: &Self::Facts) -> ObjectId {
        facts.indexed.head.tree
    }

    fn differing_input(first: &Self::Facts, second: &Self::Facts) -> CheckoutInput {
        if first.indexed == second.indexed {
            CheckoutInput::DefaultDiscovery
        } else {
            CheckoutInput::Revision
        }
    }
}

#[derive(Clone, Copy)]
struct ExactPair(ObjectId);

impl RevisionRequirement for ExactPair {
    type Facts = CommitPair;

    fn verify(self, head: ObjectId, observation: &mut Observation<'_>) -> PairResult<CommitPair> {
        let commits = checked(
            CheckoutInput::Revision,
            observation.observe_commit_pair(head, self.0),
        )?;
        // Resolve the parent's actual root tree, check kind/hash, and finish the
        // bounded existing tree decoder in each independent pass.
        checked(
            CheckoutInput::ParentTree,
            observation.visit_tree(commits.parent_tree, |_| Ok(())),
        )?;
        Ok(commits)
    }

    fn head_tree(facts: &CommitPair) -> ObjectId {
        facts.head_tree
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Clone, Copy)]
struct PreparePair<'a> {
    parent: ObjectId,
    snapshot: &'a CaptureSnapshot,
    #[cfg(target_os = "linux")]
    layout_root: &'a std::path::Path,
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Eq, PartialEq)]
enum PrepareIndexInputs {
    Exact(MatchedIndexInputs),
    ContentDrift,
    Unavailable(&'static str),
}

#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Eq, PartialEq)]
struct PrepareFacts {
    commits: CommitPair,
    discovery: RetainedDiscovery,
    indexed: PrepareIndexInputs,
}

#[cfg(any(windows, target_os = "linux"))]
impl RevisionRequirement for PreparePair<'_> {
    type Facts = PrepareFacts;

    fn verify(self, head: ObjectId, observation: &mut Observation<'_>) -> PairResult<PrepareFacts> {
        let commits = ExactPair(self.parent).verify(head, observation)?;
        let discovery = checked(
            CheckoutInput::DefaultDiscovery,
            baseline_discovery::observe(
                &mut observation.root,
                &mut observation.io,
                &mut observation.work,
                #[cfg(target_os = "linux")]
                self.layout_root,
                self.snapshot,
            ),
        )?;
        let indexed = if discovery.membership.require_exact().is_err() {
            // An admitted, complete discovery has a member absent on one side.
            // The canonical legacy drift rule counts either kind of difference.
            PrepareIndexInputs::ContentDrift
        } else {
            match observation.match_index_inputs(head, commits.head_tree, self.snapshot) {
                Ok(inputs) => PrepareIndexInputs::Exact(inputs),
                Err("index-generation-mismatch" | "index-input-stamp-mismatch")
                    if checked(
                        CheckoutInput::IndexedInputs,
                        observation
                            .has_confirmed_index_content_drift(commits.head_tree, self.snapshot),
                    )? =>
                {
                    PrepareIndexInputs::ContentDrift
                }
                Err(
                    reason @ ("index-generation-mismatch"
                    | "index-parser-version-mismatch"
                    | "index-input-stamp-missing"
                    | "index-input-stamp-mismatch"
                    | "index-input-hash-mismatch"
                    | "unsupported-input-encoding"),
                ) => PrepareIndexInputs::Unavailable(reason),
                Err(reason) => return checked(CheckoutInput::IndexedInputs, Err(reason)),
            }
        };
        Ok(PrepareFacts {
            commits,
            discovery,
            indexed,
        })
    }

    fn head_tree(facts: &PrepareFacts) -> ObjectId {
        facts.commits.head_tree
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub(crate) struct PairedPrepareProof {
    guard: ScopeGuard,
    facts: PrepareFacts,
}

#[cfg(any(windows, target_os = "linux"))]
impl PairedPrepareProof {
    pub(crate) fn head(&self) -> ObjectId {
        self.facts.commits.head
    }

    pub(super) fn guard(&self) -> &ScopeGuard {
        &self.guard
    }

    pub(super) fn config_digest(&self) -> blake3::Hash {
        self.facts.discovery.config_digest
    }

    pub(crate) fn stale_threshold_days(&self) -> u64 {
        self.facts.discovery.stale_threshold_days
    }

    pub(super) fn discovery_fingerprint(&self) -> [u8; 32] {
        self.facts.discovery.witness
    }

    pub(crate) fn content_drift(&self) -> bool {
        self.facts.indexed == PrepareIndexInputs::ContentDrift
    }

    pub(crate) fn input_unavailable(&self) -> Option<&'static str> {
        match self.facts.indexed {
            PrepareIndexInputs::Unavailable(reason) => Some(reason),
            _ => None,
        }
    }

    pub(crate) fn verified_input(&self) -> Option<(usize, blake3::Hash)> {
        match self.facts.indexed {
            PrepareIndexInputs::Exact(ref study) => Some((study.rows, study.digest)),
            _ => None,
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub(super) fn observe_prepare_pair(
    request: CheckoutRequest<'_>,
    snapshot: &CaptureSnapshot,
    limits: CheckoutLimits,
    phase: CapturePhase<'_>,
) -> PairResult<PairedPrepareProof> {
    let (guard, facts) = FirstSample::begin(
        HeadCheckoutRequest {
            layout: request.layout,
            origin: request.origin,
            head: request.head,
        },
        PreparePair {
            parent: request.parent,
            snapshot,
            #[cfg(target_os = "linux")]
            layout_root: request.layout.root.as_std_path(),
        },
        limits,
        CheckoutControl {
            deadline: phase.deadline,
            interrupted: phase.interrupted,
            phase: Some(phase),
        },
        phase.memory,
    )?
    .compare_next()?;
    Ok(PairedPrepareProof { guard, facts })
}

#[derive(Clone, Copy)]
struct CheckoutControl<'a> {
    deadline: Instant,
    interrupted: &'a AtomicBool,
    phase: Option<CapturePhase<'a>>,
}

impl CheckoutControl<'_> {
    fn check(self) -> PairResult<()> {
        if self.interrupted.load(Ordering::Acquire) {
            return Err(CheckoutPairError::Interrupted);
        }
        if Instant::now() >= self.deadline {
            return Err(CheckoutPairError::Deadline);
        }
        Ok(())
    }
}

struct CheckoutSample<'a, T> {
    guard: ScopeGuard,
    revision: T,
    refs: ObservedRefs<'a>,
    inventory: ObservedInventory,
    tracked_attributes: ObservedTrackedAttributes,
}

/// Keep the first actual owner and raw samples live while acquiring the second.
pub(crate) struct FirstCheckoutPass<'a>(FirstSample<'a, ExactPair>);

impl<'a> FirstCheckoutPass<'a> {
    pub(crate) fn begin(
        request: CheckoutRequest<'a>,
        limits: CheckoutLimits,
        phase: CapturePhase<'a>,
    ) -> PairResult<Self> {
        FirstSample::begin(
            HeadCheckoutRequest {
                layout: request.layout,
                origin: request.origin,
                head: request.head,
            },
            ExactPair(request.parent),
            limits,
            CheckoutControl {
                deadline: phase.deadline,
                interrupted: phase.interrupted,
                phase: Some(phase),
            },
            phase.memory,
        )
        .map(Self)
    }

    pub(crate) fn compare_next(self) -> PairResult<PairedCheckoutProof> {
        let (guard, commits) = self.0.compare_next()?;
        Ok(PairedCheckoutProof { guard, commits })
    }
}

pub(crate) struct FirstHeadPass<'a>(FirstSample<'a, ExactHead>);

impl<'a> FirstHeadPass<'a> {
    pub(crate) fn begin(
        request: HeadCheckoutRequest<'a>,
        limits: CheckoutLimits,
        deadline: Instant,
        interrupted: &'a AtomicBool,
        memory: &'a MemoryBudget,
    ) -> PairResult<Self> {
        FirstSample::begin(
            request,
            ExactHead,
            limits,
            CheckoutControl {
                deadline,
                interrupted,
                phase: None,
            },
            memory,
        )
        .map(Self)
    }

    pub(crate) fn compare_next(self) -> PairResult<PairedHeadProof> {
        let (guard, head) = self.0.compare_next()?;
        Ok(PairedHeadProof { guard, head })
    }
}

/// A paired exact-HEAD sample with independently matched indexed witnesses.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PairedIndexedHead {
    guard: ScopeGuard,
    facts: IndexedHeadFacts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IndexMembership {
    DefaultDiscoveryUnqualified,
}

impl PairedIndexedHead {
    pub(crate) fn head(&self) -> ObjectId {
        self.facts.head.id
    }

    pub(crate) fn verified_witnesses(&self) -> usize {
        self.facts.inputs.rows
    }

    pub(crate) fn input_digest(&self) -> blake3::Hash {
        self.facts.inputs.digest
    }

    pub(crate) fn membership(&self) -> IndexMembership {
        IndexMembership::DefaultDiscoveryUnqualified
    }
}

/// Keep the same owned SQLite snapshot borrowed through both native samples.
pub(crate) struct FirstIndexedHeadPass<'a, 'sql>(FirstSample<'a, IndexedHead<'sql>>);

impl<'a, 'sql> FirstIndexedHeadPass<'a, 'sql> {
    pub(crate) fn begin(
        request: HeadCheckoutRequest<'a>,
        snapshot: &'sql CaptureSnapshot,
        limits: CheckoutLimits,
        deadline: Instant,
        interrupted: &'a AtomicBool,
        memory: &'a MemoryBudget,
    ) -> PairResult<Self> {
        FirstSample::begin(
            request,
            IndexedHead(snapshot),
            limits,
            CheckoutControl {
                deadline,
                interrupted,
                phase: None,
            },
            memory,
        )
        .map(Self)
    }

    pub(crate) fn compare_next(self) -> PairResult<PairedIndexedHead> {
        let (guard, facts) = self.0.compare_next()?;
        Ok(PairedIndexedHead { guard, facts })
    }
}

/// A paired exact-HEAD sample with a qualified Windows default membership set.
#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PairedDefaultIndexedHead {
    guard: ScopeGuard,
    facts: DefaultIndexedHeadFacts,
}

#[cfg(any(windows, target_os = "linux"))]
impl PairedDefaultIndexedHead {
    pub(super) fn guard(&self) -> &ScopeGuard {
        &self.guard
    }

    pub(crate) fn head(&self) -> ObjectId {
        self.facts.indexed.head.id
    }

    pub(super) fn config_digest(&self) -> blake3::Hash {
        self.facts.discovery.config_digest
    }

    pub(crate) fn verified_witnesses(&self) -> usize {
        self.facts
            .discovery
            .membership
            .require_exact()
            .expect("exact admission")
    }

    pub(crate) fn stale_threshold_days(&self) -> u64 {
        self.facts.discovery.stale_threshold_days
    }

    pub(crate) fn input_digest(&self) -> blake3::Hash {
        self.facts.indexed.inputs.digest
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub(crate) struct FirstDefaultIndexedHeadPass<'a, 'sql>(FirstSample<'a, DefaultIndexedHead<'sql>>);

#[cfg(any(windows, target_os = "linux"))]
impl<'a, 'sql> FirstDefaultIndexedHeadPass<'a, 'sql>
where
    'a: 'sql,
{
    pub(crate) fn begin(
        request: HeadCheckoutRequest<'a>,
        snapshot: &'sql CaptureSnapshot,
        limits: CheckoutLimits,
        deadline: Instant,
        interrupted: &'a AtomicBool,
        memory: &'a MemoryBudget,
    ) -> PairResult<Self> {
        FirstSample::begin(
            request,
            DefaultIndexedHead {
                snapshot,
                #[cfg(target_os = "linux")]
                layout_root: request.layout.root.as_std_path(),
            },
            limits,
            CheckoutControl {
                deadline,
                interrupted,
                phase: None,
            },
            memory,
        )
        .map(Self)
    }

    pub(super) fn begin_metered(
        request: HeadCheckoutRequest<'a>,
        snapshot: &'sql CaptureSnapshot,
        limits: CheckoutLimits,
        phase: CapturePhase<'a>,
    ) -> PairResult<Self> {
        FirstSample::begin(
            request,
            DefaultIndexedHead {
                snapshot,
                #[cfg(target_os = "linux")]
                layout_root: request.layout.root.as_std_path(),
            },
            limits,
            CheckoutControl {
                deadline: phase.deadline,
                interrupted: phase.interrupted,
                phase: Some(phase),
            },
            phase.memory,
        )
        .map(Self)
    }

    pub(crate) fn compare_next(self) -> PairResult<PairedDefaultIndexedHead> {
        let (guard, facts) = self.0.compare_next()?;
        Ok(PairedDefaultIndexedHead { guard, facts })
    }
}

struct FirstSample<'a, R: RevisionRequirement> {
    request: HeadCheckoutRequest<'a>,
    requirement: R,
    limits: InventoryLimits,
    control: CheckoutControl<'a>,
    observation: Observation<'a>,
    first: CheckoutSample<'a, R::Facts>,
}

impl<'a, R: RevisionRequirement> FirstSample<'a, R> {
    fn begin(
        request: HeadCheckoutRequest<'a>,
        requirement: R,
        limits: CheckoutLimits,
        control: CheckoutControl<'a>,
        memory: &'a MemoryBudget,
    ) -> PairResult<Self> {
        control.check()?;
        let result = (|| {
            let mut observation = checked(
                CheckoutInput::Scope,
                match control.phase {
                    Some(phase) => Observation::open_scoped_metered(
                        request.layout,
                        request.origin,
                        limits,
                        phase,
                    ),
                    None => Observation::open_scoped(
                        request.layout,
                        request.origin,
                        control.deadline,
                        control.interrupted,
                        limits.read_bytes,
                        limits.work_units,
                        memory,
                    ),
                },
            )?;
            let first = sample(
                &mut observation,
                request.head,
                requirement,
                limits.inventory,
            )?;
            Ok(Self {
                request,
                requirement,
                limits: limits.inventory,
                control,
                observation,
                first,
            })
        })();
        // Some native scope readers return a generic unavailable indication
        // when an in-flight operation sees the same deadline/interrupt flag.
        control.check()?;
        result
    }

    fn compare_next(self) -> PairResult<(ScopeGuard, R::Facts)> {
        let control = self.control;
        control.check()?;
        let result = self.compare_retained();
        control.check()?;
        result
    }

    fn compare_retained(self) -> PairResult<(ScopeGuard, R::Facts)> {
        let Self {
            request,
            requirement,
            limits,
            mut observation,
            first,
            ..
        } = self;
        let (first_index, first_config) = take_exact_inputs(&mut observation)?;
        // Each pass qualifies its own mutable and packed inputs. Dropping the
        // old pack releases its decoded table reservations before the next pass.
        observation.root_ignore = None;
        observation.compiled_root_ignore = None;
        observation.packs = None;
        let path = request.layout.root.as_std_path();
        let next_root = checked(
            CheckoutInput::Scope,
            ScopeRoot::open_scoped(path, &observation.io, &mut observation.work),
        )?;
        confirm_same_scope(&observation.root, &next_root, &mut observation.work)?;
        let first_root = std::mem::replace(&mut observation.root, next_root);
        let second = sample(&mut observation, request.head, requirement, limits)?;

        // These four detached values contain only fixed-size facts/digests.
        let fixed = size_of::<ScopeGuard>()
            + size_of::<R::Facts>()
            + size_of::<ObservedInventory>()
            + size_of::<ObservedTrackedAttributes>();
        observation
            .work
            .charge(2 * fixed)
            .map_err(CheckoutPairError::Budget)?;
        if first.guard != second.guard {
            return Err(CheckoutPairError::Changed(CheckoutInput::Scope));
        }
        if first.revision != second.revision {
            return Err(CheckoutPairError::Changed(R::differing_input(
                &first.revision,
                &second.revision,
            )));
        }
        checked(
            CheckoutInput::Refs,
            first
                .refs
                .confirm_same_inputs(&second.refs, &mut observation.work),
        )?;
        compare_exact_inputs(&mut observation, &first_index, &first_config)?;
        if first.inventory != second.inventory {
            return Err(CheckoutPairError::InventoryChanged);
        }
        if first.tracked_attributes != second.tracked_attributes {
            return Err(CheckoutPairError::TrackedAttributesChanged);
        }
        if second.inventory.counts.untracked_files != 0 {
            return Err(CheckoutPairError::VisibleUntracked);
        }

        confirm_same_scope(&first_root, &observation.root, &mut observation.work)?;
        checked(
            CheckoutInput::Scope,
            observation
                .root
                .confirm_reacquired(path, &observation.io, &mut observation.work),
        )?;
        confirm_sources(&mut observation, &second.refs)?;
        observation
            .work
            .charge(0)
            .map_err(CheckoutPairError::Budget)?;
        Ok((second.guard, second.revision))
    }
}

pub(crate) fn observe_paired_checkout(
    request: CheckoutRequest<'_>,
    limits: CheckoutLimits,
    phase: CapturePhase<'_>,
) -> PairResult<PairedCheckoutProof> {
    FirstCheckoutPass::begin(request, limits, phase)?.compare_next()
}

pub(crate) fn observe_paired_head(
    request: HeadCheckoutRequest<'_>,
    limits: CheckoutLimits,
    deadline: Instant,
    interrupted: &AtomicBool,
    memory: &MemoryBudget,
) -> PairResult<PairedHeadProof> {
    FirstHeadPass::begin(request, limits, deadline, interrupted, memory)?.compare_next()
}

/// Shared native-read phase for the private explicit capture handoff.
#[allow(
    dead_code,
    reason = "Real proof acquisition is wired in the next CLI phase"
)]
pub(crate) fn observe_capture_head(
    request: HeadCheckoutRequest<'_>,
    limits: CheckoutLimits,
    phase: CapturePhase<'_>,
) -> PairResult<PairedHeadProof> {
    let (guard, head) = FirstSample::begin(
        request,
        ExactHead,
        limits,
        CheckoutControl {
            deadline: phase.deadline,
            interrupted: phase.interrupted,
            phase: Some(phase),
        },
        phase.memory,
    )?
    .compare_next()?;
    Ok(PairedHeadProof { guard, head })
}

fn checked<T>(input: CheckoutInput, result: PackResult<T>) -> PairResult<T> {
    result.map_err(|reason| match reason {
        "work-cap" | "memory-cap" | "allocation" | "deadline" | "aggregate-cap"
        | "pack-read-cap" => CheckoutPairError::Budget(reason),
        "scope-changed"
        | "refs-changed"
        | "staged-index-changed"
        | "git-config-changed"
        | "input-changed"
        | "inventory-changed" => CheckoutPairError::Changed(input),
        _ => CheckoutPairError::Input(input, reason),
    })
}

fn sample<'a, R: RevisionRequirement>(
    observation: &mut Observation<'a>,
    expected_head: ObjectId,
    requirement: R,
    limits: InventoryLimits,
) -> PairResult<CheckoutSample<'a, R::Facts>> {
    let guard = checked(
        CheckoutInput::Scope,
        guard_scope(&observation.root, &mut observation.work),
    )?;
    let (head, refs) = checked(
        CheckoutInput::Refs,
        resolve_paired_head(
            &observation.root,
            &mut observation.io,
            &mut observation.work,
        ),
    )?;
    if head != expected_head {
        return Err(CheckoutPairError::Input(
            CheckoutInput::Revision,
            "head-mismatch",
        ));
    }
    let revision = requirement.verify(head, observation)?;
    let tree = R::head_tree(&revision);
    let inventory = observation
        .observe_worktree_inventory_witness(tree, limits)
        .map_err(CheckoutPairError::Inventory)?;
    let tracked_attributes = observation
        .observe_tracked_attribute_witness(tree)
        .map_err(CheckoutPairError::Tracked)?;
    confirm_sources(observation, &refs)?;
    Ok(CheckoutSample {
        guard,
        revision,
        refs,
        inventory,
        tracked_attributes,
    })
}

fn confirm_sources<'a>(
    observation: &mut Observation<'a>,
    refs: &ObservedRefs<'a>,
) -> PairResult<()> {
    checked(CheckoutInput::Packs, observation.confirm_pack_inventory())?;
    checked(
        CheckoutInput::StagedIndex,
        observation.confirm_staged_index(),
    )?;
    checked(
        CheckoutInput::GitConfig,
        observation.confirm_local_git_config(),
    )?;
    checked(
        CheckoutInput::Refs,
        refs.confirm(
            &observation.root,
            &mut observation.io,
            &mut observation.work,
        ),
    )
}

fn confirm_same_scope<'a>(
    first: &ScopeRoot<'a>,
    second: &ScopeRoot<'a>,
    work: &mut Work<'a>,
) -> PairResult<()> {
    checked(CheckoutInput::Scope, first.confirm_same_scope(second, work))
}

fn take_exact_inputs<'a>(
    observation: &mut Observation<'a>,
) -> PairResult<(ReservedBytes<'a>, ReservedBytes<'a>)> {
    let index = checked(
        CheckoutInput::StagedIndex,
        observation
            .staged_index
            .take()
            .unwrap_or(Err("staged-index-unobserved")),
    )?;
    let config = checked(
        CheckoutInput::GitConfig,
        observation
            .git_config
            .take()
            .unwrap_or(Err("git-config-unobserved")),
    )?;
    Ok((index.raw, config.raw))
}

fn compare_exact_inputs(
    observation: &mut Observation<'_>,
    first_index: &ReservedBytes<'_>,
    first_config: &ReservedBytes<'_>,
) -> PairResult<()> {
    let index = checked(
        CheckoutInput::StagedIndex,
        observation
            .staged_index
            .as_ref()
            .ok_or("staged-index-unobserved")
            .and_then(|input| input.as_ref().map_err(|error| *error)),
    )?;
    let config = checked(
        CheckoutInput::GitConfig,
        observation
            .git_config
            .as_ref()
            .ok_or("git-config-unobserved")
            .and_then(|input| input.as_ref().map_err(|error| *error)),
    )?;
    for (kind, first, second) in [
        (
            CheckoutInput::StagedIndex,
            first_index.as_ref(),
            index.raw.as_ref(),
        ),
        (
            CheckoutInput::GitConfig,
            first_config.as_ref(),
            config.raw.as_ref(),
        ),
    ] {
        observation
            .work
            .charge(first.len() + second.len())
            .map_err(CheckoutPairError::Budget)?;
        if first != second {
            return Err(CheckoutPairError::Changed(kind));
        }
    }
    Ok(())
}
