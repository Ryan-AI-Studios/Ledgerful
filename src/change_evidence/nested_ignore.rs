//! Classify one contained ordinary worktree path using its active lexical rules.
use super::buffer::ReservedBytes;
use super::contained::{ContainedRoot, Identity, ReadBudget};
use super::git::{optional, read_error};
use super::pack::Work;
use super::root_ignore::MAX_IGNORE_FILE_BYTES;
use super::root_ignore_matcher::RootIgnoreMatcher;

const MAX_DEPTH: usize = 32;
const MAX_PATH: usize = 4_096;
const RULE_NAME: &str = "/.gitignore";

type PathResult<T> = Result<T, &'static str>;

struct Frame<'a> {
    path_end: usize,
    identity: Identity,
    raw: Option<ReservedBytes<'a>>,
    matcher: Option<RootIgnoreMatcher<'a>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathIgnoreDecision {
    Leaf(Option<bool>),
    PrunedAncestor,
}

pub(super) struct PathPolicy<'observation, 'a> {
    pub(super) root: &'observation ContainedRoot,
    pub(super) io: &'observation mut ReadBudget<'a>,
    pub(super) work: &'observation mut Work<'a>,
    pub(super) root_matcher: &'observation RootIgnoreMatcher<'a>,
    pub(super) ignore_case: Option<bool>,
}

impl<'a> PathPolicy<'_, 'a> {
    pub(super) fn decide(&mut self, path: &str, is_dir: bool) -> PathResult<PathIgnoreDecision> {
        if path.len() > MAX_PATH {
            return Err("ignore-candidate-cap");
        }
        self.work.charge(path.len() * 4)?;
        if !path.split('/').all(|name| {
            !name.is_empty()
                && !matches!(name, "." | "..")
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        }) {
            return Err("ignore-candidate-unsupported");
        }
        let ancestors = path.bytes().filter(|&byte| byte == b'/').count();
        if ancestors > MAX_DEPTH {
            return Err("ignore-depth-cap");
        }
        let table_bytes = ancestors * std::mem::size_of::<Frame<'a>>();
        if self.work.read_limit(table_bytes, 0)? != table_bytes {
            return Err("memory-cap");
        }
        let _frames = self
            .work
            .memory_budget()
            .reserve(table_bytes)
            .map_err(|_| "memory-cap")?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(ancestors)
            .map_err(|_| "allocation")?;
        if frames.capacity() != ancestors {
            return Err("memory-cap");
        }

        let mut filename = [0; MAX_PATH + RULE_NAME.len()];
        for (end, byte) in path.bytes().enumerate() {
            if byte != b'/' {
                continue;
            }
            let directory = &path[..end];
            let identity = self.observe_entry(directory, true)?;
            // A visible directory's own rules apply to its descendants. They cannot
            // include the directory itself if an already active rule prunes it.
            if self.matches(directory, true, &frames)? == Some(true) {
                self.confirm(path, &frames, &mut filename)?;
                self.confirm_entry(directory, true, identity)?;
                return Ok(PathIgnoreDecision::PrunedAncestor);
            }
            let raw = self.read_rules(directory, &mut filename)?;
            self.confirm_entry(directory, true, identity)?;
            let matcher = raw
                .as_ref()
                .map(|bytes| {
                    RootIgnoreMatcher::compile(
                        [None, Some(bytes.as_ref())],
                        self.ignore_case,
                        self.work,
                    )
                    .map_err(|error| error.reason())
                })
                .transpose()?;
            frames.push(Frame {
                path_end: end,
                identity,
                raw,
                matcher,
            });
        }

        let identity = self.observe_entry(path, is_dir)?;
        let decision = self.matches(path, is_dir, &frames)?;
        self.confirm(path, &frames, &mut filename)?;
        self.confirm_entry(path, is_dir, identity)?;
        Ok(PathIgnoreDecision::Leaf(decision))
    }

    fn matches(
        &mut self,
        path: &str,
        is_dir: bool,
        frames: &[Frame<'a>],
    ) -> PathResult<Option<bool>> {
        for frame in frames.iter().rev() {
            self.work.charge(1)?;
            if let Some(matcher) = &frame.matcher {
                let suffix = &path.as_bytes()[frame.path_end + 1..];
                if let Some(decision) = matcher
                    .leaf(suffix, is_dir, self.work)
                    .map_err(|error| error.reason())?
                {
                    return Ok(Some(decision));
                }
            }
        }
        self.root_matcher
            .leaf(path.as_bytes(), is_dir, self.work)
            .map_err(|error| error.reason())
    }

    fn read_rules(
        &mut self,
        prefix: &str,
        filename: &mut [u8; MAX_PATH + RULE_NAME.len()],
    ) -> PathResult<Option<ReservedBytes<'a>>> {
        let end = prefix.len() + RULE_NAME.len();
        self.work.charge(2 * end)?;
        filename[..prefix.len()].copy_from_slice(prefix.as_bytes());
        filename[prefix.len()..end].copy_from_slice(RULE_NAME.as_bytes());
        let relative = std::str::from_utf8(&filename[..end]).map_err(|_| "unsafe-path")?;
        let _lookup = self.work.reserve_lookup(relative.len())?;
        optional(
            self.root,
            relative,
            MAX_IGNORE_FILE_BYTES,
            self.io,
            self.work,
        )
    }

    fn observe_entry(&mut self, path: &str, directory: bool) -> PathResult<Identity> {
        let _lookup = self.work.reserve_lookup(path.len())?;
        self.root
            .observe_entry(path, directory, self.io)
            .map_err(read_error)
    }

    fn confirm_entry(&mut self, path: &str, directory: bool, identity: Identity) -> PathResult<()> {
        if self.observe_entry(path, directory)? != identity {
            return Err("ignore-path-changed");
        }
        Ok(())
    }

    fn confirm(
        &mut self,
        path: &str,
        frames: &[Frame<'a>],
        filename: &mut [u8; MAX_PATH + RULE_NAME.len()],
    ) -> PathResult<()> {
        for frame in frames {
            let directory = &path[..frame.path_end];
            self.confirm_entry(directory, true, frame.identity)?;
            let original = frame.raw.as_ref().map(AsRef::as_ref);
            let current = self.read_rules(directory, filename)?;
            self.work.charge(original.map_or(0, <[u8]>::len))?;
            if current.as_ref().map(AsRef::as_ref) != original {
                return Err("nested-ignore-changed");
            }
            self.confirm_entry(directory, true, frame.identity)?;
        }
        self.work.charge(0)
    }
}
