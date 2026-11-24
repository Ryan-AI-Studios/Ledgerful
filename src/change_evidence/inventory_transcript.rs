//! Private streamed fingerprint for one completed inventory and its local ignore policy.
use super::contained::Identity;
use super::inventory::{InventoryError, InventoryPass, reason};
use super::pack::Work;
use super::witness_stream::WitnessStream;
use gix::ObjectId;

const DOMAIN: &[u8] = b"ledgerful/change-evidence/inventory-transcript/v1\0";
type TranscriptResult<T> = Result<T, InventoryError>;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct InventoryWitness([u8; 32]);

impl InventoryWitness {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ObservedInventory {
    pub(crate) counts: InventoryPass,
    pub(crate) witness: InventoryWitness,
}

pub(super) struct InventoryTranscript(WitnessStream);

#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum RowRole {
    GitDirectory = 1,
    StateDirectory = 2,
    TrackedFile = 3,
    VisibleUntrackedFile = 4,
    IgnoredUntrackedFile = 5,
    TrackedDirectory = 6,
    VisibleUntrackedDirectory = 7,
    PrunedTrackedDirectory = 8,
    PrunedUntrackedDirectory = 9,
}

#[repr(u8)]
enum Record {
    Start = 1,
    Policy = 2,
    Directory = 3,
    Row = 4,
    DirectoryEnd = 5,
    Complete = 6,
}

impl InventoryTranscript {
    pub(super) fn new(work: &mut Work<'_>) -> TranscriptResult<Self> {
        WitnessStream::new(DOMAIN, work).map(Self).map_err(reason)
    }

    pub(super) fn start(
        &mut self,
        tree: ObjectId,
        config: &[u8],
        ignore_case: Option<bool>,
        work: &mut Work<'_>,
    ) -> TranscriptResult<()> {
        let platform = if cfg!(windows) { 1 } else { 2 };
        let case = match ignore_case {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        };
        self.record(
            Record::Start,
            &[&[platform], tree.as_bytes(), &[case], config],
            work,
        )
    }

    pub(super) fn policy(
        &mut self,
        directory: &[u8],
        source: &[u8],
        raw: Option<&[u8]>,
        identity: Option<Identity>,
        work: &mut Work<'_>,
    ) -> TranscriptResult<()> {
        let presence = [u8::from(raw.is_some())];
        let id = identity.map(Identity::inventory_bytes);
        self.record(
            Record::Policy,
            &[
                directory,
                source,
                &presence,
                id.as_ref().map_or(&[], <[u8; 24]>::as_slice),
                raw.unwrap_or_default(),
            ],
            work,
        )
    }

    pub(super) fn directory(
        &mut self,
        path: &[u8],
        identity: Identity,
        work: &mut Work<'_>,
    ) -> TranscriptResult<()> {
        self.record(
            Record::Directory,
            &[path, &identity.inventory_bytes()],
            work,
        )
    }

    pub(super) fn row(
        &mut self,
        name: &[u8],
        role: RowRole,
        identity: Identity,
        work: &mut Work<'_>,
    ) -> TranscriptResult<()> {
        self.record(
            Record::Row,
            &[name, &[role as u8], &identity.inventory_bytes()],
            work,
        )
    }

    pub(super) fn directory_end(
        &mut self,
        entries: usize,
        work: &mut Work<'_>,
    ) -> TranscriptResult<()> {
        self.record(
            Record::DirectoryEnd,
            &[&(entries as u64).to_be_bytes()],
            work,
        )
    }

    pub(super) fn complete(
        mut self,
        counts: InventoryPass,
        work: &mut Work<'_>,
    ) -> TranscriptResult<ObservedInventory> {
        let visited = (counts.directories_visited as u64).to_be_bytes();
        let pruned = (counts.directories_pruned as u64).to_be_bytes();
        let entries = (counts.entries_observed as u64).to_be_bytes();
        let untracked = (counts.untracked_files as u64).to_be_bytes();
        self.record(
            Record::Complete,
            &[&visited, &pruned, &entries, &untracked],
            work,
        )?;
        let witness = InventoryWitness(self.0.finish(work).map_err(reason)?);
        Ok(ObservedInventory { counts, witness })
    }

    fn record(
        &mut self,
        tag: Record,
        fields: &[&[u8]],
        work: &mut Work<'_>,
    ) -> TranscriptResult<()> {
        self.0.record(tag as u8, fields, work).map_err(reason)
    }
}
