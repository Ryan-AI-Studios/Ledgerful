# Ledgerful command sheet (agents)

Short flags only. Humans: `ledgerful --help`.

## Daily 5

| Command | Role |
|---|---|
| `ledgerful doctor --json` | Env readiness. `readyForPublish` is zero block findings. CLI always emits `readyForPublishScope` (`readyMeans` + `notRequiredForReady`); sidecar omits it. Standing observe-signing warns: ack via `[doctor] acknowledged_codes` or `doctor --fix --yes` (pin only). Optional embed miss is fail-fast (`embed-unreachable`); not ERROR stderr. |
| `ledgerful change-context --json` | Default pre-edit packet (does not rewrite `latest-impact.json`). `--paths` is presence / blast-if-edit, not “public types modified”. |
| `ledgerful ledger status --compact` or `--json` | Pending / drift; names `workRoot` |
| `ledgerful search …` | Discovery (`--auto-index` when stale). Code FTS; unquoted multi-word OK. Not `ledger search`. `--json` agents pin `path` + `line`; `content` is a preview (`/` paths). Hidden `search-trigrams` (0352) is a 3-character content-trigram AND path list (`--json` `kind: searchTrigrams`); not Daily 5. |
| `ledgerful verify --scope fast` | Local gate |

`ledgerful verify --dry-run` without `--scope` previews the pre-push **fast** plan (executed `verify` without flags stays **full**; `--scope full --dry-run` is the full preview). Quoted scoped nextest `-E 'test(…) + test(…)'` / `-E "…"` filtersets prepare as Direct argv; unquoted `\` skips the next character (POSIX); unescaped unquoted metas still refuse. `--json --dry-run` / `--json --health` / `--json --signatures` emit sibling `kind`s (`verifyDryRun` / `verifyHealth` / `verifySignatures`); executed `verify --json` still has no `kind`. Mixed diagnostic flags refuse (empty stdout). `verifySignatures.chain.head` is omit-empty when a stored head exists. Fully-clean auto-policy dry-run skips prediction.

Optional: `ledgerful session --json` — one-shot briefing (git/ledger/doctor/change-context/hotspots/`impactCache` + additive `configChecklist[]` of applicable gaps). Does **not** replace Daily 5. Does not rewrite `latest-impact.json`. Human `session` is a 10-line summary, not JSON. `session.next` stays structural (no `config set`). Session `git.dirtyPaths` omits `watch.ignore_patterns` (change-context parity); harness junctions are not product dirt. Session `hotspots.files[]` `score` is 0–1; additive `displayScore` is ln (same units as `hotspots --json`). Session vs CLI list windows differ — compare `provenance` before ranks. Human hotspot tables use **Display**, not a bare Score column. History walks honor `[hotspots] history_budget_secs` (default 45; `0` disables the clock). `hotspots` list/explain `--timeout` is an overall emit budget (0349). Review `--timeout` is an overall emit budget (0348). Unscoped `audit` / `ledger audit --timeout` is an overall emit budget (0350).

`ledgerful configure --json` — config-HITL catalog (`kind: configure`, same item shape as session `configChecklist[]`). No TUI / Confirm. After the human names apply-able ids: `configure --json --apply <id>[,<id>]` only for tokens that have `applyArg`. Refuse unknown / inapplicable / no-`applyArg` / apply-all. Human `configure` is a table. Cookie persist: `configure` (human and JSON) and `session --json` may write `cli-session.json` on gated/empty; human `session` does not; doctor never writes. Quote gated/empty once (`alreadyShown`); never `config set` unless the human named the id. Deploy-only `coverage.global` appears after `index --incremental` writes `deploy_manifests`.

`ledgerful config verify` — resolved settings with file/env/default provenance (`origin`/`location` on `--json` rows). Human unscoped names the four-section health catalog (Backend, Semantic, Ask, Gate) and clap next (`--verbose`, `config view --section`). Fail `--json` is `{success:false, errors, schemaVersion:1, kind:configVerify, ok:false}`. `config schema --json` adds `filePath` / `requiredness`; empty default ≠ required. `config view --json` stays a redacted Config dump (`--section`/`--key`); unscoped human Next names those flags. Human `config diff` prints sorted `file_paths`.

`ledgerful review <RANGE> --json` — range review packet (`kind: review`). Compose git files + range-correct impact counts + bounded ledger search. `ciEvidence` is target-bound: `status` is `ok` \| `unverified` \| `unavailable` (never `none`); omit-empty `boundHead` is the resolved lowercase head OID; unbound `verifyHistory` rows go to omit-empty `historical[]` (item `head` omit-empty). Empty `[review]` is valid. Conductor off unless `[review.conductor].root` is set. Use this for a git-range review instead of assembling change-context / `scan --impact`. Does not rewrite `latest-impact.json`. Not `scan --pr`. Not Daily 5.

## Provenance (not Daily 5)

| Command | Role |
|---|---|
| `ledgerful ledger search "<topic>" [--json]` | Committed-plan / TX FTS. **Quotes required** (clap `query` is one `String` token). Contrast: code `ledgerful search foo bar` stays unquoted multi-word. `--json` is a **bare array** (`Vec<LedgerEntry>`) — 0213 freeze; not a `schemaVersion` object. Key `related_tickets` frozen; **new** row values are ticket ids (or null), not staged file paths (files live on snapshot/`changed_files`). Additive omit-empty item keys `reason_kind` (`"trailer"` only; prose omits) and `risk_source` (`category` \| `explicit`). Empty `[]` is a valid FTS miss. Example: `ledgerful ledger search "0126" --json`. |
| `ledgerful ledger audit <entity> [--json]` | Entity/file history. `--json` is `{exact, related}` camelCase items. File paths union entity-string matches with exact `changed_files.path` (`matchBasis`: `entity` \| `changed_files`; related is `directory`). Additive omit-empty `reasonKind` / `riskSource` / `matchBasis`. Human `Reason:` prefixes `[trailer]` when the stored reason is trailer-only. No new clap flags. |
| `ledgerful ledger graph <tx> [--json] [--compact] [--layer …]` | Neighborhood `{exact, derived, heuristic}`. `--json` is that object (no `schemaVersion`); omit-empty `completeness` only when the 150/depth-2 cap hid a further hop or node. When completeness is present, stderr is `truncated: neighborhood capped at maxDepth 2 / maxNodes 150` (human and `--json`). `--compact` is human-only (refused with `--json`). `--layer exact\|derived\|heuristic` is repeatable. |
| `ledgerful ledger adr list [--json] [--status …]` | Human table ID/Entity/Status/Title/Created (Created display is `YYYY-MM-DD HH:MM:SS`; width-aware). `--json` is schemaVersion 1 `kind: "ledgerAdr"` (`id` is a number; `committedAt` full RFC3339). `--status` uses `AdrStatus` snake_case. Human chrome (marks, bullets, lock, arrows) follows `LEDGERFUL_TABLE_STYLE` like 0181 table borders; JSON stays UTF-8. |
| `ledgerful ledger adr export [-o PATH] [--days N]` | Writes MADR files (default dest `docs/adr`). `- **Status**` is lifecycle (`proposed`..`superseded`); `- **Change type**` is `ChangeType` Display (`CREATE`..`DELETE`). No `--json`. Always pass `--output` to a tempfile on EXEC. |
| `ledgerful ledger export-provenance [--limit N] [--offset N]` | Pretty **bare array** of committed entries, oldest first. `--limit`/`--offset` page that array; truncation is one stderr `truncated:` line. Not a `schemaVersion` wrap. |
| `ledgerful export head [--out PATH] [--stdout]` | Thin `ChainHead` checkpoint. `--stdout` / `-o -` is exact JSON bytes (0182). File-mode SUCCESS is a checkpoint write, not a verification. |
| `ledgerful export evidence [--profile soc2] [--out PATH] [--force] [--control ID]` | Writes a SOC2 zip (no `--json`). Default dest `ledgerful-soc2-evidence.zip` (demo: `ledgerful-DEMO-evidence.zip`). `gateModeDisclosure.chainContinuityStatus` `verified:` requires a gated walk plus a real stored signed head. Always pass `--out` to a tempfile on EXEC. |
| `ledgerful viz [--output PATH] [--limit N] [--depth N] [--entity ID] [--view graph\|services]` | Writes a standalone HTML file (no `--json`). Default dest `reports/graph.html` or `reports/services.html`. Always pass `--output` to a tempfile on EXEC. Both views inline vis-network 10.1.2 (no CDN). Graph stdout/`#evidence`: `source: Cozo nodes/edges`, `limit:`, `truncated:`, `communities:`, `asset:`. Services gated: `source: declared overlay; inference gated` (optional `persisted N not shown`); enabled: `source: persisted service_roots`. |

## `ledgerful ledger gc --dry-run`

Human preview only (`Gc` is not machine; no `--json`). Prints a labeled count for every requested class (`--stale` and/or `--orphans`) even when the first class is empty. Both flags use the same TTL PENDING selector (`started_at` older than `--ttl-hours`, default 72; days via `div_ceil(24)`). `--orphans` is **not** a git-commit scan. Protected sidecars (0074 `promote_failed` / `HEAD-matching orphan`) are listed, not candidates. Footer `Dry-run completed. No transactions were modified.` is unconditional. Dry-run always exits 0; write-path refuse `Err` is unchanged. Combined `--force` unions+dedupes before rollback. Never run without `--dry-run` on an operator repo.

## `ledgerful doctor --apply-hook-refresh --dry-run`

Isolated no-write preview (human path + block id; `--json` is `kind: hookRefreshPreview`). Always `executed: false` + `dryRun: true`. No health catalog, no `ensure_state_dir`, no `doctor-results.json` rewrite. `--json` without `--dry-run` stays rejected. Write-path apply (no `--dry-run`) still prints via `print_refresh_report` and continues into doctor. Third-party refuse tokens stay 0121. Never run without `--dry-run` on an operator repo.

## `ledgerful index --repair-metadata --dry-run --json`

schemaVersion 1 `kind: indexRepairPreview`. Always `executed: false` + `dryRun: true`. Age-only camelCase `assessment` (omit `emptyDiagnostics`; `staleFiles` / `unindexedFiles` are zeros, not drift). Locked sorted `proposed[]` (`force full index`, `replace metadata if successful`). Pretty JSON. Preview never writes. Human `--dry-run` copy unchanged. Executed `--yes` repair unchanged. Not `kind: indexCheck`. `--json` without `--dry-run` on repair is still silent (not this contract).

## `ledgerful web status` / `usage status` / `daemon`

`web status --json` (0328) is schemaVersion 1 PID-state evidence (`running` / `stalePid` / `reusedPid` / `noPidFile`) plus always-on `next`. Read-only: does not start, stop, or delete the PID file. Bare `web` still requires a subcommand.

`usage status` names compiled Cargo features vs telemetry consent. `usage show-payload` `features_enabled` stays ingest-frozen (`feature = "usage-metrics"`; not a default feature). Do not `usage enable` on operator home.

`daemon` (`feature = "daemon"`, not default) is an LSP on stdio — not a background Unix daemon. `--interval` is accepted and unused. Do not spawn it unless the owner asked for LSP.

## `ledgerful federate` / `sync cursor` / `sync log`

`federate status --json` (0327) is schemaVersion 1 live-peer provenance (`freshness` from sibling `schema.generated_at`, not required checks). Among Live dups, `lastScanned` is the later parsed RFC3339 instant. Bare `federate` stays human status (0179). `federate export --dry-run` is a non-writing human summary; `--json` is `kind: federateExportPreview` (bounded, HEAD + line when known). Write `federate export` / `--out` still emits full `schema.json` 1.1. Empty preview next is `ledgerful scan --impact`. `sync cursor --json` / `sync log --json` (`feature = "sync"`) name missing vs never-run vs unreadable; lag is always `unknown`; cursor may add `watermarkCompare`. `sync verify --json` is a closed `verdict` (`ok` agrees with exit). Do not `federate scan` or `sync init` unless the owner named that HITL.

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

SQLite inspect of commit-path stack rules / validators / mappings — not verify auto-policy, not `.ledgerful/rules.toml`, not `policy check`. Empty next: `ledgerful ledger register rule` and `ledgerful ledger register validator` (clap required flags on human/`--help` only; no mapping CLI; no `config set`). Human `TECH STACK RULES` prints `Blocking at start_change: yes|no` (two-factor: `ledger.enforcement_enabled` and `gate.mode=enforce`). `--json`: schemaVersion 1 object `kind: "ledgerStack"` (`empty` + `next`; snake_case item structs; omit-false `rulesNotEnforced`).

## `ledgerful ledger validator list`

Inventory of registered commit validators. Empty human: `0 registered`, consequence (none run at commit), next `ledgerful ledger register validator --help` — not an empty table. Populated table includes Args (`-` when CLI register stored the whole `-x` as executable). Footer `registered` / `enabled` only (doctor owns inspected/resolved). `--json`: bare array of `CommitValidator`; empty is `[]`.

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

`--timeout` is the overall list/explain emit budget (default 25s; `0` disables). Place `--timeout` / `--commits` **before** `explain`. History stays `[hotspots] history_budget_secs`. `hotspots explain --json` is `kind: hotspotExplanation`. `hotspots trend` / `budget` ignore parent `--timeout`.

Default list omits tests/examples/benches **and** `.md` **and** vendored `deps_src`/`vendor`/`third_party`. `--include tests` is the unfiltered `f×c` audit view (includes vendor). `--include docs` ranks markdown by frequency (`score` = `f_norm`, `complexity` 0). `--include vendor` restores vendored trees on the `f×c` list (tests + docs still omitted). `--entity` into a vendored subtree needs `--include vendor`. `--semantic` ignores `--include`. Pin JSON `score` (0–1), not `displayScore`. Item `complexity` is max across current-index symbols; C++ functions are body-scoped. `--include docs --snapshot` is refused (`hotspot_history` stores `f×c`); `--include vendor --snapshot` is allowed. MCP / `/api/hotspots` stay unfiltered. `hotspots budget` compares persisted `score` (0–1); empty history is `NO_DATA`; `--json` always names `dataset: hotspot_history` and, on an empty snapshot, omit-empty `emptyReason: noSnapshot` plus print-only `next: ledgerful hotspots --snapshot`; `--fail` needs `--threshold` or `[hotspots] budget_threshold`.

## `ledgerful audit`

Global TOP CHURNED FILES (human + `--json` `churn[]`) are unique **file paths**. ` (+N more)` labels collapse to the first path; directories and track slugs are dropped unless a TX `snapshot_id` expands via `changed_files`. `count` is distinct LOCAL ledger TXs. JSON is a bare `ProjectAuditReport` object (**no** `schemaVersion`). `--timeout` is the overall unscoped emit budget (default 25s; `0` disables Instant), not the 0308 history walk. Additive `completeness.scope=overall` when that deadline fires **or** cancel is set (0376). Human cancel copy is `Audit stopped: cancelled ({stage}).`; stderr `audit stopped: overall budget` is Budget-only. Velocity / CI trend / recent TXs stay page-sized by `--limit` / `--offset`. Entity-scoped `audit <path>` is a different history view and ignores `--timeout`.

## `ledgerful surfaces`

Read-only inventory of six advanced surfaces (ready / empty / gated). Alias: `tour`. Root `--help` statically omits `services`, `deploy`, and `observability` (live status is this inventory; help does not un-hide when a surface becomes ready). Those commands stay callable (`services --help`, `deploy --help`, `observability coverage` / `diff`). Do not `config set coverage.enabled=true` from help. `--json`: schemaVersion 1 object `kind: "surfaces"` (0185 freeze). Later emits in one CLI session collapse **human** Next for gated/empty notices to `Already shown this session.`; JSON keeps `next` and adds `sessionNotices.<id> = "already_shown"` (0300). Honor `already_shown` — do not re-ask HITL. Deploy empty `next` names `ledgerful index --incremental`, which writes `deploy_manifests`. `data-models` is ready only when default `data-models list` would show a product row; fixture-only is empty with next `ledgerful data-models list --include-fixtures`.

## `ledgerful data-models list` / `impact`

Product inventory of extracted persistence models (Rust `FromRow`/`Queryable`/`Insertable`, Go json-tagged structs, TypeScript `@Entity` or model-dir types, Python model-path classes). Default omits fixture/test-path rows; `--include-fixtures` restores. SQL migrations are **not** extracted. `--changed` is a dirty-path filter over indexed models, not field-level diffs (`fieldImpact: unsupported`). `--json`: schemaVersion 1; collections `models` / `impacted`; always `supportedExtractors` / `notWired`; omit-empty `next`.

## `ledgerful ci list`

Indexed CI-gate catalog (not a working-tree workflow diff). Alias: `diff`. Bare `ci` is the same inventory. Declared workflow jobs from local YAML — not GitHub required checks or live run status. `--json`: schemaVersion 1 object, collection `gates` (0207/0214 freeze) plus always-present `scope` and item `filePath` / `triggers` (0326). GHA `triggers` include event names at the standard two-space `on:` indent even when the mapping value is inline (`push: {branches: [main]}`); nested filter keys are not events; `[]` on a GHA file can also mean an unrecognized `on:` shape.

## `ledgerful services list`

Index topology inventory (gated empty when `coverage.enabled` is false). Alias: `diff`. `--preview` infers from the current index without persisting `service_name` or flipping coverage. `--full` lists up to 200 files per service. Gated empty names declared `[services]` when present. Keep `hide` on root `--help` (0289). `--json`: schemaVersion 1 object, collection `results`; gated empty keeps `emptyReason: "disabledByConfig"`; additive `inferenceState` / omit-empty `declared` / `--preview` `preview: true` / item `source`. Session-once (0300): second human empty in one cookie contains `Already shown this session.` and omits `config set`; `--preview` never writes the cookie. JSON `message` stays and may add `sessionNotices`. Honor `already_shown` — do not re-ask HITL.

## `ledgerful deploy impact`

Working-tree git-diff classify of deployment manifests (no impact orchestrator, no SQLite). Gated empty when `coverage.enabled` / `coverage.deploy.enabled` is false. Disabled copy names the switch and does **not** start with “No deployment impact detected.” (that sentence is enabled + no current-change hits). Keep `hide` on root `--help` (0289). `--timeout` is on **`impact`**, not parent `deploy` (`ledgerful deploy --timeout 5` is a clap error). Default overall emit budget is 25s (`0` disables; env `LEDGERFUL_DEPLOY_OVERALL_BUDGET_SECS` / `[coverage.deploy] overall_budget_secs`). `--json`: schemaVersion 1 object, collection `results`; always `defaultPatterns` / `classifiers`; gated empty keeps `emptyReason`/`message`; overall-stop completeness uses stage `deploy` and omits `emptyReason`. Stderr `deploy impact stopped: overall budget` is Budget-only (cancel is silent). Default globs: Dockerfile, docker-compose YAML, `*.tf`, `k8s/**/*.yaml`. Helm / CiWorkflow classify only with a user glob; `Unknown` is enum-parity only. Session-once (0300): second human empty contains `Already shown this session.` and omits `config set`; JSON `message` stays and may add `sessionNotices`. Honor `already_shown` — do not re-ask HITL.

## `ledgerful observability coverage`

Empty OpenSLO inventory (any empty reason). Supported inputs are repo-root `observability/` OpenSLO YAML (`kind: Service` plus matching `kind: SLO`, or `[services]` that creates the service node) after `index --analyze-graph`. `--preview` parses that directory without opening Cozo or writing state. A SLO-only DX1 template stays empty on persist. Coverage stays opt-in (`coverage.enabled` default false). Keep `hide` on root `--help` (0289). `--json`: schemaVersion 1 object, collection `results`; always `inputs` / `notWired: ["endpoints"]`; empty keeps `emptyReason`/`message`. Persist session-once (0300): second human contains `Already shown this session.` and skips the generate-template prompt; JSON `message` stays (including analyze-graph) and may add `sessionNotices.observability.empty`. `--preview` never writes the cookie. Honor `already_shown` — do not re-ask HITL.

## `ledgerful observability diff`

Working-tree OpenSLO change list (0146 git-status path match on `sourceFile`). Persist reads graph nodes (`slo` / `metric` / `alert` / `observability_signal`). `--preview` parses disk YAML without Cozo. Keep `hide` on root `--help` (0289). `--json`: schemaVersion 1 object `kind: observabilityDiff`, collection `changed`; always `unchanged_count` / `indexedCount` / `resultCount`. Item omit-empty `sourceFile`. Persist empty keeps 0215 taxonomy; preview empty does not recommend `index --analyze-graph`.

## `ledgerful gate mode`

Show prints `Gate mode: <observe|enforce>` then a warn/block + how-to-set line. Set with a positional: `ledgerful gate mode enforce`. Do not flip EXEC `gate.mode` unattended.

## `ledgerful security impact`

Unfiltered listing is a **declared Cedar inventory** (title `Security Policy Inventory`), not a changed-policy hit-list. `--changed` filters to the current diff (0208 CleanDiff when none match; title `Security Policy Impact`). Human header is `Policy | Source | Effect | Changed?` (Policy = resolved `@id` via annotations → raw `@id` → stored `cedar_id` → stored label; Source = `source_file`). Populated inventory/impact prints `declared Cedar coverage only — not runtime enforcement (daemon auth is Bearer).` immediately under the title (Ascii style uses `--` in place of `—`); omit that line on every empty arm including CleanDiff. `--json`: schemaVersion 1, collection `impacted`, `indexedCount`, plus `scope` / `authorization` / `coverage`. Coverage probe failure degrades JSON `coverage` (does not hide the inventory). Item `enforcement: "none"` and omit-empty `declaredAction`. Not a live PDP.

## `ledgerful security boundaries`

Operator `@id` → indexed endpoint (not a live PDP; daemon auth is Bearer). Human table columns: Policy / Source / Relation / Target / Enforcement (`none`). `--verbose` after this subcommand: URN / authorization-node table (does **not** enable tracing). `-v` after this subcommand is a usage error (clap same-id skip). Leading `ledgerful -v security boundaries` is logging, not the URN table. Populated human footer: `Declared coverage: {linkedEndpoints} unique endpoint targets of {N} cross-surface links; {indexedEndpoints} indexed endpoint nodes. Not all HTTP routes have a Cedar permit.` (`N` is all refined edges; `{linkedEndpoints}` counts endpoint targets only). `--json` **0359 freeze lift:** still the unwrapped object, plus additive `schemaVersion` 1 (was absent) / `kind: securityBoundaries` / `links[]` / `resultCount` (counts links) / bespoke `freshness` (`cozoGraph`; not `SurfaceFreshness`). Keep `pdp: false` + CLI-only `authorization` / `coverage`. `--json --verbose` is the same JSON as `--json`. REST `{meta, boundaries}` is unchanged. `security impact` (inventory / `--changed`) is a different command.
