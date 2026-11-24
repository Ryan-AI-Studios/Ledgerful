//! Bind an already qualified canonical checkout to its opaque repository ID.
use crate::state::layout::STATE_DIR;
use std::path::Path;
use uuid::Uuid;

const GUARD_DOMAIN: &[u8] = b"ledgerful/change-evidence/repository-scope/v1\0";

#[derive(Clone, Copy)]
pub(crate) struct CanonicalScopePaths<'a> {
    pub(crate) worktree: &'a str,
    pub(crate) git: &'a str,
    pub(crate) common: &'a str,
    pub(crate) state: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeGuard([u8; 32]);

impl ScopeGuard {
    /// The caller verifies containment, repository format and canonical path identity.
    pub(crate) fn for_conventional_layout(
        paths: Option<CanonicalScopePaths<'_>>,
        layout_from_override: bool,
    ) -> Option<Self> {
        if layout_from_override {
            return None;
        }
        let paths = paths?;
        let worktree = Path::new(paths.worktree);
        if !worktree.is_absolute()
            || paths.git != paths.common
            || worktree.join(".git").as_os_str() != Path::new(paths.git).as_os_str()
            || worktree.join(STATE_DIR).as_os_str() != Path::new(paths.state).as_os_str()
        {
            return None;
        }
        Self::for_paths(Some(paths))
    }

    /// The caller supplies the tuple only after scope qualification succeeds.
    pub(crate) fn for_paths(paths: Option<CanonicalScopePaths<'_>>) -> Option<Self> {
        let paths = paths?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(GUARD_DOMAIN);
        for path in [paths.worktree, paths.git, paths.common, paths.state] {
            if path.is_empty() {
                return None;
            }
            let len = u64::try_from(path.len()).ok()?;
            hasher.update(&len.to_be_bytes());
            hasher.update(path.as_bytes());
        }
        Some(Self(*hasher.finalize().as_bytes()))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub(crate) struct ScopeBinding {
    id: Uuid,
    guard: ScopeGuard,
}

impl ScopeBinding {
    pub(crate) fn new(id: Uuid, guard: ScopeGuard) -> Self {
        Self { id, guard }
    }

    pub(crate) fn matching_id(&self, current: Option<ScopeGuard>) -> Option<Uuid> {
        (current == Some(self.guard)).then_some(self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const CLONE_A: CanonicalScopePaths<'_> = CanonicalScopePaths {
        worktree: "C:/dev/clone-a",
        git: "C:/dev/clone-a/.git",
        common: "C:/dev/clone-a/.git",
        state: "C:/dev/clone-a/.ledgerful",
    };
    const CLONE_B: CanonicalScopePaths<'_> = CanonicalScopePaths {
        worktree: "C:/dev/clone-b",
        git: "C:/dev/clone-b/.git",
        common: "C:/dev/clone-b/.git",
        state: "C:/dev/clone-b/.ledgerful",
    };
    const ID_A: Uuid = Uuid::from_u128(0x4826_be00_5419_4a5a_9b51_27c0_383d_ad03);
    const ID_B: Uuid = Uuid::from_u128(0x280a_e6f3_bf90_4cbe_8171_70a9_f513_2b35);

    fn conventional_paths(name: &str) -> [PathBuf; 4] {
        let parent = if cfg!(windows) {
            Path::new(r"\\?\C:\dev")
        } else {
            Path::new("/dev")
        };
        let worktree = parent.join(name);
        let git = worktree.join(".git");
        let state = worktree.join(STATE_DIR);
        [worktree, git.clone(), git, state]
    }

    fn canonical_paths(paths: &[PathBuf; 4]) -> CanonicalScopePaths<'_> {
        let [worktree, git, common, state] = paths;
        CanonicalScopePaths {
            worktree: worktree.to_str().unwrap(),
            git: git.to_str().unwrap(),
            common: common.to_str().unwrap(),
            state: state.to_str().unwrap(),
        }
    }

    #[test]
    fn an_exact_conventional_layout_uses_the_existing_guard() {
        let paths = conventional_paths("clone-a");
        let paths = Some(canonical_paths(&paths));
        let repeated = conventional_paths("clone-a");
        let expected = ScopeGuard::for_paths(paths).unwrap();

        assert_eq!(
            ScopeGuard::for_conventional_layout(Some(canonical_paths(&repeated)), false),
            Some(expected)
        );
        assert_eq!(
            ScopeGuard::for_conventional_layout(paths, false),
            Some(expected)
        );
    }

    #[test]
    fn independent_conventional_clones_get_distinct_guards() {
        let first = conventional_paths("clone-a");
        let second = conventional_paths("clone-b");
        let first =
            ScopeGuard::for_conventional_layout(Some(canonical_paths(&first)), false).unwrap();
        let second =
            ScopeGuard::for_conventional_layout(Some(canonical_paths(&second)), false).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn override_refuses_even_the_exact_conventional_paths() {
        let paths = conventional_paths("clone-a");
        let paths = Some(canonical_paths(&paths));

        assert_eq!(ScopeGuard::for_conventional_layout(paths, true), None);
        assert_eq!(ScopeGuard::for_conventional_layout(None, false), None);
    }

    #[test]
    fn a_different_git_common_or_state_path_is_refused() {
        let first = conventional_paths("clone-a");
        let second = conventional_paths("clone-b");
        let local = canonical_paths(&first);
        let foreign = canonical_paths(&second);

        for paths in [
            CanonicalScopePaths {
                git: foreign.git,
                common: foreign.common,
                ..local
            },
            CanonicalScopePaths {
                common: foreign.common,
                ..local
            },
            CanonicalScopePaths {
                state: foreign.state,
                ..local
            },
        ] {
            assert_eq!(
                ScopeGuard::for_conventional_layout(Some(paths), false),
                None
            );
        }
    }

    #[test]
    fn the_same_canonical_tuple_matches_its_opaque_id() {
        let guard = ScopeGuard::for_paths(Some(CLONE_A)).unwrap();
        let binding = ScopeBinding::new(ID_A, guard);
        let repeated = ScopeGuard::for_paths(Some(CLONE_A)).unwrap();

        assert_eq!(guard.as_bytes(), repeated.as_bytes());
        assert_eq!(binding.matching_id(Some(repeated)), Some(ID_A));
    }

    #[test]
    fn a_different_clone_or_state_path_does_not_reuse_the_id() {
        let first = ScopeGuard::for_paths(Some(CLONE_A)).unwrap();
        let second = ScopeGuard::for_paths(Some(CLONE_B)).unwrap();
        let first_binding = ScopeBinding::new(ID_A, first);
        let second_binding = ScopeBinding::new(ID_B, second);
        let different_state = ScopeGuard::for_paths(Some(CanonicalScopePaths {
            state: CLONE_B.state,
            ..CLONE_A
        }));

        assert_ne!(first, second);
        assert_eq!(first_binding.matching_id(Some(second)), None);
        assert_eq!(first_binding.matching_id(different_state), None);
        assert_eq!(second_binding.matching_id(Some(second)), Some(ID_B));
    }

    #[test]
    fn swapping_path_roles_changes_the_guard() {
        let original = ScopeGuard::for_paths(Some(CLONE_A));
        let swapped = ScopeGuard::for_paths(Some(CanonicalScopePaths {
            worktree: CLONE_A.state,
            state: CLONE_A.worktree,
            ..CLONE_A
        }));

        assert_ne!(original, swapped);
    }

    #[test]
    fn missing_or_unqualified_scope_cannot_resolve_an_id() {
        let guard = ScopeGuard::for_paths(Some(CLONE_A)).unwrap();
        let binding = ScopeBinding::new(ID_A, guard);
        let missing = ScopeGuard::for_paths(Some(CanonicalScopePaths {
            state: "",
            ..CLONE_A
        }));

        assert_eq!(missing, None);
        assert_eq!(ScopeGuard::for_paths(None), None);
        assert_eq!(binding.matching_id(missing), None);
        assert_eq!(binding.matching_id(None), None);
    }
}
