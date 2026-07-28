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

## Parts shipped

| Part | Status | What it does |
|------|--------|----------------|
| **A** | Shipped | Restrict candidates to `Function` / `Method`; same-file preference on collisions; one shared resolver for full + incremental index |
| **B** | Shipped | Read `qualified_name` (e.g. `Foo.new`); Python/TS method QN backfill; dotted member callees so `json.loads` / `axios.get` do not false-resolve to a local bare name or same-file Method |
| **C** | **Deferred to 0092** | Package/module-root import mapping (Cargo workspace, `go.mod`, `tsconfig` paths, Python layout) |

## Algorithm (shared: `src/index/resolve.rs`)

Used by both `call_graph.rs` (full index) and `incremental.rs`.

1. **Normalize** the callee for QN lookup: replace `::` with `.` (`Foo::new` → `Foo.new`).
2. **Qualified match** (when the name contains `.` or is an exact QN key): among callable kinds only.
   - 1 match → `RESOLVED`
   - \>1 → `AMBIGUOUS`
   - 0 and multi-segment → **`UNRESOLVED` always** (no bare-segment fallthrough). This keeps
     `json.loads` / `axios.get` external even when a same-file Method is named `loads` / `get`.
     Local `s.process()` also stays unresolved until Part C import/receiver mapping or an exact
     `Type.method` QN match.
3. **Bare single-segment match**, callable kinds only:
   - 0 → `UNRESOLVED`
   - 1 → `RESOLVED`
   - \>1 and exactly one candidate in the caller file → that one (`RESOLVED`)
   - else → `AMBIGUOUS`

Callable kinds (locked): **`Function`**, **`Method`**.

Full-index (`call_graph.rs`) and incremental (`incremental.rs`) both convert DB rows via
`resolve_candidate_from_row` + `build_resolve_maps`, then call `resolve_callee`.

Language call extractors receive a `&[Symbol]` slice (real qualified names from the index),
but extractors do not yet consume that slice — plumbing is live for a future consumer.

## What this cannot do

The following are out of reach of a name+path heuristic (and of this track):

- Re-exports (`pub use`)
- Glob imports (`use foo::*`, `from x import *`)
- Conditional compilation (`#[cfg(...)]`)
- Macro-generated symbols
- Dynamic / reflective dispatch
- Trait / interface dispatch that needs type inference on the receiver
- Package-root import mapping (Part C → track **0092**)

Do not claim completeness against stack graphs or compiler-grade SCIP resolution.
SCIP occurrence ingest into `structural_edges` is a separate deferred track.

## External stays external

Third-party and stdlib calls must stay `UNRESOLVED`. Conflating “external” with
“we did not try” is an honesty failure. Python/TypeScript member calls are stored
as dotted `receiver.field` (same shape Go uses for import-package selectors). The
resolver refuses bare-segment fallthrough on multi-segment names, so neither a
unique local function nor a same-file Method named `loads` can absorb
`json.loads`.

## Downstream impact

`AMBIGUOUS` and `UNRESOLVED` edges write `callee_symbol_id = NULL`. Centrality,
dead-code evidence, and structural coupling only consume non-null callees. Making
resolution more precise therefore **moves** those outputs (more or fewer edges
with a concrete callee). That is intentional and is called out in `CHANGELOG.md`.
