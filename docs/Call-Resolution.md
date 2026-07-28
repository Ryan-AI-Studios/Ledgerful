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

Do not claim completeness against stack graphs or compiler-grade SCIP resolution.
SCIP occurrence ingest into `structural_edges` is a separate deferred track.

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
