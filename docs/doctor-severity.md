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
  "summary": { "block": 0, "warn": 4, "warnAction": 3, "warnOptional": 1, "info": 5 },
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
    },
    {
      "code": "PHANTOM_PROMOTED_WITHOUT_VERIFY",
      "severity": "warn",
      "category": "signing",
      "message": "2 committed row(s) have verification_status=Verified with no bound verification_results row (legacy promote phantoms; forward-only)."
    },
    {
      "code": "completion-unreachable",
      "severity": "warn",
      "category": "optional",
      "message": "Completion model unreachable (connection refused)"
    }
  ],
  "environment": {
    "platform": "…",
    "shell": "…",
    "workRoot": "…",
    "stateDir": "…",
    "pathDisplay": "…",
    "targetTriple": "…",
    "binaryVersion": "0.2.5",
    "buildSha": "b57f4472efb3",
    "githubLatest": {
      "status": "ahead",
      "tag": "v0.2.10",
      "sha": "c4a2308fe985",
      "running": "ahead",
      "worktree": "unknown"
    }
  },
  "durationMs": 842
}
```

The sample omits info findings; shown warns match `summary.warn` / `warnAction` / `warnOptional`.

`environment.platform` is Debug of `PlatformType` (`Windows` / `Linux` /
`Wsl` / `Unknown`), not serde lowercase. `Linux` includes Linux containers
(Docker/Podman guests that inherit a Docker Desktop `*-microsoft-standard-WSL2`
kernel string). `Wsl` is WSL distro userspace, not a container sharing that
kernel.

Exit code `1` iff any `block`; else `0`. Human banners (sccache/SCIP/VRAM) are skipped under `--json`.

### Header vs JSON `summary` (0209)

| Surface | Meaning |
|---|---|
| Human header “warning(s)” | `summary.warnAction` — action-critical warns (what Index Health expands) |
| JSON `summary.warn` | All `severity=warn` including optional |
| JSON `summary.warnAction` | Warn when `category != optional` |
| JSON `summary.warnOptional` | Warn when `category == optional` |
| Tools `gemini CLI` | PATH CLI (`gemini` / `gemini-cli`); **not** Cloud Ask (`Active Ask Backend: Gemini (Cloud)`) |

Invariant: `summary.warn == summary.warnAction + summary.warnOptional`. Additive keys follow the 0143 `durationMs` pattern; **schemaVersion stays 1**. Sidecar `doctor-results.json` `warn` remains the all-severity total.

## Human progressive disclosure (0174 3-tier)

Human `doctor` (not `--json`) uses **3-tier** progressive disclosure, not a raw
category split:

| Tier | Rule | Default human |
|---|---|---|
| **Block** | `severity == block` | Always expanded under **Index Health** |
| **ActionWarn** | `is_action_critical`: block always; **warn** when `category != optional`; info never | Always expanded under **Index Health** |
| **Hygiene** | `!is_action_critical` ≡ Optional category **or** Info severity (any category) | **Collapsed** by default |

Examples:

- `sig-pin` / `binary-behind-tree` / `binary-behind-latest` (non-optional **warn**) → expanded
- `completion-unreachable` (optional **warn**) → hygiene (collapsed)
- `hook-template-stale` (**info** / gate) → hygiene (collapsed) — leaves Index Health by default
- Optional accelerators section still shows embedding/completion status lines always

### Flags

| Flag / env | Effect |
|---|---|
| **(default)** | Expand Block + ActionWarn only; greppable trailer `N hygiene finding(s) collapsed — run doctor --full`. When optional warns exist, the trailer adds `(1 optional warning)` / `(N optional warnings)`; with none, the default string is unchanged. |
| **`doctor --full`** | Expand hygiene too: non-optional **info** under Index Health; **optional** findings under Optional Accelerators. Orthogonal to global `-v` (logging). |
| **`-q` / `--quiet` or `LEDGERFUL_QUIET=1\|true`** | Via shared `resolve_quiet`: suppress multi-line remediations + **VRAM** footer; keep finding one-liners + hygiene collapse. Does **not** select machine mode. |
| **`doctor --json`** | Unchanged: schemaVersion **1**, **full** findings always. `full`/`quiet` ignored for JSON content. |

VRAM section: shown under default and `--full`; **suppressed under quiet**.

Agents should keep using **`doctor --json`** as SSOT; human disclosure is for interactive scans.

### `durationMs` (0143 B5)

Optional top-level `u64` wall-clock milliseconds from after the `--json`/`--apply-hook-refresh`
conflict guard until immediately before JSON serialization. `schemaVersion` stays `1`.
Agents may use it to spot session-start regressions; not a SLI contract.

### `binary-behind-tree` (0137)

| Field | Value |
|---|---|
| `code` | `binary-behind-tree` |
| `severity` | `warn` |
| `category` | `tools` |
| When | **Engine worktree only** (`Cargo.toml` package name exactly `ledgerful` **and** `src/cli/args/mod.rs` exists). Version string lag and/or embedded build short-SHA ≠ worktree HEAD (gix). |
| Not | Consumer repos; matching version+SHA; embed `unknown` + equal version (no commit false positive). |
| `readyForPublish` | **Not** blocked (warn only). Counts in `dashboard_failures` (category ≠ optional). |
| `remediation` | Always: `cargo install --path . --force` then `ledgerful update --binary` then `ledgerful --version`. **No** auto-install from doctor. |

Agents may also read currency from `environment.binaryVersion` + `environment.buildSha` (schemaVersion stays **1**).

### `binary-behind-latest` (0205)

| Field | Value |
|---|---|
| `code` | `binary-behind-latest` |
| `severity` | `warn` |
| `category` | `tools` |
| When | **Engine worktree only.** GitHub Latest is known and the **running** binary version is older than Latest (`X.Y.Z`, tag may have a leading `v`) — including when running SHA is `unknown`/`empty`. |
| Not | Consumer repos (`githubLatest.status=skipped`, zero HTTP); `LEDGERFUL_NO_NETWORK` / fetch failure (`status=unverified`, **no finding**); cargo tip at the same version as Latest (that is `binary-ahead-of-latest`). |
| `readyForPublish` | **Not** blocked (warn only). Counts in `dashboard_failures` / Index Health (category ≠ optional). |
| `remediation` | Published tag URL `https://github.com/Ryan-AI-Studios/Ledgerful/releases/tag/{tag}` then `ledgerful --version`. **Must not** say `cargo install --path .` / `--force` (that installs cargo tip). |

0137 `binary-behind-tree` is PATH vs **this worktree** (reinstall from `.` if you want this tree). `binary-behind-latest` is running version older than **published** Latest. When both fire (T7: old PATH + cargo-tip tree), remediations **coexist unmerged** — do not concatenate into one “install `.` and the tag” sentence.

### `binary-ahead-of-latest` (0205)

| Field | Value |
|---|---|
| `code` | `binary-ahead-of-latest` |
| `severity` | `info` |
| `category` | `tools` |
| When | **Engine worktree only.** Latest known, and either: **(1) default** — running is not Latest (usable running SHA ≠ Latest SHA at equal version — 0199 exhibit class — **or** running version > Latest; T4/T9); subject is the **running** binary; **or (2) F8 exception** — also when `status=mixed` (PATH behind **and** worktree version **> Latest**); that finding’s **subject is the worktree** (cargo tip), not PATH. **Worktree-only** same-version SHA mismatch does **not** emit this (T8/T13). T7 is **not** F8 (equal-version worktree SHA mismatch is `unknown`, behind-only). PATH matching Latest with a newer tree (`status=match`, worktree=`ahead`) emits **no** 0205 finding. |
| Not | Consumer; unverified; equal-version running SHA `unknown`/`empty` (T11); PATH that already matches Latest. |
| `readyForPublish` | Unchanged (info never blocks). **Not** action-critical — hygiene / `--full` / `--json`. |
| `remediation` | Names Latest `{tag}` + 12-char SHA. **Default (running-ahead):** “This binary is not GitHub Latest {tag} ({sha}) — do not recapture public exhibits from this binary.” **F8 mixed (worktree-ahead):** “This worktree is not GitHub Latest {tag} ({sha}) — do not recapture public exhibits from this tree.” Links the tag URL. **Must not** tell the operator to install from `.` as a fix. |

**Honesty:** absence of a `binary-*-latest` finding + `githubLatest.status=unverified` is **not** a match (not current). Agents must read `status`, not infer from a missing code.

**Sidecar:** `doctor-results.json` has **no** `githubLatest` key (0109/0129 shape frozen). Sidecar top-N is action-critical only, so `ahead` (info) is omitted. Daily 5 = `ledgerful doctor --json`, not the sidecar.

**`environment.githubLatest`** (always present; schemaVersion stays **1**):

| `status` | `tag` / `sha` (12-char) | `running` / `worktree` |
|---|---|---|
| `skipped` | omitted | omitted |
| `unverified` | omitted | `"unknown"` (present) |
| `match` / `behind` / `ahead` / `mixed` / `unknown` | present | present |

Sample (T4 dogfood — PATH matches HEAD, both ahead of the published tag):

```json
"githubLatest": {
  "status": "ahead",
  "tag": "v0.2.10",
  "sha": "c4a2308fe985",
  "running": "ahead",
  "worktree": "unknown"
}
```

Latest SHA is peeled from `GET /commits/{tag_name}` — never `releases/latest.target_commitish` (live value is `"main"`).

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

**Search CLI:** when the index was empty before a query, `search --json` sets
envelope field `searchIndexStatus` (`state`, `documentCount`, optional
`remediation`). States: `was_empty` (rebuilt to N>0) | `empty_after_rebuild`
(still 0 — may mean no indexable content / ignore patterns, not only “run index
again”). Under `--json-lines`, the legacy BridgeRecord
`record_kind: search_index_status` stream remains (status first, then matches).

## Graph probe exclusivity (0133)

Doctor’s Graph surface has **two orthogonal families**. SQLite-floor findings are
**mutually exclusive among themselves** (at most one from the Graph Index Health
arm). Cozo-native findings live on a separate axis and **may co-occur** with a
SQLite-floor finding (e.g. `graph-not-initialized` info + `graph-content-stale`
warn when Cozo is unset and content drift is dirty).

### Family A — SQLite floor (age / content) — mutual exclusion

Control flow: **age first STOP**; content-hash drift only when age path is
non-stale. Repo root for drift is **`layout.root` only** (never bare `cwd`).

```text
check_index_staleness → Some(missing)  → graph-empty            (warn / index)
check_index_staleness → Some(age-stale) → graph-stale           (warn / index)
  // STOP — do not run count_content_hash_drift

check_index_staleness → None (age-fresh):
  count_content_hash_drift dirty       → graph-content-stale    (warn / index)
                                         + non-Current health line with N
  count_content_hash_drift clean       → human only: Graph state: Current
                                         (or empty-Cozo analyze-graph Current hint)
  count_content_hash_drift Err         → graph-drift-check-failed (warn / index)
                                         + non-Current health line
```

| Code | Severity | Category | When | Notes |
|---|---|---|---|---|
| `graph-empty` | warn | index | Never indexed / missing floor | Age path; no content walk |
| `graph-stale` | warn | index | Time-stale vs `stale_threshold_days` | Age path; no content walk; message includes file count |
| `graph-content-stale` | warn | index | Age-fresh + content-hash drift | Message includes **N** = `changed_or_unindexed`; greppable content/drift/stale; remediation: `index --incremental` then `index --check --json` |
| `graph-drift-check-failed` | warn | index | Age-fresh + drift walk error | Display error truncated to 80 chars; full at `tracing::debug!`; never claim Current |

**Human Index Health (content-stale example — must not contain bare success `Current`):**

`Graph state: Content-stale (N files) - run 'ledgerful index --incremental'`

**Human Index Health (drift-failed — must not claim `Current`):**

`Graph state: Drift check failed — run 'ledgerful index --check'`

**Publish / health:** all four are **warn**, not block — `readyForPublish` stays
**block-only**. Non-optional Index warns increment `dashboard_failures` (−20 each
in `compute_health_score`). Content-stale **wins** over empty-Cozo “Current
(analyze-graph…)” health when dirty.

**Naming debt (do not rename this track):** the label is “Graph state” but the
SQLite floor probe is `project_files` age/content honesty, not Cozo structural
completeness.

### Family B — Cozo native (orthogonal)

| Code | Severity | Category | When |
|---|---|---|---|
| `graph-error` | warn | index | Cozo script / open error while graph is present |
| `graph-not-initialized` | info | index | `storage.cozo` is `None` |

Info does not count in `dashboard_failures`. Cozo-None does **not** skip the
SQLite content-drift walk; clean + Cozo-None still shows the analyze-graph
Current **health** hint alongside `graph-not-initialized` info.

**Readiness SoT:** `ledgerful index --check --json` remains authoritative for
content-aware readiness JSON. Doctor Graph Index Health is honest about content
when age-fresh; it is **not** a substitute for check. See
`docs/index-freshness-policy.md`.

## Dashboard `doctor-results.json`

```
failures = count(block) + count(warn WHERE category != optional)
```

Additive fields: `readyForPublish`, `block`, `warn`, `info`, and (0129) **`findings`**.

**`findings` (agent top-N, 0129 + 0138):** action-critical only — same eligibility as
`dashboard_failures` / B1: **block always**, or **warn when category ≠ optional**;
info never. Optional-category warns are excluded from sidecar top-N (they still
appear on full `doctor --json` `findings[]`, which includes `category`).
Severity-first re-sort (block before warn, then code, then message) before cap **5**.
Optional `remediation` when present. Health/dashboard still scores only
**`failures`** / counts — unknown `findings` is ignored by older readers.
**Transition:** pre-0138 sidecars may still contain optional codes until the next
doctor write; the reader trusts the sidecar as-is (no read-time category re-filter).

**Orthogonal to readiness:** models down → ready + high health; search index corrupt
or **empty** → ready (can still verify/push) **but** health penalized via non-optional
warn (−20 per failure).

Optional backends no longer contribute the historical ~−60 health points on a models-down
machine (`failures * 20` in `compute_health_score`).
