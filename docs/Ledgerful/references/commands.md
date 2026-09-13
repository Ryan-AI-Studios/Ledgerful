# Ledgerful command sheet (agents)

Short flags only. Humans: `ledgerful --help`.

## Daily 5

| Command | Role |
|---|---|
| `ledgerful doctor --json` | Env readiness. `readyForPublish` is zero block findings. CLI always emits `readyForPublishScope` (`readyMeans` + `notRequiredForReady`); sidecar omits it. Standing observe-signing warns: ack via `[doctor] acknowledged_codes` or `doctor --fix --yes` (pin only). Optional embed miss is fail-fast (`embed-unreachable`); not ERROR stderr. |
| `ledgerful change-context --json` | Default pre-edit packet (does not rewrite `latest-impact.json`). `--paths` is presence / blast-if-edit, not “public types modified”. |
| `ledgerful ledger status --compact` or `--json` | Pending / drift; names `workRoot` |
| `ledgerful search …` | Discovery (`--auto-index` when stale). Code FTS; unquoted multi-word OK. Not `ledger search`. `--json` agents pin `path` + `line`; `content` is a preview (`/` paths). |
| `ledgerful verify --scope fast` | Local gate |

`ledgerful verify --dry-run` without `--scope` previews the pre-push **fast** plan (executed `verify` without flags stays **full**; `--scope full --dry-run` is the full preview). Quoted scoped nextest `-E 'test(…) + test(…)'` / `-E "…"` filtersets prepare as Direct argv; unquoted shell metas still refuse. `--json --dry-run` / `--json --health` / `--json --signatures` emit sibling `kind`s (`verifyDryRun` / `verifyHealth` / `verifySignatures`); executed `verify --json` still has no `kind`. Mixed diagnostic flags refuse (empty stdout). `verifySignatures.chain.head` is omit-empty when a stored head exists. Fully-clean auto-policy dry-run skips prediction.

Optional: `ledgerful session --json` — one-shot briefing (git/ledger/doctor/change-context/hotspots/`impactCache` + additive `configChecklist[]` of applicable gaps). Does **not** replace Daily 5. Does not rewrite `latest-impact.json`. Human `session` is a 10-line summary, not JSON. `session.next` stays structural (no `config set`). Session `git.dirtyPaths` omits `watch.ignore_patterns` (change-context parity); harness junctions are not product dirt. Session `hotspots.files[]` `score` is 0–1; additive `displayScore` is ln (same units as `hotspots --json`). Session vs CLI list windows differ — compare `provenance` before ranks. Human hotspot tables use **Display**, not a bare Score column. History walks honor `[hotspots] history_budget_secs` (default 45; `--timeout` on `hotspots` / `audit` / `review`; `0` disables the clock).

`ledgerful configure --json` — config-HITL catalog (`kind: configure`, same item shape as session `configChecklist[]`). No TUI / Confirm. After the human names apply-able ids: `configure --json --apply <id>[,<id>]` only for tokens that have `applyArg`. Refuse unknown / inapplicable / no-`applyArg` / apply-all. Human `configure` is a table. Cookie persist: `configure` (human and JSON) and `session --json` may write `cli-session.json` on gated/empty; human `session` does not; doctor never writes. Quote gated/empty once (`alreadyShown`); never `config set` unless the human named the id.

`ledgerful config verify` — resolved settings with file/env/default provenance (`origin`/`location` on `--json` rows). Fail `--json` is `{success:false, errors, schemaVersion:1, kind:configVerify, ok:false}`. `config schema --json` adds `filePath` / `requiredness`; empty default ≠ required. `config view --json` stays a redacted Config dump (`--section`/`--key`). Human `config diff` prints sorted `file_paths`.

`ledgerful review <RANGE> --json` — range review packet (`kind: review`). Compose git files + range-correct impact counts + bounded ledger search. `ciEvidence` is target-bound: `status` is `ok` \| `unverified` \| `unavailable` (never `none`); omit-empty `boundHead` is the resolved lowercase head OID; unbound `verifyHistory` rows go to omit-empty `historical[]` (item `head` omit-empty). Empty `[review]` is valid. Conductor off unless `[review.conductor].root` is set. Use this for a git-range review instead of assembling change-context / `scan --impact`. Does not rewrite `latest-impact.json`. Not `scan --pr`. Not Daily 5.

## Provenance (not Daily 5)

| Command | Role |
|---|---|
| `ledgerful ledger search "<topic>" [--json]` | Committed-plan / TX FTS. **Quotes required** (clap `query` is one `String` token). Contrast: code `ledgerful search foo bar` stays unquoted multi-word. `--json` is a **bare array** (`Vec<LedgerEntry>`) — 0213 freeze; not a `schemaVersion` object. Key `related_tickets` frozen; **new** row values are ticket ids (or null), not staged file paths (files live on snapshot/`changed_files`). Additive omit-empty item keys `reason_kind` (`"trailer"` only; prose omits) and `risk_source` (`category` \| `explicit`). Empty `[]` is a valid FTS miss. Example: `ledgerful ledger search "0126" --json`. |
| `ledgerful ledger audit <entity> [--json]` | Entity/file history. `--json` is `{exact, related}` camelCase items. File paths union entity-string matches with exact `changed_files.path` (`matchBasis`: `entity` \| `changed_files`; related is `directory`). Additive omit-empty `reasonKind` / `riskSource` / `matchBasis`. Human `Reason:` prefixes `[trailer]` when the stored reason is trailer-only. No new clap flags. |
| `ledgerful ledger graph <tx> [--json] [--compact] [--layer …]` | Neighborhood `{exact, derived, heuristic}`. `--json` is that object (no `schemaVersion`); omit-empty `completeness` only when the 150/depth-2 cap hid a further hop or node. `--compact` is human-only (refused with `--json`). `--layer exact\|derived\|heuristic` is repeatable. |
| `ledgerful ledger adr list [--json] [--status …]` | Human table ID/Entity/Status/Title/Created (Created display is `YYYY-MM-DD HH:MM:SS`; width-aware). `--json` is schemaVersion 1 `kind: "ledgerAdr"` (`id` is a number; `committedAt` full RFC3339). `--status` uses `AdrStatus` snake_case. Human chrome (marks, bullets, lock, arrows) follows `LEDGERFUL_TABLE_STYLE` like 0181 table borders; JSON stays UTF-8. |
| `ledgerful ledger export-provenance [--limit N] [--offset N]` | Pretty **bare array** of committed entries, oldest first. `--limit`/`--offset` page that array; truncation is one stderr `truncated:` line. Not a `schemaVersion` wrap. |
| `ledgerful export head [--out PATH] [--stdout]` | Thin `ChainHead` checkpoint. `--stdout` / `-o -` is exact JSON bytes (0182). File-mode SUCCESS is a checkpoint write, not a verification. |

## `ledgerful web status` / `usage status` / `daemon`

`web status --json` (0328) is schemaVersion 1 PID-state evidence (`running` / `stalePid` / `reusedPid` / `noPidFile`) plus always-on `next`. Read-only: does not start, stop, or delete the PID file. Bare `web` still requires a subcommand.

`usage status` names compiled Cargo features vs telemetry consent. `usage show-payload` `features_enabled` stays ingest-frozen (`feature = "usage-metrics"`; not a default feature). Do not `usage enable` on operator home.

`daemon` (`feature = "daemon"`, not default) is an LSP on stdio — not a background Unix daemon. `--interval` is accepted and unused. Do not spawn it unless the owner asked for LSP.

## `ledgerful federate` / `sync cursor` / `sync log`

`federate status --json` (0327) is schemaVersion 1 live-peer provenance (`freshness` from sibling `schema.generated_at`, not required checks). Bare `federate` stays human status (0179). `sync cursor --json` / `sync log --json` (`feature = "sync"`) name missing vs never-run vs unreadable; lag is always `unknown`. Do not `federate scan` or `sync init` unless the owner named that HITL.

## `ledgerful tests`

Requires `-e` / `--entity` or a positional entity. Missing entity is a usage error (exit 2, empty stdout) — not an empty `mappings` envelope. Structural mapping includes in-file unit tests (`SAME_FILE`); still not LCOV.

## `ledgerful symbols`

Scoped index inventory (not search). `--path` is a **prefix**. JSON `schemaVersion` 1. `qualifiedName` omits when empty, absent, or equal to `name` (not a vault key; identity is `(path, name, kind)` + `line`). Human prints stored `Type.method` when it differs. Class is populated (C++/TS); Interface is populated (TS/Go; this index may be 0). Rust inherent impl methods are Function + `Type.method`; trait methods are Method. Default limit 200 / max 5000. See `docs/agent-output-contract.md`.

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
`--format json` always includes `evaluation` (checked / idle / off / skipped
counts + `rules[]`). Live `rulesDeclared` is 5; serde default zeros + empty
`rules[]` is a legacy/unknown sentinel. Human path does not print those
counts. `ledger validator doctor` is human-only: path-probe counts
(`registered` / `enabled` / `inspected` / `resolved`); empty catalog is
not a health pass.

## `ledgerful hotspots`

Default list omits tests/examples/benches **and** `.md` **and** vendored `deps_src`/`vendor`/`third_party`. `--include tests` is the unfiltered `f×c` audit view (includes vendor). `--include docs` ranks markdown by frequency (`score` = `f_norm`, `complexity` 0). `--include vendor` restores vendored trees on the `f×c` list (tests + docs still omitted). `--entity` into a vendored subtree needs `--include vendor`. `--semantic` ignores `--include`. Pin JSON `score` (0–1), not `displayScore`. Item `complexity` is max across current-index symbols; C++ functions are body-scoped. `--include docs --snapshot` is refused (`hotspot_history` stores `f×c`); `--include vendor --snapshot` is allowed. MCP / `/api/hotspots` stay unfiltered. `hotspots budget` compares persisted `score` (0–1); empty history is `NO_DATA`; `--fail` needs `--threshold` or `[hotspots] budget_threshold`.

## `ledgerful audit`

Global TOP CHURNED FILES (human + `--json` `churn[]`) are unique **file paths**. ` (+N more)` labels collapse to the first path; directories and track slugs are dropped unless a TX `snapshot_id` expands via `changed_files`. `count` is distinct LOCAL ledger TXs. JSON is a bare `ProjectAuditReport` object (**no** `schemaVersion`). Velocity / CI trend / recent TXs are unchanged. Entity-scoped `audit <path>` is a different history view.

## `ledgerful surfaces`

Read-only inventory of six advanced surfaces (ready / empty / gated). Alias: `tour`. Root `--help` statically omits `services`, `deploy`, and `observability` (live status is this inventory; help does not un-hide when a surface becomes ready). Those commands stay callable (`services --help`, `deploy --help`, `observability coverage` / `diff`). Do not `config set coverage.enabled=true` from help. `--json`: schemaVersion 1 object `kind: "surfaces"` (0185 freeze). Later emits in one CLI session collapse **human** Next for gated/empty notices to `Already shown this session.`; JSON keeps `next` and adds `sessionNotices.<id> = "already_shown"` (0300). Honor `already_shown` — do not re-ask HITL.

## `ledgerful ci list`

Indexed CI-gate catalog (not a working-tree workflow diff). Alias: `diff`. Bare `ci` is the same inventory. Declared workflow jobs from local YAML — not GitHub required checks or live run status. `--json`: schemaVersion 1 object, collection `gates` (0207/0214 freeze) plus always-present `scope` and item `filePath` / `triggers` (0326).

## `ledgerful services list`

Index topology inventory (gated empty when `coverage.enabled` is false). Alias: `diff`. Keep `hide` on root `--help` (0289). `--json`: schemaVersion 1 object, collection `results`; gated empty keeps `emptyReason: "disabledByConfig"`. Session-once (0300): second human empty in one cookie contains `Already shown this session.` and omits `config set`; JSON `message` stays and may add `sessionNotices`. Honor `already_shown` — do not re-ask HITL.

## `ledgerful deploy impact`

Gated empty when `coverage.enabled` / `coverage.deploy.enabled` is false. Disabled copy names the switch and does **not** start with “No deployment impact detected.” (that sentence is enabled + no current-change hits). Keep `hide` on root `--help` (0289). Flag is on `impact`, not parent `deploy`. `--json`: schemaVersion 1 object, collection `results`; gated empty keeps `emptyReason`/`message`. Session-once (0300): second human empty contains `Already shown this session.` and omits `config set`; JSON `message` stays and may add `sessionNotices`. Honor `already_shown` — do not re-ask HITL.

## `ledgerful observability coverage`

Empty OpenSLO inventory (any empty reason). Supported inputs are repo-root `observability/` OpenSLO YAML (`kind: Service` plus matching `kind: SLO`, or `[services]` that creates the service node) after `index --analyze-graph`. A SLO-only DX1 template stays empty. Coverage stays opt-in (`coverage.enabled` default false). Keep `hide` on root `--help` (0289). `--json`: schemaVersion 1 object, collection `results`; empty keeps `emptyReason`/`message`. Session-once (0300): second human contains `Already shown this session.` and skips the generate-template prompt; JSON `message` stays (including analyze-graph) and may add `sessionNotices.observability.empty`. `observability diff` does **not** participate. Honor `already_shown` — do not re-ask HITL.

## `ledgerful gate mode`

Show prints `Gate mode: <observe|enforce>` then a warn/block + how-to-set line. Set with a positional: `ledgerful gate mode enforce`. Do not flip EXEC `gate.mode` unattended.

## `ledgerful security impact`

Unfiltered listing is a **declared Cedar inventory** (title `Security Policy Inventory`), not a changed-policy hit-list. `--changed` filters to the current diff (0208 CleanDiff when none match; title `Security Policy Impact`). Human header is `Policy | Source | Effect | Changed?` (Policy = resolved `@id`; Source = `source_file`). Populated inventory/impact prints `declared Cedar coverage only — not runtime enforcement (daemon auth is Bearer).` immediately under the title (Ascii style uses `--` in place of `—`); omit that line on every empty arm including CleanDiff. `--json`: schemaVersion 1, collection `impacted`, `indexedCount`, plus `scope` / `authorization` / `coverage`. Item `enforcement: "none"` and omit-empty `declaredAction`. Not a live PDP.

## `ledgerful security boundaries`

Operator `@id` → indexed endpoint (not a live PDP; daemon auth is Bearer). `--verbose` after this subcommand: URN / authorization-node table (does **not** enable tracing). `-v` after this subcommand is a usage error (clap same-id skip). Leading `ledgerful -v security boundaries` is logging, not the URN table. Populated human footer: `Declared coverage: {linkedEndpoints} unique endpoint targets of {N} cross-surface links; {indexedEndpoints} indexed endpoint nodes. Not all HTTP routes have a Cedar permit.` (`N` is all refined edges; `{linkedEndpoints}` counts endpoint targets only). `--json`: unwrapped object + additive `pdp:false` + CLI-only `authorization` / `coverage`, no `schemaVersion` (0207 freeze). REST `{meta, boundaries}` is unchanged. `security impact` (inventory / `--changed`) is a different command.
