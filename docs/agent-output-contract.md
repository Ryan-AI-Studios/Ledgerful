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

## Four verbosity states (`cli_summary` layer)

| State | Filter | Selected by | Effect |
|---|---|---|---|
| **Default** | `INFO` | anything else (track 0100) | Aggregate visible; **hide** per-entry `VALID`/`SKIP` detail |
| **Verbose** | `DEBUG` | `--verbose` / `-v` | Restore per-entry signature detail + aggregate (pre-0100 default) |
| **Quiet** | `INFO` | `--quiet` / `-q` / `LEDGERFUL_QUIET=1` | Same filter as default for signatures (hide per-entry; **keep aggregate**) |
| **Machine** | `WARN` | `--json` on any subcommand, `scan --format json`, `mcp` | No human `cli_summary` line reaches stdout |

**Precedence:** machine → `WARN` (wins over everything); else if verbose →
`DEBUG` (explicit `-v` wins over quiet); else → `INFO`. **`--json` selects
machine mode, not quiet** — quiet would still emit aggregate `info!` lines
around the JSON payload.

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

## `change-context --json` schema (v1)

Track **0114**. Canonical agent-consumable change packet composing impact
structure, doctor readiness, open ledger work, and a budgeted `readSet`.

```json
{
  "schemaVersion": 1,
  "status": "ready",
  "summary": "…",
  "headHash": "…",
  "baseRef": "origin/main",
  "riskLevel": "medium",
  "riskReasons": ["…"],
  "readSet": [
    { "path": "src/foo.rs", "reason": "changed", "priority": 1 }
  ],
  "readSetCapped": false,
  "readSetTotalCandidates": 1,
  "blast": {
    "depth": 1,
    "mustTouchFileCount": 0,
    "mustTouchSymbolCount": 0,
    "confidenceSummary": {
      "scipBound": 0,
      "resolved": 0,
      "ambiguous": 0,
      "unresolved": 0,
      "capped": 0,
      "unknown": 0,
      "expandable": 0,
      "total": 0
    }
  },
  "testCoverage": {
    "status": "available",
    "sourceSeedCount": 12,
    "mappedCount": 7,
    "fileMappedCount": 2,
    "unmappedCount": 3,
    "unmappedCapped": false,
    "unmappedTotal": 3,
    "unmapped": [
      {
        "symbol": "execute_foo",
        "file": "src/commands/foo.rs",
        "qualifiedName": "commands::foo::execute_foo",
        "mappingKind": "none"
      }
    ],
    "mappedSample": [
      {
        "symbol": "bar",
        "file": "src/bar.rs",
        "coveringTestCount": 2,
        "mappingKind": "symbol"
      }
    ],
    "notes": [
      "Structural test_mapping only (IMPORT/NAMING_CONVENTION); not line coverage",
      "LCOV COVERAGE mapping kind does not currently persist (DDL NOT NULL on test_symbol_id)"
    ]
  },
  "doctor": {
    "status": "ok",
    "readyForPublish": true,
    "block": 0,
    "warn": 0,
    "info": 0,
    "topFindings": []
  },
  "ledger": {
    "pendingCount": 0,
    "activeTx": []
  },
  "analysisWarnings": [],
  "nextActions": ["ledgerful verify --scope fast"],
  "impactSchemaVersion": "v1"
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | u32 | Always **`1`** for this packet (doctor/verify style) |
| `impactSchemaVersion` | string | Forwarded from `ImpactPacket.schema_version` (different field/type) |
| `status` | string | `ready` \| `empty` \| `not_ready` |
| `readSetCapped` / `readSetTotalCandidates` | bool / usize | **Required**; `true` / total when truncated by `--max-files` |
| `blast` | object | **Counts only** — not full edges. Always includes nested `confidenceSummary` (class counts: `scipBound`, `resolved`, `ambiguous`, `unresolved`, `capped`, `unknown`, `expandable`, `total`) at both `minimal` and `standard` detail. Same shape as ImpactPacket `blastRadius.confidenceSummary`. Full edges remain on `impact --json` only (0114 token-budget fence). |
| `testCoverage` | object | **0115 deepened** structural test-gap report (same schema as PR `testGaps`). Status: `available` \| `empty_mapping` \| `missing_table` \| `no_source_seeds` \| `unavailable`. **Never** bare `"empty"`. Empty `ImpactPacket.test_coverage` vec is **not** full cover — use these counts/status. Caps: unmapped ≤20, mappedSample ≤5. Notes always include structural + LCOV ceiling. |
| `doctor` / `ledger` | object | **Always present** on successful builds (including `status=empty`) |
| `doctor.topFindings` | array | Best-effort from sidecar `findings` when present; production `doctor-results.json` may omit findings, yielding `[]` even when `block > 0` — use `ledgerful doctor --json` for full detail |

| `status` | When |
|---|---|
| `ready` | Non-empty file changes, **or** clean tree with `ledger.pendingCount >= 1` |
| `empty` | No file changes **and** `ledger.pendingCount == 0` (doctor still present) |
| `not_ready` | Layout/impact hard failure; `reason` + `nextActions` set |

### Stream rules

- `--json` → **machine mode**: pure JSON on **stdout only** (0093).
- Human mode (no `--json`): short summary (status, risk, readSet count, readyForPublish, next steps).

### `--base-ref` present-tense rule

**`--base-ref` only time-travels structural impact / `readSet` / risk.** Doctor and
ledger always report **present-tense** local workspace/DB state. CI agents should
still run `doctor --json` first; change-context will surface a missing/stale
sidecar honestly when doctor has not run.

### Truncation

When `readSetCapped` is true, deep-dive with `ledgerful scan --impact --json`
for the full change set. Do not assume a capped `readSet` is complete.

### MCP

Tool name: `change_context` (params: `detail`, `max_files`, `base_ref`,
`blast_depth`). Same builder as the CLI; impact is in-memory and does **not**
rewrite `.ledgerful/reports/latest-impact.json`.

### Invocation

```powershell
ledgerful doctor --json
ledgerful change-context --json
ledgerful change-context --json --detail minimal --max-files 5
ledgerful change-context --json --base-ref HEAD~1
```

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
| **Fatal execution error** | empty / unparseable | non-zero (`1`, or **`101`** on panic) | tool could not complete a verification result | fix environment / flags; **do not** treat as a clean pass |

A non-zero exit with **no** JSON is **not** a verification result. Always check
**exit code and** that stdout parsed.

### What is (and is not) fatal under `verify --json`

| Case | Behaviour | Outcome class |
|---|---|---|
| Clap / invalid flags (e.g. bad `--scope`, rejected `--json` combos) | no payload; non-zero; message on stderr | **Fatal** |
| Panic in the main thread | exit **101**; no payload | **Fatal** |
| Hard `Err` before the payload is emitted (e.g. cwd unreadable, rejected combo) | no payload; non-zero | **Fatal** |
| Plan step failure after the run completes | **JSON present**, `ok: false`, non-zero | **Validation rejection** |
| **Config load failure** | **not fatal** — warn + defaults (post-0094 honesty path); verification still runs | continue; may see stderr WARN |
| **SQLite / packet open failure** | **not fatal** — prediction disabled with warn; plan still runs | continue; may see stderr WARN |

Do **not** assume "missing `.ledgerful/config.toml` or a soft config parse
error" means no JSON. Soft config and storage failures degrade; only hard
pre-payload `Err`s and panics produce the empty-stdout fatal class.

---

## PowerShell note

`NativeCommandError` under Windows PowerShell **5.1** requires **all three** of:

1. Windows PowerShell 5.1 (not PowerShell 7+)
2. `$ErrorActionPreference = 'Stop'`
3. Stream merge (`2>&1`)

PowerShell 7+ does not throw on stderr alone. Stream discipline cannot eliminate
legitimate warnings forever; the durable agent invocation is:

```powershell
# Supported agent invocation: machine mode (selected by --json alone).
ledgerful verify --json
# --quiet is optional and only collapses cli_summary per-entry detail;
# it is not required for empty-stderr success under --json.
```

Machine mode keeps human product lines off stdout **and** silences normal_layer
progress `INFO` on stderr. A successful plan run under `--json` with no
degradation warnings should write **empty stderr**. Soft config/storage
degradation, would-block observe warnings, and CRITICAL refusals still use
stderr by design — agents must not merge streams under Windows PowerShell 5.1
+ `$ErrorActionPreference='Stop'`.

---

## Related docs

- [`operator-surface-policy.md`](operator-surface-policy.md) §3 — stream authority
- [`verify-performance.md`](verify-performance.md) — what `--scope fast` actually runs
- [`pr-scan-schema.md`](pr-scan-schema.md) — `scan --pr --format json` schema
