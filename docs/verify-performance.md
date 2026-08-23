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
decided in `commands/verify/mod.rs` (before `build_plan_scoped_with_options`)
so plan units stay hermetic. A **dirty** tree then **overlays live git paths**
onto the classifier packet (replace, not union; `head_hash` = live HEAD;
ignore-filter drops `.ledgerful/**` / `.agents/**`). None-packet dirty uses
the same overlay. The rest lives in `build_plan_scoped_with_options`:

| Class | Detection | Default under `--scope fast` |
|---|---|---|
| **LiveEmpty** | Working tree has **no material** changes (git status) — **before** SharedInfra; ignores non-empty saved impact packet | **EmptyChanges** cheap plan (Rust: fmt+clippy; non-Rust: zero steps). Exit 0 if steps pass. Prevents phantom scoped work after merge/pull. |
| **SharedInfra** | (live dirty, after overlay) changed paths match shared-infra globs (Cargo.toml, cli/args, config/**, migrations/**, …) | **Full suite** + announce (`scopeExecuted: "full"`) — justified. SharedInfra’s `.ledgerful/**` glob is **unreachable** on the fast+dirty overlay path (watch ignore-list wins). A `.ledgerful/rules.toml`-only dirty tree is LiveEmpty / EmptyChanges, **not** SharedInfra. |
| **NonCodeCheap** | Every classified path matches the cheap glob set: exact `CHANGELOG.md` / `README.md` / `LICENSE` / `SECURITY.md` / `AGENTS.md` / `Agents.md` / `Claude.md` / `scripts/bump-manifests.ps1` / `.sh`; prefix `docs/**` **except** `docs/api/openapi.json`; prefix `packaging/**`. **Not** cheap: `.agents/**` (watch-ignored; skill edits are not docs-cheap), `src/**`, OpenAPI JSON. | Skip freshness. **Docs/CHANGELOG-only:** fmt+clippy, **zero** nextest, `fallback_reason=None`, `scopeExecuted: "fast"`. **Packaging / bump-script (A2):** inject scoped nextest `test(bump_manifests)` (not workspace `--profile ci`). Mixed src **mapped** + packaging **unions** `bump_manifests` into ScopedOk. Mixed src **unmapped** + packaging still MappingRefuse (do not cheap mixed). |
| **EmptyChanges** | `packet.changes` empty after overlay (checked **before** stem query) | Same cheap plan as LiveEmpty |
| **HeadMismatch** | `test_mapping` populated + index `head_hash` ≠ packet head | **Auto-repair once** (bounded incremental) **without** `--auto-index`; re-classify; still lag → **refuse**. Not silent ScopedOk on stems alone. |
| **EmptyMapping** | `test_mapping` count == 0 / table missing | **Refuse** unless `--auto-index` (try once; still empty → refuse). Bootstrap cost is opt-in. |
| **ScopedOk** | freshness Ok + `test_mapping` yields stems for changed files | Existing 3-step scoped plan; **union** stem `bump_manifests` when any packaging / bump-script path is in the classified set (A2 mixed) |
| **MappingRefuse** | empty mapping (no flag), head-lag still unusable after repair, PacketHeadMissing without flag, no stems, no DB, unmapped src/openapi | **Refuse** — do **not** execute full. Exit ≠ 0. `scopeExecuted: "refused"`, `plan.refused=true`, empty steps |

**Escape hatches:**

| Hatch | Behavior |
|---|---|
| `--scope full` | Authoritative full suite (unchanged) |
| `--allow-full-fallback` | Restore 0061 mapping-path full execute + announce |
| `--auto-index` | Required for **empty** mapping bootstrap; also repairs PacketHeadMissing. Head-lag repairs **without** this flag. Still-cannot → **refuse** unless allow also set |
| Pre-push | `verify --scope fast` **without** allow — benefits from LiveEmpty + head-lag auto-repair; empty mapping still blocks until index fixed |

Human dry-run first product line is always `scope:` (`scope: fast` or
`scope: full (pre-push uses --scope fast)`). On MappingRefuse `--dry-run`,
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
