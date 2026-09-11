# Ledgerful command sheet (agents)

Short flags only. Humans: `ledgerful --help`.

## Daily 5

| Command | Role |
|---|---|
| `ledgerful doctor --json` | Env readiness. Standing observe-signing warns: ack via `[doctor] acknowledged_codes` or `doctor --fix --yes` (pin only). Optional embed miss is fail-fast (`embed-unreachable`); not ERROR stderr. |
| `ledgerful change-context --json` | Default pre-edit packet (does not rewrite `latest-impact.json`). `--paths` is presence / blast-if-edit, not “public types modified”. |
| `ledgerful ledger status --compact` or `--json` | Pending / drift; names `workRoot` |
| `ledgerful search …` | Discovery (`--auto-index` when stale). Code FTS; unquoted multi-word OK. Not `ledger search`. `--json` agents pin `path` + `line`; `content` is a preview (`/` paths). |
| `ledgerful verify --scope fast` | Local gate |

`ledgerful verify --dry-run` without `--scope` previews the pre-push **fast** plan (executed `verify` without flags stays **full**; `--scope full --dry-run` is the full preview; `--json --dry-run` is refused). Fully-clean auto-policy dry-run skips prediction.

Optional: `ledgerful session --json` — one-shot briefing (git/ledger/doctor/change-context/hotspots/`impactCache` + additive `configChecklist[]` of applicable gaps). Does **not** replace Daily 5. Does not rewrite `latest-impact.json`. Human `session` is a 10-line summary, not JSON. `session.next` stays structural (no `config set`). Session `git.dirtyPaths` omits `watch.ignore_patterns` (change-context parity); harness junctions are not product dirt. Session `hotspots.files[]` `score` is 0–1; additive `displayScore` is ln (same units as `hotspots --json`). Session vs CLI list windows differ — compare `provenance` before ranks. Human hotspot tables use **Display**, not a bare Score column. History walks honor `[hotspots] history_budget_secs` (default 45; `--timeout` on `hotspots` / `audit` / `review`; `0` disables the clock).

`ledgerful configure --json` — config-HITL catalog (`kind: configure`, same item shape as session `configChecklist[]`). No TUI / Confirm. After the human names apply-able ids: `configure --json --apply <id>[,<id>]` only for tokens that have `applyArg`. Refuse unknown / inapplicable / no-`applyArg` / apply-all. Human `configure` is a table. Cookie persist: `configure` (human and JSON) and `session --json` may write `cli-session.json` on gated/empty; human `session` does not; doctor never writes. Quote gated/empty once (`alreadyShown`); never `config set` unless the human named the id.

`ledgerful review <RANGE> --json` — range review packet (`kind: review`). Compose git files + range-correct impact counts + bounded ledger search. Empty `[review]` is valid. Conductor off unless `[review.conductor].root` is set. Use this for a git-range review instead of assembling change-context / `scan --impact`. Does not rewrite `latest-impact.json`. Not `scan --pr`. Not Daily 5.

## Provenance (not Daily 5)

| Command | Role |
|---|---|
| `ledgerful ledger search "<topic>" [--json]` | Committed-plan / TX FTS. **Quotes required** (clap `query` is one `String` token). Contrast: code `ledgerful search foo bar` stays unquoted multi-word. `--json` is a **bare array** (`Vec<LedgerEntry>`) — 0213 freeze; not a `schemaVersion` object. Key `related_tickets` frozen; **new** row values are ticket ids (or null), not staged file paths (files live on snapshot/`changed_files`). Empty `[]` is a valid FTS miss. Example: `ledgerful ledger search "0126" --json`. |

## `ledgerful tests`

Requires `-e` / `--entity` or a positional entity. Missing entity is a usage error (exit 2, empty stdout) — not an empty `mappings` envelope. Structural mapping includes in-file unit tests (`SAME_FILE`); still not LCOV.

## `ledger start --force` vs `ledger commit --force`

- **`ledger start --force`:** bypasses the **pending-entity collision lock** (0223). A PENDING TX whose entity overlaps the new `--entity` or any current dirty path otherwise refuses with `[Ledgerful] Collision:` (exit 2). Owner self-collision is intended — commit/abort first, or pass `--force`.
- **`ledger commit --force`:** bypasses the **verification gate**. Unrelated to the start collision lock. Do not treat these flags as interchangeable.

## `ledgerful status`

Pending/drift slice of `ledger status` (`--json` / `--compact` only). Bare `ledgerful status` is valid human pending/drift. Ledger-only flags (`--entity`, `--exit-code`, `--global`, `--all`, `--strict-observe-signal`, `--verify-signatures`, and `--repo`/`--reindex`/`--opt-in`/`--opt-out` which require `--global`) live on `ledgerful ledger status`. Daily 5 stays `ledger status --compact` / `--json`. Git hooks stay `ledger status --compact --exit-code`.

## `ledgerful ledger stack`

SQLite inspect of commit-path stack rules / validators / mappings — not verify auto-policy, not `.ledgerful/rules.toml`, not `policy check`. Empty next: `ledgerful ledger register rule` and `ledgerful ledger register validator` (no mapping CLI; no `config set`). `--json`: schemaVersion 1 object `kind: "ledgerStack"` (`empty` + `next`; snake_case item structs).

## `ledgerful policy check`

Evaluates declared `.ledgerful/policy.toml` (or synthesized defaults). Machine
flag is `--format json` (not `--json`). `passed` is no-violations. Synthesized
+ idle (no bound verify) is human `IDLE (synthesized; not a merge gate)` with
JSON `idle: true`; it is not a merge-gate pass. Observe still exit 0.

## `ledgerful hotspots`

Default list omits tests/examples/benches **and** `.md` **and** vendored `deps_src`/`vendor`/`third_party`. `--include tests` is the unfiltered `f×c` audit view (includes vendor). `--include docs` ranks markdown by frequency (`score` = `f_norm`, `complexity` 0). `--include vendor` restores vendored trees on the `f×c` list (tests + docs still omitted). `--entity` into a vendored subtree needs `--include vendor`. `--semantic` ignores `--include`. Pin JSON `score` (0–1), not `displayScore`. Item `complexity` is max across current-index symbols; C++ functions are body-scoped. `--include docs --snapshot` is refused (`hotspot_history` stores `f×c`); `--include vendor --snapshot` is allowed. MCP / `/api/hotspots` stay unfiltered. `hotspots budget` compares persisted `score` (0–1); empty history is `NO_DATA`; `--fail` needs `--threshold` or `[hotspots] budget_threshold`.

## `ledgerful audit`

Global TOP CHURNED FILES (human + `--json` `churn[]`) are unique **file paths**. ` (+N more)` labels collapse to the first path; directories and track slugs are dropped unless a TX `snapshot_id` expands via `changed_files`. `count` is distinct LOCAL ledger TXs. JSON is a bare `ProjectAuditReport` object (**no** `schemaVersion`). Velocity / CI trend / recent TXs are unchanged. Entity-scoped `audit <path>` is a different history view.

## `ledgerful surfaces`

Read-only inventory of six advanced surfaces (ready / empty / gated). Alias: `tour`. Root `--help` statically omits `services`, `deploy`, and `observability` (live status is this inventory; help does not un-hide when a surface becomes ready). Those commands stay callable (`services --help`, `deploy --help`, `observability coverage` / `diff`). Do not `config set coverage.enabled=true` from help. `--json`: schemaVersion 1 object `kind: "surfaces"` (0185 freeze). Later emits in one CLI session collapse **human** Next for gated/empty notices to `Already shown this session.`; JSON keeps `next` and adds `sessionNotices.<id> = "already_shown"` (0300). Honor `already_shown` — do not re-ask HITL.

## `ledgerful ci list`

Indexed CI-gate catalog (not a working-tree workflow diff). Alias: `diff`. Bare `ci` is the same inventory. `--json`: schemaVersion 1 object, collection `gates` (0207/0214 freeze).

## `ledgerful services list`

Index topology inventory (gated empty when `coverage.enabled` is false). Alias: `diff`. Keep `hide` on root `--help` (0289). `--json`: schemaVersion 1 object, collection `results`; gated empty keeps `emptyReason: "disabledByConfig"`. Session-once (0300): second human empty in one cookie contains `Already shown this session.` and omits `config set`; JSON `message` stays and may add `sessionNotices`. Honor `already_shown` — do not re-ask HITL.

## `ledgerful deploy impact`

Gated empty when `coverage.enabled` / `coverage.deploy.enabled` is false. Keep `hide` on root `--help` (0289). Flag is on `impact`, not parent `deploy`. `--json`: schemaVersion 1 object, collection `results`; gated empty keeps `emptyReason`/`message`. Session-once (0300): second human empty contains `Already shown this session.` and omits `config set`; JSON `message` stays and may add `sessionNotices`. Honor `already_shown` — do not re-ask HITL.

## `ledgerful observability coverage`

Empty OpenSLO inventory (any empty reason). Keep `hide` on root `--help` (0289). `--json`: schemaVersion 1 object, collection `results`; empty keeps `emptyReason`/`message`. Session-once (0300): second human contains `Already shown this session.` and skips the generate-template prompt; JSON `message` stays (including analyze-graph) and may add `sessionNotices.observability.empty`. `observability diff` does **not** participate. Honor `already_shown` — do not re-ask HITL.

## `ledgerful gate mode`

Show prints `Gate mode: <observe|enforce>` then a warn/block + how-to-set line. Set with a positional: `ledgerful gate mode enforce`. Do not flip EXEC `gate.mode` unattended.

## `ledgerful security boundaries`

Operator `@id` → indexed endpoint (not a live PDP; daemon auth is Bearer). `--verbose` after this subcommand: URN / authorization-node table (does **not** enable tracing). `-v` after this subcommand is a usage error (clap same-id skip). Leading `ledgerful -v security boundaries` is logging, not the URN table. `--json`: unwrapped object + additive `pdp:false`, no `schemaVersion` (0207 freeze); `security impact` (0208) is a different command.
