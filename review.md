**Findings**

`critical`: none.

`high`: none.

`medium` [src/commands/hook_post_commit.rs](/abs/path/C:/dev/ledgerful/src/commands/hook_post_commit.rs:101)  
Unconditional debug `eprintln!` calls were left in the production `post-commit` hook. Every commit now emits `POST-COMMIT HOOK RUNNING!...`, and the mismatch path also dumps both message hashes plus the full current/cleaned commit message to stderr at [src/commands/hook_post_commit.rs](/abs/path/C:/dev/ledgerful/src/commands/hook_post_commit.rs:127). That is a user-visible regression in normal git commits and can leak commit contents into CI logs or wrapper tooling that treats hook stderr as diagnostic output.

`low` [tests/integration/cli_verify.rs](/abs/path/C:/dev/ledgerful/tests/integration/cli_verify.rs:515) [src/state/storage/verification.rs](/abs/path/C:/dev/ledgerful/src/state/storage/verification.rs:59)  
The new persistence test only proves `verification_runs.tx_id` is written. The feature also writes `tx_id` onto every `verification_results` row, and that per-step linkage is part of the actual mapping contract, but there is no direct coverage for it. A regression in result-row persistence would currently slip through.

`low` [tests/integration/hook_commit_msg.rs](/abs/path/C:/dev/ledgerful/tests/integration/hook_commit_msg.rs:343)  
The hook integration tests assert success/failure and DB state, but they do not assert that the hook stays quiet on stdout/stderr. That gap is exactly why the new debug prints in `hook_post_commit` can land without a test failure.

