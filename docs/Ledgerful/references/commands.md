# Ledgerful command sheet (agents)

Short flags only. Humans: `ledgerful --help`.

## Daily 5

| Command | Role |
|---|---|
| `ledgerful doctor --json` | Env readiness. Standing observe-signing warns: ack via `[doctor] acknowledged_codes` or `doctor --fix --yes` (pin only). Optional embed miss is fail-fast (`embed-unreachable`); not ERROR stderr. |
| `ledgerful change-context --json` | Default pre-edit packet (does not rewrite `latest-impact.json`). `--paths` is presence / blast-if-edit, not “public types modified”. |
| `ledgerful ledger status --compact` or `--json` | Pending / drift; names `workRoot` |
| `ledgerful search …` | Discovery (`--auto-index` when stale). Code FTS; unquoted multi-word OK. Not `ledger search`. |
| `ledgerful verify --scope fast` | Local gate |

Optional: `ledgerful session --json` — one-shot briefing (git/ledger/doctor/change-context/hotspots/`impactCache`). Does **not** replace Daily 5. Does not rewrite `latest-impact.json`. Human `session` is a 10-line summary, not JSON. Session `hotspots.files[]` `score` is 0–1; additive `displayScore` is ln (same units as `hotspots --json`). Human hotspot tables use **Display**, not a bare Score column.

## Provenance (not Daily 5)

| Command | Role |
|---|---|
| `ledgerful ledger search "<topic>" [--json]` | Committed-plan / TX FTS. **Quotes required** (clap `query` is one `String` token). Contrast: code `ledgerful search foo bar` stays unquoted multi-word. `--json` is a **bare array** (`Vec<LedgerEntry>`) — 0213 freeze; not a `schemaVersion` object. Empty `[]` is a valid FTS miss. Example: `ledgerful ledger search "0126" --json`. |

## `ledgerful tests`

Requires `-e` / `--entity` or a positional entity. Missing entity is a usage error (exit 2, empty stdout) — not an empty `mappings` envelope. Structural mapping includes in-file unit tests (`SAME_FILE`); still not LCOV.

## `ledger start --force` vs `ledger commit --force`

- **`ledger start --force`:** bypasses the **pending-entity collision lock** (0223). A PENDING TX whose entity overlaps the new `--entity` or any current dirty path otherwise refuses with `[Ledgerful] Collision:` (exit 2). Owner self-collision is intended — commit/abort first, or pass `--force`.
- **`ledger commit --force`:** bypasses the **verification gate**. Unrelated to the start collision lock. Do not treat these flags as interchangeable.

## `ledgerful status`

Pending/drift slice of `ledger status` (`--json` / `--compact` only). Bare `ledgerful status` is valid human pending/drift. Ledger-only flags (`--entity`, `--exit-code`, `--global`, `--all`, `--strict-observe-signal`, `--verify-signatures`, and `--repo`/`--reindex`/`--opt-in`/`--opt-out` which require `--global`) live on `ledgerful ledger status`. Daily 5 stays `ledger status --compact` / `--json`. Git hooks stay `ledger status --compact --exit-code`.

## `ledgerful ledger stack`

SQLite inspect of commit-path stack rules / validators / mappings — not verify auto-policy, not `.ledgerful/rules.toml`, not `policy check`. Empty next: `ledgerful ledger register rule` and `ledgerful ledger register validator` (no mapping CLI; no `config set`). `--json`: schemaVersion 1 object `kind: "ledgerStack"` (`empty` + `next`; snake_case item structs).

## `ledgerful audit`

Global TOP CHURNED FILES (human + `--json` `churn[]`) are unique **file paths**. ` (+N more)` labels collapse to the first path; directories and track slugs are dropped unless a TX `snapshot_id` expands via `changed_files`. `count` is distinct LOCAL ledger TXs. JSON is a bare `ProjectAuditReport` object (**no** `schemaVersion`). Velocity / CI trend / recent TXs are unchanged. Entity-scoped `audit <path>` is a different history view.

## `ledgerful security boundaries`

Operator `@id` → indexed endpoint (not a live PDP; daemon auth is Bearer). `--verbose` after this subcommand: URN / authorization-node table (does **not** enable tracing). `-v` after this subcommand is a usage error (clap same-id skip). Leading `ledgerful -v security boundaries` is logging, not the URN table. `--json`: unwrapped object + additive `pdp:false`, no `schemaVersion` (0207 freeze); `security impact` (0208) is a different command.
