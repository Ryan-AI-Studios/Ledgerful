# Verify Performance Guide

This document explains the speed levers available for `ledgerful verify` and
their safe combinations.

## Fast vs full scope boundary

- `ledgerful verify --scope full` is the **authoritative gate**. It always runs
the complete suite (fmt, clippy, tests, doctests, slow/compile-fail tiers). CI
uses this scope.
- `ledgerful verify --scope fast` is the **local convenience gate**. It uses
`test_mapping` to run only the tests that cover changed files, falling back to
the full suite when shared infrastructure is touched or the mapping is empty.
The pre-push hook uses this scope.

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
# Install sccache v0.16.0+ (Windows path fixes ship in v0.16.0)
cargo install sccache --version ^0.16

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
`--scope fast` normally falls back to the full suite with an announcement. With
`--auto-index`, verify refreshes the index for changed files first and retries
scoped selection once. This is opt-in because indexing can add noticeable
latency and should not surprise the user by default.

## Troubleshooting timeouts

If a step times out, the error now includes:

- The exact command that timed out.
- The elapsed time.
- A likely cause (cold build or feature-resolution mismatch).
- A next step: run `ledgerful index --incremental` or use `--scope full`
deliberately.
