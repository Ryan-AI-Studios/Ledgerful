//! Policy decisions over already observed conventional default-discovery inputs.
use super::contained::MAX_RELATIVE_BYTES;
use super::pack::Work;
use super::root_ignore_matcher::{RootIgnoreError, RootIgnoreMatcher};
use crate::index::staleness::is_supported_code_path;
use std::cmp::Ordering;

const MAX_POLICY_LEVELS: usize = 64;
const MAX_DISCOVERED_PATHS: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MembershipError {
    Policy(RootIgnoreError),
    Budget(&'static str),
    Unqualified,
    UndiscoveredIndexRow,
    UnindexedDiscoveredPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiscoveryEntry {
    Prune,
    Directory,
    CodeFile,
    OtherFile,
}

pub(super) struct RuleFrame<'work, 'path> {
    prefix: &'path [u8],
    matcher: RootIgnoreMatcher<'work>,
}

impl<'work, 'path> RuleFrame<'work, 'path> {
    pub(super) fn compile(
        prefix: &'path [u8],
        input: Option<&[u8]>,
        work: &mut Work<'work>,
    ) -> Result<Self, MembershipError> {
        if !prefix.is_empty() && !ordinary_path(prefix) {
            return Err(MembershipError::Unqualified);
        }
        // ignore 0.4.33 uses case-sensitive glob matching by default on both platforms.
        let matcher = RootIgnoreMatcher::compile([None, input], Some(false), work)
            .map_err(MembershipError::Policy)?;
        Ok(Self { prefix, matcher })
    }

    pub(super) fn decide(
        &self,
        path: &[u8],
        directory: bool,
        work: &mut Work<'work>,
    ) -> Result<Option<bool>, MembershipError> {
        work.charge(self.prefix.len() + 1)
            .map_err(MembershipError::Budget)?;
        let suffix = if self.prefix.is_empty() {
            path
        } else {
            path.strip_prefix(self.prefix)
                .and_then(|suffix| suffix.strip_prefix(b"/"))
                .ok_or(MembershipError::Unqualified)?
        };
        self.matcher
            .leaf(suffix, directory, work)
            .map_err(MembershipError::Policy)
    }
}

/// All rule frames name actual qualified parent directories, nearest first.
/// Outer path spellings are relative to the same admitted lexical root.
pub(super) struct PolicyLanes<'rules, 'work, 'path> {
    pub(super) local_ignore: &'rules [RuleFrame<'work, 'path>],
    pub(super) ancestor_ignore: &'rules [RuleFrame<'work, 'path>],
    pub(super) local_gitignore: &'rules [RuleFrame<'work, 'path>],
    pub(super) info_exclude: &'rules [RuleFrame<'work, 'path>],
    pub(super) global_exclude: &'rules [RuleFrame<'work, 'path>],
}

impl<'work> PolicyLanes<'_, 'work, '_> {
    pub(super) fn classify(
        &self,
        path: &str,
        directory: bool,
        windows_hidden: bool,
        work: &mut Work<'work>,
    ) -> Result<DiscoveryEntry, MembershipError> {
        let mut selected = None;
        for rules in [
            self.local_ignore,
            self.ancestor_ignore,
            self.local_gitignore,
            self.info_exclude,
            self.global_exclude,
        ] {
            if rules.len() > MAX_POLICY_LEVELS {
                return Err(MembershipError::Unqualified);
            }
            for rule in rules {
                selected = rule.decide(path.as_bytes(), directory, work)?;
                if selected.is_some() {
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }
        classify_selected(path, directory, windows_hidden, selected, work)
    }
}

pub(super) fn classify_selected(
    path: &str,
    directory: bool,
    windows_hidden: bool,
    selected: Option<bool>,
    work: &mut Work<'_>,
) -> Result<DiscoveryEntry, MembershipError> {
    let path = path.as_bytes();
    if path.len() > MAX_RELATIVE_BYTES {
        return Err(MembershipError::Unqualified);
    }
    work.charge(2 * path.len())
        .map_err(MembershipError::Budget)?;
    if !ordinary_path(path) {
        return Err(MembershipError::Unqualified);
    }
    let basename = path
        .rsplit(|byte| *byte == b'/')
        .next()
        .ok_or(MembershipError::Unqualified)?;
    let hidden = windows_hidden || basename.starts_with(b".");
    if selected.unwrap_or(hidden) {
        return Ok(DiscoveryEntry::Prune);
    }
    if directory {
        return Ok(DiscoveryEntry::Directory);
    }
    let path = std::str::from_utf8(path).map_err(|_| MembershipError::Unqualified)?;
    if is_supported_code_path(path) {
        Ok(DiscoveryEntry::CodeFile)
    } else {
        Ok(DiscoveryEntry::OtherFile)
    }
}

/// Compare two completed sequences in raw-byte order under the caller's Work.
/// Success only qualifies the two supplied path sets, never their origin.
pub(super) fn match_paths<'path>(
    discovered: impl ExactSizeIterator<Item = &'path str>,
    indexed: impl ExactSizeIterator<Item = &'path str>,
    work: &mut Work<'_>,
) -> Result<usize, MembershipError> {
    let count = discovered.len();
    let indexed_count = indexed.len();
    if count > MAX_DISCOVERED_PATHS || indexed_count > MAX_DISCOVERED_PATHS {
        return Err(MembershipError::Unqualified);
    }
    let mut previous: Option<&str> = None;
    for (found, stored) in discovered.zip(indexed) {
        if found.len() > MAX_RELATIVE_BYTES || stored.len() > MAX_RELATIVE_BYTES {
            return Err(MembershipError::Unqualified);
        }
        work.charge(3 * (found.len() + stored.len()) + 1)
            .map_err(MembershipError::Budget)?;
        match found.as_bytes().cmp(stored.as_bytes()) {
            Ordering::Less => return Err(MembershipError::UnindexedDiscoveredPath),
            Ordering::Greater => return Err(MembershipError::UndiscoveredIndexRow),
            Ordering::Equal => (),
        }
        if previous.is_some_and(|path| path.as_bytes() >= found.as_bytes()) {
            return Err(MembershipError::Unqualified);
        }
        previous = Some(found);
    }
    // zip consumption alone cannot establish the membership of an extra suffix.
    match count.cmp(&indexed_count) {
        Ordering::Less => return Err(MembershipError::UndiscoveredIndexRow),
        Ordering::Greater => return Err(MembershipError::UnindexedDiscoveredPath),
        Ordering::Equal => (),
    }
    work.charge(0).map_err(MembershipError::Budget)?;
    Ok(count)
}

fn ordinary_path(path: &[u8]) -> bool {
    path.len() <= MAX_RELATIVE_BYTES
        && path.split(|byte| *byte == b'/').all(|name| {
            !name.is_empty()
                && !matches!(name, b"." | b"..")
                && name
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(byte))
        })
}
