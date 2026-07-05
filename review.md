**Findings**

`critical`: none.

`high` [src/commands/hook_commit_msg.rs](/abs/path/C:/dev/ledgerful/src/commands/hook_commit_msg.rs:160)  
The stale-sidecar guard still rejects a legitimate in-flight hook re-run when the new commit message text is identical to `HEAD` (for example `git commit --amend --no-edit`). `matches_head` is checked before `matches_editmsg`, so an active commit whose `COMMIT_EDITMSG` equals the previous commit message is treated as “previous commit succeeded but post-commit failed” and the hook aborts, even though the comment says this path should allow re-runs. The current tests do not cover the “sidecar exists + active amend + same message as HEAD” case.

`medium` [src/commands/hook_commit_msg.rs](/abs/path/C:/dev/ledgerful/src/commands/hook_commit_msg.rs:153) [src/commands/verify.rs](/abs/path/C:/dev/ledgerful/src/commands/verify.rs:527) [src/commands/ledger/reporting.rs](/abs/path/C:/dev/ledgerful/src/commands/ledger/reporting.rs:223)  
Track 0036 now depends on `.git\index.lock` as the discriminator for “active commit in flight” across cleanup, auto-bind, and status reporting. The integration coverage only simulates that by touching a dummy file, so the behavior is still resting on an unproven Git-for-Windows timing assumption. If that assumption is wrong, valid sidecars will be classified as stale or skipped, which directly breaks verify-to-transaction mapping.

`low` [tests/integration/hook_commit_msg.rs](/abs/path/C:/dev/ledgerful/tests/integration/hook_commit_msg.rs:233)  
`test_real_shell_git_commit_amend_success` is not exercising the real two-hook lifecycle: it installs `commit-msg` only, not `post-commit`. That means it does not validate the actual sidecar promotion/cleanup path that makes amend and verify auto-binding safe, so it gives weaker confidence than its name suggests.

