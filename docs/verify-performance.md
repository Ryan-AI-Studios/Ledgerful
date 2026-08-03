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
(`src/verify/plan.rs` → `build_fast_scoped_plan`):

1. `cargo fmt --all -- --check` — always
2. `cargo clippy --all-targets --all-features -- -D warnings` — always
3. Scoped `cargo nextest` (or equivalent) selected via `test_mapping` for
   changed files — **this** is the only part that is scoped

So a successful scoped fast run still shows fmt → clippy → tests. That is
intentional: fmt and clippy are cheap and catch issues the test suite does not.

### Fast-or-refuse class table (never surprise full hang)

Classifier order is load-bearing (`build_plan_scoped_with_options`):

| Class | Detection | Default under `--scope fast` |
|---|---|---|
| **SharedInfra** | changed paths match shared-infra globs (Cargo.toml, cli/args, config/**, migrations/**, …) | **Full suite** + announce (`scopeExecuted: "full"`) — justified |
| **EmptyChanges** | `packet.changes` empty (checked **before** stem query) | **Cheap plan:** Rust repos → fmt + clippy only (no nextest); non-Rust / undetected profile → zero steps (still exit 0 — do not invent cargo). Exit 0 if steps pass |
| **ScopedOk** | `test_mapping` yields stems for changed files | Existing 3-step scoped plan |
| **MappingRefuse** | empty/stale mapping, no stems, no DB, auto-index still cannot scope | **Refuse** — do **not** execute full. Exit ≠ 0. `scopeExecuted: "refused"`, `plan.refused=true`, empty steps |

**Escape hatches:**

| Hatch | Behavior |
|---|---|
| `--scope full` | Authoritative full suite (unchanged) |
| `--allow-full-fallback` | Restore 0061 mapping-path full execute + announce |
| `--auto-index` | Try refresh once; on still-cannot → **refuse** unless allow also set |
| Pre-push | `verify --scope fast` **without** allow — refuse blocks push until index fixed |

Human refuse first line is greppable:

```text
fast scope unavailable — <trigger>; refusing full suite (~5-8 min)
Next: ledgerful index --incremental
      ledgerful verify --scope fast --auto-index
      ledgerful verify --scope full
      ledgerful verify --scope fast --allow-full-fallback
```

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

## Why `--scope fast` does not parallelize fmt with clippy

The fast path runs `cargo fmt --all -- --check` (read-only) sequentially before
clippy. A mutating `cargo fmt` (without `--check`) rewrites `.rs` files in place,
which would cause `rustc`/clippy torn reads, spurious errors, and
incremental-cache invalidation. The ~2s potential saving is not worth the risk,
so fmt stays first and sequential.

## `--auto-index`

When `test_mapping` is empty or stale relative to the current `HEAD`,
`--scope fast` **refuses** by default (does not start a multi-minute full suite).
With `--auto-index`, verify refreshes the index for changed files first and
retries scoped selection once. On success → ScopedOk; if still cannot scope →
**refuse** unless `--allow-full-fallback` is also set. Opt-in because indexing
can add noticeable latency.

## Troubleshooting timeouts

If a step times out, the error now includes:

- The exact command that timed out.
- The elapsed time.
- A likely cause (cold build or feature-resolution mismatch).
- A next step: run `ledgerful index --incremental` or use `--scope full`
deliberately.
