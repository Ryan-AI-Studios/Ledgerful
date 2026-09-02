# Ledgerful command sheet (agents)

Short flags only. Humans: `ledgerful --help`.

## Daily 5

| Command | Role |
|---|---|
| `ledgerful doctor --json` | Env readiness |
| `ledgerful change-context --json` | Default pre-edit packet (does not rewrite `latest-impact.json`) |
| `ledgerful ledger status --compact` or `--json` | Pending / drift; names `workRoot` |
| `ledgerful search …` | Discovery (`--auto-index` when stale) |
| `ledgerful verify --scope fast` | Local gate |

Optional: `ledgerful session --json` — one-shot briefing (git/ledger/doctor/change-context/hotspots/`impactCache`). Does **not** replace Daily 5. Does not rewrite `latest-impact.json`. Human `session` is a 10-line summary, not JSON.

## `ledger start --force` vs `ledger commit --force`

- **`ledger start --force`:** bypasses the **pending-entity collision lock** (0223). A PENDING TX whose entity overlaps the new `--entity` or any current dirty path otherwise refuses with `[Ledgerful] Collision:` (exit 2). Owner self-collision is intended — commit/abort first, or pass `--force`.
- **`ledger commit --force`:** bypasses the **verification gate**. Unrelated to the start collision lock. Do not treat these flags as interchangeable.
