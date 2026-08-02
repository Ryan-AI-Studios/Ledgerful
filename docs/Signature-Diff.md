# Signature Diff

> Function/method **signature** extraction and impact risk — not Ed25519 ledger
> crypto. See also `src/index/signature.rs` module docs.

## What this is

Ledgerful extracts a normalized **textual shape** of function and method
signatures during tree-sitter symbol indexing, and on `impact` compares the
working tree against HEAD to emit a distinct risk reason when the **shape**
changes.

This is a **floor**, not a completeness claim:

- A signature change is a real, observable API surface change.
- **Absence of a signature change is not absence of a breaking change.**
- Surfaces must never emit “no breaking changes detected” or equivalent.

Even mature tools in this space (e.g. cargo-semver-checks) document that they
cannot catch all parameter type changes in the general case. We deliberately
do **not** run a type checker.

## Three change classes

| Class | Trigger | Risk reason? |
|-------|---------|--------------|
| **Shape** | Arity, ordered parameter types, return type, or behavioural modifiers change | **Yes** — `Signature changed: {name}: {prev} → {curr}` |
| **Cosmetic** | Parameter *rename* only (same arity, types, return, modifiers) | Recorded on the packet, **not** scored |
| **Unknown** | Language states no static type at a position (`_` in the shape) | Never inferred into a risk claim |

**Modifiers are part of the shape.** Example: `fn foo()` → `async fn foo()` is a
Shape change even though the AST return type is unchanged. Any modifier change
is treated as Shape (no grading of which modifiers “break harder”).

## Packet surface

`ImpactPacket.signatureDeltas` (camelCase JSON) carries:

- `filePath`, `symbolName`, `previousSignature`, `currentSignature`, `changeClass`
- Sorted deterministically; omitted when empty

`signature_hash` on `project_symbols` is the blake3 hex of `metadata.signatureShape`,
derived in one place (`symbol_to_project_symbol`) for all languages.

## Coverage table (DoD-12 / 0091 DoD-9)

Silence from this tool means either *nothing changed* or *the extractor never
looked*. Use this table to tell them apart.

| Language | Covered declaration forms | Not covered |
|----------|---------------------------|-------------|
| **Rust** | `function_item` (free functions and methods with bodies); `function_signature_item` (trait method declarations without default bodies), qualified as `Trait.method` | Impl methods are already captured as `function_item` when they appear as such. Associated consts, macros, type-alias “callability”, and generics/`type_parameters` are out of scope (parity across languages). |
| **Go** | `function_declaration`; `method_declaration` (receiver-qualified); interface `method_elem` members (pinned grammar name — **not** `method_spec`), qualified as `Iface.Method`. Multi-return via `result` field. | Embedded interface promotion; type-set-only interfaces without methods; generics |
| **TypeScript** | `function_declaration`; `method_definition` (class + object-literal); `method_signature` / `abstract_method_signature` (qualified as `Owner.method` for class/interface/abstract class); `function_signature` (`declare function`); **named** `arrow_function` only when a naming host is found — `variable_declarator` (identifier), `public_field_definition` → `Class.field`, or `export default` → **`{path.with_ext_stripped.dots}.default`** (e.g. `src/foo/index.ts` → `src.foo.index.default`; same stem in different dirs does not collide). Params: required/optional (optionality encoded as `type?`), rest (`...type`), return via `type_annotation` / `asserts_annotation` / `type_predicate_annotation` (leading `:` stripped). Modifiers: accessibility, `static`, `override`, `readonly`, `async`, get/set, generator `*`, optional method `?`. **`.tsx`/`.jsx` use `LANGUAGE_TSX`** for symbol extraction (0091 DoD-8 Fix). | **Anonymous arrows** (`xs.map(x => …)`) — no symbol (avoids dead-code pollution / 0092 collision load). Destructuring arrow hosts. `function_expression` / overload sets. **All TypeScript decorators** (routing belongs to `extract_routes`, not shape). Generics/`type_parameters`. Grammar vintage: pinned `tree-sitter-typescript 0.23.2` (2024-11-11, latest on crates.io) predates TS 5.7–7.0 — newer syntax may parse to `ERROR`. Calls/routes/models/observability extractors still use `LANGUAGE_TYPESCRIPT` (symbols-only TSX routing). |
| **Python** | `function_definition` (free + `Class.method`); full parameter grammar including typed/default/variadic/`/`/`*` separators; return type; `async`; **binding decorators** only (`staticmethod`, `classmethod`, `property`, `abstractmethod` — trailing-identifier match so `@abc.abstractmethod` hits). Separators encoded as modifiers `posonly-after=N` / `kwonly-after=N` (not params — arity stays honest). Type text: surrounding quotes stripped, internal whitespace collapsed. Offsets populated. | **Non-binding decorators** (`@app.route`, pytest, wraps, …) deliberately ignored — not calling-contract. Generics/`type_parameters` (PEP 695). Nested function defaults inferred from values (defaults never enter the shape). |

### Unannotated Python ceiling (present tense)

Unannotated parameters normalize to `_` in the shape. Reorders of unannotated
params are invisible; only arity, separators, and variadics are detectable at
those positions. Types are **never** inferred from defaults, call sites, or
docstrings.

### PEP 649 / 749

Deferred annotation evaluation (Python 3.14) does **not** affect this extractor:
annotations are read as source text and never evaluated.

## HEAD comparison

Previous content is read with **gix** (`git::read_head_blob`) — not a
`Command::new("git")` subprocess. Failure modes (no HEAD, added file, deleted,
rename via `old_path`, unparseable) degrade to **no delta**, never a false
positive “signature changed”.

Matching is by `(kind, qualified_name || name)`. Unmatched symbols are
adds/deletes, not signature changes.

## Coexistence with public-symbol risk wording

Public-symbol risk reasons use a **status-aware verb** (0129) from
`ChangedFile.status`:

| File status | Reason prefix |
|---|---|
| `Added` | `Public symbol added: {name}` |
| `Deleted` | `Public symbol deleted: {name}` |
| `Renamed` | `Public symbol renamed: {name}` |
| `Modified` / other | `Public symbol modified: {name}` |

The reason still fires for **every public symbol in a touched file** (not only
symbols whose body actually changed). Signature shape risk sits **beside** it
with distinct wording (`Signature changed: …`). Filtering to symbols that
actually changed remains deferred (0088 residual — risk weight distribution).

## See also

- Tracks: `0088-SignatureExtractionAndDiff`, `0091-SignatureExtractionTsPython`
- Module: `src/index/signature.rs`
- Provider: `src/impact/enrichment/signature_delta.rs`
