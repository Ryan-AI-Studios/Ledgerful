# Breaking And Compatibility Notes

This document tracks dependency and project-level compatibility concerns for the current Ledgerful implementation.

## Summary Table

| Dependency / Area | Current Version / Status | Compatibility Notes | Impact |
| :--- | :--- | :--- | :--- |
| **thiserror** | 2.0.x | v2 removed some raw-identifier formatting behavior. | Low |
| **rusqlite** | 0.40.1 | Tight statement validation; unsigned integer SQL conversions are not default. | Moderate |
| **clap** | 4.6.1 | v4 stable; v5 remains future work. | Low |
| **miette** | 7.6.0 | v7 stable. | Low |
| **tree-sitter** | 0.26.8 | Parser family must be upgraded together. | Moderate |
| **gix** | 0.84.0 | High-churn pre-1.0 API. | Moderate |
| **notify-debouncer-full** | 0.7.0 | Watch behavior is platform-sensitive. | Low |
| **tower-lsp-server** | 0.23.0 | Optional daemon feature; uses `ls-types` and native async trait methods. | Moderate |
| **Ledgerful state** | Phase 2 schema | Adds symbol complexity, federated links, federated dependencies, and richer reports. | Moderate |

## Project-Level Changes

### CLI Additions

Phase 2 added:

- `hotspots`
- `federate export`
- `federate scan`
- `federate status`
- `daemon` behind `--features daemon`
- `impact --all-parents`
- `hotspots --json --dir --lang --all-parents`
- `verify --no-predict`
- `ask --narrative`
- reset recovery flags

The previously planned standalone `export-schema` command is implemented as `ledgerful federate export`.

### Verification Behavior

`verify` remains deterministic for identical repository/config/SQLite state. Prediction now uses:

- current repository import scanning
- latest impact packet data
- historical impact packets
- temporal couplings, recomputed when missing and possible

Prediction degradation appears in `latest-verify.json` under `prediction_warnings`.

### State And Reports

Phase 2 state is still repo-local under `.ledgerful/`. Important generated artifacts include:

- `.ledgerful/reports/latest-impact.json`
- `.ledgerful/reports/latest-verify.json`
- `.ledgerful/reports/fallback-impact.json`
- `.ledgerful/state/ledger.db`
- `.ledgerful/state/schema.json`

SQLite migrations add symbol complexity columns and federation tables. Older databases should be allowed to migrate forward through `rusqlite_migration`; Phase 1 binaries should not be expected to understand Phase 2 data.

## Dependency Details

### rusqlite 0.40.1

- Keep SQL operations single-statement.
- Store Rust `usize` and `u64` values as `i64` when writing SQLite rows.
- The daemon opens SQLite read-only and must not execute write-capable PRAGMAs from that connection.

### tower-lsp-server 0.23.0

- The daemon uses `tower-lsp-server` 0.23 and `ls-types`.
- `LanguageServer` methods are native async trait methods.
- Tokio features must include runtime, stdio, macros, and time support.

### tree-sitter 0.26.x

- Rust, TypeScript, and Python parser crates should be upgraded together.
- Re-run symbol, import/export, runtime usage, and complexity tests after parser changes.
- Complexity behavior is intentionally native; the `arborist-metrics` decision is documented in [architecture/arborist-metrics-decision.md](architecture/arborist-metrics-decision.md).

### gix 0.84.0

- `gix` is pre-1.0 and changes quickly.
- Re-check status, diff, first-parent traversal, and tree-diff assumptions after upgrades.
- Temporal tests include a real git fixture for first-parent behavior.

### Gemini CLI

- Ledgerful shells out to `gemini --model <selected-model> --prompt ""`.
- Missing Gemini CLI must produce: `Gemini CLI not found. Install Gemini CLI to enable narrative summaries.`
- Narrative mode uses one structured prompt rather than the generic question template.

## Validation After Changes

Run:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features
```

For daemon-specific changes, also run:

```powershell
cargo test --all-features --test daemon_lifecycle -- --test-threads=1
```
