//! Observe repository-local attributes for the matched tracked tree.
use super::attribute_policy::validate_admitted_attribute_rules;
use super::buffer::{Reservation, ReservedBytes};
use super::contained::{ContainedRoot, Identity, ReadBudget, ReadError};
use super::filemode::FileModePolicy;
use super::git::{optional, read_error};
use super::pack::Work;
use super::tree_index::{TreeIndexWalk, folded_cmp};
use super::witness_stream::WitnessStream;
use gix::ObjectId;
use std::io::ErrorKind;
use std::mem::size_of;

const RULE_NAME: &str = ".gitattributes";
const INFO_ATTRIBUTES: &str = ".git/info/attributes";
const MAX_POLICY_BYTES: usize = 4_096;
const MAX_PATH_BYTES: usize = 4_096;
const WITNESS_DOMAIN: &[u8] = b"ledgerful/change-evidence/tracked-attributes-witness/v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrackedAttributeError {
    InfoAttributesPresent,
    UnsupportedPolicy,
    InvalidPolicy,
    PolicyChanged,
    PolicyInput(&'static str),
    Budget(&'static str),
    Tracked(&'static str),
}

type AttributeResult<T> = Result<T, TrackedAttributeError>;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct TrackedAttributeWitness([u8; 32]);

impl TrackedAttributeWitness {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ObservedTrackedAttributes {
    pub(crate) files_verified: usize,
    pub(crate) witness: TrackedAttributeWitness,
}

pub(super) struct TrackedAttributeTranscript(WitnessStream);

#[repr(u8)]
enum Record {
    Start = 1,
    InfoPolicy = 2,
    DirectoryPolicy = 3,
    Complete = 4,
}

impl TrackedAttributeTranscript {
    pub(super) fn new(work: &mut Work<'_>) -> AttributeResult<Self> {
        WitnessStream::new(WITNESS_DOMAIN, work)
            .map(Self)
            .map_err(resource_error)
    }

    pub(super) fn start(
        &mut self,
        tree: ObjectId,
        mode: FileModePolicy,
        work: &mut Work<'_>,
    ) -> AttributeResult<()> {
        let platform = if cfg!(windows) { 1 } else { 2 };
        let file_mode = u8::from(matches!(mode, FileModePolicy::Compare));
        self.record(
            Record::Start,
            &[&[platform], tree.as_bytes(), &[file_mode]],
            work,
        )
    }

    pub(super) fn complete(
        mut self,
        files_verified: usize,
        work: &mut Work<'_>,
    ) -> AttributeResult<ObservedTrackedAttributes> {
        self.record(
            Record::Complete,
            &[&(files_verified as u64).to_be_bytes()],
            work,
        )?;
        let witness = TrackedAttributeWitness(self.0.finish(work).map_err(resource_error)?);
        Ok(ObservedTrackedAttributes {
            files_verified,
            witness,
        })
    }

    fn record(
        &mut self,
        tag: Record,
        fields: &[&[u8]],
        work: &mut Work<'_>,
    ) -> AttributeResult<()> {
        self.0
            .record(tag as u8, fields, work)
            .map_err(resource_error)
    }
}

struct Rules<'a> {
    identity: Identity,
    raw: ReservedBytes<'a>,
}

struct DirectoryPolicy<'a, 'tree> {
    directory: &'tree [u8],
    identity: Identity,
    rules: Option<Rules<'a>>,
}

pub(super) struct TrackedAttributes<'a, 'tree> {
    inputs: Vec<DirectoryPolicy<'a, 'tree>>,
    _frames: Reservation<'a>,
}

pub(super) struct AttributeRead<'observation, 'a> {
    pub(super) root: &'observation ContainedRoot,
    pub(super) io: &'observation mut ReadBudget<'a>,
    pub(super) work: &'observation mut Work<'a>,
}

impl<'a, 'tree> TrackedAttributes<'a, 'tree> {
    pub(super) fn capture(
        walk: &'tree TreeIndexWalk<'a>,
        read: &mut AttributeRead<'_, 'a>,
    ) -> AttributeResult<Self> {
        read.require_no_info_attributes()?;
        let count = 1 + walk.directory_count();
        let table_bytes = count * size_of::<DirectoryPolicy<'a, 'tree>>();
        if read
            .work
            .read_limit(table_bytes, 0)
            .map_err(resource_error)?
            != table_bytes
        {
            return Err(TrackedAttributeError::Budget("memory-cap"));
        }
        let frames = read
            .work
            .memory_budget()
            .reserve(table_bytes)
            .map_err(|_| TrackedAttributeError::Budget("memory-cap"))?;
        let mut inputs = Vec::new();
        inputs
            .try_reserve_exact(count)
            .map_err(|_| TrackedAttributeError::Budget("allocation"))?;
        if inputs.capacity() != count {
            return Err(TrackedAttributeError::Budget("memory-cap"));
        }
        let directories = walk
            .tracked_directories(read.work)
            .map_err(resource_error)?;
        let mut filename = [0; MAX_PATH_BYTES];
        for directory in std::iter::once(b"".as_slice()).chain(directories) {
            let sample = read.directory_policy(directory, &mut filename)?;
            if let Some(rules) = &sample.rules {
                validate_admitted_attribute_rules(rules.raw.as_ref(), read.work)
                    .map_err(validation_error)?;
            }
            inputs.push(sample);
        }
        read.work.charge(0).map_err(resource_error)?;
        Ok(Self {
            inputs,
            _frames: frames,
        })
    }

    pub(super) fn confirm(&self, read: &mut AttributeRead<'_, 'a>) -> AttributeResult<()> {
        let mut filename = [0; MAX_PATH_BYTES];
        for original in &self.inputs {
            let current = read.directory_policy(original.directory, &mut filename)?;
            if original.identity != current.identity {
                return Err(TrackedAttributeError::PolicyChanged);
            }
            let content = |rules: Option<&Rules<'a>>| rules.map(|rules| rules.identity);
            if content(original.rules.as_ref()) != content(current.rules.as_ref()) {
                return Err(TrackedAttributeError::PolicyChanged);
            }
            if let Some(original) = &original.rules {
                let current = current
                    .rules
                    .as_ref()
                    .ok_or(TrackedAttributeError::PolicyChanged)?;
                read.work
                    .charge(original.raw.as_ref().len())
                    .map_err(resource_error)?;
                if original.raw.as_ref() != current.raw.as_ref() {
                    return Err(TrackedAttributeError::PolicyChanged);
                }
            }
        }
        read.require_no_info_attributes()?;
        read.work.charge(0).map_err(resource_error)
    }

    /// Stream already captured and rechecked policy inputs without copying rule data.
    pub(super) fn write_witness(
        &self,
        transcript: &mut TrackedAttributeTranscript,
        work: &mut Work<'a>,
    ) -> AttributeResult<()> {
        transcript.record(
            Record::InfoPolicy,
            &[INFO_ATTRIBUTES.as_bytes(), &[0]],
            work,
        )?;
        for input in &self.inputs {
            let presence = [u8::from(input.rules.is_some())];
            let identity = input
                .rules
                .as_ref()
                .map(|rules| rules.identity.inventory_bytes());
            let raw = input
                .rules
                .as_ref()
                .map_or(&[][..], |rules| rules.raw.as_ref());
            transcript.record(
                Record::DirectoryPolicy,
                &[
                    input.directory,
                    &input.identity.inventory_bytes(),
                    RULE_NAME.as_bytes(),
                    &presence,
                    identity.as_ref().map_or(&[], <[u8; 24]>::as_slice),
                    raw,
                ],
                work,
            )?;
        }
        Ok(())
    }

    /// Supply the exact captured bytes when this tracked input is itself a policy file.
    pub(super) fn captured_tracked_rule(
        &self,
        path: &str,
        work: &mut Work<'a>,
    ) -> Result<Option<&[u8]>, &'static str> {
        work.charge(path.len())?;
        let (directory, name) = path.rsplit_once('/').unwrap_or(("", path));
        if name != RULE_NAME {
            return Ok(None);
        }
        let directory = directory.as_bytes();
        let comparisons = (usize::BITS - self.inputs.len().leading_zeros()) as usize + 1;
        // The matched tree iterator supplies the same folded path order.
        work.charge(comparisons * (directory.len() + 1))?;
        let index = self
            .inputs
            .binary_search_by(|input| folded_cmp(input.directory, directory))
            .map_err(|_| "attributes-parent-unobserved")?;
        let input = &self.inputs[index];
        if input.directory != directory {
            return Err("attributes-parent-unobserved");
        }
        let rules = input.rules.as_ref().ok_or("worktree-file-missing")?;
        Ok(Some(rules.raw.as_ref()))
    }
}

impl<'a> AttributeRead<'_, 'a> {
    fn require_no_info_attributes(&mut self) -> AttributeResult<()> {
        if self.optional_identity(INFO_ATTRIBUTES)?.is_some() {
            return Err(TrackedAttributeError::InfoAttributesPresent);
        }
        Ok(())
    }

    fn optional_identity(&mut self, path: &str) -> AttributeResult<Option<Identity>> {
        let _lookup = self
            .work
            .reserve_lookup(path.len())
            .map_err(resource_error)?;
        match self.root.observe_entry(path, false, self.io) {
            Ok(identity) => Ok(Some(identity)),
            Err(ReadError::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(input_error(read_error(error))),
        }
    }

    fn directory_identity(&mut self, directory: &str) -> AttributeResult<Identity> {
        if directory.is_empty() {
            return Ok(self.root.identity());
        }
        let _lookup = self
            .work
            .reserve_lookup(directory.len())
            .map_err(resource_error)?;
        self.root
            .observe_entry(directory, true, self.io)
            .map_err(|error| input_error(read_error(error)))
    }

    fn directory_policy<'tree>(
        &mut self,
        directory: &'tree [u8],
        filename: &mut [u8; MAX_PATH_BYTES],
    ) -> AttributeResult<DirectoryPolicy<'a, 'tree>> {
        let len = directory.len() + usize::from(!directory.is_empty()) + RULE_NAME.len();
        if len > MAX_PATH_BYTES {
            return Err(TrackedAttributeError::PolicyInput("attributes-path-cap"));
        }
        self.work.charge(3 * len).map_err(resource_error)?;
        let directory_text = std::str::from_utf8(directory)
            .map_err(|_| TrackedAttributeError::PolicyInput("attributes-path"))?;
        let identity = self.directory_identity(directory_text)?;
        let prefix = if directory.is_empty() {
            0
        } else {
            filename[..directory.len()].copy_from_slice(directory);
            filename[directory.len()] = b'/';
            directory.len() + 1
        };
        filename[prefix..len].copy_from_slice(RULE_NAME.as_bytes());
        let path = std::str::from_utf8(&filename[..len])
            .map_err(|_| TrackedAttributeError::PolicyInput("attributes-path"))?;
        let before = self.optional_identity(path)?;
        let raw = {
            let _lookup = self
                .work
                .reserve_lookup(path.len())
                .map_err(resource_error)?;
            optional(self.root, path, MAX_POLICY_BYTES, self.io, self.work).map_err(input_error)?
        };
        let after = self.optional_identity(path)?;
        if before != after || identity != self.directory_identity(directory_text)? {
            return Err(TrackedAttributeError::PolicyChanged);
        }
        let rules = match (after, raw) {
            (Some(identity), Some(raw)) => Some(Rules { identity, raw }),
            (None, None) => None,
            _ => return Err(TrackedAttributeError::PolicyChanged),
        };
        Ok(DirectoryPolicy {
            directory,
            identity,
            rules,
        })
    }
}

fn validation_error(reason: &'static str) -> TrackedAttributeError {
    match reason {
        "unsupported" => TrackedAttributeError::UnsupportedPolicy,
        "input-cap" | "memory-cap" | "work-cap" | "deadline" => resource_error(reason),
        _ => TrackedAttributeError::InvalidPolicy,
    }
}

fn input_error(reason: &'static str) -> TrackedAttributeError {
    match reason {
        "stored-bytes-cap" | "aggregate-cap" | "memory-cap" | "work-cap" | "deadline" => {
            resource_error(reason)
        }
        "input-changed" => TrackedAttributeError::PolicyChanged,
        _ => TrackedAttributeError::PolicyInput(reason),
    }
}

fn resource_error(reason: &'static str) -> TrackedAttributeError {
    TrackedAttributeError::Budget(reason)
}
