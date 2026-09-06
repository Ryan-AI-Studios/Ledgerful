# PR scan schema (`scan --pr --format json`)

This document defines the stable, versioned machine-readable output produced by
`ledgerful scan --pr <range> --format json`. It is the contract that the
`ledgerful-action` GitHub Action pins. Breaking changes bump `schemaVersion`.

**Current version: 2** (0086). Action accepts `schemaVersion` **1 or 2** during
rollout; v2 fields are optional on the Action side when reading older reports.

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

## Schema (v2)

```json
{
  "schemaVersion": 2,
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
      "changeType": "modified",
      "churn": 4,
      "lastCommitAt": "2026-07-10T08:00:00+00:00",
      "isSensitive": false
    },
    {
      "path": "src/bar.rs",
      "changeType": "renamed",
      "oldPath": "src/old_bar.rs",
      "churn": 1,
      "lastCommitAt": "2026-07-16T15:30:00+00:00",
      "isSensitive": false
    }
  ],
  "riskLevel": "low",
  "riskReasons": [],
  "analysisWarnings": [],
  "historyWindowCommits": 128,
  "historyTruncated": false,
  "testGaps": {
    "status": "unavailable",
    "sourceSeedCount": 0,
    "mappedCount": 0,
    "fileMappedCount": 0,
    "unmappedCount": 0,
    "unmappedCapped": false,
    "unmappedTotal": 0,
    "unmapped": [],
    "mappedSample": [],
    "notes": [
      "Structural test_mapping only (IMPORT/NAMING_CONVENTION/SAME_FILE); not line coverage",
      "LCOV COVERAGE mapping kind does not currently persist (DDL NOT NULL on test_symbol_id)"
    ]
  },
  "affectedFlows": {
    "status": "unavailable",
    "flowCount": 0,
    "flowCapped": false,
    "flowTotal": 0,
    "flows": [],
    "notes": [
      "Registered HTTP routes only (api_routes); not distributed traces or CRG-style call-chain flows."
    ]
  }
}
```

### Field reference

| Field | Type | Description |
|---|---|---|
| `schemaVersion` | integer | `2` for current engine output. Breaking schema changes increment this value. |
| `generatedAt` | ISO 8601 string | UTC timestamp in RFC 3339 format. Volatile; the Action does **not** pin this. |
| `baseRef` | string | Base ref used for the diff. |
| `headRef` | string | Head ref used for the diff. |
| `headHash` | string (omitted when unknown) | Commit hash at HEAD. **Omitted** (not `null`) when unavailable — e.g. some edge cases. |
| `branchName` | string (omitted when unknown) | Current branch name. **Omitted** on detached HEAD (typical `actions/checkout` PR checkout). Never serialized as JSON `null`. |
| `treeClean` | boolean | Whether the diff between `baseRef` and `headRef` is empty (no changes). In CI/PR mode this reflects diff emptiness, not working-tree dirtiness. |
| `changeCount` | integer | `len(changes)`. |
| `changes` | array | Sorted by `path`. Forward-slash normalized for cross-platform determinism. |
| `changes[].path` | string | Forward-slash normalized path. |
| `changes[].changeType` | string | `added`, `modified`, `deleted`, or `renamed`. |
| `changes[].oldPath` | string (omitted when not a rename) | Present only when `changeType` is `renamed`; otherwise the field is omitted. |
| `changes[].churn` | integer (u32) | Commits in the history walk window that touched this path. `0` if the path has no history in the window. Always emitted in v2. |
| `changes[].lastCommitAt` | ISO 8601 string (omitted when unknown) | Committer time of the most recent touch in the walk window. |
| `changes[].isSensitive` | boolean | Whether the path matches a known sensitive-path pattern. Always emitted in v2. |
| `riskLevel` | string | `low`, `medium`, or `high`. |
| `riskReasons` | array of strings | Sorted alphabetically, deterministic reasons for the risk level. |
| `analysisWarnings` | array of strings | **Reserved.** Engine always emits `[]` today; not a live warning channel until a real source is wired deliberately. |
| `historyWindowCommits` | integer (u32) | How many commits were walked for history enrichment (≤ bound, default 1000). |
| `historyTruncated` | boolean | `true` if the walk stopped because it hit the max-commit bound. Without this, `churn` would look absolute when it is bounded. |
| `testGaps` | object | **Always present** (0115, additive on schema **v2** — no v3 bump). Structural test-mapping gaps for changed source paths. Soft-opens `ledger.db` read-only only; never runs index or `init_with_layout`. CI without an index → `status: "unavailable"` (honest default, not a product failure). File-level only on this path (no symbol resolution). |
| `affectedFlows` | object | **Always present** (0118, additive on schema **v2** — no v3 bump). Registered HTTP routes touched by the change set. Soft-opens `ledger.db` read-only only; never runs index or `init_with_layout`. CI without an index → `status: "unavailable"`. File-path seeds only on this path (no blast). **Route map ≠ CRG call-chain traces.** |

### `testGaps` field reference

| Field | Type | Description |
|---|---|---|
| `status` | string | `available` \| `empty_mapping` \| `missing_table` \| `no_source_seeds` \| `unavailable`. Never bare `"empty"`. |
| `sourceSeedCount` | integer | Non-test source paths considered. |
| `mappedCount` | integer | Symbol-level mappings (0 on pure file-level PR path). |
| `fileMappedCount` | integer | Paths with ≥1 covering test file via `tested_file_id`. |
| `unmappedCount` / `unmappedTotal` | integer | Source paths with zero covering tests. |
| `unmappedCapped` | boolean | `true` when `unmapped` list was truncated (cap **20**). |
| `unmapped` | array | Capped unmapped entries (`file`, optional `symbol`/`qualifiedName`, `mappingKind: "none"`). |
| `mappedSample` | array | Up to **5** mapped samples by covering count (`mappingKind: "symbol"` \| `"file"`). |
| `notes` | array of strings | Always includes structural-only + LCOV COVERAGE ceiling honesty. |

**Honesty:** this is **not** line coverage. LCOV `COVERAGE` mapping kind does not currently persist (DDL `test_symbol_id NOT NULL`). Do not invent coverage percentages or treat `unavailable` as a merge block. Empty mapped lists / `unavailable` / `empty_mapping` are **not** “fully covered.”

### `affectedFlows` field reference

| Field | Type | Description |
|---|---|---|
| `status` | string | `available` \| `empty_map` \| `missing_table` \| `no_change_seeds` \| `unavailable`. Never bare `"empty"`. |
| `flowCount` / `flowTotal` | integer | Matched routes after / before the library cap (**20**). |
| `flowCapped` | boolean | `true` when `flows` list was truncated. |
| `flows` | array | Capped entries: `method`, `pathPattern`, optional `handlerSymbolName` / `handlerFile`, `framework`, `matchKind`, optional `routeConfidence` / `confidenceClass` / `evidence`. |
| `notes` | array of strings | Always includes registered-routes-only honesty (not distributed traces / not CRG path-trace). |

**Honesty:** Ledgerful affected flows = **HTTP route registrations** touched by the change set (handler symbol / impl file / registration file / optional blast edges). This is **not** CRG `get_affected_flows` execution-path traces, not OpenTelemetry, and not a complete middleware chain. `available` + `flowCount` 0 is an all-clear (no registered routes touched), not a failure. Soft-open never creates `.ledgerful` in CI.

### What is deliberately not included

- **Author / contributor names.** Recency and churn are risk signals; naming a
  person in an automated public PR comment is a social cost with no analytic
  gain. Do not add author fields to this schema without an explicit product
  decision.
- **Hotspots / impact / verify.** Those require a local index that is not
  present in a fresh CI checkout (`.ledgerful/` is gitignored). PR scan stays
  index-free by design.

## Determinism contract

For the same `(baseRef, headHash, repoState, history window)`, running
`scan --pr` twice produces byte-identical JSON except for `generatedAt`. The
caller must strip or ignore `generatedAt` when diffing or hashing.

Specific guarantees:

- `changes` is sorted by `path` ascending.
- `riskReasons` and `analysisWarnings` are sorted alphabetically.
- Paths are forward-slash normalized (`\` → `/`).
- `schemaVersion` is a stable integer; breaking changes bump it.
- History enrichment uses a bounded first-parent walk (`DEFAULT_MAX_COMMITS =
  1000`); same history ⇒ same `churn` / `lastCommitAt` / window fields.

## Risk derivation

Risk is lightweight and deterministic; it does **not** depend on the full
impact-analysis enrichment pipeline or on history churn.

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
- `.ledgerful/` (directory-prefix match)
- `deny.toml` (exact file-name match)
- `SECURITY.md` (exact file-name match)

## History enrichment (index-free)

Implemented by `git::metadata::collect_path_history`, shared walk core with
`collect_git_metadata` (web API + indexer). First-parent, newest-first, bound at
1000 commits. No storage, no network. CI workflows should use `fetch-depth: 0`
so the walk has history to read.

## Out of scope for this surface

- Full impact analysis / indexing / LLM enrichment
- Network calls
- Author identity in the JSON payload
