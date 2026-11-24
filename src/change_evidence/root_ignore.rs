//! Observe the two root-level local ignore inputs through the retained checkout.
use super::buffer::ReservedBytes;
use super::contained::{ContainedRoot, ReadBudget};
use super::git::optional;
use super::pack::Work;

type IgnoreResult<T> = Result<T, &'static str>;
const ROOT_IGNORE_PATHS: [&str; 2] = [".git/info/exclude", ".gitignore"];
pub(super) const MAX_IGNORE_FILE_BYTES: usize = 16 * 1024;

pub(crate) struct RootIgnoreSnapshot<'a> {
    raw: [Option<ReservedBytes<'a>>; 2],
    ignore_case: Option<bool>,
}

impl<'a> RootIgnoreSnapshot<'a> {
    pub(super) fn capture(
        root: &ContainedRoot,
        io: &mut ReadBudget<'_>,
        work: &mut Work<'a>,
        ignore_case: Option<bool>,
    ) -> IgnoreResult<Self> {
        let [exclude, gitignore] = ROOT_IGNORE_PATHS;
        Ok(Self {
            raw: [
                optional(root, exclude, MAX_IGNORE_FILE_BYTES, io, work)?,
                optional(root, gitignore, MAX_IGNORE_FILE_BYTES, io, work)?,
            ],
            ignore_case,
        })
    }

    pub(crate) fn ignore_case(&self) -> Option<bool> {
        self.ignore_case
    }

    pub(super) fn raw_inputs(&self) -> [Option<&[u8]>; 2] {
        self.raw
            .each_ref()
            .map(|raw| raw.as_ref().map(AsRef::as_ref))
    }

    /// Exclude precedes the root .gitignore. A missing input is distinct from an empty input.
    pub(crate) fn inputs(&self) -> impl Iterator<Item = (&'static str, Option<&[u8]>)> {
        ROOT_IGNORE_PATHS
            .into_iter()
            .zip(self.raw.iter().map(|raw| raw.as_ref().map(AsRef::as_ref)))
    }

    pub(super) fn confirm(
        &self,
        root: &ContainedRoot,
        io: &mut ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> IgnoreResult<()> {
        for (path, expected) in self.inputs() {
            let current = optional(root, path, MAX_IGNORE_FILE_BYTES, io, work)?;
            work.charge(expected.map_or(0, <[u8]>::len))?;
            if current.as_ref().map(AsRef::as_ref) != expected {
                return Err("root-ignore-changed");
            }
        }
        work.charge(0)
    }
}
