# Self-timing facility (local-only)

Ledgerful records **how long the tool itself took**, on disk, in the current
repository. Use it to answer “which of *my* commands is slow, and why?” without
leaving the CLI and without attaching a profiler for the *find* step.

This is **not telemetry**. Nothing is uploaded. Capture and query stay local.

## CLI vs capture feature flag

The `ledgerful timings` **query/export/opt-out surface is always available** in
the binary (migration `m52` is unconditional). **Automatic capture** (the
`TimedCommand` RAII guard and `TimingLayer`) is gated on the Cargo feature
`self-timing`, which is **on by default**. Builds with
`--no-default-features` (or without `self-timing`) still serve historical rows
and opt-out config, but do not record new invocations.

## What is recorded

Per CLI invocation (outer row) and, when existing `tracing` spans close long
enough, per inner span:

| Field | Meaning |
|---|---|
| `command` | Stable subcommand path (e.g. `verify`, `scan`) — no user values |
| `duration_ms` | Wall time for the command or span |
| `exit_code` | Process-level success/failure of the host command |
| `ts_utc` | UTC timestamp |
| `run_id` | Links outer + inner rows for one invocation |
| `argv_hash` | Hash of *canonicalized* command shape (subcommand + sorted flag **names**; values stripped) |
| `repo_size_bytes` | Usually `NULL`. Filled only when **`index`** reports size opportunistically as `SUM(file_size)` from already-indexed `project_files` rows — **never** a dedicated walk, and **`scan` does not set it** (no free byte total during scan). File counts alone are not written here. |
| `ledger_tx_id` | Optional association to a ledger transaction **only** when the host command calls `set_current_ledger_tx_id` (explicit API). Schema contract: `NULL` unless the command intentionally produced or bound a tx. There is **no** automatic `pending_hook_tx` sidecar attribution. Best-effort; never fails the host command. |
| `span_name` / `parent_span_id` | Engine-internal span names from existing `#[instrument]` / `info_span!` hooks. Inner `parent_span_id` is **run-scoped** (`{run_id}:{tracing_span_id}`) so concurrent runs never collide; outer rows keep `parent_span_id = NULL`. |

## What is never recorded

- File paths, file contents, branch names, commit messages
- Authors, remotes, environment variables, IPs
- Raw argv or any flag **values** (paths, tx-ids, queries, free text)
- Anything over the network
- The `timings` command itself (self-exclusion — querying/exporting history must not pollute the series with self-observation noise)

## Where it lives

Per-repo SQLite: `.ledgerful/state/ledger.db`, table `command_timings`.

## How data leaves the machine

**Only** if you explicitly run `ledgerful timings --export <path>` (or copy the
DB yourself). There is no flush, sync, or share path.

## Enable / disable

Default: **on** (absent config key = enabled).

```powershell
ledgerful timings --opt-out   # writes self_timing = false to ~/.ledgerful/config.toml
ledgerful timings --opt-in    # re-enable
```

## Retention

Suggested defaults (CLI):

```powershell
ledgerful timings --prune --older-than 90d          # outer rows
ledgerful timings --prune --inner --older-than 30d  # inner spans
```

`doctor` warns if total rows exceed 10 000 or if distinct `span_name` values in
30 days exceed 1 000 (cardinality guard).

## Relationship to `usage-metrics`

| | Self-timing (this feature) | Usage metrics |
|---|---|---|
| Default | On | Off (opt-in) |
| Network | Forbidden | Allowed only when opted in |
| Content | Local durations / span names | Anonymous command counters |
| Storage | Per-repo `command_timings` | Global usage config + per-repo counters |

They share a database file but **never** a code path. Do not unify them.

## Self-exclusion (`timings` command)

`ledgerful timings …` (query, export, prune, opt-in/out) is **not** recorded as
a timed invocation. Capture deliberately no-ops when the command name is
`timings`, so inspecting history cannot create self-observation noise.

## Span names

Span names are **engine-internal** labels from existing instrumentation
(e.g. `tantivy_index`, `run_tests`). They are stable per `#[instrument]` and
must not embed user data (paths, queries). High-cardinality names are a bug;
`doctor` will warn.

Inner span ids and `parent_span_id` values are **run-scoped**
(`{run_id}:{tracing_span_id}`) so concurrent CLI processes never collide when
joining flame/query graphs. Outer (command) rows always have `parent_span_id = NULL`.

## Profiler ceiling

This is **span-level observability** — the tier between “command total” and a
full flamegraph. It is a first stop for “why is verify slow?”. It is **not** a
replacement for `cargo flamegraph` or a sampling profiler.

## Commands

```powershell
ledgerful timings --top 10
ledgerful timings --days 7 --json
ledgerful timings --inner --command verify
ledgerful timings --flame --command verify
ledgerful timings --explain verify
ledgerful timings --export out.json
ledgerful timings --opt-out
```

## Privacy CI

CI greps the capture and query modules for `ureq|reqwest|tokio_tungstenite` and
fails if any match. The capture module itself is marked:

> local-only; do not add network calls.
