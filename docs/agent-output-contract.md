# Agent CLI output contract

This document is the machine-facing contract for non-interactive consumers
(agents, CI wrappers, PowerShell scripts) that parse Ledgerful CLI output.

**Authority for streams:** [`operator-surface-policy.md`](operator-surface-policy.md)
§3 ("Stdout is the contract"). This page does not restate that policy; it
names which flags select which streams and documents the versioned JSON
payloads.

**Track:** 0093-AgentCliOutputContract.

---

## Stream policy (summary)

| Kind of output | Stream |
|---|---|
| Machine-readable payload (`--json`, `scan --format json`) | **stdout only** |
| Product lines from `cli_summary` at `info!` | **stdout** (level-split writer) |
| Diagnostics from `cli_summary` at `warn!` / `error!` | **stderr** |
| Progress / backend chatter (`ask`, retries) | **stderr** |
| Hard signature failures (`INVALID`, required `UNSIGNED`) | **stderr** (raw `eprintln!`) |
| Non-`cli_summary` progress `INFO` under machine mode | **suppressed** (normal_layer max `WARN`) |

A command advertised as JSON must emit **only** JSON on stdout. Warnings must
not precede or follow that JSON on stdout.

---

## Three verbosity states (`cli_summary` layer)

| State | Filter | Selected by | Effect |
|---|---|---|---|
| **Default** | `DEBUG` | anything else | Per-entry signature detail and aggregate both visible (unchanged from pre-0093) |
| **Quiet** | `INFO` | `--quiet` / `-q` / `LEDGERFUL_QUIET=1` | Hide per-entry `VALID`/`SKIP` detail; **keep aggregate** |
| **Machine** | `WARN` | `--json` on any subcommand, `scan --format json`, `mcp` | No human `cli_summary` line reaches stdout |

Machine mode wins over quiet if both are set. **`--json` selects machine mode,
not quiet** — quiet would still emit aggregate `info!` lines around the JSON
payload.

**Machine mode also raises the non-`cli_summary` (`normal_layer`) EnvFilter to
`WARN`**, so progress `INFO` lines (for example `Running verification command
via Shell: …`) do not appear on stderr during a successful `verify --json`
run. `WARN` / `ERROR` on `normal_layer` are **not** suppressed (Wave 0 honesty).

`INVALID` and signing-required `UNSIGNED` are raw `eprintln!` outside the
layer; no filter state suppresses them.

---

## Rejected flag combinations (`verify --json`)

`verify --json` is only defined for the plan-execution payload. Combining it
with surfaces that have no versioned JSON schema is a hard error:

| Combo | Error |
|---|---|
| `verify --json --health` | `verify --json cannot be combined with --health` |
| `verify --json --dry-run` | `verify --json cannot be combined with --dry-run` |
| `verify --json --signatures` | `verify --json cannot be combined with --signatures, --chain, or --against-export` |
| `verify --json --chain` | same as above |
| `verify --json --against-export …` | same as above |

These reject rather than emit empty stdout under machine mode.

---

## `verify --json` schema (v1)

```json
{
  "schemaVersion": 1,
  "ok": true,
  "scopeRequested": "fast",
  "scopeExecuted": "full",
  "fallbackReason": "fast scope unavailable — empty test_mapping; running full (~5-8 min)",
  "steps": [
    {
      "name": "cargo fmt --all",
      "command": "cargo fmt --all -- --check",
      "status": "pass",
      "exitCode": 0,
      "durationMs": 1234
    }
  ],
  "timestamp": "2026-07-28T12:00:00+00:00",
  "txId": "optional-pending-tx-id"
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | integer | Always `1` for this contract |
| `ok` | boolean | `true` iff every step has `exitCode == 0` |
| `scopeRequested` | string | `fast` or `full` as passed on the CLI |
| `scopeExecuted` | string | `full` when `fallbackReason` is set; else equals requested |
| `fallbackReason` | string (omitted when null) | Passthrough from the plan; present only on fast→full fallback |
| `steps` | array | **Plan order** (not alphabetically sorted) |
| `steps[].status` | string | `"pass"` if `exitCode == 0`, else `"fail"` |
| `steps[].failureDetail` | string (omitted on pass) | stderr summary preferred |
| `timestamp` | ISO 8601 | From the run report |
| `txId` | string (omitted when null) | Bound pending transaction if any |

This payload is a **CLI wire contract**. It is built from
`VerificationReport` but does **not** extend the persisted
`.ledgerful/reports/latest-verify.json` artifact.

### Invocation

```powershell
ledgerful verify --json
ledgerful verify --json --scope fast
ledgerful verify --json --quiet   # quiet is redundant for agents; machine mode already wins
```

---

## `ledger status --json` schema (v1)

```json
{
  "schemaVersion": 1,
  "pendingCount": 1,
  "unauditedCount": 0,
  "pendingTxIds": ["aaaaaaaa-....", "bbbbbbbb-...."],
  "unauditedFileCount": 0,
  "promoteOrphan": false,
  "headUncovered": false
}
```

| Field | Notes |
|---|---|
| `schemaVersion` | `1` (added in 0093) |
| `pendingTxIds` | **Sorted** lexicographically for determinism |
| `promoteOrphanTxId` / `promoteError` | Omitted when absent |

Observe-mode would-block diagnostics go to **stderr** via `cli_summary`
`warn!`. Stdout remains parseable JSON alone.

---

## Exit codes

### `verify` / signature path (`sig_exit`)

| Code | Meaning |
|---|---|
| `0` | OK |
| `1` | INVALID signature / chain break / verification failed |
| `2` | POLICY (reserved; not currently enforced) |
| `3` | UNSIGNED under `require_signing` or `--strict-signatures` |

For `verify --json`, **`ok` and the process exit must agree**: `ok: true` ⇒
exit `0`; validation rejection ⇒ `ok: false` and non-zero exit with JSON still
on stdout.

### `ledger status --exit-code`

| Mode | Would-block exit |
|---|---|
| enforce | `1` |
| observe (default) | `0` + stderr warning |
| observe + `--strict-observe-signal` or `LEDGERFUL_STRICT_OBSERVE_SIGNAL=1` | `2` |

---

## Three outcomes agents must distinguish

| Outcome | stdout | Exit | Meaning | Agent action |
|---|---|---|---|---|
| **Pass** | valid JSON, `ok: true` | `0` | checks passed | proceed |
| **Validation rejection** | valid JSON, `ok: false` | non-zero (`1`/`3`) | repo failed a check | read `steps[]`, fix code |
| **Fatal execution error** | empty / unparseable | non-zero (`1`, or **`101`** on panic) | tool could not run | fix environment; **do not** treat as a verification result |

A non-zero exit with **no** JSON is an environment failure (config, missing
state, panic), not a clean verification pass. Always check **exit code and**
that stdout parsed.

---

## PowerShell note

`NativeCommandError` under Windows PowerShell **5.1** requires **all three** of:

1. Windows PowerShell 5.1 (not PowerShell 7+)
2. `$ErrorActionPreference = 'Stop'`
3. Stream merge (`2>&1`)

PowerShell 7+ does not throw on stderr alone. Stream discipline cannot eliminate
legitimate warnings forever; the durable agent invocation is:

```powershell
ledgerful verify --json
# or, when you need empty stderr on a successful run:
ledgerful verify --json --quiet
```

Machine mode keeps human product lines off stdout **and** silences normal_layer
progress `INFO` on stderr. A successful run under `--json` should write
**empty stderr** (or only true `WARN`/`ERROR` diagnostics). A would-block or
CRITICAL still uses stderr by design.

---

## Related docs

- [`operator-surface-policy.md`](operator-surface-policy.md) §3 — stream authority
- [`verify-performance.md`](verify-performance.md) — what `--scope fast` actually runs
- [`pr-scan-schema.md`](pr-scan-schema.md) — `scan --pr --format json` schema
