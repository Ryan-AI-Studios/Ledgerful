# Agent CLI output contract

This document is the machine-facing contract for non-interactive consumers
(agents, CI wrappers, PowerShell scripts) that parse Ledgerful CLI output.

**Authority for streams:** [`operator-surface-policy.md`](operator-surface-policy.md)
§3 ("Stdout is the contract"). This page does not restate that policy; it
names which flags select which streams and documents the versioned JSON
payloads.

**Track:** 0093-AgentCliOutputContract; extended by **0136** (search envelope),
**0149** (uniform machine JSON: top-level `status`, `dead-code`, index-check
purity, scan incomplete-flag tips), **0180** (`scan --json`/`--out` gitScan
envelope without mandatory `--impact`; escalate remains `--impact --json`),
**0207** (populated list `--json` is a schemaVersion-1 object; `index --check --json`
camelCase CLI DTO).

---

## Agent-relevant inventory (`--json` purity)

High-traffic surfaces agents parse. **Pure success** means stdout is the
machine payload only and **stderr is empty** on the happy path (no human Info
banners, spinners, or SUCCESS lines). Fail paths may still print diagnostics
on stderr.

| Command | Has `--json` | Pure success stderr | Notes |
|---|---|---|---|
| `doctor --json` | yes | yes | schemaVersion 1 findings; additive `environment.githubLatest` (0205); schemaVersion stays 1. Sidecar `doctor-results.json` does **not** include `githubLatest` |
| `release pins --json` (bare `release --json`) | yes (0201) | yes | schemaVersion 1 object `kind: "releasePins"`; exit **0** match / **1** drift / **2** skipped or unverified. Parent `--json` (T18). Not Daily 5 |
| `change-context --json` | yes | yes | impact-shaped packet |
| `ledger status --json` | yes | yes | schemaVersion 1 |
| `status --json` | yes (0149) | yes | **same payload** as `ledger status --json` |
| `search --json` | yes | yes | 0136 envelope; empty results OK |
| `verify --json` | yes | yes* | plan-execution payload; see rejected combos |
| `index --check --json` | yes | yes (0149) | schemaVersion 1 + `kind: "indexCheck"` camelCase DTO (0207); Info suppressed under json; Error still on stderr |
| `index --semantic --json` | yes (0161) | yes | One final JSON object (`schemaVersion`, `mode`, `reason`, counts, `upToDate`); zero human mid-run lines on stdout |
| `index --json` (main / `--auto-scip` / `--scip`) | yes | yes* | Merged index stats object; top-level **`scip`** (0157/0166): `status`, `edges_added`/`edges_updated`, `definitions_mapped`/`definitions_seen`, `files_skipped`, skip/recovery tallies (`edges_skipped_enclosing_disagreement`, `edges_recovered_nest_prefer`, `edges_skipped_unmapped`, `edges_skipped_invalid_occ_range`, `edges_skipped_duplicate`, `definitions_skipped_invalid_range`, `invalid_enclosing_fallback`), `references_seen`, optional `message`. On Success skip/recovery fields are always present (incl. 0). WARN summary for disagreement/invalid-range is **stderr** only (O(1)); not part of the JSON payload |
| `dead-code --json` | yes (0149) | yes | schemaVersion 1 envelope; see rejected combos |
| `hotspots --json` | yes | yes | schemaVersion 1 object; collection `files`; list and `--semantic` echo `limit` (0207). **MCP `hotspots` stays an in-process array** |
| `hotspots trend --json` | yes (0151) | yes | schemaVersion 1; modes summary/full/entity; see schema below |
| `endpoints --json` | yes | yes | schemaVersion 1 object; collection `results` (0207). MCP `endpoints_changed` re-execs CLI and rides this envelope |
| `symbols --json` | yes (0163) | yes | schemaVersion **1** inventory; path/changed/kind/pub filters; COUNT-backed `totalMatching`; optional `indexStatus`; see schema below |
| `data-models list --json` | yes | yes | schemaVersion 1 object; collection `models` (0207); item `file_path` stays snake (0155); one row per logical model identity |
| `ci diff --json` / `ci list --json` | yes | yes | schemaVersion 1 object; collection `gates` (0207). Empty catalog is `gates: []`, `resultCount: 0`, no fake `emptyReason` |
| `config schema --json` | yes | yes | schemaVersion 1 object; collection `results` (0207). Empty keeps `emptyReason`/`message` |
| `dependencies list --json` | yes (0153) | yes | schemaVersion **1** envelope; `mode`: `direct` (default) \| `all`; live Cargo.toml+lock — not Cozo; see schema below |
| `scan --impact --json` | yes | yes | impact packet (`schemaVersion` string `"v1"`; no top-level `kind`) |
| `scan --json` / `scan --out` (no `--impact`) | yes (0180) | yes | **gitScan** envelope: numeric `schemaVersion` **1** + top-level **`kind: "gitScan"`** + ScanReport fields; **not** auto-impact |
| `scan --pr <range> --format json` | via `--format` | yes | PR-range machine output (not impact packet) |

\* Non-essential progress INFO suppressed under machine mode; hard failures still
use stderr.

### `release pins --json` schema (0201)

Pure stdout. Diffs GitHub Latest (`tag_name` + archive `assets[].digest`) against
in-tree packaging templates, live Homebrew tap / Scoop bucket remotes, and npm
`@ledgerful/mcp-server` `ledgerfulEngineTag`. Web launch-facts is **advisory**
and never flips overall status.

```json
{
  "schemaVersion": 1,
  "kind": "releasePins",
  "status": "match",
  "latest": { "tag": "v0.2.10", "sha": "c4a2308fe985" },
  "surfaces": [
    {
      "id": "mcp.inTree",
      "status": "match",
      "local": { "version": "0.1.19", "ledgerfulEngineTag": "v0.2.10" },
      "expected": { "ledgerfulEngineTag": "v0.2.10" },
      "remote": null
    }
  ],
  "advisory": {
    "launchFactsPath": "…/launch-facts.ts",
    "releaseTag": "v0.2.10",
    "mcpEngineTag": "v0.2.10",
    "status": "match"
  }
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | number **1** |
| `kind` | always **`"releasePins"`** |
| `status` | `match` (exit 0) / `drift` (exit 1) / `unverified` or `skipped` (exit 2) |
| `latest` | omitted on `skipped` and when Latest fetch failed; `sha` omitted when peel fails |
| `surfaces` | six required ids, **sorted by `id`**. Empty `[]` when `skipped` |
| `advisory` | present only if sibling `../ledgerful-web/src/lib/content/launch-facts.ts` existed; mismatch does **not** change overall status |

**Caveat:** `--auto-index` on any surface may print human progress on stderr when
an index refresh actually runs (ambient `try_auto_index` path). Prefer a fresh
index, or parse **stdout only** (not `2>&1`) when combining `--json --auto-index`.

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
| Non-`cli_summary` diagnostic `INFO` under default human (no `-v`, no `RUST_LOG`) | **suppressed** (0154: normal_layer max **WARN**) |

A command advertised as JSON must emit **only** JSON on stdout. Warnings must
not precede or follow that JSON on stdout.

**Human colour:** product colour uses `if_supports_color` (stream-aware). Honour
`NO_COLOR` (force off), `FORCE_COLOR` / `CLICOLOR_FORCE` non-empty non-`0`
(force on), else TTY/CI auto. Machine JSON paths stay colour-free. See track
0131.

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

**0154 extends the `normal_layer` WARN floor to default human runs** when
`RUST_LOG` is unset (or empty): non-verbose interactive CLI no longer emits
timestamped tracing-style `INFO target:` diagnostics on stderr. Dogfood-hot
probes (embed probe, semantic init, federated Scanning progress) are demoted
to `debug!` so ambient `RUST_LOG=info` does not re-flood them. Diagnostic
detail returns with `-v` / `RUST_LOG=debug` (or a specific `RUST_LOG` directive).
Product notices (web/viz bind, init success, layout migration, watch sync
summary, verify step-start) use `println!` / `eprintln!` / `cli_summary`, not
filterable `normal_layer` INFO. The `cli_summary` four-state table above is
**unchanged**.

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

### Rejected flag combinations (`doctor`)

| Combo | Error |
|---|---|
| `doctor --json --apply-hook-refresh` | `doctor --json cannot be combined with --apply-hook-refresh` |

Apply is always a human path (rewrites `.git/hooks` under opt-in). Detect-only
`doctor --json` remains pure schema-v1 findings JSON.

### Rejected flag combinations (`dead-code --json`)

| Combo | Error |
|---|---|
| `dead-code --json --prune` | `dead-code --json cannot be combined with --prune` |
| `dead-code --json --explain …` | `dead-code --json cannot be combined with --explain` |

Interactive prune and human explain have no machine schema (explain types lack
`Serialize`). Rejects run **before** storage/scan.

### Scan machine flags (`scan`)

| Combo | Behavior |
|---|---|
| `scan --json` / `scan --out` without `--impact` | exit **0** (product OK): **gitScan** summary envelope (0180). Escalate with `scan --impact --json` for the full impact packet. |
| `scan --summary` without `--impact` | exit **1**; `--summary requires --impact (impact brief summary)` — no PR `--format json` tip |
| `scan --json` with `--pr` | exit **1**; use `--format json` with `--pr` |
| `scan --json` with `--paths` | exit **1**; `--paths requires --impact` |
| `scan --json` auto-implies `--impact` | **Not supported** — impact analysis is expensive; pass `--impact` explicitly |

### `scan --json` gitScan schema (0180)

Pure stdout (or file only when `--out` is set — no dual dump). **Does not**
modify on-disk `latest-scan.json` schema (`ScanReport` remains without
`schemaVersion`/`kind`). Top-level **`kind` is intentional and new** on this
surface (other numeric-schema envelopes omit `kind`).

```json
{
  "schemaVersion": 1,
  "kind": "gitScan",
  "headHash": "…",
  "branchName": "main",
  "isClean": true,
  "changes": [],
  "diffSummaries": []
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | number **1** (not the impact packet string `"v1"`) |
| `kind` | always **`"gitScan"`** — primary discriminator vs impact / PR reports |
| `headHash` / `branchName` / `isClean` / `changes` / `diffSummaries` | Same semantics as durable `ScanReport` (camelCase) |
| `--base-ref` without impact | OK; `diffSummaries` often **`[]`** (working-tree diffs skipped) |

**Escalate:** `scan --impact --json` → full impact packet (string
`schemaVersion: "v1"`, risk/blast/agentSummary, …). Never emitted as gitScan-only.

---

## Default human `verify` contract (hooks / binary-first, 0121)

Installed pre-push shells call `ledgerful verify --scope fast` **without**
`--json`. After a PATH upgrade alone:

| Outcome | Default (non-verbose) stdout |
|---|---|
| **Pass** | Per-step `[i/n] Running:` + compact `ok` + elapsed; trailing `Verification passed`; **no** SUCCESS banner / plan banner / Suggested Actions |
| **Fail** | Per-step `[i/n] Running:` → `FAILURE` lines → structured fail block → Suggested Actions (if any) → miette on stderr; exit non-zero |

`--verbose` / `-v` restores plan banner, per-step SUCCESS (as-is, no compact ok
elapsed), aggregate “Running N step(s)…” progress `info!`, and Suggested Actions
on green. Step-start `[i/n] Running:` still emits under verbose. `--json`
never emits step-start / compact ok (pure schema + existing `durationMs`).

### Structured fail block (stdout, non-json fail)

Printed **before** Suggested Actions and before the final miette error:

```text
[Ledgerful] verify failed
step: cargo fmt --all
command: cargo fmt --all -- --check
exitCode: 1
failureDetail: <tool summary>
failedPaths: path1 path2   # only when formatter path extract yields ≥1 path
```

Field names use camelCase to correlate with the JSON wire. Paths are
best-effort from known formatter output (`cargo fmt`/`rustfmt --check`:
`Diff in <path>:` / `Diff in <path> at line N:`; `ruff format --check`:
`Would reformat: <path>`). Never invented; `\` → `/`; cap 50.

---

## `verify --json` schema (v1)

Primary example — **MappingRefuse** (default when fast cannot scope; empty
`test_mapping`, no DB connection, auto-index fail without allow). Exit ≠ 0,
`ok: false`, empty steps. Do **not** treat empty mapping as `running full`.

```json
{
  "schemaVersion": 1,
  "ok": false,
  "scopeRequested": "fast",
  "scopeExecuted": "refused",
  "fallbackReason": "fast scope unavailable — test_mapping is empty; run `ledgerful index --incremental` or use `--auto-index`; refusing full suite (~5-8 min)",
  "steps": [],
  "timestamp": "2026-07-28T12:00:00+00:00",
  "txId": "optional-pending-tx-id"
}
```

Optional — **full fallback** only for SharedInfra (always) or when the operator
passed `--allow-full-fallback` (restores pre-0135 surprise-full for mapping miss):

```json
{
  "schemaVersion": 1,
  "ok": true,
  "scopeRequested": "fast",
  "scopeExecuted": "full",
  "fallbackReason": "fast scope unavailable — shared infrastructure touched; running full (~5-8 min)",
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

Failed step example with path enrichment (additive; **schemaVersion stays 1**):

```json
{
  "name": "cargo fmt --all",
  "command": "cargo fmt --all -- --check",
  "status": "fail",
  "exitCode": 1,
  "durationMs": 400,
  "failureDetail": "Diff in src/lib.rs:",
  "failedPaths": ["src/lib.rs"]
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | integer | Always `1` for this contract |
| `ok` | boolean | `true` iff every step has `exitCode == 0`; **always `false` when refused** |
| `scopeRequested` | string | `fast` or `full` as passed on the CLI |
| `scopeExecuted` | string | ∈ {`fast`, `full`, `refused`}: `refused` when plan refused mapping-cannot-scope; `full` when SharedInfra / `--allow-full-fallback` fallback; else equals requested |
| `fallbackReason` | string (omitted when null) | Passthrough from the plan; present on fast→full fallback **and** on MappingRefuse (refusing string) |
| `steps` | array | **Plan order** (not alphabetically sorted); **`[]` when `scopeExecuted` is `refused`** |
| `steps[].status` | string | `"pass"` if `exitCode == 0`, else `"fail"` |
| `steps[].failureDetail` | string (omitted on pass) | stderr summary preferred |
| `steps[].failedPaths` | string[] (omitted when empty/pass) | Best-effort formatter paths; same sources as human fail block |
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

## `ledger status --json` / top-level `status --json` schema (v1)

Top-level `status --json` (track **0149**) routes into the **same**
`execute_ledger_status` path as `ledger status --json` — identical field set,
no second DTO. Top-level `status` accepts `--json` and `--compact` (not a full
alias of `ledger status`; no `--global` / `--all`). Track **0200** adds
`workRoot` and `stateDir` (schemaVersion stays **1**). Linked worktree
(**0108**): `workRoot` is this worktree; `stateDir` is the main `.ledgerful`.

```json
{
  "schemaVersion": 1,
  "workRoot": "C:\\dev\\ledgerful",
  "stateDir": "C:\\dev\\ledgerful\\.ledgerful",
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
| `schemaVersion` | `1` (added in 0093; **0200** additive, not a v2 bump) |
| `workRoot` | Absolute git worktree this command bound (same string as doctor `environment.workRoot`) |
| `stateDir` | Absolute `.ledgerful` directory (same string as doctor `environment.stateDir`; linked worktree shares main) |
| `pendingTxIds` | **Sorted** lexicographically for determinism |
| `promoteOrphanTxId` / `promoteError` | Omitted when absent |

Observe-mode would-block diagnostics go to **stderr** via `cli_summary`
`warn!`. Stdout remains parseable JSON alone.

### Invocation

```powershell
ledgerful status --json
ledgerful ledger status --json   # same payload
```

---

## `hotspots trend --json` schema (v1)

Track **0151**. Default is a **top-N file summary** (limit **20**), not the full
timestamp×file matrix. Pure stdout under `--json` (no human table). Mode
precedence: `--entity` > `-a/--all` > summary.

### Summary mode (default)

```json
{
  "schemaVersion": 1,
  "mode": "summary",
  "days": 30,
  "limit": 20,
  "truncated": true,
  "historyAvailable": true,
  "bootstrapHint": null,
  "totalFiles": 28,
  "totalEntries": 3690,
  "snapshotCount": 369,
  "files": [
    {
      "filePath": "src/commands/index/modes.rs",
      "latestScore": 0.044,
      "displayScore": 3.809,
      "priorDisplayScore": 3.790,
      "delta": 0.019,
      "sampleCount": 369,
      "lastRecordedAt": "2026-08-08T15:59:19.529795500+00:00",
      "commitHash": "85ebb481e9c882e0c7d90c5e6281fa739a9c7dc6"
    }
  ]
}
```

### Full mode (`--all`) and entity mode (`--entity`)

- **`mode`:** `"full"` or `"entity"`
- **`entries`:** row array with **snake_case** keys (`file_path`, `recorded_at`,
  `score`, `display_score`, `commit_hash`) — same as pre-0151 matrix dump
- **`limit` / `files`:** omitted
- **`truncated`:** always `false` (no file-rank cap)

### Per-mode field rules

| Field | summary | full (`--all`) | entity |
|---|---|---|---|
| `mode` | `"summary"` | `"full"` | `"entity"` |
| `totalEntries` | `rows.len()` | same | same (entity-filtered) |
| `snapshotCount` | distinct `recorded_at` | same | same |
| `totalFiles` | distinct `file_path` | same | **1** if any rows else **0** |
| `limit` | effective limit | omit | omit |
| `truncated` | `totalFiles > limit` | **false** | **false** |
| `files` | top-N array | omit | omit |
| `entries` | omit | full row array | entity row array |

| Field | Rules |
|---|---|
| `schemaVersion` | always **1** |
| `historyAvailable` | **true** when `totalEntries > 0` **or** bootstrap/history flags say history exists — **never** false while `files`/`entries` non-empty |
| `bootstrapHint` | null when history available; bootstrap command string when empty |
| `commitHash` | **full** hash (no abbreviate) |
| `priorDisplayScore` / `delta` | omit when `sampleCount < 2` (never invent Δ from 0) |

### Invocation

```powershell
ledgerful hotspots trend --json
ledgerful hotspots trend --limit 5 --json
ledgerful hotspots trend --all --json
ledgerful hotspots trend --entity src/lib.rs --json
```

---

## `symbols --json` schema (v1)

Track **0163**. Pure stdout object — scoped, index-backed symbol inventory
(not search ranking). Default `--limit 200`, hard max **5000** (clap range).
`totalMatching` is a true `COUNT(*)` under the same filters (not overfetch).
Identity for consumers (e.g. AI-Brains T233): **`(path, name, kind)`** (+
`line` when present) — **`qualifiedName` is not globally unique**.

```json
{
  "schemaVersion": 1,
  "scope": {
    "path": "src/commands",
    "changed": false,
    "kind": "Function",
    "pubOnly": true
  },
  "limit": 200,
  "truncated": false,
  "resultCount": 12,
  "totalMatching": 12,
  "symbols": [
    {
      "name": "execute_step",
      "kind": "Function",
      "path": "src/verify/engine.rs",
      "line": 40,
      "isPublic": true,
      "qualifiedName": "execute_step"
    }
  ],
  "indexStatus": {
    "state": "missing",
    "remediation": "ledgerful index --incremental"
  }
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | always **1** |
| `scope.path` / `scope.kind` | **null** when unset; `kind` is **canonical** PascalCase (never raw `fn`) |
| `scope.changed` / `scope.pubOnly` | always present bools |
| `truncated` | `totalMatching > limit` |
| `resultCount` | `symbols.len()` (≤ limit) |
| `totalMatching` | **COUNT** before limit (same filters) |
| `symbols[].line` | **omit** when unknown — never JSON `null` |
| `symbols[].qualifiedName` | omit when empty/absent; not a global vault key |
| `indexStatus` | **optional**; omit when index usable. When present: `state` + optional `remediation`. Used when `ledger.db` is **missing** without `--auto-index` (exit **0**, empty `symbols`). Other open failures propagate as errors (not silent empty). |
| Empty | full envelope, `symbols: []`, exit **0** |

### Flags

| Flag | Notes |
|---|---|
| `--path` | **Prefix** (`file_path == prefix` OR starts with `prefix/`); not endpoints substring. Trailing `/` trimmed; empty-after-trim → error |
| `--changed` | WT change set ∩ indexed paths; empty change set → empty inventory exit 0; **includes Deleted** and rename **old_path** still in index until re-index; membership **case-insensitive on Windows only** |
| `--kind` | Single kind; aliases (`fn`→`Function`, `mod`/`module`→`Module`, …); Class/Interface reserved/unpopulated |
| `--pub` | `is_public = 1` |
| `-l/--limit` | default 200; range `1..=5000` |
| `--auto-index` | bootstrap missing DB + `try_auto_index`; fatal under `--json` emits no partial machine stdout |

### Invocation

```powershell
ledgerful symbols --path src/commands --pub --limit 50 --json
ledgerful symbols --changed --json
ledgerful symbols --kind fn --path src/cli --json
```

---

## `dependencies list --json` schema (v1)

Track **0153**. Pure stdout object (not a bare array). Default mode lists
**declared direct** dependencies from live `Cargo.toml` with locked versions
from the **root package’s** `Cargo.lock` `dependencies` array. Full lock is
`--all`. No Cozo / index required. No progress on stdout under `--json`.

```json
{
  "schemaVersion": 1,
  "mode": "direct",
  "ecosystem": "rust/cargo",
  "root": {
    "name": "ledgerful",
    "version": "0.2.7",
    "source": "manifest"
  },
  "directCount": 96,
  "lockPackageCount": 859,
  "packages": [
    {
      "name": "clap",
      "version": "4.6.1",
      "kind": "normal",
      "ecosystem": "rust/cargo",
      "source": "registry+https://github.com/rust-lang/crates.io-index",
      "optional": false,
      "req": "4.6.1"
    }
  ]
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | always **1** |
| `mode` | `"direct"` (default) or `"all"` (`--all`) |
| `ecosystem` | `"rust/cargo"` for this surface |
| `root` | Live `[package]` name + version; `source` is `"manifest"` when version is a plain string in the manifest, or `"lock"` when filled from the root lock package (e.g. `version.workspace = true`) |
| `directCount` | Declared direct rows after kind expansion (all kinds + target tables) |
| `lockPackageCount` | `[[package]]` count from live lock (0 if no lock) |
| `packages` | Direct rows (`mode=direct`) or full lock rows (`mode=all`); sorted kind then name (direct) or name/version (all) |
| `packages[].version` | Locked version string, or **omitted/null** when not selected / no lock |
| `packages[].kind` | `normal` \| `build` \| `dev` in direct mode; omit in `--all` |
| `packages[].source` | Lock source string when known; omit/null for path/workspace |
| `packages[].optional` / `req` / `target` | Present when known from manifest (direct mode) |
| `truncated` | **Omitted** (no truncation logic on this surface) |

### Invocation

```powershell
ledgerful dependencies list --json
ledgerful dependencies list --all --json
ledgerful dependencies list -v --json
```

---

## `dead-code --json` schema (v1)

Track **0149**. Single camelCase object on stdout. Spinners, human tables,
SUCCESS lines, and stale-index banners are off under `--json`.

```json
{
  "schemaVersion": 1,
  "threshold": 0.75,
  "limit": 50,
  "includeTraits": false,
  "truncated": false,
  "findingCount": 1,
  "findings": [
    {
      "symbolName": "unused",
      "filePath": "src/u.rs",
      "confidence": 0.81,
      "factors": [
        "noTestCoverage",
        { "gitInactive": { "daysSinceLastCommit": 42 } }
      ],
      "recommendation": "…",
      "lineStart": 10,
      "lineEnd": 20
    }
  ],
  "heuristicNote": "Heuristic evidence — not proof of dead code. Factors include reachability, git activity, and test coverage."
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | number | Always **1** |
| `threshold` / `limit` / `includeTraits` | echo of CLI flags | |
| `truncated` | bool | **Honest overfetch:** `scan_repo(limit + 1)` then `truncated = len > limit`; display cap is `limit` |
| `findingCount` | number | `findings.len()` after cap |
| `findings` | array | Sorted confidence desc; reuses `DeadCodeFinding` Serialize |
| `findings[].factors` | **mixed shape** | Unit variants serialize as **camelCase strings** (`"noTestCoverage"`, `"unreachableFromEntrypoints"`); `GitInactive` is an object `{"gitInactive":{"daysSinceLastCommit":N}}`. **Do not** flatten to string-only |
| `heuristicNote` | string | Always present; findings are heuristic, not proof |

**Empty results:** `findings: []`, `findingCount: 0`, exit **0**.

### Invocation

```powershell
ledgerful dead-code --json --threshold 0.75 --limit 50
```

---

## List `--json` envelope (0207)

Populated and empty **list** commands share one object family. Collection
field names stay command-specific (`results` / `impacted` / `files` /
`models` / `gates` / `mappings`). No `--json-raw`. Nested item keys such as
`file_path` / `slo_count` stay as-is.

```json
{
  "schemaVersion": 1,
  "results": [ { "method": "GET", "path": "/health" } ],
  "resultCount": 1
}
```

| Command | Collection key | Extra |
|---|---|---|
| `endpoints --json` | `results` | Empty keeps `emptyReason`/`message` |
| `config schema --json` | `results` | Empty keeps `emptyReason`/`message` |
| `security impact --json` | `impacted` | `indexedCount` (unfiltered denominator); empty and populated |
| `observability coverage --json` | `results` | Item `slo_count` / `metric_count` stay snake |
| `data-models list --json` | `models` | Item `file_path` stays snake |
| `data-models impact --json` | `impacted` | |
| `hotspots --json` (list + `--semantic`) | `files` | List and `--semantic` echo `limit`. No `truncated` (no extra overfetch) |
| `ci diff --json` / `ci list --json` | `gates` | Empty catalog: `gates: []`, `resultCount: 0`, **no** `emptyReason` |
| `tests --json` (mapped) | `mappings` | Additive `resolvedPath` (omit when none); empty arms use the helper |

Empty helper arm: `emptyReason` + `message` present. Populated helper arm:
those keys **omitted** (never JSON `null`). `schemaVersion` stays **1**.
No top-level `kind` on this helper (0180 `kind` is gitScan-only).

**MCP honesty:** `endpoints_changed` text is the CLI envelope (re-exec
`endpoints --changed --json`). MCP `hotspots` is in-process and **remains a
hotspot array** — do not parse it as `{files:[…]}`.

---

## `index --check --json` schema (0149 purity + 0207 camelCase)

CLI DTO at the print site. Domain `IndexStatus` / `IndexFreshnessAssessment`
stay snake internally (no `rename_all`). On **success**, human Info lines
(e.g. `Index is up to date.`) are **not** emitted on stderr (0149; previously
Info was routed to stderr under json, which broke `2>&1 | ConvertFrom-Json`).
On **failure**, Error diagnostics still go to **stderr** (including under
`--json`) so CI gates keep a human reason; JSON is printed first when the
check path still emits status before `process::exit`.

```json
{
  "schemaVersion": 1,
  "kind": "indexCheck",
  "totalFiles": 760,
  "totalSymbols": 19134,
  "staleFiles": 0,
  "lastIndexedAt": "2026-08-22T12:00:00Z",
  "assessment": {
    "state": "FreshPopulated",
    "staleFiles": 0
  }
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | number **1** |
| `kind` | always **`"indexCheck"`** |
| `totalFiles` / `totalSymbols` / `staleFiles` / `lastIndexedAt` | camelCase; `lastIndexedAt` omitted when absent |
| `assessment.state` | Enum **values** stay **PascalCase**: `FreshPopulated`, `ContentStalePopulated`, `NeverIndexed`, `StaleEmpty`, `StalePopulated`, `FreshEmpty`, `Indeterminate`. Nested `emptyReason` / `source` values also PascalCase (`AllIndexableCandidatesIgnored`, `RepositoryMetadata`, …) |
| Nested assessment fields | camelCase (`emptyReason`, `staleFiles`, `emptyDiagnostics`, `indexedFiles`, …). Absent optionals **omitted** (never JSON `null`) |

`assessment.state` already carries Fresh/Stale — do not require stderr Info
for machine consumers. **Ban:** `FreshPopulated` with top-level `staleFiles > 0`.

---

## `search --json` schema (v1)

Track **0136**. Single camelCase object on stdout (same mental model as
`doctor` / `change-context` / `verify`). Whole-stdout parsers
(`ConvertFrom-Json`, one `serde_json::from_str`, MCP tool text) succeed on
multi-hit output.

```json
{
  "schemaVersion": 1,
  "query": "change-context",
  "mode": "bm25",
  "limit": 3,
  "truncated": false,
  "resultCount": 3,
  "results": [
    {
      "kind": "bm25_match",
      "path": "src/commands/change_context/mod.rs",
      "score": 16.9,
      "content": "plain snippet (no ANSI, no HTML entities)"
    }
  ]
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | number | Always **1** |
| `query` | string | Echo of the search query |
| `mode` | string | Requested/selected engine: `bm25` \| `regex` \| `semantic` \| `hybrid`. Fuzzy fallback **keeps parent mode**; per-hit source is `results[].kind` (`fuzzy_match`) |
| `limit` | number | Requested limit |
| `truncated` | bool | `true` when overfetch shows more hits than `limit` |
| `resultCount` | number | `results.len()` |
| `results` | array | Match hits only — **not** status/readiness meta |
| `results[].kind` | string | `bm25_match` \| `regex_match` \| `fuzzy_match` \| `insight` |
| `results[].path` | string | Repo-relative path |
| `results[].line` | number \| **omitted** | Present only when known — never JSON `null` |
| `results[].score` | number \| **omitted** | Same omit policy as `line` |
| `results[].content` | string | **Plain** snippet |
| `searchIndexStatus` | object \| **omitted** | Empty-index / FTS-rebuild honesty (`state`, `documentCount`, optional `remediation` / `error`) |
| `semantic` | object \| **omitted** | On `--semantic` paths: readiness fields + optional `error` |
| `fallbackUsed` | string \| **omitted** | When hybrid empty path used identifier-literal AllPaths fallback and produced ≥1 hit: `"identifier_literal"`. `schemaVersion` stays **1**; kind vocabulary unchanged (`regex_match` for those hits) |

**Empty results:** full envelope with `results: []`, `resultCount: 0`, exit **0**.

**Fatal auto-index** (`try_auto_index` Err under `--auto-index`): **no** machine
stdout (no partial envelope), non-zero exit; diagnostics on stderr.

### Migration: `--json-lines`

Pre-0136 `--json` emitted **NDJSON** BridgeRecord lines (`record_kind`,
`bridge_version`, timestamps). That stream is opt-in:

```powershell
ledgerful search --json-lines --limit 5 -- "change-context"
```

Do **not** whole-parse `--json-lines` stdout. `--json` and `--json-lines`
conflict (clap reject).

### Invocation

```powershell
ledgerful search --json foo bar
ledgerful search --json --limit 5 -- "change-context" | ConvertFrom-Json
ledgerful search --json --semantic -- "blast radius"
```

Unquoted multi-word argv (`search --json foo bar`) joins to the same `query`
string as `search --json "foo bar"`. Flags may appear before or after words.
Keep `--` for hyphen-leading tokens (`search -- --json`). Shell quotes do not
hide a leading hyphen from clap. Envelope `query` remains one string
(schemaVersion 1; no `queryTokens`).

MCP tool `search` spawns `search --json` (envelope; never `--json-lines`).

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
  "affectedFlows": {
    "status": "available",
    "flowCount": 2,
    "flowCapped": false,
    "flowTotal": 2,
    "flows": [
      {
        "method": "GET",
        "pathPattern": "/api/health",
        "handlerSymbolName": "health_handler",
        "handlerFile": "src/handlers/health.rs",
        "framework": "Axum",
        "matchKind": "handler_impl_file",
        "routeConfidence": 1.0
      }
    ],
    "notes": [
      "Registered HTTP routes only (api_routes); not distributed traces or CRG-style call-chain flows."
    ]
  },
  "changeHints": {
    "kind": "greenfield",
    "mostlyAdded": true,
    "addedCount": 3,
    "totalChanged": 3,
    "newPackagePrefixes": ["src/newpkg"],
    "surfaceTags": ["cli_surface", "new_entrypoint", "new_module"],
    "suggestedTests": [
      {
        "path": "src/newpkg/cli_test.rs",
        "kind": "convention",
        "reason": "conventional test path (to be created)"
      },
      {
        "path": "tests/newpkg/mod.rs",
        "kind": "convention",
        "reason": "conventional test path (to be created)"
      }
    ],
    "notes": [
      "No structural test_mapping for new paths; suggestions are path conventions, not proven coverage."
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
  "nextActions": [
    "ledgerful verify --scope fast",
    "review changeHints.suggestedTests and add covering tests for new surfaces"
  ],
  "impactSchemaVersion": "v1"
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | u32 | Always **`1`** for this packet (doctor/verify style) |
| `impactSchemaVersion` | string | Forwarded from `ImpactPacket.schema_version` (different field/type) |
| `status` | string | `ready` \| `empty` \| `not_ready` |
| `summary` | string | Freeform one-line human/JSON summary (0114+). **Unchanged** by 0173. |
| `agentSummary` | object | **0173** structured scannable header. **Coexists** with `summary` — does not replace it. Present on `ready`/`empty`; **omitted** on `not_ready`. Fields: `riskOneLiner`, `changed` (`total`/`code`/`governance`/`contract`), `topSymbols` (≤5), `mustTouchSample` (≤5), `suggestedTestsSample` (≤3), `demotedTemporalCount`, `pathMode` (`code`\|`all`), `analysisMode` (`working_tree`\|`base_ref`\|`prospective`). |
| `readSetCapped` / `readSetTotalCandidates` | bool / usize | **Required**; `true` / total when truncated by `--max-files` |
| `blast` | object | **Counts only** — not full edges. Always includes nested `confidenceSummary` (class counts: `scipBound`, `resolved`, `ambiguous`, `unresolved`, `capped`, `unknown`, `expandable`, `total`) at both `minimal` and `standard` detail. Same shape as ImpactPacket `blastRadius.confidenceSummary`. Full edges remain on `impact --json` only (0114 token-budget fence). |
| `testCoverage` | object | **0115 deepened** structural test-gap report (same schema as PR `testGaps`). Status: `available` \| `empty_mapping` \| `missing_table` \| `no_source_seeds` \| `unavailable`. **Never** bare `"empty"`. Empty `ImpactPacket.test_coverage` vec is **not** full cover — use these counts/status. Caps: unmapped ≤20, mappedSample ≤5. Notes always include structural + LCOV ceiling. |
| `affectedFlows` | object | **0118** nested affected HTTP-route summary (same schema as ImpactPacket / PR `affectedFlows`). Status: `available` \| `empty_map` \| `missing_table` \| `no_change_seeds` \| `unavailable`. **Never** bare `"empty"`. **Route map only** (registered `api_routes` + handler binds + optional blast edges) — **not** CRG-style call-chain / execution-path traces. Present at both `minimal` and `standard` detail. Sample caps: **`flows` take(5)** when `detail=minimal`, **take(10)** when `standard` (counts `flowCount`/`flowTotal` pass through full report; impact library cap is 20). `endpoints --changed` uses the **same match library with uncapped keys** (filter is not truncated at 20). `available` + `flowCount` 0 = all-clear (no registered routes touched). **Framework fence:** Rust Axum/Actix/Rocket; Go Gin/`net/http`; TS Express/Fastify; Python FastAPI/Flask — not all languages. **Agent metadata:** CRG may ship `context_savings` (token estimates); Ledgerful ships `affectedFlows` + `testCoverage`/`testGaps` + blast/`confidenceSummary` counts — **different signals**, not substitutes. Go route extractors exist; CRG-style path recall on Go is weaker (~33% on some peer fixtures) — registration map only here. See `docs/Call-Resolution.md` §Affected HTTP flows. |
| `changeHints` | object | **0127** greenfield / new-surface hints. Present only when file changes exist; **omitted** on `status=empty` / `not_ready` (no clean-tree noise). `kind`: `greenfield` \| `mixed` \| `none`. Pure-add = `status==Added` **and** no `old_path` (renames excluded). `mostlyAdded` when pure-added/source-like ≥ 0.6 (or all-added ≥2). `newPackagePrefixes` ≤5 (isolated pure-add dirs). `surfaceTags`: `new_module` / `new_entrypoint` / `cli_surface` / `new_test` (path/basename primary). `suggestedTests` ≤10 path-unique, ladder **mapped → convention → adjacent** (`kind` + honesty `reason`). Convention reasons encode exists-on-disk vs to-be-created — **not** proven coverage. Notes cap ≤5. Summary may append `greenfield-ish (N added / M total; prefixes: …)` (≤3 prefixes, last-2 segments if deep). |
| `doctor` / `ledger` | object | **Always present** on successful builds (including `status=empty`) |
| `doctor.topFindings` | array | From sidecar `findings` after a successful `doctor` write (0129 + **0138**): **action-critical** only — severity `block` always, or `warn` when category ≠ `optional` (optional-category warns **excluded** so flaky backends do not crowd the cap-5 budget). Severity-first (block before warn), then code/message, **cap ≤5**. Each entry: `code`, `severity`, `message`, optional `remediation` when present (never `null`). **Empty is OK** when the only warns are optional (or only info) — `doctor.warn` may still be >0; inspect full `ledgerful doctor --json` for optional backends. Full `doctor --json` `findings[]` remains the complete SoT and **includes `category`** on each finding (agents can self-filter). Empty also when doctor not run / sidecar missing / pre-0129 count-only sidecar. |
| `analysisWarnings` | array | Ambient analysis health (not diff risk). Empty-tree federation schema-unavailable/invalid lands here (same greppable string as historical medium riskReasons): `Cross-repo impact: Sibling '…' schema is unavailable or invalid.` Clean tree with only those warnings → `riskLevel=low` and empty/non-medium sole riskReasons. Real `[FEDERATED]` modify / interface-removed stay on `riskReasons`. |
| Empty-tree risk | — | `status=empty` is independent of `analysisWarnings` (file changes + pending ledger only). Do **not** escalate solely because historical medium federation noise — product routes schema-miss to warnings (0129). |

| `status` | When |
|---|---|
| `ready` | Non-empty file changes, **or** clean tree with `ledger.pendingCount >= 1` |
| `empty` | No file changes **and** `ledger.pendingCount == 0` (doctor still present) |
| `not_ready` | Layout/impact hard failure; `reason` + `nextActions` set |

### Stream rules

- `--json` → **machine mode**: pure JSON on **stdout only** (0093).
- Human mode (no `--json`): print **`agentSummary` header first**, then status,
  freeform `summary`, risk, readSet count, readyForPublish, next steps.

### Path mode (code vs governance) — 0173 / 0202

Default **`pathMode=code`**: process/governance temporal couplings (conductor /
deferred / process docs, and code↔governance pairs) are **demoted** from risk
weight/reasons and from `readSet` priority-3. **0202** also demotes
`CHANGELOG.md` temporal pairs (CHANGELOG stays **Contract** / p1-when-changed)
and ancestor-path (directory-prefix) pairs such as `packaging` ↔
`packaging/homebrew/ledgerful.rb`. Full `temporalCouplings` remain on
`impact --json` for audit; `demotedTemporalCount` is honest. Restore pre-0173
process demotion and the **pre-0202 CHANGELOG wall** with
**`--include-governance`** (`pathMode=all`). Other contract allowlist paths
(agent-output-contract, Engineering, SKILL.md, Cargo.toml, …) never demote.

### Prospective `--paths` — 0173

```powershell
ledgerful change-context --json --paths src/foo.rs
ledgerful change-context --json --paths src/a.rs,src/b.rs
ledgerful impact --paths src/foo.rs --summary
ledgerful scan --impact --json --paths src/foo.rs
```

- Synthetic snapshot: on-disk → Modified; missing → **Added** (greenfield).
- `analysisMode=prospective`; `is_clean=false` so empty-tree short-circuit does not fire.
- Mutually exclusive with `--base-ref`. Cap ≤ 50. Empty/whitespace → usage error.
- **Write policy:** prospective does **not** rewrite `latest-impact.json`
  (in-memory only). Working-tree impact without `--paths` keeps current write.

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
`blast_depth`, **`paths[]`**, **`include_governance`**). Same builder as the CLI;
impact is in-memory and does **not** rewrite `.ledgerful/reports/latest-impact.json`.

**MCP `scan`:** remains a full-impact dump **without** `paths` / `include_governance`
in v1 — use CLI or `change_context` for prospective / path mode.

### Invocation

```powershell
ledgerful doctor --json
ledgerful change-context --json
ledgerful change-context --json --detail minimal --max-files 5
ledgerful change-context --json --base-ref HEAD~1
ledgerful change-context --json --paths src/impact/analysis/temporal.rs
ledgerful change-context --json --include-governance
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
