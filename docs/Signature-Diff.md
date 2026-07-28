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

## Coverage table (DoD-12)

Silence from this tool means either *nothing changed* or *the extractor never
looked*. Use this table to tell them apart.

| Language | Covered declaration forms | Not covered (Part A) |
|----------|---------------------------|----------------------|
| **Rust** | `function_item` (free functions and methods with bodies); `function_signature_item` (trait method declarations without default bodies), qualified as `Trait.method` | Impl methods are already captured as `function_item` when they appear as such; TypeScript/Python N/A. Associated consts, macros, and type-alias “callability” are out of scope. |
| **Go** | `function_declaration`; `method_declaration` (receiver-qualified); interface `method_elem` members (pinned grammar name — **not** `method_spec`), qualified as `Iface.Method`. Multi-return via `result` field. | Embedded interface promotion; type-set-only interfaces without methods |
| **TypeScript** | *Not covered in Part A* | `function_declaration`, `method_definition`, `method_signature`, `abstract_method_signature`, `arrow_function`, `function_signature` — planned for Part B |
| **Python** | *Not covered in Part A* | `function_definition` (offsets currently all `None`); variadics/separators — planned for Part B |

### Unannotated Python ceiling (stated, not a bug)

When Part B lands, unannotated parameters normalize to `_`. Reorders of
unannotated params are invisible; only arity (and separators/variadics) are
detectable. Types are **never** inferred from defaults, call sites, or
docstrings.

## HEAD comparison

Previous content is read with **gix** (`git::read_head_blob`) — not a
`Command::new("git")` subprocess. Failure modes (no HEAD, added file, deleted,
rename via `old_path`, unparseable) degrade to **no delta**, never a false
positive “signature changed”.

Matching is by `(kind, qualified_name || name)`. Unmatched symbols are
adds/deletes, not signature changes.

## Coexistence with “Public symbol modified”

The existing `Public symbol modified: {name}` reason still fires for every
public symbol in a changed file. Signature shape risk sits **beside** it with
distinct wording. Making the public-symbol signal accurate is a separate
behaviour change (out of scope for 0088).

## See also

- Track: `0088-SignatureExtractionAndDiff`
- Module: `src/index/signature.rs`
- Provider: `src/impact/enrichment/signature_delta.rs`
