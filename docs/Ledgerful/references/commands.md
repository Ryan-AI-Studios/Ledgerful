# Ledgerful command sheet (agents)

Short flags only. Humans: `ledgerful --help`.

## Daily 5

| Command | Role |
|---|---|
| `ledgerful doctor --json` | Env readiness. Standing observe-signing warns: ack via `[doctor] acknowledged_codes` or `doctor --fix --yes` (pin only). |
| `ledgerful change-context --json` | Default pre-edit packet (does not rewrite `latest-impact.json`) |
| `ledgerful ledger status --compact` or `--json` | Pending / drift; names `workRoot` |
| `ledgerful search …` | Discovery (`--auto-index` when stale). Code FTS; unquoted multi-word OK. Not `ledger search`. |
| `ledgerful verify --scope fast` | Local gate |

Optional: `ledgerful session --json` — one-shot briefing (git/ledger/doctor/change-context/hotspots/`impactCache`). Does **not** replace Daily 5. Does not rewrite `latest-impact.json`. Human `session` is a 10-line summary, not JSON.

## Provenance (not Daily 5)

| Command | Role |
|---|---|
| `ledgerful ledger search "<topic>" [--json]` | Committed-plan / TX FTS. **Quotes required** (clap `query` is one `String` token). Contrast: code `ledgerful search foo bar` stays unquoted multi-word. `--json` is a **bare array** (`Vec<LedgerEntry>`) — 0213 freeze; not a `schemaVersion` object. Empty `[]` is a valid FTS miss. Example: `ledgerful ledger search "0126" --json`. |

## `ledger start --force` vs `ledger commit --force`

- **`ledger start --force`:** bypasses the **pending-entity collision lock** (0223). A PENDING TX whose entity overlaps the new `--entity` or any current dirty path otherwise refuses with `[Ledgerful] Collision:` (exit 2). Owner self-collision is intended — commit/abort first, or pass `--force`.
- **`ledger commit --force`:** bypasses the **verification gate**. Unrelated to the start collision lock. Do not treat these flags as interchangeable.
