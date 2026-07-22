# Enforce lifecycle integrity

Track **0074** — make `enforce` a real local control without destroying
provenance, and keep the honesty copy matched to what client hooks can
actually guarantee.

## Fail-closed is a state machine (not post-commit exit 1)

Git has already created the commit when `post-commit` runs. Ledgerful therefore
**never** fails the git commit from post-commit. "Fail closed" means:

1. **commit-msg** creates a PENDING ledger row + `pending_hook_tx` sidecar
   (message-hash bind).
2. Git commit succeeds.
3. **post-commit promote**
   - **OK** → COMMITTED; drop sidecar; `verification_status = Unverified`
     (or `None` if risk was TRIVIAL); `verification_basis = None`.
   - **FAIL under enforce** → keep PENDING + sidecar; set
     `promote_failed=true` + `promote_error`; emit CRITICAL; return Ok to git.
4. **Next commit-msg** / `ledger status --exit-code` / `doctor` gate the
   orphan until recovery.

## HEAD bind = message-hash heuristic

There is **no** `git_commit` oid column in this track. Coverage is bound by
SHA-256 of the cleaned commit message body. That is amend-ambiguous and not a
cryptographic object-id bind — do not claim otherwise.

"HEAD-covered" means a COMMITTED or durable `[SKIPPED]` row whose sidecar hash
matched HEAD at promote time, **or** an explicit promote-fail orphan still
retained for that hash.

### `HEAD_UNCOVERED` signal (honest minimum)

Status / doctor **`HEAD_UNCOVERED`** is **not** a full “material last commit
without any ledger row” scan. The implemented minimum signal is co-set with
orphan detection via the `pending_hook_tx` sidecar:

| Condition | `promote_orphan` | `head_uncovered` |
|---|---|---|
| Sidecar with `promote_failed` | yes | yes |
| Sidecar whose message-hash matches HEAD (pending not yet promoted) | yes | yes |
| No sidecar | no | no |

There is **no** independent walk of COMMITTED/SKIPPED rows against HEAD when the
sidecar is absent. Residual: a material HEAD with no row and no sidecar is
outside this heuristic (client `--no-verify` can create that gap). Do not claim
a full HEAD-coverage audit from this code alone.

## SKIPPED rows

Under enforce, adaptive trivial bypass and TUI Skip write a durable pending
sidecar whose summary starts with `[SKIPPED]`. Risk is non-TRIVIAL (`MEDIUM`)
so promote sets Unverified. After promote:

| Field | Value |
|---|---|
| Counts as coverage for HEAD-uncovered | Yes |
| Counts as verified / export PASS | **No** |
| `verification_status` | Unverified (never Verified) |
| `risk` | MEDIUM (not TRIVIAL — TRIVIAL promote would be `None`) |

**Ceiling:** SKIPPED = acknowledged non-coverage / non-material — never a
second phantom green.

## Recovery

```text
ledgerful ledger recover-orphan --promote
ledgerful ledger recover-orphan --abandon --reason "<required text>"
```

- `--promote` — commit PENDING as Unverified; drop sidecar.
- `--abandon` — durable MAINTENANCE row with reason; rollback pending; drop
  sidecar. Never silent delete without reason.

## Status exit codes (reconcile 0050)

| Mode | `--exit-code` |
|---|---|
| **enforce** | Exit **1** on pending, unaudited, promote orphan, or HEAD-uncovered |
| **observe** | Exit **0** + banner WARN (0050: no blocking exits by default) |
| **observe + opt-in** | `--strict-observe-signal` or `LEDGERFUL_STRICT_OBSERVE_SIGNAL=1` → exit **2** |

## Doctor CRITICAL codes

| Code | Meaning |
|---|---|
| `PROMOTE_ORPHAN` | promote_failed sidecar and/or HEAD-matching pending without COMMITTED |
| `HEAD_UNCOVERED` | same orphan/sidecar heuristic as `PROMOTE_ORPHAN` (enforce CRITICAL); **not** a full material-HEAD-without-row scan — see above |
| `INTENT_NEVER_UNDER_ENFORCE` | `intent.required=never` while gate=enforce |
| `PHANTOM_PROMOTED_WITHOUT_VERIFY` | WARN: legacy Verified without verification_results row |

Doctor exits **1** when any CRITICAL is present.

## Client-hook ceiling (honesty)

Local hooks are always outrun by:

- `git commit --no-verify`
- `core.hooksPath` pointing elsewhere
- missing / replaced hook binaries

**Green local hooks ≠ unbypassable enforced control.** Hard enforcement for
shared policy needs a CI/remote rule. This track hardens the *local* invariant
and makes that ceiling explicit.
