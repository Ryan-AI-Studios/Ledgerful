# Semantic Search

> Code-chunk embeddings over a local vector store (`snippet_embedding` in
> CozoDB). Requires a **configured embedding backend** — Ledgerful does not
> ship or download a model. See also `src/semantic/` module docs.

## What this is

`ledgerful search --semantic` and the semantic path of `ledgerful ask` rank
AST-chunked code snippets by cosine distance to a query embedding. Results are
only as meaningful as the embedding model behind `local_model.base_url` (or
`local_model.embedding_url`).

This is a **capability that can be off**:

- Unconfigured is a **valid, supported install state**.
- Absence of semantic matches must never be read as “the search ran and found
  nothing” when the backend was not configured or was unreachable.

## Requirements

| Item | Config key / command |
|------|----------------------|
| Embedding server URL | `local_model.base_url` or `local_model.embedding_url` |
| Model name | `local_model.embedding_model` |
| Dimensions (optional; probed when 0) | `local_model.dimensions` |
| Inspect without writing | `ledgerful index --semantic-dry-run` |
| Populate the index | `ledgerful index --semantic` (**only after** the backend is configured) |

There is **no** `--print-semantic-config` flag. Use `--semantic-dry-run`.

## States (backend × index)

Backend health and index emptiness are **orthogonal**:

| Backend | Index | What you see |
|---------|-------|--------------|
| **Not configured** | any | Configure `local_model.base_url` / `embedding_url`. Inspect with `--semantic-dry-run`. **Never** “run `index --semantic`” alone — that cannot populate a meaningful index. |
| **Unreachable** | any | Check the model server at the configured URL. |
| **Ready** | empty | Run `ledgerful index --semantic` to populate. |
| **Ready** | populated | Semantic ranking runs; “no matches” means the query found nothing, not that semantic search was skipped. |
| dimension mismatch | — | Run `ledgerful update --migrate` after aligning dimensions. |

### Ban the absence claim

Surfaces must distinguish:

1. **Semantic search did not run** (not configured / unreachable) — fallback copy says so.
2. **Semantic index empty** (backend ready, no vectors) — remediate with `index --semantic`.
3. **No semantic matches** (backend ready, index populated, query returned nothing).
4. **Semantic search failed** (backend ready, but embed/query errored) — say **failed** with a short error and fall back to BM25/`ask` non-semantic context. Never claim “no matches.”

Never collapse (1), (2), or (4) into “no semantic matches.”

## What happens without a backend

- `ledgerful index --semantic` **refuses** with a message naming the config key
  to set. It does **not** write zero vectors.
- `ledgerful search --semantic` warns with the not-configured message and falls
  back to BM25. It does **not** recommend `index --semantic` until a backend is
  ready.
- `ledgerful doctor` reports `Embedding Model: Not configured` (including the
  partial case where only the model *name* is set). That counts as a doctor
  failure for the advertised capability.

## Legacy zero-vector rows

Older versions could store all-zero embeddings when `index --semantic` ran
without a backend. Those rows are **detected and reported** (count + remediation);
they are **not** auto-deleted. Query time excludes zero-magnitude stored vectors
so ranking remains useful once a real backend is configured. To replace junk
rows: configure the backend, then re-run `ledgerful index --semantic`.

## Work-root isolation

Semantic keys are **work-root-relative** (slash-normalized, e.g. `src/foo.rs`).
Linked worktrees that share a tree intentionally share the same relative keys
and may share `state_dir` (see worktree layout / 0108).

| Guarantee | Behavior |
|-----------|----------|
| **Write** | New `snippet_embedding` / `semantic_file_hash` keys are relative to the active work root. |
| **Read** | `search --semantic`, `ask` semantic context, and semantic hotspots drop foreign absolute / out-of-root keys even if the store still holds legacy poison. |
| **Prune** | Full and incremental `index --semantic` purge foreign keys from both relations. |
| **Honesty** | Envelope `search --json` may include `semantic.filteredForeignCount` when foreign hits were filtered (omit when zero). Residual count is **envelope-only** — not present on `--json-lines`. |

**After upgrade or moving the repo to a new absolute path:** old absolute keys look empty / filtered until you run `ledgerful index --semantic` (full preferred) to purge leftovers and rewrite relative keys. Do not interpret residual empty results as “no matches” without checking that the index was rebuilt under the new root.

**Do not share `LEDGERFUL_STATE_DIR` across unrelated repos.** Relative keys can collide (`src/main.rs` from repo A vs B). Linked worktrees of the **same** tree are fine; multi-root semantic federation is not productized on the default path.

## Related

- Signature honesty precedent: `docs/Signature-Diff.md` (coverage table; ban on
  overstated absence claims).
- Doctor availability helper: factored for extension by optional toolchains
  (track 0095) — optional absence must not land in `DoctorReport.tools`.
