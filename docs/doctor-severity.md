# Doctor severity and publish readiness (0109)

## Severity model

Every doctor finding has:

| Field | Values |
|---|---|
| `severity` | `block` \| `warn` \| `info` |
| `category` | `lifecycle` \| `signing` \| `tools` \| `index` \| `optional` \| `migration` \| `layout` \| `gate` \| `other` |
| `code` | Stable machine id (e.g. `PROMOTE_ORPHAN`, `sig-pin`, `tool-git`, `embed-unreachable`) |

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
      "message": "…"
    }
  ],
  "environment": { "platform": "…", "shell": "…", "workRoot": "…", "stateDir": "…", "pathDisplay": "…", "targetTriple": "…" }
}
```

Exit code `1` iff any `block`; else `0`. Human banners (sccache/SCIP/VRAM) are skipped under `--json`.

## Dashboard `doctor-results.json`

```
failures = count(block) + count(warn WHERE category != optional)
```

Additive fields: `readyForPublish`, `block`, `warn`, `info`.

**Orthogonal to readiness:** models down → ready + high health; search index corrupt →
ready (can still verify/push) **but** health penalized via non-optional warn.

Optional backends no longer contribute the historical ~−60 health points on a models-down
machine (`failures * 20` in `compute_health_score`).
