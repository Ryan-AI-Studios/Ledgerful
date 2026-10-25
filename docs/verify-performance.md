# Verify Performance Guide

This document explains the speed levers available for `ledgerful verify` and
their safe combinations.

## Fast vs full scope boundary

- `ledgerful verify --scope full` runs the configured full tier: `cargo fmt`, `cargo clippy`, `cargo nextest run --workspace --all-features --profile ci` when that profile exists, doctests, and `--profile slow` when that profile exists. The configured full tier is not population completeness and does not establish affected-test coverage. `results-recorded` means command results were recorded. CI is split jobs
(fmt, clippy, `test`, `test-slow`), not one `verify` invocation.
- `ledgerful verify --scope fast` is the **local convenience gate**. The pre-push
hook uses this scope.

### What `--scope fast` actually runs (when ScopedOk)

When scoped selection succeeds, the plan includes three steps
(`src/verify/plan/scoped.rs` → `build_fast_scoped_plan`):

1. `cargo fmt --all -- --check` — always
2. `cargo clippy --all-targets --all-features -- -D warnings` — always
3. Scoped `cargo nextest` (or equivalent) selected via `test_mapping` for
   changed files — **this** is the only part that is scoped

So a successful scoped fast run still shows fmt → clippy → tests. That is
intentional: fmt and clippy are cheap and catch issues the test suite does not.

### Fast-or-refuse class table (never surprise full hang)

Classifier order is load-bearing (`--scope fast` only). **LiveEmpty** is
decided in `commands/verify/execute.rs` (before `build_plan_scoped_with_options`)
so plan units stay hermetic. A **dirty** tree then **overlays live git paths**
onto the classifier packet (replace, not union; `head_hash` = live HEAD;
ignore-filter drops `.ledgerful/**` / `.agents/**`). None-packet dirty uses
the same overlay. The rest lives in `build_plan_scoped_with_options`:

| Class | Detection | Default under `--scope fast` |
|---|---|---|
| **LiveEmpty** | Working tree has **no material** changes (git status). *Material* (0549), when the memoized Rust detection is Present or Unknown, is every changed path except `.ledgerful/**`, the `cargo`/`cargo.bat`/`cargo.cmd` test shims, and `.git/`, `target/`, `node_modules/` trees — so a dirty `.txt` template or `.csv` fixture is **not** a clean tree (0321 is superseded for Rust; non-Rust profiles keep the legacy material definition) — **before** SharedInfra; ignores non-empty saved impact packet | **EmptyChanges** cheap plan (Rust: fmt+clippy; non-Rust: zero steps). Exit 0 if steps pass. Prevents phantom scoped work after merge/pull. |
| **SharedInfra** | (live dirty, after overlay) changed paths match shared-infra globs (Cargo.toml, cli/args, config/**, migrations/**, …) | **Full suite** + announce (`scopeExecuted: "full"`) — justified. SharedInfra’s `.ledgerful/**` glob is **unreachable** on the fast+dirty overlay path (watch ignore-list wins). A `.ledgerful/rules.toml`-only dirty tree is LiveEmpty / EmptyChanges, **not** SharedInfra. |
| **NonCodeCheap** | Every classified path matches the cheap glob set: exact README / CHANGELOG / LICENSE / SECURITY / agent front doors / bump scripts, `docs/**` except `docs/api/openapi.json`, or `packaging/**`. | Unreferenced cheap paths keep fmt+clippy without freshness gating. **Embedded-material lookup (0549):** uses tri-state Rust detection: present, absent, or inconclusive. Discovery is memoized per fast invocation and runs when a dirty path differs between strict and legacy materiality, or when the repository has no root Cargo.toml and a cheap-set path (docs, README, LICENSE, packaging) is dirty — the only class that can otherwise return a zero-step green; with a root manifest, docs-only edits do not trigger it, and a non-Rust repo is not walked for ordinary source changes. A missing memo means legacy materiality and planning never initiates a discovery walk. Inconclusive discovery fails closed for material that needs the lookup; cheap-only and empty plans retain their existing behavior. Full-scope planning does not discover nested manifests or synthesize Rust profiles. A malformed or unreadable root manifest refuses a non-empty fast plan, including cheap docs; an empty plan is exempt. Nested Rust monorepos are classified but referenced material refuses at their Git root. Compile-time includes and literal runtime reads select includer test function names in one scoped nextest step. The engine uses `INCLUDE_STR_MATERIAL` only for cheap paths and lookup for non-cheap candidates; third-party trees always use lookup. Drift assertions cover cheap includes and literal runtime reads. Packaging still unions `bump_manifests` into the same step; mixed unmapped source still refuses. No `--no-tests=pass`. In foreign roots, unreferenced non-cheap paths outside the legacy-material set (`.png`, `.txt`, `.csv`) annotate-and-pass only without unresolved reads; this engine checkout refuses unreferenced non-cheap material; legacy-material paths (`.json`, `.yaml`, `.sql`, nested manifests) refuse even alongside mapped source. Literal-path file consumers refuse matching candidates (including directory prefixes); literal-less consumers remain repo-wide uncertainty. Unreadable/unlexable sources refuse or use explicitly authorized `allow_full_fallback`. Bounds refuse non-cheap paths; cheap docs retain 0203 behavior with the annotation `embedded lookup bounded: not verified`. `OUT_DIR` generated includes and runtime reads do not count as unresolved or dynamic reads. Oversized sources use 64 KiB streaming chunks with overlap, up to 8 MiB; inspected bytes do not consume the 64 MiB lexical budget. Token-free oversized sources are skipped with annotation; token-bearing or larger sources are bounded. Bounds remain 2500 Rust files, 512 KiB per lexed file, 64 MiB aggregate. Absolute in-repo targets normalize to relative paths; outside-repo targets are ignored with annotation. The pinned walk includes hidden and ignored sources and disables parent and machine ignore rules. `.git`, `.ledgerful`, `node_modules`, `.venv`, and `venv` stay excluded at every depth. `vendor`, `deps_src`, and `third_party` are excluded only as direct children of the walk root. `target` is excluded only when its parent contains `Cargo.toml`. Skip names stay case-insensitive on Windows. Includes resolve relative to the includer; manifest-relative includes use the nearest Cargo manifest; runtime reads use crate and repo roots. Nested-only referenced material refuses; add a root workspace manifest or use a checkout rooted at the crate. `-C` alone does not change the Git work root. Automatic profiles stay rooted at the Git repository. Lookup-derived stems are function names; existing D3(d) `query_scoped_test_files` stems may remain file-level (`0549-P3-2`, 0550); a `__slow` or default-filter selector can be one way a nextest run exits 4; assurance says `runner reported no tests` and does not detect an exclusion (`0549-P3-1` wording, 0555; detection stays 0550). Non-Rust embedding is 0570. A literal path passed to a production helper is attributed through one hop in the same crate. Two helpers with the same final ident in one crate share those literals. A path built at runtime stays an accepted limit and that read does not refuse every unreferenced file. The annotation `no direct include/read found by the lexical lookup` remains when no literal consumer, include, or helper call names the candidate. An askama `#[template(path = ...)]` also resolves under `<crate>/templates/`. An empty embed key is that crate root and matches every path under it. A custom `askama.toml` directory list is not read. Legacy-material paths still refuse. Annotations are visible in unit-level descriptions and live `--verbose` plans when there is a step; zero-step plans have no step to annotate. No annotation field is added to dry-run JSON. `scopeExecuted` remains fast/refused; fallback uses existing `fallbackReason`, with stable unreadable/nonUtf8/oversized/unlexable classes and no OS error text. |
| **EmptyChanges** | `packet.changes` empty after overlay (checked **before** stem query) | Same cheap plan as LiveEmpty |
| **HeadMismatch** | `test_mapping` populated + index `head_hash` ≠ packet head | **Auto-repair once** (bounded incremental) **without** `--auto-index`; re-classify; still lag → **refuse**. Not silent ScopedOk on stems alone. |
| **EmptyMapping** | `test_mapping` count == 0 / table missing | **Refuse** unless `--auto-index` (try once; still empty → refuse). Bootstrap cost is opt-in. |
| **ScopedOk** | freshness Ok + `test_mapping` yields stems for changed files | Existing 3-step scoped plan; **union** stem `bump_manifests` when any packaging / bump-script path is in the classified set (A2 mixed); **union** `INCLUDE_STR_MATERIAL` stems when any `include_str!` cheap path is in the classified set **and** Rust is detected **and** the canonical work root equals this binary's `CARGO_MANIFEST_DIR` (0480 mixed, 0483 engine-tree gate; skip doc inject when `profile.rust` is None or the root is a third-party tree; packaging A2 still injects `bump_manifests` without rust and without that gate, including on a tree that does not have the stem) |
| **MappingRefuse** | empty mapping (no flag), head-lag still unusable after repair, PacketHeadMissing without flag, no stems, no DB, unmapped src/openapi | **Refuse** — do **not** execute full. Exit ≠ 0. `scopeExecuted: "refused"`, `plan.refused=true`, empty steps |

**Escape hatches:**

| Hatch | Behavior |
|---|---|
| `--scope full` | Configured full tier (fmt, clippy, nextest `ci` when that profile exists, doctests, slow profile when it exists). Not population completeness. |
| `--allow-full-fallback` | Restore 0061 mapping-path full execute + announce |
| `--auto-index` | Required for **empty** mapping bootstrap; also repairs PacketHeadMissing. Head-lag repairs **without** this flag. Still-cannot → **refuse** unless allow also set |
| Pre-push | `verify --scope fast` **without** allow — benefits from LiveEmpty + head-lag auto-repair; empty mapping still blocks until index fixed |

Human dry-run first product line is always `scope:` (`scope: fast` or
`scope: full (pre-push uses --scope fast)`). Omitted `--scope` on
`--dry-run` is **fast** (pre-push / Daily 5). Executed `verify` without
`--scope` stays **full**. Explicit `--scope full --dry-run` keeps the
full line. Fully-clean auto-policy dry-run skips `OutcomePredictor`
(no stale Predicted Impacts). Docs-only dirty trees (CHANGELOG / `*.md`)
are material and still predict. On MappingRefuse `--dry-run`,
that line is **above** the greppable ℹ reason:

```text
scope: fast
ℹ fast scope unavailable — <trigger>; refusing full suite (~5-8 min)
Next: ledgerful index --incremental
      ledgerful verify --scope fast --auto-index
      ledgerful verify --scope full
      ledgerful verify --scope fast --allow-full-fallback
```

Live (non-dry-run) refuse still prints the ℹ reason first (no `scope:`
banner). The ℹ line remains greppable either way.

All speed measures in this guide apply to `--scope fast` only. `--scope full`
stays the configured full tier described above. It does not become a claim that every project test ran.

## Incremental compilation (`CARGO_INCREMENTAL=1`)

On a warm local checkout, `ledgerful verify --scope fast` sets
`CARGO_INCREMENTAL=1` for cargo steps (clippy, nextest). This keeps the
incremental cache warm across repeated local runs.

Requirements:

- Only on `--scope fast`.
- Only when `CI` is not set to `true`.
- Only when `RUSTC_WRAPPER` is unset (sccache is not active).

## sccache

For cold builds, CI, or machines with multiple checkouts, sccache is the better
lever. It caches dependency crates across clean builds.

```bash
# Install sccache v0.17.0+ (prefer current crates.io; Windows path fixes since 0.16)
cargo install sccache --version ^0.17

# Use RUSTC_WRAPPER, not RUSTC_WORKSPACE_WRAPPER. The workspace wrapper only
# wraps workspace members and skips dependency caching, which is the whole win.
export RUSTC_WRAPPER=sccache

# sccache cannot cache incrementally-compiled crates, so this is required:
export CARGO_INCREMENTAL=0
```

**Never combine `CARGO_INCREMENTAL=1` with sccache.** Choose one per context:

| Context | Lever |
|---|---|
| Warm local checkout, solo dev | `CARGO_INCREMENTAL=1` (fast path only) |
| Cold build / CI / multi-checkout | `RUSTC_WRAPPER=sccache` + `CARGO_INCREMENTAL=0` |

`sccache` is surfaced as guidance only (e.g. `ledgerful doctor` hints). It is
not wired into verify's command generation.

## Link time on Windows

- **mold** is Linux-only and not applicable here.
- On Windows, the link-time lever is switching to `rust-lld` via
`RUSTFLAGS="-C link-arg=-fuse-ld=lld"` or a `config.toml` linker setting. This
is optional and not part of the verify plan.

## Progress UX (0148)

Human `ledgerful verify` shows which plan step is active while multi-second
children run. Machine `--json` stays pure (no product progress lines; timings
already live in `steps[].durationMs`).

| Line | Default | Verbose | JSON |
|---|---|---|---|
| Aggregate “Running N step(s)…” | demoted `debug!` | keep (INFO) | skip |
| `[i/n] Running: <cmd>` | yes | yes | no |
| compact `ok (2.2s)` | yes | **no** | no |
| SUCCESS banner | no | yes (as-is) | no |
| FAILURE | yes | yes | no |

LiveEmpty / EmptyChanges walls of tens of seconds under `--scope fast` are
**real work** (fmt + clippy on a clean tree), not hangs — progress lines prove
the step is alive. See the [fast-or-refuse class table](#fast-or-refuse-class-table-never-surprise-full-hang).

**Stdout rationale:** step-start and compact ok go on **stdout** (not stderr) so
pre-push / agent combined-stream logs (`2>&1`) keep progress greppable next to
the trailing `Verification passed`. This is a deliberate departure from
clig.dev’s “messaging on stderr” guidance; do not “fix” progress to stderr
without revisiting hook log UX.

## Why `--scope fast` does not parallelize fmt with clippy

The fast path runs `cargo fmt --all -- --check` (read-only) sequentially before
clippy. A mutating `cargo fmt` (without `--check`) rewrites `.rs` files in place,
which would cause `rustc`/clippy torn reads, spurious errors, and
incremental-cache invalidation. The ~2s potential saving is not worth the risk,
so fmt stays first and sequential.

## `--auto-index`

**Head lag** (populated `test_mapping`, index `head_hash` ≠ packet/HEAD) is
repaired **automatically once** under `--scope fast` without this flag (0145).

When `test_mapping` is **empty** (or PacketHeadMissing), `--scope fast`
**refuses** by default (does not start a multi-minute full suite). With
`--auto-index`, verify refreshes the index for changed files first and retries
scoped selection once. On success → ScopedOk; if still cannot scope →
**refuse** unless `--allow-full-fallback` is also set. Opt-in because empty
bootstrap can add noticeable latency.

## Troubleshooting timeouts

If a step times out, the error now includes:

- The exact command that timed out.
- The elapsed time.
- A likely cause (cold build or feature-resolution mismatch).
- A next step: run `ledgerful index --incremental` or use `--scope full`
deliberately.

`--timeout` caps each auto-policy step. Fast empty-changes stays fmt 60 /
clippy 400. Test-class steps use `[verify] suite_timeout_secs` or
`DEFAULT_TEST_SUITE_TIMEOUT_SECS` (400). Auto `--timeout 0` uses the 400
ceiling (it does not disable the clock). Manual `--timeout 0` is an immediate timeout row (`Duration::ZERO`),
not a disabled clock. A hit emits verify JSON (`exitCode` 124, `failureDetail`
contains `Step timed out after`) and does not start later steps. Full scope
is not a 40-second guarantee: the ceiling is per step, then stop — not a
second cumulative deadline.

Production includers without inline tests use fresh `test_mapping` rows
(D3(d)), checked separately for each includer. Lookup-derived stems are test
function names; stems returned by the existing mapping route may still be
file-level (accepted residual `0549-P3-2`, owner 0550).

## Execution assurance (0555)

`executionAssurance` is a nested packet on executed verify JSON, dry-run JSON, and the MCP `verify_plan` preview. It is not `verifySignatures.assurance`. Root `schemaVersion` stays 1. The nested `schemaVersion` is 1. `testCasesRun` is JSON `null` in v1 and the key is present. `null` is not zero. `selectionCompleteness` stays `not-established`.

`results recorded` means the recorded commands match the plan in order. It is not pass wording. A failing run with `failedSteps` at least 1 still says `results recorded` and does not say `Verification passed`.

`runner reported no tests` is the human phrase for limitation `runner-reported-no-tests`. A recognized direct `cargo nextest run` that exits 4 adds that code. The code does not say why the runner reported no tests. A `__slow` selector or a default filter can be one cause. This packet does not detect an excluded profile. Detection stays 0550. `cargo test` exit 4 does not add the code. `--no-tests=pass` with exit 0 stays `test commands attempted` and does not add the code.

### Limitation codes

Sorted ASCII, duplicates removed. The three always-on codes are `affected-tests-not-proven-complete`, `case-counts-not-collected`, and `exclusions-not-inventoried`.

| Code | When | Human fragment |
|---|---|---|
| `affected-tests-not-proven-complete` | always | `affected-test completeness not established` |
| `case-counts-not-collected` | always | `test-case count unavailable` |
| `exclusions-not-inventoried` | always | `exclusions not inventoried` |
| `result-correspondence-unknown` | `execution` is `unknown` | `result correspondence unknown` |
| `commands-not-executed` | valid preview, refusal, or no steps | state phrase below; this token is not printed |
| `no-verification-steps` | executed plan with no steps | `no verification steps planned; nothing executed` |
| `partial-command-results` | matching prefix shorter than the plan | `partial` |
| `unknown-command-semantics` | command class unknown and correspondence is valid | `test commands unknown` |
| `runner-reported-no-tests` | direct `cargo nextest run` exit 4 | `; runner reported no tests` |

`unknown-command-semantics` includes a manual command, a shell step, a custom runner, and a global `--help` / `--version`. `npm run test:ci`, `yarn run test:ci`, `pnpm run test:ci`, and `deno task test:ci` are examples: `test commands unknown`.

Human line, one ASCII line:

`Assurance: {R}/{N} command results ({S}); test commands {T}; test-case count unavailable; affected-test completeness not established; exclusions not inventoried{U}.`

`{S}` is `results recorded`, `preview; no verification commands executed`, `refused; no verification commands executed`, `no verification steps planned; nothing executed`, `partial`, or `result correspondence unknown`. `{T}` is `not invoked`, `attempted (case execution unproven)`, or `unknown`. `{U}` is `; runner reported no tests` only when that code is present.
