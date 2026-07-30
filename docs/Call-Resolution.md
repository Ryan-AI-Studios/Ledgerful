# Call Resolution

How Ledgerful turns a call expression into a `structural_edges` row with an
optional `callee_symbol_id`.

## What this is

Resolution is a **unique-local-candidate heuristic**. When exactly one local
callable matches the rules below, the edge is recorded as `RESOLVED` with that
symbol id. That is a floor: “resolved to a unique local candidate,” not a
claim that the edge is the true runtime target with certainty.

Surfaces must **not** state or imply:

- that a `RESOLVED` edge is *the* call target with certainty, or
- that `UNRESOLVED` means “no local target exists.”

`UNRESOLVED` usually means the name is external (stdlib, crates.io, npm, PyPI)
or could not be narrowed to a single local candidate under the heuristic.
**Resolution counts are not a quality score** — most unresolved edges are
correctly external by design (see honesty ceiling below).

## Parts shipped

| Part | Status | What it does |
|------|--------|----------------|
| **A** | Shipped | Restrict candidates to `Function` / `Method`; same-file preference on collisions; one shared resolver for full + incremental index |
| **B** | Shipped | Read `qualified_name` (e.g. `Foo.new`); Python/TS method QN backfill; dotted member callees so `json.loads` / `axios.get` do not false-resolve to a local bare name or same-file Method |
| **C (module + import bindings)** | **Shipped (0092 Part 1)** | Persist per-file bindings (`file_bindings` / m54) from `use` / `mod` / imports; derive module paths; resolve `crate::` / `self::` / `super::` and first-segment-local callees **only** when an enumerable binding proves locality |
| **C (package roots / manifests)** | **Declined by default (0092 Part 2)** | Cargo workspace / nested `go.mod` / `tsconfig` paths / Python layout — gated; not built without a multi-package fixture |

## Algorithm (shared: `src/index/resolve.rs`)

Used by both `call_graph.rs` (full index) and `incremental.rs`.

1. **Normalize** the callee for QN lookup: replace `::` with `.` (`Foo::new` → `Foo.new`).
2. **Module + binding arm (0092, ahead of QN)** for multi-segment names:
   - `crate` / `self` / `super` rooted paths expand via the caller's module path
     and match callables whose declaring file sits under the target module.
   - First-segment-local callees resolve **only** when the caller file has an
     **enumerable** binding for that segment that proves locality (`use` of a
     `crate`/`self`/`super` path, or a `mod` declaration).
     Example: `pub mod fs;` + `fs::write` may resolve to local `crate::…::fs`;
     `use std::fs;` + `fs::write` stays **`UNRESOLVED`**. Wildcards
     (`use foo::*`) are stored as non-enumerable and never prove locality.
   - Name-only matching of a path segment against a global module-name set
     (without a binding) is forbidden.
3. **Qualified match** (when the name contains `.` or is an exact QN key): among callable kinds only.
   - 1 match → `RESOLVED`
   - \>1 → `AMBIGUOUS`
   - 0 and multi-segment → **`UNRESOLVED` always** (no bare-segment fallthrough). This keeps
     `json.loads` / `axios.get` external even when a same-file Method is named `loads` / `get`.
4. **Bare single-segment match**, callable kinds only:
   - 0 → `UNRESOLVED`
   - 1 → `RESOLVED`
   - \>1 and exactly one candidate in the caller file → that one (`RESOLVED`)
   - else → `AMBIGUOUS`

Callable kinds (locked): **`Function`**, **`Method`**.

Full-index (`call_graph.rs`) and incremental (`incremental.rs`) both convert DB rows via
`resolve_candidate_from_row` + `build_resolve_maps`, then call `resolve_callee` with the
caller's module path and bindings.

Language call extractors receive a `&[Symbol]` slice (real qualified names from the index),
but extractors do not yet consume that slice — plumbing is live for a future consumer.

## Honesty ceiling (quantitative)

Measured on a clean full index of this repo (mostly Rust), the realistic ceiling
for name+path heuristics is roughly **1–2% of structural edges**:

- The **unresolved majority is third-party / stdlib by design** (`unwrap`,
  `join`, `to_string`, `clone`, `expect`, crates.io, npm, PyPI). Binding those
  names would be a regression, not progress.
- The **ambiguous majority is `METHOD_CALL`** and needs **receiver-type
  inference** this engine does not perform (stack-graphs / SCIP territory).
- **Glob / wildcard imports** bind names this engine cannot enumerate and never
  prove locality.
- **Go imports** whose package name diverges from the import path's last segment
  (e.g. `…/my-util` declaring `package util`) are not statically resolvable at
  file level without a directory-level package parse.

## SCIP augment (0095)

`ledgerful index --auto-scip` (or `--scip <path>`) **augments** the native call graph; it never
replaces it. Ordering:

1. Native full/incremental index builds `project_symbols` and heuristic `structural_edges`.
2. SCIP index is generated (or loaded) and **definitions** are mapped to native symbol ids via
   document path + range containment (0-based SCIP → 1-based native; innermost span wins).
3. SCIP **reference** occurrences become additional `structural_edges` with `evidence = scip:ref`,
   `resolution_status = RESOLVED`, high confidence. Unmapped caller or callee → skip (never guess).

**Precedence (deterministic):** when SCIP and native both have an edge for the same
`(caller_symbol_id, callee_symbol_id)` — **regardless of `call_kind`** — prefer SCIP evidence:
update the existing row's `evidence` to `scip:ref` rather than inserting a duplicate. Native
method edges are often `METHOD_CALL` while SCIP emits `DIRECT`; matching only on call_kind would
duplicate rows and skip the upgrade.

**Call sites (mutually exclusive, §2.2b):** without `--analyze-graph`, SCIP runs once after
`build_call_graph` in the main index path; with `--analyze-graph`, SCIP runs only inside
`run_graph_analysis` after `infer_services` (never both — avoids double rust-analyzer runs).

**Output:** `cg_*` fields in `index --json` are **native call-graph only**. SCIP deltas are
additive under the top-level `scip` object (`edges_added` / `edges_updated` / status).

**What SCIP does not do here:** write `project_symbols` (that path was removed — external symbols,
off-by-one lines, and last-occurrence ranges are gone with it); flip on by default; cover every
language in one run (detection still picks one toolchain for generation by Rust → TS → Python
priority); receiver-type inference for the ambiguous `METHOD_CALL` majority on machines without an
indexer; claim stack-graph completeness.

**Honesty:** "SCIP did not run" (`scip.status = did_not_run` in `index --json`) is distinct from
"SCIP ran and added zero edges" (`status = success` with `edges_added = 0`). SCIP only adds edges
where the native-span resolver hits; stdlib/external symbols must not appear as project
`scip:ref` callees. Requested augment **always re-applies** edges (idempotent); hash is audit-only.

A prior native index is **no longer required** for `--auto-scip`: native indexing always runs first.

## Structural blast radius (0106)

`ledgerful impact` and `ledgerful scan --impact` enrich the ImpactPacket with
additive **`blastRadius`** (call-graph punchlist). This is **not** deploy
`highBlastResources`.

| Rule | Behavior |
|------|----------|
| Default depth | **1** (direct reverse callers of changed symbols) |
| `--blast-depth 2` | Hop 2 only from nodes reached via high-confidence discovery edges, along high-confidence expansion edges |
| High confidence | `resolution_status = RESOLVED` **or** `evidence` starts with `scip:` |
| Never expand | AMBIGUOUS / UNRESOLVED / CAPPED (AMBIGUOUS may appear on hop-1 punchlist with labels) |
| Seed join | `file_path` + `symbol_name` and/or `qualified_name` — **never** bare name alone |
| Caps | Fan-out 50/hop, total 200 (config); CLI max depth 2; config ceiling 3 |
| Query path | No SCIP generate / full reindex |

`structural_couplings` is **derived** from blast hop-1. Depth-1 + confidence
filters are a portable floor, not a complete or compiler-proven call graph.
Read `blastRadius.mustTouchFiles` / edges before edits; use `--json` for the
full edge list.

## What this cannot do

The following remain out of reach of a name+path heuristic:

- Re-exports (`pub use`)
- Glob imports (`use foo::*`, `from x import *`) — recorded non-enumerable; never prove locality
- Conditional compilation (`#[cfg(...)]`)
- Macro-generated symbols
- Dynamic / reflective dispatch
- Trait / interface dispatch that needs type inference on the receiver
- Multi-package root mapping (Cargo workspace members, nested `go.mod`,
  `tsconfig` paths / legacy `baseUrl`) — 0092 Part 2, declined without a fixture

## External stays external

Third-party and stdlib calls must stay `UNRESOLVED`. Conflating “external” with
“we did not try” is an honesty failure. Python/TypeScript member calls are stored
as dotted `receiver.field` (same shape Go uses for import-package selectors). The
resolver refuses bare-segment fallthrough on multi-segment names, so neither a
unique local function nor a same-file Method named `loads` can absorb
`json.loads`. The same rule applies to Rust `use std::fs` + `fs::write` even
when a local `mod fs` exists elsewhere in the crate.

## Downstream impact

`AMBIGUOUS` and `UNRESOLVED` edges write `callee_symbol_id = NULL`. Centrality,
dead-code evidence, and structural coupling only consume non-null callees. Making
resolution more precise therefore **moves** those outputs (more or fewer edges
with a concrete callee). That is intentional and is called out in `CHANGELOG.md`.
