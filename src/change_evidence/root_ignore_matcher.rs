//! Decide single paths under the admitted ordinary root ignore rules.
use super::buffer::Reservation;
use super::pack::Work;
use super::root_ignore::MAX_IGNORE_FILE_BYTES;
use gix::bstr::ByteSlice;
use gix::glob::{
    Pattern,
    pattern::{Case, Mode},
    wildmatch,
};

const MAX_RULES: usize = 64;
const MAX_RULE_BYTES: usize = 128;
const MAX_PATH_BYTES: usize = 4_096;

type PolicyResult<T> = Result<T, RootIgnoreError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RootIgnoreError {
    Case,
    InputBytes,
    RuleCount,
    RuleBytes,
    Policy,
    Memory,
    Allocation,
    Parser,
    PathBytes,
    Path,
    Budget(&'static str),
}

impl RootIgnoreError {
    pub(super) fn reason(self) -> &'static str {
        match self {
            Self::Case => "ignore-case-unqualified",
            Self::InputBytes => "ignore-input-cap",
            Self::RuleCount => "ignore-rule-cap",
            Self::RuleBytes => "ignore-rule-byte-cap",
            Self::Policy => "ignore-subset-unsupported",
            Self::Memory => "memory-cap",
            Self::Allocation => "allocation",
            Self::Parser => "ignore-parse-mismatch",
            Self::PathBytes => "ignore-candidate-cap",
            Self::Path => "ignore-candidate-unsupported",
            Self::Budget(reason) => reason,
        }
    }
}

pub(super) struct RootIgnoreMatcher<'a> {
    rules: Vec<Pattern>,
    case: Option<Case>,
    _memory: Reservation<'a>,
}

#[derive(Clone, Copy)]
struct Expected<'a> {
    text: &'a [u8],
    flags: Mode,
    wildcard: Option<usize>,
}

impl<'a> RootIgnoreMatcher<'a> {
    pub(super) fn compile(
        inputs: [Option<&[u8]>; 2],
        ignore_case: Option<bool>,
        work: &mut Work<'a>,
    ) -> PolicyResult<Self> {
        let case = match ignore_case {
            Some(true) => Some(Case::Fold),
            Some(false) => Some(Case::Sensitive),
            #[cfg(target_os = "linux")]
            None => None,
            #[cfg(not(target_os = "linux"))]
            None => return Err(RootIgnoreError::Case),
        };
        let mut count = 0;
        let mut text_bytes = 0;
        for bytes in inputs.into_iter().flatten() {
            if bytes.len() > MAX_IGNORE_FILE_BYTES {
                return Err(RootIgnoreError::InputBytes);
            }
            // Charge bounded line, admission, parse, and verification passes before parsing.
            work.charge(bytes.len() * 16)
                .map_err(RootIgnoreError::Budget)?;
            if !bytes.is_ascii() || bytes.contains(&0) {
                return Err(RootIgnoreError::Policy);
            }
            for line in bytes.lines() {
                if let Some(rule) = admit(line)? {
                    count += 1;
                    if count > MAX_RULES {
                        return Err(RootIgnoreError::RuleCount);
                    }
                    text_bytes += rule.text.len();
                }
            }
        }
        let storage = count * std::mem::size_of::<Pattern>() + text_bytes;
        // The pinned parser creates one owned BString per admitted rule. That allocation
        // moves into the already reserved Vec, while the observation still owns the raw inputs.
        if work
            .read_limit(storage, 0)
            .map_err(RootIgnoreError::Budget)?
            != storage
        {
            return Err(RootIgnoreError::Memory);
        }
        let held = work
            .memory_budget()
            .reserve(storage)
            .map_err(|_| RootIgnoreError::Memory)?;
        let mut rules = Vec::new();
        rules
            .try_reserve_exact(count)
            .map_err(|_| RootIgnoreError::Allocation)?;
        if rules.capacity() != count {
            return Err(RootIgnoreError::Memory);
        }
        for bytes in inputs.into_iter().flatten() {
            let mut parsed = gix::ignore::parse(bytes, false);
            for line in bytes.lines() {
                let Some(expected) = admit(line)? else {
                    continue;
                };
                work.charge(1).map_err(RootIgnoreError::Budget)?;
                let (pattern, _, _) = parsed.next().ok_or(RootIgnoreError::Parser)?;
                if pattern.text.as_slice() != expected.text
                    || pattern.text.capacity() != expected.text.len()
                    || pattern.mode != expected.flags
                    || pattern.first_wildcard_pos != expected.wildcard
                {
                    return Err(RootIgnoreError::Parser);
                }
                rules.push(pattern);
            }
            if parsed.next().is_some() {
                return Err(RootIgnoreError::Parser);
            }
        }
        work.charge(0).map_err(RootIgnoreError::Budget)?;
        Ok(Self {
            rules,
            case,
            _memory: held,
        })
    }

    /// Decide this path only. A caller walking directories must account for ignored ancestors.
    pub(super) fn leaf(
        &self,
        path: &[u8],
        is_dir: bool,
        work: &mut Work<'a>,
    ) -> PolicyResult<Option<bool>> {
        if path.len() > MAX_PATH_BYTES {
            return Err(RootIgnoreError::PathBytes);
        }
        work.charge(path.len() * 3)
            .map_err(RootIgnoreError::Budget)?;
        if !qualified_path(path) {
            return Err(RootIgnoreError::Path);
        }
        let basename = path.iter().rposition(|&byte| byte == b'/').map(|i| i + 1);
        if let Some(case) = self.case {
            return self.match_leaf(path, basename, is_dir, case, work);
        }
        // Git defaults to sensitive matching, but the absent local key leaves
        // external case settings unknown. Require the same result in either mode.
        let sensitive = self.match_leaf(path, basename, is_dir, Case::Sensitive, work)?;
        let folded = self.match_leaf(path, basename, is_dir, Case::Fold, work)?;
        if sensitive != folded {
            return Err(RootIgnoreError::Case);
        }
        Ok(sensitive)
    }

    fn match_leaf(
        &self,
        path: &[u8],
        basename: Option<usize>,
        is_dir: bool,
        case: Case,
        work: &mut Work<'a>,
    ) -> PolicyResult<Option<bool>> {
        for rule in self.rules.iter().rev() {
            // Account for the direct comparison and the pinned debug basename check.
            work.charge(path.len() * 2 + rule.text.len() + 1)
                .map_err(RootIgnoreError::Budget)?;
            if rule.matches_repo_relative_path(
                path.as_bstr(),
                basename,
                Some(is_dir),
                case,
                wildmatch::Mode::NO_MATCH_SLASH_LITERAL,
            ) {
                work.charge(0).map_err(RootIgnoreError::Budget)?;
                return Ok(Some(!rule.is_negative()));
            }
        }
        work.charge(0).map_err(RootIgnoreError::Budget)?;
        Ok(None)
    }
}

fn admit(line: &[u8]) -> PolicyResult<Option<Expected<'_>>> {
    let last = line.iter().rposition(|&byte| byte != b' ');
    let Some(last) = last else {
        return Ok(None);
    };
    let line = &line[..=last];
    if line.starts_with(b"#") {
        return Ok(None);
    }
    if line.len() > MAX_RULE_BYTES {
        return Err(RootIgnoreError::RuleBytes);
    }

    let mut text = line;
    let mut flags = Mode::empty();
    if let Some(rest) = text.strip_prefix(b"!") {
        text = rest;
        flags |= Mode::NEGATIVE;
    }
    if let Some(rest) = text.strip_prefix(b"/") {
        text = rest;
        flags |= Mode::ABSOLUTE;
    }
    if let Some(rest) = text.strip_suffix(b"/") {
        text = rest;
        flags |= Mode::MUST_BE_DIR;
    }
    if !text.contains(&b'/') {
        flags |= Mode::NO_SUB_DIR;
    }
    let wildcard = if let Some(suffix) = text.strip_prefix(b"*") {
        // An unanchored basename suffix always takes gix's direct suffix branch.
        if !flags.contains(Mode::NO_SUB_DIR) || flags.contains(Mode::ABSOLUTE) || !component(suffix)
        {
            return Err(RootIgnoreError::Policy);
        }
        flags |= Mode::ENDS_WITH;
        Some(0)
    } else {
        if !qualified_path(text) {
            return Err(RootIgnoreError::Policy);
        }
        None
    };
    Ok(Some(Expected {
        text,
        flags,
        wildcard,
    }))
}

fn qualified_path(path: &[u8]) -> bool {
    path.split(|&byte| byte == b'/').all(component)
}

fn component(name: &[u8]) -> bool {
    !name.is_empty()
        && !matches!(name, b"." | b"..")
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(byte))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{RootIgnoreError, RootIgnoreMatcher, Work};
    use crate::change_evidence::buffer::MemoryBudget;
    use std::{
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };

    #[test]
    fn omitted_local_case_policy_requires_an_invariant_ignore_decision() {
        let interrupted = AtomicBool::new(false);
        let memory = MemoryBudget::new(16 * 1024);
        let mut work = Work::new(
            Instant::now() + Duration::from_secs(5),
            &interrupted,
            64 * 1024,
            &memory,
        );
        let comments =
            RootIgnoreMatcher::compile([Some(b"# ordinary Git template\n"), None], None, &mut work)
                .unwrap();
        assert_eq!(comments.leaf(b"README.md", false, &mut work), Ok(None));

        let rules = [None, Some(b"*.log\n!keep.log\n".as_slice())];
        let unresolved = RootIgnoreMatcher::compile(rules, None, &mut work).unwrap();
        assert_eq!(
            unresolved.leaf(b"temp.log", false, &mut work),
            Ok(Some(true))
        );
        assert_eq!(
            unresolved.leaf(b"keep.log", false, &mut work),
            Ok(Some(false))
        );
        assert_eq!(unresolved.leaf(b"README.md", false, &mut work), Ok(None));
        assert_eq!(
            unresolved.leaf(b"TEMP.LOG", false, &mut work),
            Err(RootIgnoreError::Case)
        );

        let sensitive = RootIgnoreMatcher::compile(rules, Some(false), &mut work).unwrap();
        let folded = RootIgnoreMatcher::compile(rules, Some(true), &mut work).unwrap();
        assert_eq!(sensitive.leaf(b"TEMP.LOG", false, &mut work), Ok(None));
        assert_eq!(folded.leaf(b"TEMP.LOG", false, &mut work), Ok(Some(true)));
    }
}
