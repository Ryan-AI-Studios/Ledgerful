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
| `git show <base>:.ledgerful/policy.toml` | `--pr` without `--policy` | `base-branch` |
| Working-tree `.ledgerful/policy.toml` or synthesized defaults | Local / default mode | `local` |

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
| `--pr <range>` | **Committed range only** — risk from `scan --pr`-equivalent diff; ledger signatures / pending txs / verification from the repo ledger DB. Does **not** inspect the `pending_hook_tx` sidecar. |
| Local / default (no `--pr`) | Ledger pending txs **and** the `pending_hook_tx` sidecar **and** working-tree risk (when evaluable). A green local check for committed content predicts CI; local mode also catches uncommitted/pending problems before push. |

## Config (flat TOML, no DSL)

Default path: `.ledgerful/policy.toml` in the repo (overridable with `--policy`).

```toml
# .ledgerful/policy.toml
# preset can be omitted — derived from gate.mode if absent
preset = "enforce"  # or "observe"

[rules]
require_signed_entries = true
no_pending_tx = true
verification_must_pass = true
max_risk_without_adr = "high"   # off | low | medium | high
fail_on = "high"                # off | low | medium | high
```

### Committing the policy file

`ledgerful init` adds `.ledgerful/` to `.gitignore` (state/logs must stay local).
The policy file is the intentional exception for CI: **force-add** it so the
base branch can serve it via `git show`:

```bash
git add -f .ledgerful/policy.toml
git commit -m "Add ledgerful CI policy"
```

Without a committed base-branch policy, `--pr` mode synthesizes defaults from
`gate.mode` (still bypass-proof — the PR head's working-tree copy is never used).

If no policy file exists, defaults are synthesized from `gate.mode`
(0050 subsumption):

- **observe** preset: all rules enabled; violations are warnings; exit 0 always
- **enforce** preset: all rules enabled; violations block (exit nonzero)

`gate.mode` continues to work exactly as today for commit-path enforcement.
Mode transitions still write signed `MAINTENANCE` ledger entries via
`ledgerful gate mode`.

## Built-in rules (stable ids)

| Rule id | Default | Behavior |
|---|---|---|
| `require_signed_entries` | on | Any committed entry with missing or invalid signature/public_key → violation. |
| `no_pending_tx` | on | Pending ledger transactions → violation. Local mode also flags `pending_hook_tx` sidecar. |
| `verification_must_pass` | on | Latest verification run must have `overall_pass=true`. **Fail-closed:** if no runs are recorded, emit a violation. |
| `max_risk_without_adr` | `high` | When risk ≥ threshold and no ADR entry exists (`entry_type=ARCHITECTURE` or `is_breaking=1`) → violation. Set to `off` to disable. |
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

Violations are sorted deterministically by `(ruleId, file, message)`.

## CI example (pairs with 0047 Action)

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

- `fetch-depth: 0` (or an explicit fetch of the base ref) is required; shallow
  clones cannot resolve `git show <base>:.ledgerful/policy.toml`.
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
