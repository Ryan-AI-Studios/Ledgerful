# Verify Performance Guide

This document explains the speed levers available for `ledgerful verify` and
their safe combinations.

## Fast vs full scope boundary

- `ledgerful verify --scope full` is the **authoritative gate**. It always runs
the complete suite (fmt, clippy, tests, doctests, slow tier). CI uses this
scope.
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
| **LiveEmpty** | Working tree has **no material** changes (git status). *Material* (0549), where Rust-crate detection is `Present` or `Unknown` (root `Cargo.toml`, else a bounded pinned walk run once per invocation and only when a dirty path's strict/legacy materiality differs), is every changed path except `.ledgerful/**`, the `cargo`/`cargo.bat`/`cargo.cmd` test shims, and `.git/`, `target/`, `node_modules/` trees — so a dirty `.txt` template or `.csv` fixture is **not** a clean tree (0321 is superseded for Rust; detection that is `Absent` keeps the legacy material definition; an inconclusive walk or a malformed/unreadable root `Cargo.toml` fails **closed**: strict material plus a `Rust detection inconclusive: <class>` refusal, never a 0-step green) — **before** SharedInfra; ignores non-empty saved impact packet | **EmptyChanges** cheap plan (Rust: fmt+clippy; non-Rust: zero steps). Exit 0 if steps pass. Prevents phantom scoped work after merge/pull. |
| **SharedInfra** | (live dirty, after overlay) changed paths match shared-infra globs (Cargo.toml, cli/args, config/**, migrations/**, …) | **Full suite** + announce (`scopeExecuted: "full"`) — justified. SharedInfra’s `.ledgerful/**` glob is **unreachable** on the fast+dirty overlay path (watch ignore-list wins). A `.ledgerful/rules.toml`-only dirty tree is LiveEmpty / EmptyChanges, **not** SharedInfra. |
| **NonCodeCheap** | Every classified path matches the cheap glob set: exact README / CHANGELOG / LICENSE / SECURITY / agent front doors / bump scripts, `docs/**` except `docs/api/openapi.json`, or `packaging/**`. | Unreferenced cheap paths keep fmt+clippy without freshness gating. **Embedded-material lookup (0549):** applies where Rust crates are `Present` (root manifest or a nested one found by the bounded pinned walk) or `Unknown`. Nested-only Rust (no root `Cargo.toml`) **refuses** a referenced path with `Rust crates found only below the repo root (no root Cargo.toml); add a root workspace Cargo.toml or run verify from a checkout rooted at the crate` — `-C <dir>` does not help, and Ledgerful does not rewrite cargo commands; `RepositoryProfile.rust` stays root-based. Compile-time includes and literal runtime reads select includer test function names in one scoped nextest step. The engine uses `INCLUDE_STR_MATERIAL` only for cheap paths and lookup for non-cheap candidates; third-party trees always use lookup. Drift assertions cover cheap includes and literal runtime reads. Packaging still unions `bump_manifests` into the same step; mixed unmapped source still refuses. No `--no-tests=pass`. Unreferenced non-cheap paths outside the legacy-material set (`.png`, `.txt`, `.csv`) annotate-and-pass only without unresolved reads; legacy-material paths (`.json`, `.yaml`, `.sql`, nested manifests) refuse even alongside mapped source. Unresolved consumers, unreadable/unlexable sources and bounds refuse or use explicitly authorized `allow_full_fallback`. `OUT_DIR` generated includes and reads do not count as unresolved. Unmodeled file consumers (`*_file!`, `#[template(path=..)]`, `include_dir!`, RustEmbed `#[folder=..]`) are attributed per candidate by their literal path and refuse only the matching paths; a consumer with no literal path is a repo-wide unresolved read. Include/consumer targets outside the repo (absolute or `..`-escaping) are ignored with an annotation; absolute in-repo targets are normalized. A `Bounded` scan refuses non-cheap candidates while cheap docs keep the 0203 behavior with `embedded lookup bounded: not verified` (residual `0549-P3-5`). Why an unreferenced `.csv` passes while an unreferenced `.json` refuses: `.csv`/`.png`/`.txt` were not material before 0549, so they get the annotate-and-pass outcome ("no direct include/read found by the lexical lookup" — not a claim that no test depends on them, e.g. a literal passed through a production helper is not attributed), whereas `.json`/`.yaml`/`.sql`/nested manifests were material and keep refusing. Oversized sources are stream-inspected (never loaded whole, not counted toward the aggregate): token-free ones are skipped with annotation; consumer-bearing ones or any above 8 MiB report `Bounded`. Lookup stem sets above 50 carry a `broad stem set (N)` annotation. Bounds remain 2500 Rust files, 512 KiB per lexed file, 64 MiB aggregate. The pinned walk includes hidden/ignored sources, disables parent/machine ignore rules, and excludes target/Git/Ledgerful/node_modules/vendor/deps_src/third_party. Includes resolve relative to the includer; manifest-relative includes use the nearest Cargo manifest; runtime reads use crate and repo roots. Lookup-derived stems are function names; existing D3(d) `query_scoped_test_files` stems may remain file-level (`0549-P3-2`, 0550); `__slow` annotation remains `0549-P3-1` (0555). Non-Rust embedding is 0570. Annotations are visible in unit-level descriptions and live `--verbose` plans, not dry-run JSON. `scopeExecuted` remains fast/refused; fallback uses existing `fallbackReason`, with stable unreadable/nonUtf8/oversized/unlexable classes and no OS error text. |
| **EmptyChanges** | `packet.changes` empty after overlay (checked **before** stem query) | Same cheap plan as LiveEmpty |
| **HeadMismatch** | `test_mapping` populated + index `head_hash` ≠ packet head | **Auto-repair once** (bounded incremental) **without** `--auto-index`; re-classify; still lag → **refuse**. Not silent ScopedOk on stems alone. |
| **EmptyMapping** | `test_mapping` count == 0 / table missing | **Refuse** unless `--auto-index` (try once; still empty → refuse). Bootstrap cost is opt-in. |
| **ScopedOk** | freshness Ok + `test_mapping` yields stems for changed files | Existing 3-step scoped plan; **union** stem `bump_manifests` when any packaging / bump-script path is in the classified set (A2 mixed); **union** `INCLUDE_STR_MATERIAL` stems when any `include_str!` cheap path is in the classified set **and** Rust is detected **and** the canonical work root equals this binary's `CARGO_MANIFEST_DIR` (0480 mixed, 0483 engine-tree gate; skip doc inject when `profile.rust` is None or the root is a third-party tree; packaging A2 still injects `bump_manifests` without rust and without that gate, including on a tree that does not have the stem) |
| **MappingRefuse** | empty mapping (no flag), head-lag still unusable after repair, PacketHeadMissing without flag, no stems, no DB, unmapped src/openapi | **Refuse** — do **not** execute full. Exit ≠ 0. `scopeExecuted: "refused"`, `plan.refused=true`, empty steps |

**Escape hatches:**

| Hatch | Behavior |
|---|---|
| `--scope full` | Authoritative full suite (unchanged) |
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
remains unchanged.

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
