# PR scan schema (`scan --pr --format json`)

This document defines the stable, versioned machine-readable output produced by
`ledgerful scan --pr <range> --format json`. It is the contract that the
`ledgerful-action` GitHub Action pins. Breaking changes bump `schemaVersion`.

## Invocation

```bash
ledgerful scan --pr main...HEAD --format json
ledgerful scan --pr main...HEAD --format json --out pr-scan.json
ledgerful scan --pr main...HEAD --format text
```

- `--pr <range>` accepts `base...head`, `base..head`, or a bare `base`
  (defaulting head to `HEAD`). It is mutually exclusive with `--impact`.
- `--format` accepts `json` or `text`. Default is `text`.
- `--out <path>` writes the JSON report to a file.

## Schema

```json
{
  "schemaVersion": 1,
  "generatedAt": "2026-07-17T12:00:00+00:00",
  "baseRef": "main",
  "headRef": "HEAD",
  "headHash": "abc123...",
  "branchName": "feature/x",
  "treeClean": true,
  "changeCount": 3,
  "changes": [
    {
      "path": "src/foo.rs",
      "changeType": "modified"
    },
    {
      "path": "src/bar.rs",
      "changeType": "renamed",
      "oldPath": "src/old_bar.rs"
    }
  ],
  "riskLevel": "low",
  "riskReasons": [],
  "analysisWarnings": []
}
```

### Field reference

| Field | Type | Description |
|---|---|---|
| `schemaVersion` | integer | Breaking schema changes increment this value. The Action pins a specific version. |
| `generatedAt` | ISO 8601 string | UTC timestamp in RFC 3339 format. Volatile; the Action does **not** pin this. |
| `baseRef` | string | Base ref used for the diff. |
| `headRef` | string | Head ref used for the diff. |
| `headHash` | string \| null | Commit hash at HEAD, if available. |
| `branchName` | string \| null | Current branch name, if available. |
| `treeClean` | boolean | Whether the diff between `baseRef` and `headRef` is empty (no changes). In CI/PR mode this reflects diff emptiness, not working-tree dirtiness. |
| `changeCount` | integer | `len(changes)`. |
| `changes` | array | Sorted by `path`. Forward-slash normalized for cross-platform determinism. |
| `changes[].path` | string | Forward-slash normalized path. |
| `changes[].changeType` | string | `added`, `modified`, `deleted`, or `renamed`. |
| `changes[].oldPath` | string (omitted when not a rename) | Present only when `changeType` is `renamed`; otherwise the field is omitted. |
| `riskLevel` | string | `low`, `medium`, or `high`. |
| `riskReasons` | array of strings | Sorted alphabetically, deterministic reasons for the risk level. |
| `analysisWarnings` | array of strings | Sorted alphabetically; partial-scan or other non-fatal notes. |

## Determinism contract

For the same `(baseRef, headHash, repoState)`, running `scan --pr` twice
produces byte-identical JSON except for `generatedAt`. The caller must strip or
ignore `generatedAt` when diffing or hashing.

Specific guarantees:

- `changes` is sorted by `path` ascending.
- `riskReasons` and `analysisWarnings` are sorted alphabetically.
- Paths are forward-slash normalized (`\` → `/`).
- `schemaVersion` is a stable integer; breaking changes bump it.

## Risk derivation

Risk is lightweight and deterministic; it does **not** depend on the full
impact-analysis enrichment pipeline.

Start at `low`.

- `changeCount >= 10` → `medium` (reason: "N files changed (>= 10)").
- Any changed path matches a sensitive-path pattern → `high` (reason:
  "sensitive path touched: <path>").
- `changeCount >= 30` → `high` regardless (reason: "N files changed (>= 30)").

Sensitive patterns:

- `Cargo.toml` (exact file-name match)
- `Cargo.lock` (exact file-name match)
- `.github/workflows/` (directory-prefix match)
- `crypto.rs` (exact file-name match; covers any `crypto.rs` at any depth)
- `migrations/` (directory-prefix match)
- `.ledgerful/` (directory-prefix match; covers all ledgerful state files including `config.toml`)
- `deny.toml` (exact file-name match)
- `SECURITY.md` (exact file-name match)

All `riskReasons` are sorted alphabetically.

## Missing base commit error

When the base commit is not in the local clone (typical with
`actions/checkout` default `fetch-depth: 1`), the engine emits a clear,
actionable error instead of a cryptic git failure:

```text
error: base commit '<base>' is not present in the local clone.
       This usually means the checkout was shallow (fetch-depth: 1).
       Fix: set `fetch-depth: 0` in your actions/checkout step, or fetch the base ref explicitly.
```

This is detected from git stderr containing any of:
`Not a valid object name`, `unknown revision`, `bad revision`,
`does not exist`, or `Invalid symmetric difference expression`.

## No-network invariant

The engine slice adds zero network code. `scan --pr` shells out to `git` and
reads the local repo state, exactly like the existing `--base-ref` path. The
Action wrapper (a separate repo) owns the GitHub API call. The privacy grep for
`ureq`, `reqwest`, and `tokio_tungstenite` in the scan code path must stay green.
