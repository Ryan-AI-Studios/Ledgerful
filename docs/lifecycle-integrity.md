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

## Manual ledger commit and the git hook (0557)

`transactions.source` says who opened the row. A CLI `ledger start` stores `CLI`. The commit-msg fallback stores `HOOK`. `ledger_entries.origin` stays `LOCAL` for both and is not this distinction. `source` is not part of the signature basis. A linked PENDING row keeps the source it already has.

Commit-msg reads a message reference before it counts open rows. `Ledger:` wins over `Ledger-Tx:`. The value must be a bare UUID on its own line.

| Reference | Classification | Second transaction |
|---|---|---|
| No reference, exactly one PENDING | `LinkPending` of that row | No. Source stays as stored. |
| Bare UUID that resolves to a PENDING id | `LinkPending` of that id, even when other PENDING rows are open | No. |
| Bare UUID that resolves to a live COMMITTED transaction | `AlreadyCommitted` | No. |
| Bare UUID with no transaction row and at least one ledger entry | `AlreadyCommitted` (committed-entry fallback) | No. This is not a live transaction status and it is not `verification_status = verified`. |
| No reference and zero PENDING; a reference whose live status is neither PENDING nor COMMITTED, including `ROLLED_BACK`; a reference with no row and no entries; or no reference and more than one PENDING | `Fallback`, or `AmbiguousMulti` which falls through to the same capture | When staged files are present, a new `HOOK` row. Not a deletion. Nothing staged appends no row. |

Three orders:

| Order | Rows | What each row is | Verification |
|---|---|---|---|
| One PENDING and no message reference, or a `Ledger:` / `Ledger-Tx:` line that resolves to that PENDING id | 1 | The already-open transaction, linked and then promoted. Name it by its stored `source`. | `unverified`, or null when hook risk is `TRIVIAL`. Never `verified`. |
| `ledger commit`, then a git message line `Ledger: <that committed uuid>` or `Ledger-Tx: <that uuid>` that resolves COMMITTED or to the committed-entry fallback. `ledger commit --with-git` writes `Ledger: {tx_id}` from the default template only when no `git_commit_template` is configured and `--git-message` is not passed | 1 | The committed row. The hook does not start another transaction | The status `ledger commit` stored. The normal commit path stores SQL null. The entry fallback is not `verified`. |
| `ledger commit`, then a git commit with no such line, and with zero PENDING | 2 | The CLI commit, plus a `HOOK` row whose summary is the commit subject | CLI row stays SQL null. `HOOK` row is `unverified` when its risk is not `TRIVIAL`. Neither value is `verified`. |

Same summary does not merge those rows and does not make them two verified changes. A later commit with a different subject and no ledger line, while nothing is PENDING, appends another `HOOK` row. An amend with staged files and no ledger line, while nothing is PENDING, appends another `HOOK` row and leaves the earlier row in place. An amend with no staged files appends nothing. A second post-commit with no sidecar does not insert a row.

A recognized reference resolves to a live COMMITTED transaction, a live PENDING transaction, or the committed-entry fallback. It does not mean `verification_status = verified`. An unknown or missing reference falls back to a new `HOOK` row when files are staged.

`verify --tx-id` can store the id on `verification_runs` and `verification_results` while the ledger entry fields stay null. Those tables are not a `verified` claim on the entry. Track 0556 owns that readout.
