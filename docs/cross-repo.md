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
