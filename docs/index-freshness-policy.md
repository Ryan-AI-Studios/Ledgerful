# Index freshness policy (three tiers)

Ledgerful keeps search, graph, and impact signals useful without silent always-on
heavy work. This document is the product contract for **when** the index refreshes
and **what** each path is allowed to do.

Related: `docs/Features.md`, `docs/Call-Resolution.md` (SCIP / analyze-graph),
`docs/doctor-severity.md`.

## Three tiers

| Tier | How it runs | What it may do | What it must never do |
|---|---|---|---|
| **Light continuous** | User starts `ledgerful watch` (foreground) | Debounced FS events → incremental native index/graph sync (`IncrementalSyncEngine`); **mega-batch safety** below | SCIP generate; silent OS service; unbounded branch-switch chew |
| **Light on-demand** | Shared path: `--auto-index` on **`search`, `ask`, `hotspots`, `dead-code`** via `try_auto_index`. **`verify --auto-index`** is a **separate** scoped path (see below). | Shared path: refresh when **time-stale**, **never-indexed / missing**, or **drift-stale** (content-hash vs worktree); **full bootstrap** when no usable floor exists | SCIP; surprise `--analyze-graph`; “incremental only” that fails on empty DB |
| **Heavy scheduled / explicit** | `schedule setup-nightly` / `run-nightly` **or** user `index --full` / `--auto-scip` / `--analyze-graph` | Full graph analysis / optional SCIP | Default-idle trigger; run without install/opt-in |

**Not an indexing tier:** `ledgerful daemon` (feature-gated LSP / risk-alert IPC;
ships in release builds via `--all-features`) is a **stateless reader** over whatever
index already exists. It does **not** index, does not run SCIP, and is **not** the
forbidden idle background indexer.

**Note (not a product tier):** the post-commit hook may call `incremental_index`
when installed. That is not user-facing `--auto-index`.

## Forbidden

- Idle SCIP scavenger / silent always-on full reindex
- Hidden second indexer after `init`
- Installing `watch` or `schedule` as a default OS service from `init`
- SCIP on the light continuous or light on-demand path
- Treating doctor green as “index is fresh”

## Doctor Graph Index Health (age + content)

Doctor’s **Graph state** Index Health line is **age first**, then **content** when
the age path is non-stale (not never-indexed / not time-stale):

| Age path (`check_index_staleness`) | Content path (`count_content_hash_drift` on repo root) | Graph Index Health |
|---|---|---|
| Some (never-indexed / age-stale) | **not run** (STOP) | `graph-empty` \| `graph-stale` findings only |
| None (age-fresh) | dirty (`changed_or_unindexed > 0`) | `graph-content-stale` warn + content-stale health line (**never** success `Current`) |
| None | clean | `Graph state: Current` (or empty-Cozo analyze-graph hint) |
| None | Err | `graph-drift-check-failed` warn (**never** `Current`) |

**Still not the readiness JSON SoT.** `ledgerful index --check --json` remains the
authoritative content-aware readiness assessment (`ContentStalePopulated`,
`stale_files`, etc.). Doctor Graph honesty closes the false-`Current` gap; it does
**not** replace check JSON. Forbidden above still applies: doctor green ≠ index
fresh.

See also `docs/doctor-severity.md` (Graph probe exclusivity — SQLite family vs
Cozo native).

## Bootstrap carve-out (light on-demand)

| State | Behaviour under `--auto-index` |
|---|---|
| No DB / `NeverIndexed` / empty unusable floor | **`full_index()`** first build |
| Populated + time-stale or drift-stale | **`incremental_index()`** only |
| Age-fresh + content-hash clean | No-op |

Never SCIP. Never `--analyze-graph` on this path.

## Drift-stale vs time-stale

- **Time-stale:** `last_indexed_at` older than `index.stale_threshold_days` (default **3**).
  Covers quiet multi-day lag even when the tree looks unchanged.
- **Drift-stale:** worktree supported-source content hashes differ from
  `project_files.content_hash` (includes never-stored / new files **and** indexed
  files removed from the worktree). Covers **same-day** agent edits and deletes
  that time-only checks would miss. Incremental refresh marks missing rows
  `DELETED` and prunes dependents.

Both signals matter. Shared-path `--auto-index` runs when either fires.

## Readiness honesty (`index --check`)

Age-only `assess_index_freshness` stays cheap for non-readiness callers
(`stale_files = 0` — content drift is not computed). **`index --check`** runs
**one** `count_content_hash_drift` walk and overrides the assessment before
serialize:

| Field | Meaning on `--check` |
|---|---|
| `assessment.state` | Age + content: `FreshPopulated` only when age-fresh **and** content-clean |
| `ContentStalePopulated` | Age-fresh metadata + content-hash drift (never “fresh” with dirty tree) |
| `assessment.stale_files` | Same content-drift count as top-level `stale_files` |
| `assessment.indexed_files` | Active non-deleted row count (not drift) |
| Top-level `stale_files` | `changed_or_unindexed` from content-hash drift |

**Ban:** `FreshPopulated` with top-level `stale_files > 0`. Human and JSON agree.

## Search `--auto-index` and Tantivy (full-text)

There is **no** incremental Tantivy API. After shared-path `--auto-index`:

| Auto-index action | SQLite / graph | Tantivy (BM25) |
|---|---|---|
| `None` (age-fresh + content-clean) | no-op | **no** full FTS rebuild (unless `document_count == 0`) |
| `FullBootstrap` / `Incremental` | full or incremental index | **full** clear + `StreamIndexer` rebuild |
| Explicit `search --index` | — | full FTS rebuild |

So: no full FTS reindex on **every** search when auto-index no-ops; full FTS
**after** legitimate SQLite work is required for BM25 truth. If the FTS rebuild
fails after auto-index, search emits a greppable WARN / `search_index_status`
with remediation `ledgerful index --incremental`.

### `verify --auto-index` (not `try_auto_index`)

`verify --auto-index` only helps **`--scope fast`** when `test_mapping` is empty or
stale relative to the impact packet `head_hash`. It runs a **changed-files
incremental** refresh and retries scoped test selection — it does **not** perform
the general time/drift/bootstrap `try_auto_index` refresh used by search/ask/
hotspots/dead-code. For a full symbol/search floor, use those commands or
`ledgerful index --incremental` / `--full`. See `docs/verify-performance.md`.

## Watch mega-batch safety

Default threshold: **1000** unique paths in a single debounced batch
(`watch.mega_batch_threshold` in config; additive default).

When exceeded:

1. Do **not** run unbounded `process_batch` on that batch.
2. Mark index **STALE** (`last_indexed_at` → epoch so age checks fire).
3. Print/log: run `ledgerful index --full`.
4. Keep watching subsequent smaller batches.

This is refuse-and-honest, not a silent full reindex inside watch.

## Surfaces that do **not** take `--auto-index`

- **`scan` / `scan --impact`** — agents must refresh first when freshness matters
  (`doctor`, `index --check`, `index --incremental` / `--full`, or a prior
  `--auto-index` command).
- Other commands unless explicitly documented.

## Heavy path honesty

- Default nightly argv is `index --analyze-graph` (**no** `--auto-scip`).
- Opt-in install: `schedule setup-nightly` (`--dry-run`, `--uninstall`).
- **`analyze-graph` destroys and rebuilds structural edges** on its second pipeline
  pass (not mere “duplicate work”). Full dedup is a separate track (0095 residual).
- Optional SCIP remains CLI-driven: `index --auto-scip` / `index --scip <path>`.

## Notify / platform limits (watch)

Watch inherits OS filesystem watcher limits (`notify` **8.2** +
`notify-debouncer-full` **0.7** coupled pair — do not half-bump):

- NFS / network filesystems may emit **no** events
- Linux `inotify` `max_user_watches` on large trees
- Editor truncate-vs-replace / atomic save patterns
- macOS FSEvents edge cases

Missing events are not always a Ledgerful logic bug.

## Agent decision tree

```text
Need readiness?
  → ledgerful doctor [--json]          # env / block findings; ≠ index fresh
  → ledgerful index --check [--json]   # age / floor signals

Need fresher symbols/search same session?
  → prefer --auto-index on: search | ask | hotspots | dead-code
    (time-stale + content-hash drift + full bootstrap)
  → verify --auto-index only repairs test_mapping for --scope fast (not general index)
  → else: ledgerful index --incremental   # or --full if never indexed / mega-batch

Continuous local session?
  → ledgerful watch   # foreground; mega-batch safe

Overnight / heavy graph warm?
  → schedule setup-nightly   # opt-in; analyze-graph; no auto-scip

Precision reference edges?
  → explicit index --auto-scip / --scip <path>   # never idle

About to scan --impact?
  → refresh first (doctor/check/index); scan has no --auto-index
```

## Defaults after `init`

`init` alone starts **no** watcher and installs **no** nightly schedule.
Background work is always user opt-in.
