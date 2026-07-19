# Policy check (`ledgerful policy check`)

Evaluate a **declared, flat policy** against PR/diff/ledger state and exit
nonzero on violation. This is the CI merge gate that pairs with the 0047
`scan --pr` Action surface.

> **Honest limit:** `policy check` evaluates the *declared* rules over the
> *presented* ledger and risk inputs. It is **not** a compliance verdict,
> certification, or proof that the change is safe. With chain continuity
> (0046) the "presented" ledger is stronger; it is still not a substitute
> for your org's change-management process.

## Base-branch policy constraint (read this)

**In CI the enforced policy is the base branch's, not your PR's.**

When you run:

```bash
ledgerful policy check --pr origin/main...HEAD
```

Ledgerful loads `.ledgerful/policy.toml` via
`git show <base_ref>:.ledgerful/policy.toml`. Edits to that file *in the PR
branch* do **not** change the gate. A policy change only takes effect after
it is **merged and reviewed on the base branch**.

This is intentional and bypass-proof: otherwise a PR could disable
`require_signed_entries` or `verification_must_pass` in its own copy of the
file and pass its own gate.

To use an org-level or CI-pinned policy file instead of the base-branch
copy, pass a **trusted path**:

```bash
ledgerful policy check --pr origin/main...HEAD --policy /path/to/org-policy.toml
```

| Source | When | `policySource` in JSON |
|---|---|---|
| Explicit `--policy <path>` | Always wins | `trusted-path` |
| `git show <base>:.ledgerful/policy.toml` | `--pr` without `--policy` **and** file exists on base | `base-branch` |
| Working-tree `.ledgerful/policy.toml` | Local / default mode when the file exists | `local` |
| Synthesized defaults | No policy file loaded (missing base path or local file) | `synthesized` |

## Invocation

```bash
ledgerful policy check
ledgerful policy check --pr origin/main...HEAD
ledgerful policy check --pr origin/main...HEAD --fail-on high
ledgerful policy check --policy .ledgerful/policy.toml
ledgerful policy check --pr origin/main...HEAD --format json
```

| Flag | Description |
|---|---|
| `--pr <range>` | PR-style range: `base...head`, `base..head`, or bare `base` (head defaults to `HEAD`). Evaluates the **committed range** only. |
| `--fail-on <level>` | Override config `rules.fail_on` for this run: `off` \| `low` \| `medium` \| `high`. |
| `--policy <path>` | Trusted policy file (org/CI). Skips base-branch and working-tree resolution. |
| `--format json\|text` | Machine contract (`json`) or human report (`text`, default). |

## Evaluation target

| Mode | What is inspected |
|---|---|
| `--pr <range>` | **Committed range only** — risk from `scan --pr`-equivalent diff; committed ledger signatures and **bound** verification runs from a *presented* ledger DB (if present). Does **not** inspect pending DB transactions or the `pending_hook_tx` sidecar (both are workspace state CI would not see). |
| Local / default (no `--pr`) | Ledger pending txs **and** the `pending_hook_tx` sidecar **and** working-tree risk (when evaluable). A green local check for committed content predicts CI; local mode also catches uncommitted/pending problems before push. |

### Git-only vs ledger-backed rules

`.ledgerful/` is gitignored (state/logs stay local). Only `policy.toml` is
typically force-added. A clean CI checkout therefore has **no** `ledger.db`
unless your pipeline presents one as an artifact.

| Rule | Needs ledger DB? | Evaluable from git alone? |
|---|---|---|
| `fail_on` | No | Yes (diff / risk) |
| `max_risk_without_adr` | Partially (ledger ADR entities help; changed ADR docs also satisfy) | Risk yes; full ADR cover may need ledger |
| `no_pending_tx` | Yes (local only; **skipped under `--pr`**) | N/A in CI |
| `require_signed_entries` | **Yes** — fail-closed if DB absent | No |
| `verification_must_pass` | **Yes** — fail-closed if DB absent; requires **bound** runs (`tx_id`) | No |

**CI-safe defaults** (synthesized when `--pr` has no base `policy.toml`) only
enable git-evaluable rules. Enable ledger-backed rules only when your CI
presents a ledger artifact (or run those rules locally / in a job that has one).

### Per-rule evaluation targets

| Rule | `--pr` | Local / default |
|---|---|---|
| `require_signed_entries` | Committed ledger entries (violation if ledger DB absent) | Committed ledger entries (violation if ledger DB absent) |
| `no_pending_tx` | **Skipped** (pending is workspace state) | Pending DB txs + `pending_hook_tx` sidecar |
| `verification_must_pass` | **Bound** verification run covering the PR change set (see below) | Latest **bound** verification run |
| `max_risk_without_adr` | Risk + covering ADR for the committed-range change set | Risk + covering ADR for the working-tree change set |
| `fail_on` | Risk from committed-range diff | Risk from working-tree changes |

### Verification binding (`tx_id`)

`verification_must_pass` never accepts an unbound global verify. A run is
**bound** when `verification_runs.tx_id` is non-null and non-empty (written by
`ledgerful verify --tx-id <id>` or commit-path hooks).

| Mode | Selection |
|---|---|
| Local | Latest bound run (`ORDER BY id DESC`). No bound run → violation. |
| `--pr` | **Full change-set coverage with newest-covering-run semantics.** For each changed path, the **newest** bound verification run whose committed ledger `entity` covers that path is decisive: if that run passed, the path is covered; if it failed, the path fails (a newer fail vetoes an older pass). Paths with no covering bound run → uncovered violation. Partial overlap (one of N paths covered) → violation. No bound runs → violation. Empty change set → fall back to latest bound overall_pass. |

Unrelated later verifies (or verifies without `--tx-id`) cannot greenwash or
red-wash a PR.

## Config (flat TOML, no DSL)

Default path: `.ledgerful/policy.toml` in the repo (overridable with `--policy`).

### CI-safe example (force-add this file)

Recommended for base branch when CI does **not** present a ledger.db artifact.
Only git-evaluable rules are on:

```toml
# .ledgerful/policy.toml  (force-add; see below)
preset = "enforce"

[rules]
require_signed_entries = false   # needs presented ledger.db
no_pending_tx = true             # skipped under --pr; useful locally
verification_must_pass = false   # needs presented ledger.db + bound runs
max_risk_without_adr = "high"    # off | low | medium | high
fail_on = "high"                 # off | low | medium | high
```

### Full local / gate-mirroring example

When a ledger is available (local dev, or CI job with a presented artifact):

```toml
preset = "enforce"

[rules]
require_signed_entries = true
no_pending_tx = true
verification_must_pass = true
max_risk_without_adr = "high"
fail_on = "high"
```

Notes:

- If `preset` is omitted **locally**, mode is derived from `gate.mode`.
- If `preset` is omitted under **`--pr`** (or policy is missing and synthesized),
  mode defaults to **enforce** — never fail-open via working-tree `gate.mode`.

### Preset default rules

| Context | `preset` present | `preset` omitted / no policy file |
|---|---|---|
| Local / default | use declared preset; rules default **all on** (full gate mirror) | synthesize from working-tree `gate.mode` with all rules on |
| `--pr` | use declared preset (from base-branch or `--policy`) | **enforce** + **CI-safe** rule set (ledger-backed rules off) |

### Committing the policy file

`ledgerful init` adds `.ledgerful/` to `.gitignore` (state/logs must stay local).
The policy file is the intentional exception for CI: **force-add** it so the
base branch can serve it via `git show`:

```bash
git add -f .ledgerful/policy.toml
git commit -m "Add ledgerful CI policy"
```

**CI should commit a base-branch `policy.toml` with `preset = "enforce"` and
CI-safe rules** (see example above). Without a committed base-branch policy,
`--pr` mode synthesizes the same CI-safe enforce defaults and reports
`policySource: "synthesized"` (still bypass-proof — the PR head's working-tree
copy is never used). Invalid base refs surface as errors rather than silent
missing-policy.

Local mode without a policy file synthesizes from `gate.mode` (0050 subsumption):

- **observe** preset: all rules enabled; violations are warnings; exit 0 always
- **enforce** preset: all rules enabled; violations block (exit nonzero)

`gate.mode` continues to work exactly as today for commit-path enforcement.
Mode transitions still write signed `MAINTENANCE` ledger entries via
`ledgerful gate mode`.

## Built-in rules (stable ids)

| Rule id | Default (local / full) | Behavior |
|---|---|---|
| `require_signed_entries` | on | Any committed entry with missing or invalid signature/public_key → violation. **Fail-closed** if ledger DB is absent (actionable message: present artifact or disable the rule). |
| `no_pending_tx` | on | **Local only:** pending ledger transactions → violation; also flags `pending_hook_tx` sidecar. **Skipped under `--pr`** (committed range only). |
| `verification_must_pass` | on | **Bound** verification for the evaluation target must pass (see binding table above). **Fail-closed** if ledger DB is absent or no bound run exists. Unbound runs never satisfy. Under `--pr`, coverage is **full change-set**: every changed path needs a passing bound run whose entity covers it — partial coverage is a violation. |
| `max_risk_without_adr` | `high` | When risk ≥ threshold, require **full change-set ADR coverage** for **this evaluation's change set** (not any ADR in history, and not any-path overlap). **Every** changed path must be covered: (a) the path is itself an ADR document (`/adr/`, `/adrs/`, `.adr.md`, `architecture-decision`), **or** (b) a ledger ADR entry (`entry_type=ARCHITECTURE` or `is_breaking=1`) has a non-empty `entity` that equals the path, is a parent scope (`path` starts with `entity/`), or is more specific under a changed tree (`entity` starts with `path/`). An ADR that covers only one of several changed paths does **not** clear the rule. Empty-entity ADRs never blanket-satisfy. Fail-closed when risk is high but any path is uncovered (including empty change sets). Violation messages may list up to 5 uncovered paths. Set to `off` to disable. |
| `fail_on` | `high` | When risk ≥ threshold → violation. Risk is the same deterministic level as `scan --pr`. Set to `off` to disable. |

Risk thresholds compare inclusively: `fail_on = medium` fires on medium **and** high.

## Exit codes and presets

| Preset / mode | Violations present | Exit |
|---|---|---|
| `observe` | yes or no | always **0** (severities marked `warn`) |
| `enforce` | no | **0** |
| `enforce` | yes | **nonzero** (severities marked `error`) |

JSON `passed` is `true` only when there are zero violations (independent of mode).

## JSON machine contract (`--format json`)

Versioned, camelCase (matches 0047 `PrScanReport` discipline). Breaking changes
bump `schemaVersion`.

```json
{
  "schemaVersion": 1,
  "violations": [
    {
      "ruleId": "no_pending_tx",
      "file": ".ledgerful/state/ledger.db",
      "line": null,
      "message": "pending ledger transaction a1b2c3d4 (entity=src/foo.rs)",
      "severity": "error"
    }
  ],
  "passed": false,
  "mode": "enforce",
  "policySource": "base-branch"
}
```

`notes` is an additive optional array of non-blocking evaluation messages
(e.g. risk rules skipped when risk is not evaluable). It is omitted from JSON
when empty (`skip_serializing_if`); when present it looks like
`"notes": ["risk not evaluable: ..."]`. `schemaVersion` stays 1.

Violations are sorted deterministically by `(ruleId, file, message)`.

## CI example (pairs with 0047 Action)

Commit a **CI-safe** base-branch policy so clean runners (no ledger.db) only
evaluate git-visible rules:

```toml
# .ledgerful/policy.toml  (force-add; see above)
preset = "enforce"

[rules]
require_signed_entries = false
no_pending_tx = true
verification_must_pass = false
max_risk_without_adr = "high"
fail_on = "high"
```

```yaml
# .github/workflows/ledgerful-policy.yml
name: Ledgerful policy gate

on:
  pull_request:

permissions:
  contents: read          # git fetch / show base policy
  pull-requests: read     # Action wrapper may annotate the PR
  checks: write           # optional: native check-run from 0047 wrapper

jobs:
  policy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0   # required so base ref exists for --pr

      - name: Install ledgerful
        run: cargo install ledgerful --locked
        # or: use the 0047 composite Action which installs a pinned binary

      - name: Policy check
        run: |
          ledgerful policy check \
            --pr origin/${{ github.base_ref }}...HEAD \
            --format json \
            --fail-on high
```

Notes:

- Prefer a committed base-branch `policy.toml` with `preset = "enforce"` and
  CI-safe rules. If the base has no policy file, `--pr` synthesizes the same
  CI-safe enforce defaults (`policySource: "synthesized"`) so CI does not
  fail-open via a local `gate.mode=observe` and does not fail-closed on
  missing ledger.db for ledger-only rules.
- To enforce `require_signed_entries` / `verification_must_pass` in CI, present
  a ledger artifact (e.g. restore `.ledgerful/state/ledger.db` before the
  check) **and** enable those rules in the base-branch policy. Bound verifies
  need `verify --tx-id` / hooks.
- `fetch-depth: 0` (or an explicit fetch of the base ref) is required; shallow
  clones cannot resolve `git show <base>:.ledgerful/policy.toml`. Invalid base
  refs fail hard (not treated as missing policy).
- Posting check-run annotations is the **Action wrapper's** job (0047), not
  the engine. `policy check` only evaluates and exits.
- The engine path is offline: no `ureq` / `reqwest` / network in policy code.

## Relationship to 0050 gate.mode

| 0050 `gate.mode` | Policy preset | Commit path | `policy check` |
|---|---|---|---|
| `observe` | observe | warn, never block | warn, exit 0 |
| `enforce` | enforce | block on pending/unsigned/… | block on rule violations |

Policy generalizes 0050 with per-rule knobs; it does **not** replace or break
`gate.mode`. Signing basis (`tx_id`, `category`, `summary`, `reason`,
`committed_at`) is untouched — policy/mode never enter the signed payload.
