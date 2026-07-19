# Cross-repo rollup

## Global rollup

`ledgerful ledger status --global` reads the per-repo `ledger.db` files that the user already owns across their own repositories on disk. It does not move, copy, or transmit anything. It is a local file-system read.

- Each per-repo database is opened **read-only**.
- The walk is local only: no network requests, no central store, no account, no sync.
- The cache at `~/.ledgerful/rollup/cache.sqlite` is derived-only, deletable, and recreated from the per-repo DBs on demand.
- `--opt-out` disables the view; `--opt-in` restores it. Both write only to the user config at `~/.ledgerful/config.toml`.

## Configuration

The optional `[global_rollup]` table in `~/.ledgerful/config.toml` controls the behavior:

| Field | Type | Default | Description |
|---|---|---|---|
| `roots` | `[PathBuf]` | `["~"]` | Directories to walk looking for `.ledgerful/state/ledger.db`. |
| `timeout_secs` | `u64` | `30` | Hard backstop deadline for the discovery walk (seconds). |
| `staleness_secs` | `u64` | `3600` | Cache entries older than this trigger a re-walk. |
| `max_depth` | `Option<usize>` | `None` | Optional maximum directory depth (`None` = unlimited). |
| `enabled` | `bool` | `true` | Master switch controlled by `--opt-out` / `--opt-in`. |

## JSON output

`ledger status --global --json` emits a single JSON object with the following schema (field names use camelCase):

### `GlobalPostureOutput`

| Field | Type | Description |
|---|---|---|
| `totalRepos` | `number` | Number of repos successfully queried. |
| `skippedRepos` | `number` | Number of repos skipped due to per-repo errors. |
| `repos` | `[RepoPosture]` | Per-repo posture summaries, sorted worst-first. |
| `warnings` | `[string]` | One-line warnings for skipped roots or per-repo failures. |

### `RepoPosture`

| Field | Type | Description |
|---|---|---|
| `repoPath` | `string` | Absolute path to the repository root. |
| `unsignedEntries` | `number` | Count of ledger entries that fail signature validation. |
| `pendingTx` | `number` | Count of pending transactions. |
| `drift` | `number` | Count of unaudited drift transactions. |
| `lastVerifyResult` | `string \| null` | `PASS` or `FAIL` from the latest verification run, if any. |
| `lastVerifyAt` | `string \| null` | ISO-8601-ish timestamp of the latest verification run, if any. |

### Example

```json
{
  "totalRepos": 2,
  "skippedRepos": 0,
  "repos": [
    {
      "repoPath": "/home/user/dev/alpha",
      "unsignedEntries": 3,
      "pendingTx": 1,
      "drift": 0,
      "lastVerifyResult": "FAIL",
      "lastVerifyAt": "2026-07-17T12:34:56"
    },
    {
      "repoPath": "/home/user/work/beta",
      "unsignedEntries": 0,
      "pendingTx": 0,
      "drift": 0,
      "lastVerifyResult": null,
      "lastVerifyAt": null
    }
  ],
  "warnings": []
}
```

The text table printed without `--json` contains the same values.

## Global timings

`ledgerful timings --global` unions per-repo `command_timings` rows across the
same discovered repos as the posture rollup (same `[global_rollup]` roots,
discovery walk, and derived cache for repo discovery).

### Behavior

- Each per-repo database is opened **read-only** (sequential open → query → close).
- If `command_timings` is **absent** on a repo (pre-m52 / not yet migrated), that
  repo is skipped for timings — not an error.
- Per-repo open/query failures are **warn-and-skip** (non-fatal).
- Outer summaries **pool raw duration samples** across repos, then recompute
  p50/p95/p99/total/runs on the pooled set (they do **not** average per-repo
  percentiles).
- Sorted by `total_ms` DESC, then `command` ASC; `--top` applies after sort.
- If no discovered repo has the table: honest message
  `per-repo timing not enabled (… see 0043 / self-timing)` and exit 0.
- If tables exist but the window is empty: `no global timing rows …` and exit 0.
- No fabricated rows. No network. Signing basis is untouched.
- Respects `global_rollup.enabled`: when disabled, prints the same style
  one-liner as `ledger status --global` and exits 0.

### Supported flags

| Flag | Global behavior |
|---|---|
| `--json` | JSON envelope (see below) |
| `--top N` | Cap commands after sort (default 20) |
| `--days N` | Window for outer/inner/flame (default 30) |
| `--export PATH` | Write JSON summary (or collapsed text with `--flame`) |
| `--inner` | Aggregate `span_name` samples across repos |
| `--command NAME` | Filter `--inner` / `--flame` to one command |
| `--flame` | Collapsed stacks with `{repo_basename};{command}[;span] duration` |
| `--explain COMMAND` | Pool last 7d + prior 7d outer samples across repos; one sentence |
| `--opt-in` / `--opt-out` | User-config self-timing capture (same as local; works with or without `--global`) |
| `--prune` | **Refused** — global path never writes per-repo DBs |

Flame stacks prefix the **repo basename** so identical command/span names from
different repos remain distinguishable in speedscope.

### JSON output (`timings --global --json`)

Field names use **camelCase on the envelope** (`schemaVersion`, `totalRepos`,
`reposWithTimings`, …). Nested objects in `data[]` and `repos[]` use **snake_case**
to match the local timings schema (`command`, `runs`, `p50_ms`, … / `repo_path`).

`--inner` uses the same camelCase envelope; each `data[]` element is a
`GlobalInnerAgg` with snake_case keys matching local `timings --inner`
(`span_name`, `samples`, `total_ms`, `max_ms`).

### `GlobalTimingsSummary`

| Field | Type | Description |
|---|---|---|
| `schemaVersion` | `number` | Always `1` for this shape. |
| `totalRepos` | `number` | Discovered repos considered. |
| `reposWithTimings` | `number` | Repos whose DB has `command_timings`. |
| `skippedRepos` | `number` | Repos skipped due to open/query errors. |
| `timingsAbsent` | `number` | Repos opened successfully but missing the table. |
| `warnings` | `[string]` | Per-repo / walk warnings. |
| `message` | `string \| omitted` | Honest empty-state text when `data` is empty. |
| `data` | `[CommandTimingSummary]` | Pooled outer summaries (top-N). |
| `repos` | `[RepoCommandTiming]` | Per-repo breakdown for honesty. |

### `RepoCommandTiming`

| Field | Type | Description |
|---|---|---|
| `repo_path` | `string` | Absolute repository root. |
| `command` | `string` | Command name. |
| `runs` | `number` | Outer run count in this repo. |
| `p50_ms` / `p95_ms` / `p99_ms` | `number` | Percentiles for this repo only. |
| `total_ms` | `number` | Sum of outer durations in this repo. |

### `GlobalInnerAgg` (`--inner`)

| Field | Type | Description |
|---|---|---|
| `span_name` | `string` | Engine-internal span label (or `<unnamed>`). |
| `samples` | `number` | Count of inner rows across repos. |
| `total_ms` | `number` | Sum of durations. |
| `max_ms` | `number` | Max single-sample duration. |

### Example

```json
{
  "schemaVersion": 1,
  "totalRepos": 2,
  "reposWithTimings": 2,
  "skippedRepos": 0,
  "timingsAbsent": 0,
  "warnings": [],
  "data": [
    {
      "command": "verify",
      "runs": 5,
      "p50_ms": 30,
      "p95_ms": 50,
      "p99_ms": 50,
      "total_ms": 150
    }
  ],
  "repos": [
    {
      "repo_path": "/home/user/dev/alpha",
      "command": "verify",
      "runs": 3,
      "p50_ms": 20,
      "p95_ms": 30,
      "p99_ms": 30,
      "total_ms": 60
    }
  ]
}
```

See also [self-timing.md](./self-timing.md) for capture semantics and the local
`timings` surface.
