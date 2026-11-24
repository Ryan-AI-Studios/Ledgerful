//! Compare a sampled executable bit under the admitted local Git policy.

#[derive(Clone, Copy)]
pub(super) enum FileModePolicy {
    Ignore,
    Compare,
}

impl FileModePolicy {
    pub(super) fn from_local(filemode: Option<bool>) -> Result<Self, &'static str> {
        match filemode {
            Some(true) => Ok(Self::Compare),
            Some(false) => Ok(Self::Ignore),
            None => Err("local-filemode-unqualified"),
        }
    }

    pub(super) fn verify(self, expected: bool, observed: Option<bool>) -> Result<(), &'static str> {
        if matches!(self, Self::Ignore) {
            return Ok(());
        }
        match observed {
            Some(actual) if actual == expected => Ok(()),
            Some(_) => Err("worktree-filemode-mismatch"),
            None => Err("worktree-filemode-unavailable"),
        }
    }
}
