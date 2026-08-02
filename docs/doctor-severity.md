# Doctor severity and publish readiness (0109)

## Severity model

Every doctor finding has:

| Field | Values |
|---|---|
| `severity` | `block` \| `warn` \| `info` |
| `category` | `lifecycle` \| `signing` \| `tools` \| `index` \| `optional` \| `migration` \| `layout` \| `gate` \| `other` |
| `code` | Stable machine id (e.g. `PROMOTE_ORPHAN`, `sig-pin`, `tool-git`, `embed-unreachable`) |
| `remediation` | Optional multi-step copy-paste block (exact CLI lines). Omitted from JSON when absent (`skip_serializing_if`). Machine source of truth for next commands; human path prints it under the finding. |

## `readyForPublish`

`true` iff **zero** findings have severity `block`.

Canonical definition lives in this doc and the agent skill — **not** embedded as prose
in the JSON payload.

**Does not mean:** `verify` passed, tests green, or full CI green.

**Block set (publish-env hard stop):** lifecycle CRITICAL codes (`PROMOTE_ORPHAN`,
`HEAD_UNCOVERED`, `INTENT_NEVER_UNDER_ENFORCE`, `sig-require`) and missing **git**.

**Optional backends never block:** embedding, completion, SCIP, sccache, gemini CLI —
even when warn (e.g. unreachable URL) they stay `category=optional`.

**Impact corrupt stays warn** (not block): pre-push does not require a usable impact
packet; dashboard health still penalizes via non-optional warn + independent risk path.

## `ledgerful doctor --json`

Pure stdout schema v1 (`schemaVersion` is integer `1`):

```json
{
  "schemaVersion": 1,
  "readyForPublish": true,
  "summary": { "block": 0, "warn": 2, "info": 5 },
  "findings": [
    {
      "code": "sig-pin",
      "severity": "warn",
      "category": "signing",
      "message": "no intent.trusted_public_keys pinned; crypto-valid signatures report VALID (unknown key). Pin keys after init or re-sign. Next: pin the current identity via config set (see remediation).",
      "remediation": "ledgerful config set 'intent.trusted_public_keys=[\"<hex>\"]'\nledgerful doctor --json\nledgerful verify --signatures"
    },
    {
      "code": "sig-version",
      "severity": "warn",
      "category": "signing",
      "message": "intent.min_sig_version=1 still accepts legacy v1 signatures. N LOCAL row(s) have sig_version < 2. Upgrade with `ledger re-sign --all`, then set min_sig_version=2 to close the downgrade path.",
      "remediation": "ledgerful ledger re-sign --all --dry-run\nledgerful ledger re-sign --all --yes\nledgerful config set intent.min_sig_version=2\nledgerful verify --signatures"
    }
  ],
  "environment": { "platform": "…", "shell": "…", "workRoot": "…", "stateDir": "…", "pathDisplay": "…", "targetTriple": "…" }
}
```

Exit code `1` iff any `block`; else `0`. Human banners (sccache/SCIP/VRAM) are skipped under `--json`.

### Remediation notes (0125)

- **schemaVersion stays 1** — `remediation` is additive optional; consumers must tolerate unknown fields.
- **sig-pin pin command** uses **outer single quotes** around the `key=value` argument so PowerShell does not strip the array quotes (`config set 'intent.trusted_public_keys=["…"]'`). Bare `config set intent.trusted_public_keys=["hex"]` fails under PowerShell.
- Pinning proves **identity allowlist**, not free-text ground truth of intent (Wave-0 honesty).
- Default `doctor` never writes config or re-signs; follow remediation commands explicitly.
- `ledger re-sign --all` upgrades LOCAL rows with `sig_version < current` (and repairs invalids); `--all-invalid` remains key-repair only.

## Search index probe exclusivity (0126)

Tantivy search-index arms are **mutually exclusive** for one index path:

```text
!exists            → search-missing      (warn / index)
open Err           → search-load-failed  (warn / index)
integrity Err      → search-corrupt      (warn / index)
document_count==0  → search-empty        (warn / index) + non-OK human Index Health line
document_count>0   → human only: Search index: OK (N documents)  — no finding
```

### `search-empty`

| Field | Value |
|---|---|
| `code` | `search-empty` |
| `severity` | `warn` |
| `category` | `index` |
| `message` | present but empty; full-text search unusable until populated |
| `remediation` | exact `ledgerful index` (primary); optional note that first `search` also rebuilds when empty; `ledgerful doctor --json` |

**Human Index Health** when empty (must **not** contain `OK`):

`Search index: Empty (0 documents — run 'ledgerful index')`

Never emit healthy `OK (0 documents)`. Empty ≠ missing ≠ corrupt.

**Greenfield health:** non-optional warn increments `dashboard_failures` → **−20**
health score until the first real index (`failures * 20` in `compute_health_score`).
This is intentional honesty, not a publish block: `readyForPublish` remains
**block-only** (warn does not flip it).

**Search CLI:** when the index was empty before a query, `search --json` emits a
`record_kind: search_index_status` Insight (status first, then matches if any).
States: `was_empty` (rebuilt to N>0) | `empty_after_rebuild` (still 0 — may mean
no indexable content / ignore patterns, not only “run index again”).

## Dashboard `doctor-results.json`

```
failures = count(block) + count(warn WHERE category != optional)
```

Additive fields: `readyForPublish`, `block`, `warn`, `info`, and (0129) **`findings`**.

**`findings` (agent top-N, 0129):** block \|\| warn only — **no category filter**
(optional-category warns appear even when they do not increment `failures`).
Severity-first re-sort (block before warn, then code, then message) before cap **5**.
Info excluded. Optional `remediation` when present. Health/dashboard still scores
only **`failures`** / counts — unknown `findings` is ignored by older readers.

**Orthogonal to readiness:** models down → ready + high health; search index corrupt
or **empty** → ready (can still verify/push) **but** health penalized via non-optional
warn (−20 per failure).

Optional backends no longer contribute the historical ~−60 health points on a models-down
machine (`failures * 20` in `compute_health_score`).
