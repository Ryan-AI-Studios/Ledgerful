# Reviewer read-only sandbox path

How independent reviewers (Codex `-s read-only`, restricted subagents, cross-model
review) should ground “what changed / what to test” **without** a fully writable
agent checkout — and what they must **not** claim.

Canonical matrix for agent skills (`ledgerful` dual skill, `codex-review`).
Implementers on a normal writable tree keep the default preflight ladder
(`doctor` → `audit` → `ledger status` → `change-context`); this doc is the
**reviewer sibling**.

---

## Honesty ceiling (B1)

```text
Full verify / cargo test / nextest / index rebuild / ledger start|commit
  → REQUIRE a writable environment (workspace-write or unrestricted).
  → NEVER claim these work in a pure zero-write sandbox.

Independent review in pure RO:
  → git + read tools first; ledgerful optional for grounding if the binary
    and readable state exist.
  → Prefer change-context / status / audit when they succeed.
  → On storage/write failure: report unavailable — do not invent impact.
```

There is **no** product “RO mode” flag. Soft-open of existing `ledger.db` and
honest `not_ready` packets are the engine-side support; full gates remain
writable-env work for the orchestrator/implementer.

---

## Command matrix (B2)

Agent-critical classes only (not every CLI surface):

| Class | Commands | Pure RO FS expectation |
|---|---|---|
| **A — Git-only** | `git status` / `diff` / `log` / `show` | Always available to reviewers |
| **B — Read-heavy ledgerful** | `ledger status --json` / `--compact`, `audit` | Prefer existing `ledger.db`; RO open when DB present |
| **C — Write-open today** | `doctor` (**always** write-mode), `change-context` (soft-open when DB exists) | Doctor: create/migrate/WAL + may write `doctor-results.json` (still Class C — **skip on pure RO**). change-context: prefers RO open when `ledger.db` exists; degrades to `not_ready` on RO fail |
| **D — Explicit write / exec** | `index`, `scan` / `scan --impact` / `impact`, `verify`, `ledger start`/`commit`, `update`, hooks | **Out of RO reviewer path** for durable gates — orchestrator / implementer. Residual (0174): impact / scan(--impact) **soft-open** when `ledger.db` exists and **soft-skip** durable report write under RO (stdout-only + greppable `report write unavailable under RO`); prefer **change-context** for grounding |
| **E — Network** | embedding/completion probes, cloud ask, npm/uv caches | Sandbox network + cache dirs separate from FS RO |

### Sandbox host table (not a fifth command class)

| Host | FS posture | Ledgerful implication |
|---|---|---|
| **Codex `-s read-only`** (incl. **native Windows** PowerShell sandbox) | Pure RO for agent commands | Soft-open + error honesty are load-bearing; full ladder only if RO open works |
| **Codex `-s workspace-write`** | Cwd (and often tmp) writable; **not** deprecated `--full-auto` | Can run Class C/D when orchestrator authorizes; **required** for verify/index/doctor |
| **Claude Code Bash sandbox** | **cwd + session `$TMPDIR` writable by default** | **≠ pure RO** — state under repo `.ledgerful/` may accept write-open. Do not claim “grounding unavailable in RO” solely because Claude sandbox is on. Native Windows: Claude `/sandbox` **inapplicable** (WSL2 only) |

**Codex hygiene:** use `-s read-only` or `--sandbox workspace-write` only.
Do **not** invent `--full-auto` (deprecated) or `codex exec -a never`
(`-a` / `--ask-for-approval` is not a `codex exec` flag).

If docs recommend multi-root scoping via `--add-dir` under workspace-write,
treat it as convenience, not hard isolation (known confinement caveats).

---

## Reviewer session ladder (B3)

```text
Independent review:

  Codex pure RO:  codex exec -C <repo> -s read-only …   (native Windows OK)
  Claude:         restricted tools; if Bash sandbox on, cwd is often writable
                  (see host table — not equivalent to Codex pure RO)

1. Scope: git status + git diff (base..HEAD or working tree) — always.
2. If ledgerful on PATH and .ledgerful (or LEDGERFUL_STATE_DIR → populated) exists:
   a. ledgerful ledger status --json   # or --compact
   b. ledgerful audit                  # if provenance matters
   c. ledgerful change-context --json  # preferred grounding packet (soft-open)
      optional: --base-ref <merge-base>
   d. ledgerful doctor --json          # Class C write-mode — SKIP on pure RO
                                       # unless orchestrator pre-wrote doctor-results.json
                                       # or sandbox is workspace-write / cwd-writable
3. If change-context fails with RO/permission class: git-only review + note
   "ledgerful grounding unavailable under pure RO" — still complete the DoD audit.
   (Do not use that phrase for Claude cwd-writable sandbox without evidence.)
4. NEVER run ledgerful verify / index / scan --impact as the reviewer's job
   unless sandbox is workspace-write (or stronger) AND the orchestrator
   authorized artifact-writing gates (codex-review: orchestrator owns gates).
```

**Note:** success-path `change-context` may still list `ledgerful doctor --json`
in `nextActions` when the doctor sidecar is missing/stale. That is
**implementer / writable-env** advice, not pure-RO reviewer advice.

---

## Env vars (B4)

| Var | Meaning | Reviewer guidance |
|---|---|---|
| `LEDGERFUL_STATE_DIR` | Absolute path **to** the `.ledgerful` directory (contains `state/`, `config.toml`, …) | **Only** point at an **existing, populated** `.ledgerful`. Empty temp → empty index false confidence. Relative paths **rejected**. |
| Worktree (0108) | Default shared main `.ledgerful` | Run from worktree cwd; do not copy state into the linked tree |
| Codex pure RO | `-s read-only` (native Windows or WSL2) | Prefer soft-open change-context; do not expect doctor |
| Codex workspace-write | `--sandbox workspace-write` (**not** `--full-auto`) | For doctor/verify/index when authorized; `writable_roots` if state is outside cwd |

### Empty `LEDGERFUL_STATE_DIR` footgun

Pointing the override at a **new empty writable temp** can succeed but yields an
empty index and a low-value packet — **false confidence**. Prefer the main
worktree’s populated `.ledgerful` (or the 0108 shared path). Never invent a
temp state “just so doctor works” during pure RO review.

Consumer caches (`uv`, cargo target, npm) are outside Ledgerful; configure them
in the consumer repo, not as an engine RO mode.

---

## Error honesty (change-context)

When storage/layout open fails, `change-context --json` emits
`status: "not_ready"` (schemaVersion 1) with class-aware `nextActions`:

| Class | nextActions shape |
|---|---|
| **PermissionDenied / RO** | Populated `LEDGERFUL_STATE_DIR` if override wrong; re-run outside pure RO (`--sandbox workspace-write`); continue git-only. **No** `doctor` / `init` / `index` |
| **MissingDb** | Writable-env `init` / `scan` / `index` once (implementer), then retry |
| **SchemaStale** | Migrate/upgrade in writable env, then retry |
| **LayoutUnavailable** | Fix cwd / repo discover; git-only fallback |

Reasons remain greppable (`storage unavailable:` and, for RO class,
`state directory not writable`).

---

## Impact / scan residual (0174)

There is still **no product RO flag**. Class D commands remain non-default for pure RO
review. When a writable implementer (or accidental Class D invoke) runs `impact` /
`scan --impact` against existing state:

- Storage is **write-first**; on write-open failure with existing `ledger.db`,
  falls back to RO / sqlite-only RO for analysis (stdout-only residual).
- `latest-impact.json` / scan report writes **soft-skip** when storage is RO or
  write hits RO-class `PermissionDenied` — process does **not** hard-fail solely
  on report write.
- Human + `analysisWarnings` emit greppable: `report write unavailable under RO`.
- Never claim “Wrote impact report” / “impact report refreshed” when skip.
- Writable trees still write durable reports when write succeeds.

**Reviewers:** prefer **`change-context --json`** for grounding; do not treat
`scan --impact` as the RO path. Doctor remains Class C (write-open) — skip on pure RO.

## Related

- Dual skill RO section (`.agents/skills/ledgerful/SKILL.md`,
  `docs/Ledgerful/skill.md`)
- `codex-review` skill: orchestrator owns write-class gates
- Worktree state sharing: [Engineering.md — Git worktrees](Engineering.md#git-worktrees-state-sharing)
- Default implementer preflight: dual skill + `AGENTS.md` / `Claude.md`
- Human doctor progressive disclosure: [doctor-severity.md](doctor-severity.md)
