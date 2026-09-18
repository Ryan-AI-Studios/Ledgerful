# Agent CLI output contract

This document is the machine-facing contract for non-interactive consumers
(agents, CI wrappers, PowerShell scripts) that parse Ledgerful CLI output.

**Authority for streams:** [`operator-surface-policy.md`](operator-surface-policy.md)
§3 ("Stdout is the contract"). This page does not restate that policy; it
names which flags select which streams and documents the versioned JSON
payloads.

**Track:** 0093-AgentCliOutputContract; extended by **0136** (search envelope),
**0149** (uniform machine JSON: top-level `status`, `dead-code`, index-check
purity, scan incomplete-flag tips), **0180** (`scan --json`/`--out` gitScan
envelope without mandatory `--impact`; escalate remains `--impact --json`),
**0207** (populated list `--json` is a schemaVersion-1 object; `index --check --json`
camelCase CLI DTO).

---

## Agent-relevant inventory (`--json` purity)

High-traffic surfaces agents parse. **Pure success** means stdout is the
machine payload only and **stderr is empty** on the happy path (no human Info
banners, spinners, or SUCCESS lines). Fail paths may still print diagnostics
on stderr. Doctor / schema / security / ADR `--json` remain UTF-8 **data**;
human chrome (marks, bullets, lock, arrows) is not part of the machine
contract and follows `LEDGERFUL_TABLE_STYLE`. Purity inventory unchanged
(JSON still stdout-only).

| Command | Has `--json` | Pure success stderr | Notes |
|---|---|---|---|
| `doctor --json --apply-hook-refresh --dry-run` | yes (0373) | yes (exit 0) | schemaVersion 1 object `kind: "hookRefreshPreview"`. Always `executed: false` + `dryRun: true`. Omit `ok`. Omit `emptyDiagnostics`. CamelCase `wouldRefresh[]` / `alreadyCurrent[]` items `{label, path}`; `skippedUnknown[]` `{label, reason}` (`reason` is a single-line token, no snippet). Omit-empty `refused` / `discoveryNotes` / `hooksDir`. `hooksDir` and item `path` are repo-relative `/` (never `\`). Pretty JSON + one trailing newline. Isolated preview: no health `findings` / `summary`, no writes, no `.ledgerful` create. `--json --apply-hook-refresh` without `--dry-run` stays rejected. Write-path apply stays human. CLI-only; no MCP. |
| `doctor --json` | yes | yes | schemaVersion 1 findings; additive `environment.githubLatest` (0205); additive per-finding `sessionPriority` `now`\|`later` (0225 emission-only; **0295** hygiene/`is_hygiene` is `later` on observe and enforce; 0225 signing trio later-on-observe only); additive per-finding `acknowledged` / `acknowledgedAt` (0226); additive top-level `completionReadiness` (0311: `cold`\|`loading`\|`busy`\|`ready`\|`unreachable`\|`fallback_failed`; omit when generation is not configured). Additive CLI-only `readyForPublishScope` `{readyMeans: "zeroBlockFindings", notRequiredForReady: ["chain", "fullTests", "signerTrust"]}` always present (0325; including `readyForPublish == false`). Do **not** emit `readyForPublishDefinition` or `notEvaluated`. schemaVersion stays 1. Sidecar `doctor-results.json` does **not** include `githubLatest`, `sessionPriority`, `acknowledged`, `acknowledgedAt`, `completionReadiness`, or `readyForPublishScope`. Optional embed miss is `embed-unreachable` (optional warn), not `ERROR ledgerful::embed::client` (0285). |
| `release pins --json` (bare `release --json`) | yes (0201) | yes | schemaVersion 1 object `kind: "releasePins"`; exit **0** match / **1** drift / **2** skipped or unverified. Parent `--json` (T18). Not Daily 5 |
| `change-context --json` | yes | yes | impact-shaped packet. Additive omit-empty `completeness` (0308/0347). `--timeout` is an **overall analysis** budget (`hotspots` list/explain `--timeout` is overall emit, 0349; unscoped `audit` `--timeout` is overall emit, 0350); omitted on working-tree; prospective default 25s. Token `prospective analysis stopped: overall budget` is stderr-only |
| `session --json` | yes (0224) | yes | schemaVersion 1 object `kind: "session"`; human default is **not** JSON. Does not rewrite `latest-impact.json`. `collisions[]` lives here (not status v1). No `warnAction`. Additive `configChecklist[]` (0301; applicable gaps; may be `[]`). No `sessionNotices` on this envelope. CLI-only. `hotspots.files[]` item keys `path` / `score` (0–1) / additive `displayScore` (ln). Human hotspot tables use column **Display** = `displayScore` |
| `configure --json` | yes (0302) | yes | schemaVersion 1 object `kind: "configure"`; `items[]` is the 0301 `ConfigChecklistItem` shape; optional `applied[]` only when `--apply` was used. Not `setup --json`. CLI-only. Human path is a table (no Confirm). `--apply` refuse: empty stdout, stderr diagnostic, no cookie |
| `review --json` | yes (0304) | yes | schemaVersion 1 object `kind: "review"`; `analysisMode: "range"`. Compose `base…head` files + in-memory impact counts + bounded ledger search. `ciEvidence.status` is `ok` \| `unverified` \| `unavailable` (never `none`). Additive omit-empty `boundHead` (resolved lowercase OID) and `historical[]`; item `verifyHistory.head` omit-empty. Additive omit-empty `completeness` (0308/0347/0348): overall emit stop has `scope: "overall"` + `stage` slug; a history-only 0308 copy omits `scope`. `--timeout` is the **overall review emit** budget (default 25s; `0` disables; `hotspots` list/explain `--timeout` is overall emit, 0349; unscoped `audit` `--timeout` is overall emit, 0350). Token `review stopped: overall budget` is stderr-only on overall budget stop. Does **not** rewrite `latest-impact.json`. Human path is 2–4 lines. CLI-only. Not `scan --pr`. Not change-context |
| `ledger search --json` | yes | yes | **Bare array** of ledger entries (0213 freeze; not a `schemaVersion` object). Additive omit-empty item keys `reason_kind` (`"trailer"` only) and `risk_source` (`category` \| `explicit`). Does not rename `reason` / `related_tickets` / `risk`. Human table has no Reason column. |
| `ledger audit --json` | yes | yes | Object `{exact, related}` camelCase items. Additive omit-empty `reasonKind` / `riskSource` / `matchBasis` (`entity` \| `changed_files` on exact; `directory` on related). File-path exact unions entity-string matches with `changed_files.path` equality. |
| `ledger graph --json` | yes | no (truncate on stderr when completeness present) | Object `{exact, derived, heuristic}` (no `schemaVersion`). Additive omit-empty `completeness` `{stop: "cap", maxDepth, maxNodes}` only when assemble hid a further hop or node. `--compact` refused with `--json`. Truncation warning is stderr. |
| `ledger adr list --json` | yes (0320) | yes | schemaVersion 1 object `kind: "ledgerAdr"`; `items[].id` is JSON number (`ledger_entries.id`). Omit-empty `supersedes` / `supersededBy`. Not a bare array. |
| `ledger adr export` | no (writes MADR files) | yes (human SUCCESS on stdout) | Default dest `docs/adr`; files stamp lifecycle Status (proposed..superseded) and Change type (Display: CREATE..DELETE); no `--json`. |
| `ledger export-provenance` | no `--json` (always JSON array) | no (truncate on stderr) | **Bare array** of `LedgerEntry` (oldest first). `--limit`/`--offset` page that array; `truncated: offset=… limit=… total=…` on stderr when more rows remain. Do not wrap in `schemaVersion`. |
| `export head --stdout` / `-o -` | n/a | yes | 0182: exact `serialize_chain_head` bytes. File mode prints SUCCESS (checkpoint write, not a verification) on stdout and writes the same JSON body to the file. |
| `export evidence` | no (writes zip) | yes (human SUCCESS) | No `--json`. Default dest `ledgerful-soc2-evidence.zip` (demo: `ledgerful-DEMO-evidence.zip`). `manifest.json` `gateModeDisclosure.chainContinuityStatus` `verified:` is a gated LOCAL walk plus a real stored signed head; otherwise `signed-head-only:` / `INVALID` / `not verified`. Zip CRC/sig is not a full-chain walk. |
| `viz` / `viz --view services` | no | yes (path + `#evidence` tokens) | No `--json`. Default dest `reports/graph.html` / `reports/services.html`. Offline vis-network 10.1.2. Graph: `source: Cozo nodes/edges`. Services gated: `source: declared overlay; inference gated`. Machine=false. |
| `ledger status --json` | yes | yes | schemaVersion 1 |
| `ledger stack --json` | yes (0281) | yes | schemaVersion 1 object `kind: "ledgerStack"`; `empty` true iff filtered rules/validators/mappings are all empty; `next` is the two clap register commands only when empty; `enforcementEnabled` from live config; omit-false `rulesNotEnforced` when rules exist and `start_change` would not block (`enforcementEnabled` and `gate.mode=enforce`). Item structs stay snake_case. No `emptyReason`. Not Daily 5 |
| `ledger validator list --json` | yes | yes | **Bare array** of `CommitValidator` (snake_case). Empty catalog is `[]`. Not a schemaVersion object. Not Daily 5. Human empty is not a table (0 registered + next `--help`). |
| `status --json` | yes (0149) | yes | **same payload** as `ledger status --json` |
| `search --json` | yes | yes | 0136 envelope; empty results OK. Constructor `Err` sets existing `semantic.error` and does not emit Ready+empty readiness (0377). |
| `search-trigrams --json` | yes (0352) | yes | schemaVersion 1 object `kind: "searchTrigrams"`. Hidden CLI. Count-backed `totalMatching` is **not** an 0136 `search` key. `emptyReason` omit when `resultCount > 0`. `next` omit unless runnable (`ledgerful index` or `ledgerful search {accepted…}`). No `line`/`content`. CLI-only; no MCP. |
| `bridge query --json` | yes (0366) | yes (exit 0) | Hidden CLI. schemaVersion 1 object `kind: "bridgeQuery"`. Closed `status`: `disabled` \| `unavailable` \| `failed` \| `empty` \| `populated`. `ok` agrees with process exit (`disabled`/`empty`/`populated` → 0; `unavailable`/`failed` → 1 after JSON). Omit-empty `source` (`ipc` \| `cli`), `providerCommand`, `resultCount`, `results`, `skippedLines`, `message`, `next`. `results[]` only on `populated`. Not BridgeRecord NDJSON (0136 `--json-lines`). CLI-only; no MCP. |
| `bridge export --json` / `--stdout` / `-o -` | yes (0367) | yes (exit 0) | Hidden CLI. **Not** `kind: bridgeExport`. Body is one BridgeRecord **0.3** Snapshot. Additive camelCase `payload.datasets[]` (always `impact` plus requested `--hotspots`/`--ledger`/`--madr`). `--json` with no `--out` implies stdout (no default `.ledgerful` write). `--json --out <path>` writes the file and leaves stdout empty. `-o -` is stdout. `--stdout` + `--out <path>` errors. `--madr` row is `notWired`. Compact one-line is NDJSON-compatible; pretty `--json` is not importable as NDJSON lines. CLI-only; no MCP. |
| `verify --json` | yes | yes* | plan-execution payload; see rejected combos |
| `index --check --json` | yes | yes (0149) | schemaVersion 1 + `kind: "indexCheck"` camelCase DTO (0207); Info suppressed under json; Error still on stderr |
| `index --repair-metadata --dry-run --json` | yes (0371) | yes (exit 0) | schemaVersion 1 object `kind: "indexRepairPreview"`. Always `executed: false` + `dryRun: true`. Omit `ok`. Nested age-only camelCase `assessment` (always `state` / `source` / `indexedFiles` / `staleFiles` / `unindexedFiles`; omit-empty `emptyReason` / `lastIndexedAt` / `daysSinceIndexed` / `samplePaths` / `warnings`). **Omit `emptyDiagnostics`.** `staleFiles` / `unindexedFiles` are age-only zeros — not a content-drift verdict. Locked sorted `proposed[]`: `force full index`, `replace metadata if successful`. Pretty JSON + one trailing newline. Preview never writes (no shadow / no metadata mutation). Not `kind: indexCheck`. Human `--dry-run` copy unchanged. Executed `--yes` repair unchanged. CLI-only; no MCP. |
| `index --semantic --json` | yes (0161) | yes | One final JSON object (`schemaVersion`, `mode`, `reason`, counts, `upToDate`); zero human mid-run lines on stdout |
| `index --json` (main / `--auto-scip` / `--scip`) | yes | yes* | Merged index stats object; top-level **`scip`** (0157/0166): `status`, `edges_added`/`edges_updated`, `definitions_mapped`/`definitions_seen`, `files_skipped`, skip/recovery tallies (`edges_skipped_enclosing_disagreement`, `edges_recovered_nest_prefer`, `edges_skipped_unmapped`, `edges_skipped_invalid_occ_range`, `edges_skipped_duplicate`, `definitions_skipped_invalid_range`, `invalid_enclosing_fallback`), `references_seen`, optional `message`. On Success skip/recovery fields are always present (incl. 0). WARN summary for disagreement/invalid-range is **stderr** only (O(1)); not part of the JSON payload |
| `dead-code --json` | yes (0149) | yes | schemaVersion 1 envelope; see rejected combos |
| `hotspots --json` | yes | yes | schemaVersion 1 object; collection `files`; list and `--semantic` echo `limit` (0207). Additive `completeness` (0308) only when the history walk stops early (`stop`: `budget`\|`cancelled`\|`error`); omitted on a complete window. Additive `provenance` (0309) is **always** on the live list (`source: live`; `commitsRequested`; `daysRequested` only with `--days`; `limit`; `filter`; `head` when known; `snapshotAt`/`snapshotAgeSecs` when `hotspot_history` has a row). `--semantic --json` omits `provenance`. Per-file `presence: "historical"` is emit-time only when HEAD is resolvable and the ranked path is absent from HEAD (omit when current, when HEAD is unborn/unresolvable, or on `--semantic`). CLI default list omits test/example/bench paths (0222; `--include tests` restores) **and** markdown (0293; `--include docs` is a frequency lane) **and** vendored `deps_src`/`vendor`/… (0297; `--include vendor` restores `f×c`). **`score` is 0–1**; `displayScore` is ln display. No `scoreUnit`. **MCP `hotspots` stays an in-process array** and stays unfiltered. `--semantic` ignores `--include`. `--timeout` is the **overall list/explain emit** budget (default 25s; `0` disables; env `LEDGERFUL_HOTSPOTS_OVERALL_BUDGET_SECS` / `[hotspots] overall_budget_secs`). History stays `[hotspots] history_budget_secs` (default 45) / `LEDGERFUL_HISTORY_BUDGET_SECS`, capped by the overall Instant. Additive `completeness.scope=overall` + `stage` slug when that deadline fires; history-only 0308 objects omit `scope`. Token `hotspots stopped: overall budget` is stderr-only on overall stop. Place `--timeout` / `--commits` **before** `explain`. `--semantic` honors the same overall Instant. `hotspots trend` / `hotspots budget` ignore parent `--timeout` (documented no-op). |
| `hotspots trend --json` | yes (0151) | yes | schemaVersion 1; modes summary/full/entity; additive `provenance` (0309, `source: trends`) always present — see schema below |
| `hotspots explain --json` | yes (0349) | yes | schemaVersion 1 object `kind: "hotspotExplanation"`. Parent `hotspots --json explain PATH` **or** `hotspots explain PATH --json`. `--timeout` / `--commits` / `--days` stay on the parent (place them **before** `explain`). Additive omit-empty `completeness` (0347 tokens). `score` is 0–1 when a breakdown exists; `displayScore` is ln. `couplingsWarning` omit-empty (overall skip is untrusted, not a trusted empty list). No MCP tool. See schema below. |
| `hotspots budget --json` | yes | yes | Versionless object (no `schemaVersion`). `status`: `OK` \| `VIOLATION` \| `NO_DATA` \| `NOT_CONFIGURED`. Always `dataset: "hotspot_history"` and `scoreUnit: "score"` (persisted `hotspot_history.score`, 0–1). `threshold` + `thresholdSource` (`cli` \| `config` \| `default`) omit on `NOT_CONFIGURED`. Informational default threshold **0.5**. `--fail` is the only exit-1 gate and requires `--threshold` or `[hotspots] budget_threshold`. Only `status == "OK"` is in-budget. Always `evaluated` + `violations[]` (`path` / `score` / `threshold`). Additive `snapshotAt` / `snapshotAgeSecs` / `head` / `legacyScoreCount` / `skippedNonFinite`. Omit-empty `emptyReason` (`noSnapshot` \| `allNonFinite`) and `next` (`ledgerful hotspots --snapshot`) only on `NO_DATA` (`next` only for `noSnapshot`; print-only, do not auto-run). No `provenance`. List / session / trend / MCP / `GET /api/hotspots` stay without `scoreUnit`. See schema below. |
| `endpoints --json` | yes | yes | schemaVersion 1 object; collection `results` (0207). Always echoes `includeFixtures` + `fixturesOmitted` (empty and populated). Default omits test-path + `route_source=TEST`; `--include-fixtures` restores. Post-omit product-empty without `--changed` is `emptyReason: noMatches` (after the current SQL/filter + omit — not a catalog-wide “zero product routes” claim). `--changed` empty stays `cleanDiff`. Additive item keys: `registrationFile`, `handlerFile`, `handlerUnresolvedReason`, `mountPrefix`, `mountedPath`, `mountProvenance`, `authSource: "inferred"`, `authParse`/`consumersParse`. MCP `endpoints_changed` re-execs CLI default omit (no extra flags) |
| `symbols --json` | yes (0163) | yes | schemaVersion **1** inventory; path/changed/kind/pub filters; COUNT-backed `totalMatching`; optional `indexStatus`; see schema below |
| `data-models list --json` | yes | yes | schemaVersion 1 object; collection `models` (0207); item `file_path` stays snake (0155); one row per logical model identity. Always echoes `includeFixtures` + `fixturesOmitted` (empty and populated). Always `supportedExtractors` / `notWired` (SQL migrations not wired). Omit-empty `next` (`list --include-fixtures` when product-empty and the flag is off). Default omits `is_test_path`; `--include-fixtures` restores. Item `fieldImpact: "unsupported"` on `models[]` (absent when empty). No MCP tool; no `/api/data-models*` |
| `ci list --json` / `ci diff --json` (alias) | yes | yes | schemaVersion 1 object; collection `gates` (0207). `list` is the documented name; `diff` is a visible alias. Empty catalog is `gates: []`, `resultCount: 0`, no fake `emptyReason`. Additive always-present `scope` (`inventory: declaredWorkflowJobs`, `notIncluded: ["branchProtectionRequired","liveRunStatus"]`). Item additive: always `filePath` + `triggers` (GHA event names at the standard two-space `on:` indent, including inline `push: {branches: […]}` / `push: null` / `workflow_dispatch: {}`; nested filter keys are not events; `[]` on other platforms; GHA `[]` can also mean an unrecognized `on:` shape); omit-empty `jobIf` / `needs` / `uses`. Not required-check or live-run status |
| `policy check --format json` | via `--format json` (not `--json`) | yes | schemaVersion 1 object; `passed` is no-violations; `policySource`; additive `notes` omit-empty; additive `idle: true` omit-false (synthesized idle is not a merge-gate pass). Additive always-present `evaluation` (0325): integers `rulesDeclared` / `rulesChecked` / `rulesIdle` / `rulesOff` / `rulesSkipped` plus always-emitted `rules[]` `{ruleId, status}` (`checked`\|`idle`\|`off`\|`skipped`), sorted by `ruleId`. Live emit `rulesDeclared` is **5** (declared `PolicyRules` fields, not `rules.len()`). Identity: declared == checked + idle + off + skipped. serde default zeros + empty `rules[]` is the **legacy/unknown sentinel**, not a live 0-rule measurement. Human Result is `IDLE (synthesized; not a merge gate)` in that case; human path does **not** print evaluation counts. |
| `ledger validator doctor` | no | n/a | Human-only path probe (0325). Prints `Validators: {n} registered / {n} enabled / {n} inspected / {n} resolved`. `enabled == 0` is **not** a health pass (no “All enabled validators are healthy!”); next line names `ledgerful ledger register validator --help`. Never runs the executable. No `--json`. `ledger validator list --json` stays a bare array. |
| `config schema --json` | yes | yes | schemaVersion 1 object; collection `results` (0207). Empty keeps `emptyReason`/`message`. Additive omit-empty item keys `filePath` and `requiredness` (`optional` \| `unknown`; `unknown` wins when `docs` or `confidence < 1.0`). `confidence` is the stored column (not hardcoded 1.0). |
| `dependencies list --json` | yes (0153) | yes | schemaVersion **1** envelope; `mode`: `direct` (default) \| `all`; live Cargo.toml+lock — not Cozo; see schema below |
| `impact --json` | yes | yes | impact packet (`schemaVersion` string `"v1"`). `--timeout` is an overall analysis budget (0347; `hotspots` list/explain `--timeout` is overall emit, 0349; unscoped `audit` `--timeout` is overall emit, 0350). Prospective `--paths` default 25s. Working-tree without `--timeout` keeps 0034/0308. Additive omit-empty `completeness` with `scope: "overall"` when the overall analysis **stops** (budget Instant **or** cancel), including working-tree without `--timeout` (0374). Token `prospective analysis stopped: overall budget` is stderr-only |
| `scan --impact --json` | yes | yes | impact packet (`schemaVersion` string `"v1"`; no top-level `kind`). `--timeout` requires `--impact` and is an overall analysis budget (0347; `hotspots` list/explain `--timeout` is overall emit, 0349; unscoped `audit` `--timeout` is overall emit, 0350). Prospective `--paths` default 25s. Additive omit-empty `completeness` with `scope: "overall"` when the overall analysis **stops** (budget Instant **or** cancel), including working-tree without `--timeout` (0374) |
| `scan --json` / `scan --out` (no `--impact`) | yes (0180) | yes | **gitScan** envelope: numeric `schemaVersion` **1** + top-level **`kind: "gitScan"`** + ScanReport fields; **not** auto-impact |
| `scan --pr <range> --format json` | via `--format` | yes | PR-range machine output (not impact packet) |
| `audit --json` | yes | yes | Bare `ProjectAuditReport` object, **no** `schemaVersion`. Additive `completeness` (0308) sibling of `hotspots` when the walk stops early or history `Err` (`stop=error` omits `commitsWalked`); overall emit stop has `scope: "overall"` + `stage` slug (`storage` \| `velocity` \| `federated` \| `churn` \| `hotspots` \| `ci_trend` \| `recent`) when the overall Instant fires **or** cancel is set (0376). `churn[].entity` is a unique file path; `count` is distinct LOCAL TXs. `--timeout` is the **overall unscoped audit emit** budget (default 25s; `0` disables Instant; env `LEDGERFUL_AUDIT_OVERALL_BUDGET_SECS` / `[audit] overall_budget_secs`; cancel still leaves). History stays `[hotspots] history_budget_secs` (default 45) / `LEDGERFUL_HISTORY_BUDGET_SECS`, capped by the overall Instant. History-only 0308 objects omit `scope`. Token `audit stopped: overall budget` is stderr-only on Budget overall stop; cancel is silent on stderr. Human cancel line is `Audit stopped: cancelled ({stage}).`. Entity-scoped `ledger audit --json` stays `{exact, related}` and ignores `--timeout`. Not Daily 5 |
| `surfaces --json` | yes (0185) | yes | schemaVersion 1 object `kind: "surfaces"`. Item keys `id`/`name`/`command`/`status`/`gate`/`reason`/`next` stay. Additive `sessionNotices` object (0300; skip empty): notice id → `"already_shown"`. `next` strings stay on later emits. Deploy empty `next` (`ledgerful index --incremental`) is honest: that command writes `deploy_manifests`. `data-models` **ready** matches default `data-models list` product scope (0316 omit); fixture-only is `empty` with next `ledgerful data-models list --include-fixtures` (0354). |
| `services list --json` / `services diff --json` | yes | yes | schemaVersion 1 object, collection `results`. Gated empty keeps `emptyReason`/`message`. Always `inferenceState`. Omit-empty `declared`. `--preview` adds `preview: true` and never writes `sessionNotices`. Additive `sessionNotices` (0300; skip empty) on persist-empty only. Item additive `source` / omit-empty `root` / `--full` `files` + `filesTruncated` |
| `deploy impact --json` | yes | yes | schemaVersion 1 object, collection `results`. Cheap git-diff classify (no SQLite / orchestrator). Always `defaultPatterns` / `classifiers` (`Unknown` is enum-parity; Helm/CiWorkflow are not default globs). Gated empty keeps `emptyReason`/`message`. Disabled `message` must **not** start with “No deployment impact detected.” (that sentence is enabled `noMatches` only). Overall-stop empty omits `emptyReason`/`message` and adds omit-empty `completeness` (`stop` `budget`\|`cancelled`, `scope: "overall"`, `stage: "deploy"`, `budgetSecs`). Item omit-empty `coupledFiles` / `highBlastResources`. `--timeout` on `impact` (not parent `deploy`) is overall emit (default 25s; `0` unlimited; env `LEDGERFUL_DEPLOY_OVERALL_BUDGET_SECS` / `[coverage.deploy] overall_budget_secs`). Token `deploy impact stopped: overall budget` is stderr-only on Budget stop; cancel is silent. Additive `sessionNotices` (0300; skip empty). |
| `observability coverage --json` | yes | yes | schemaVersion 1 object, collection `results`. Always `inputs` (`directory` / `ingest` / sorted `kinds`) + `notWired: ["endpoints"]`. Empty keeps `emptyReason`/`message`. Item always `health` (`covered`\|`missing`) plus snake `slo_count` / `metric_count`. `--preview` is disk-only (no Cozo) and sets `preview: true` (no 0300 cookie). Persist empty keeps 0215 taxonomy + additive `sessionNotices` (0300; skip empty). Omit-empty `parseErrors[]` `{path, reason}` |
| `observability diff --json` | yes | yes | schemaVersion 1 object `kind: "observabilityDiff"`. Collection `changed`. Always `unchanged_count` / `indexedCount` / `resultCount` (`resultCount` = `changed.len()`). Empty keeps `emptyReason`/`message`. Item omit-empty `sourceFile`. `--preview` disk-only + `preview: true`. Persist 0215 empty taxonomy; preview empty does **not** tell the user to `index --analyze-graph`. Omit-empty `parseErrors[]` |
| `federate status --json` | yes (0327) | yes | schemaVersion 1 object; `peers[]` live only (0184 omit Self/Dead/dups in `omitted`); item `name`/`path`/`lastScanned` + omit-empty `schemaGeneratedAt`; among Live dups `lastScanned` is the later **parsed** RFC3339 instant (RFC 3339 §5.1 lex sort is not enough when frac width or offset spelling differs); `freshness.status` `available`\|`stale`\|`unavailable` from sibling `generated_at` vs last scan (not wall-clock catalog). Omit-empty `next`: `ledgerful federate scan` when stale or `peers` empty. No `emptyReason`. Bare `federate` stays human. |
| `federate export --json` / `--dry-run --json` | yes (0355) | yes | schemaVersion 1 object `kind: "federateExportPreview"`. Non-writing preview (`dryRun: true` even without `--dry-run`). Always `repoName` / `binaryVersion` / `wireSchemaVersion` (`"1.1"`) / `limit` / `truncated` / `resultCount` / `totalMatching` / `interfaces[]` / `ledgerCount`. Omit-empty `head` / `generatedAt` / `interfaces[].line` / `next` (`ledgerful scan --impact` when `totalMatching==0`). `--limit` default 200, clap `1..=5000`, preview-only (write path stays full `schema.json`). Human `--dry-run` is a summary (no JSON, no `FEDERATED SCHEMA PREVIEW` banners). `--json`/`--dry-run` conflict with `--out`. |
| `sync cursor --json` | yes (0327; 0365; `feature = "sync"`) | yes | schemaVersion 1; `initialized`; `lastExtractHlc`/`lastApplyHlc` null when missing; `lag.status` always `unknown` with `reason` `notInitialized`\|`neverRun`\|`hlcNotWallClock`. Additive omit-empty `watermarkCompare`: omit if either HLC missing; `incomparable` if both present but unparseable; `extractAhead`\|`applyAhead`\|`equal` when both parse. `nextAction` is `sync init` or `sync setup` (no NAS probe). Conflicts with `--set`. |
| `sync log --json` | yes (0327; 0365; `feature = "sync"`) | yes* | schemaVersion 1; `logState` `neverInitialized`\|`noLog`\|`unreadable`\|`ok`\|`partial`; `lineCount`/`skippedLines` are whole-file; `lines` is the displayed tail (default 20). Additive omit-empty `events[]` (`ts`/`event`/`ok` + omit-empty `bundle`/`detail`) is the parsed subset of **displayed** lines. `--failed` filters parsed `ok==false` **then** tails; omit-false `failed: true` when the flag is set. Missing file exit 0; unreadable writes JSON then exit **1**. |
| `sync verify --json` | yes (0365; `feature = "sync"`) | yes* | schemaVersion 1; **no** `kind`. Always `ok` (bool) + `verdict` (`ok`\|`missingFile`\|`missingSecret`\|`decryptFailed`\|`unknownDevice`\|`signatureFailed`\|`integrityFailed`\|`schemaInvalid`). **`ok` and process exit must agree** (`ok` → 0; every non-`ok` → 1 with JSON then `request_exit(1)`). Success always `version`/`deviceId`/`bundleHlc`/`entryCount`. Failure omit-empty `message` (`unknownDevice` names the id; `missingSecret` names `LEDGERFUL_SYNC_SECRET`). |
| `web status --json` | yes (0328) | yes | schemaVersion 1 object; **no** `kind`. Compact single line + newline. `state` `running`\|`stalePid`\|`reusedPid`\|`noPidFile`; always `pidFile` + `next` (`ledgerful web stop` when running, else `ledgerful web start`); omit-empty `pid`. Invalid PID file: empty stdout, non-zero, file kept. Status does not remove the PID file. Windows `is_alive` permission-denied may report `stalePid` instead of `reusedPid`. Bare `web` still requires a subcommand. |
| `timings --json` | yes (0043; 0330; 0346; 0364) | yes | schemaVersion 1 envelope `{schemaVersion, data}` — **no** `kind`. `data[]` keeps `command` / `runs` / `p50_ms` / `p95_ms` / `p99_ms` / `total_ms`. Additive always: `comparable` (bool, never omitted), `workload_count`, `nonzero_exit_runs`, `window_days`. Additive omit-empty: `duration_spread` (`single`\|`mixed`), `incomparable_reason` (`unhashedArgv` when the sole success bucket is `"<unhashed>"`), `sample_note`, `workloads[]`. `runs` is success (`exit_code==0`) count. `--explain --json` `data` is an object with always `explain` / `command` / `runs` / `comparable` plus omit-empty `p50_ms` / `prior_p50_ms` / `incomparable_reason`. No success prior baseline (empty **or** all-failed) omits `prior_p50_ms` (never dummy `0`). Global explain keeps camelCase repo counts around that `data` object. `--inner --json` `data[]` is `{command, span_name, samples, total_ms, max_ms}`; omit-empty `coverage[]` `{command, outer_ms, inner_ms, uninstrumented_ms}` (present whenever outer rows exist; wall-clock, all exits; `--top` truncates `data[]` only). `--flame --json` `data` is `{collapsed}` plus omit-empty `unique_stacks` / `total_weight_ms` (snake_case). Capture filter and inner `notes.span_id` do not retro-clean historical tokio rows. |

\* Non-essential progress INFO suppressed under machine mode; hard failures still
use stderr.

### `sessionNotices` (0300)

Additive object on `surfaces --json` and gated/empty `services` / `deploy impact`
/ `observability coverage` envelopes. Omitted when nothing in this session has
already been shown. Keys are closed v1 ids (`coverage.global`,
`coverage.services`, `coverage.deploy`, `observability.empty`); values are the
string `"already_shown"`. Item `next` / empty `message` stay. Human collapse is
`Already shown this session.` `session --json` does **not** grow `sessionNotices`.
Human `session` does **not** write `.ledgerful/cli-session.json`.
`session --json` and `configure` (human or `--json`) **may** write the
cookie when `configChecklist[]` / configure `items[]` has `gated` or
`empty` rows (0301/0302; marks mapped 0300 notice ids + `config.checklist`).
`doctor --json` still neither grows `sessionNotices` nor writes the cookie.

```json
{
  "schemaVersion": 1,
  "sessionNotices": {
    "coverage.global": "already_shown"
  }
}
```

First emit in a session omits `sessionNotices`. Later emits keep frozen
`next` / `message` and add only the ids already shown.

### `configure --json` schema (0302)

Pure stdout. New envelope (`kind: "configure"`). Item shape is the same
`ConfigChecklistItem` as session `configChecklist[]` (camelCase; `applyArg`
omitted when none). `applied` is present only when `--apply` was used
(`skip_serializing_if` empty). Session envelope is unchanged.

```json
{
  "schemaVersion": 1,
  "kind": "configure",
  "items": [
    {
      "id": "coverage.global",
      "status": "gated",
      "applicable": true,
      "next": "ledgerful config set coverage.enabled=true",
      "alreadyShown": false,
      "applyArg": "coverage.global"
    }
  ],
  "applied": [
    {
      "id": "coverage.global",
      "ok": true,
      "message": ""
    }
  ]
}
```

Deploy-only repos (indexed `deploy_manifests`, no HTTP) emit
`coverage.global` after `index --incremental`.

`--apply` is explicit ids only (three `applyArg` tokens). Refuse unknown /
inapplicable / no-`applyArg` / apply-all / empty-after-trim with no writes,
empty stdout, and no cookie. Ready applyArg rows are idempotent
(`message: "already enabled"`).

### `ledger stack --json` schema (0281)

Pure stdout. New envelope for this command (it had no `--json` before). Freeze
`schemaVersion` **1**. Item arrays reuse existing serde (`TechStackRule`,
`CommitValidator`, `CategoryStackMapping` — snake_case). Do **not** wrap as
0213 bare array or an `emptyReason` list envelope.

```json
{
  "schemaVersion": 1,
  "kind": "ledgerStack",
  "empty": true,
  "enforcementEnabled": false,
  "rules": [],
  "validators": [],
  "mappings": [],
  "next": [
    "ledgerful ledger register rule",
    "ledgerful ledger register validator"
  ]
}
```

`empty` is computed on the optional positional `[CATEGORY]` filter. `next` is
non-empty only when `empty` is true. JSON `next` strings stay the two bare
clap commands (required flags are human/`--help` only). Additive omit-false
`rulesNotEnforced: true` when `rules` is non-empty **and** start_change would
not block (needs both `enforcementEnabled` and `gate.mode=enforce`). Empty
catalog omits the key. `enforcementEnabled: true` with default
`gate.mode=observe` still emits `rulesNotEnforced: true`.

### `release pins --json` schema (0201)

Pure stdout. Diffs GitHub Latest (`tag_name` + archive `assets[].digest`) against
in-tree packaging templates, live Homebrew tap / Scoop bucket remotes, and npm
`@ledgerful/mcp-server` `ledgerfulEngineTag`. Web launch-facts is **advisory**
and never flips overall status.

```json
{
  "schemaVersion": 1,
  "kind": "releasePins",
  "status": "match",
  "latest": { "tag": "v0.2.10", "sha": "c4a2308fe985" },
  "surfaces": [
    {
      "id": "mcp.inTree",
      "status": "match",
      "local": { "version": "0.1.19", "ledgerfulEngineTag": "v0.2.10" },
      "expected": { "ledgerfulEngineTag": "v0.2.10" },
      "remote": null
    }
  ],
  "advisory": {
    "launchFactsPath": "…/launch-facts.ts",
    "releaseTag": "v0.2.10",
    "mcpEngineTag": "v0.2.10",
    "status": "match"
  }
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | number **1** |
| `kind` | always **`"releasePins"`** |
| `status` | `match` (exit 0) / `drift` (exit 1) / `unverified` or `skipped` (exit 2) |
| `latest` | omitted on `skipped` and when Latest fetch failed; `sha` omitted when peel fails |
| `surfaces` | six required ids, **sorted by `id`**. Empty `[]` when `skipped` |
| `advisory` | present only if sibling `../ledgerful-web/src/lib/content/launch-facts.ts` existed; mismatch does **not** change overall status |

**Caveat:** `--auto-index` on any surface may print human progress on stderr when
an index refresh actually runs (ambient `try_auto_index` path). Prefer a fresh
index, or parse **stdout only** (not `2>&1`) when combining `--json --auto-index`.

---

## Stream policy (summary)

| Kind of output | Stream |
|---|---|
| Machine-readable payload (`--json`, `scan --format json`) | **stdout only** |
| Product lines from `cli_summary` at `info!` | **stdout** (level-split writer) |
| Diagnostics from `cli_summary` at `warn!` / `error!` | **stderr** |
| Progress / backend chatter (`ask`, retries) | **stderr** |
| Hard signature failures (`INVALID`, required `UNSIGNED`) | **stderr** (raw `eprintln!`) |
| Non-`cli_summary` progress `INFO` under machine mode | **suppressed** (normal_layer max `WARN`) |
| Non-`cli_summary` diagnostic `INFO` under default human (no `-v`, no `RUST_LOG`) | **suppressed** (0154: normal_layer max **WARN**) |

A command advertised as JSON must emit **only** JSON on stdout. Warnings must
not precede or follow that JSON on stdout.

**Human colour:** product colour uses `if_supports_color` (stream-aware). Honour
`NO_COLOR` (force off), `FORCE_COLOR` / `CLICOLOR_FORCE` non-empty non-`0`
(force on), else TTY/CI auto. Machine JSON paths stay colour-free. See track
0131.

---

## Four verbosity states (`cli_summary` layer)

| State | Filter | Selected by | Effect |
|---|---|---|---|
| **Default** | `INFO` | anything else (track 0100) | Aggregate visible; **hide** per-entry `VALID`/`SKIP` detail |
| **Verbose** | `DEBUG` | `--verbose` / `-v` | Restore per-entry signature detail + aggregate (pre-0100 default) |
| **Quiet** | `INFO` | `--quiet` / `-q` / `LEDGERFUL_QUIET=1` | Same filter as default for signatures (hide per-entry; **keep aggregate**) |
| **Machine** | `WARN` | `--json` on any subcommand, `scan --format json`, `mcp` | No human `cli_summary` line reaches stdout |

**Precedence:** machine → `WARN` (wins over everything); else if verbose →
`DEBUG` (explicit `-v` wins over quiet); else → `INFO`. **`--json` selects
machine mode, not quiet** — quiet would still emit aggregate `info!` lines
around the JSON payload.

**Machine mode also raises the non-`cli_summary` (`normal_layer`) EnvFilter to
`WARN`**, so progress `INFO` lines (for example `Running verification command
via Shell: …`) do not appear on stderr during a successful `verify --json`
run. `WARN` / `ERROR` on `normal_layer` are **not** suppressed (Wave 0 honesty).

**0154 extends the `normal_layer` WARN floor to default human runs** when
`RUST_LOG` is unset (or empty): non-verbose interactive CLI no longer emits
timestamped tracing-style `INFO target:` diagnostics on stderr. Dogfood-hot
probes (embed probe, semantic init, federated Scanning progress) are demoted
to `debug!` so ambient `RUST_LOG=info` does not re-flood them. Diagnostic
detail returns with `-v` / `RUST_LOG=debug` (or a specific `RUST_LOG` directive).
Product notices (web/viz bind, init success, layout migration, watch sync
summary, verify step-start) use `println!` / `eprintln!` / `cli_summary`, not
filterable `normal_layer` INFO. The `cli_summary` four-state table above is
**unchanged**.

`INVALID` and signing-required `UNSIGNED` are raw `eprintln!` outside the
layer; no filter state suppresses them.

---

## Diagnostic `verify --json` kinds (0321)

Executed `verify --json` (no diagnostic flags) stays `VerifyCliJson`:
schemaVersion 1, **no** `kind`. Absence of `kind` is the plan-execution
discriminator.

Diagnostic envelopes are sibling kinds (schemaVersion 1, required `kind`).
They do **not** write `latest-verify.json`.

| Combo | `kind` |
|---|---|
| `verify --json --dry-run` | `verifyDryRun` (`executed: false`, no `ok`) |
| `verify --json --health` | `verifyHealth` (`ok` = tools only) |
| `verify --json --signatures` / `--chain` / `--against-export` | `verifySignatures` |

`verifySignatures.checkpoint` (only with `--against-export`): `match` and
`extends` are `ok: true` / exit 0 (same as human extends-or-equals).
`diverges` / `exactMismatch` / `exportSigInvalid` are `ok: false` / exit 1.

`verifySignatures.chain.head` (omit-empty): present only when a stored
chain head exists. Fields: `signatureValid`, `hashMatch`, `lengthMatch`.
A stored-head signature / hash / length fail can be `ok: false` with
`signatures.invalid == 0` and `breaks: []` — do not treat empty `breaks`
as a silent pass. `--chain`-only keeps `signatures.checked: false`.
Unsigned-required with `--chain` stays `exitCode` 3 / `ok: false` when
`breakCount` is 0, checkpoint is pass-or-absent, and `chain.head` (if
present) is all-true. Do not treat signature-only `first_human` as a
chain break. Stdout JSON is authoritative if stderr first-write still
mentions exit 3.

Reject (miette, empty stdout, no partial JSON) mixed diagnostic modes:

| Combo | Error |
|---|---|
| `--health` + `--dry-run` | `verify --health cannot be combined with --dry-run` |
| `--health` + (`--signatures` / `--chain` / `--against-export`) | `verify --health cannot be combined with --signatures, --chain, or --against-export` |
| `--dry-run` + (`--signatures` / `--chain` / `--against-export`) | `verify --dry-run cannot be combined with --signatures, --chain, or --against-export` |

These reject rather than emit empty stdout under machine mode.
`--json --explain` stays human-skipped. `--exact` still requires `--against-export`.

### Rejected flag combinations (`doctor`)

| Combo | Error |
|---|---|
| `doctor --json --apply-hook-refresh` (no `--dry-run`) | `doctor --json cannot be combined with --apply-hook-refresh` |
| `doctor --fix` (no `--yes` / `--dry-run`) | `doctor --fix requires --yes or --dry-run` |
| `doctor --dry-run` (no `--fix` / `--apply-hook-refresh`) | clap: `--dry-run` requires `--fix` or `--apply-hook-refresh` |
| `doctor --fix --apply-hook-refresh` | clap: `--fix` conflicts with `--apply-hook-refresh` |

`--json --fix --dry-run` is allowed (additive top-level `fix` plan; schemaVersion stays 1). `--json --apply-hook-refresh --dry-run` is the isolated `hookRefreshPreview` envelope (no health catalog). Write-path `--apply-hook-refresh` (no `--dry-run`) stays human. Detect-only `doctor --json` remains pure schema-v1 findings JSON.

### Rejected flag combinations (`dead-code --json`)

| Combo | Error |
|---|---|
| `dead-code --json --prune` | `dead-code --json cannot be combined with --prune` |
| `dead-code --json --explain …` | `dead-code --json cannot be combined with --explain` |

Interactive prune and human explain have no machine schema (explain types lack
`Serialize`). Rejects run **before** storage/scan.

### Scan machine flags (`scan`)

| Combo | Behavior |
|---|---|
| `scan --json` / `scan --out` without `--impact` | exit **0** (product OK): **gitScan** summary envelope (0180). Escalate with `scan --impact --json` for the full impact packet. |
| `scan --summary` without `--impact` | exit **1**; `--summary requires --impact (impact brief summary)` — no PR `--format json` tip |
| `scan --json` with `--pr` | exit **1**; use `--format json` with `--pr` |
| `scan --json` with `--paths` | exit **1**; `--paths requires --impact` |
| `scan --json` auto-implies `--impact` | **Not supported** — impact analysis is expensive; pass `--impact` explicitly |
| `scan --mode docs` without `--impact` | exit **1**; `--mode requires --impact` (miette; reject **before** gitScan). Unknown `--mode fast` is clap-rejected |
| `scan --impact --mode docs` | Docs presentation: human **Actionable (≤5)** first; JSON additive `actionableLead` (cap 5, sorted) + `glossary` for `no_source_seeds` / `mapped=0`. Full `temporalCouplings` remain. Does **not** rewrite `latest-impact.json`. Auto-detects when **every** dirty/`--paths` entry is documentation-shaped (`.md`/`.txt`/`.rst`/`docs/**`/conductor process docs). Mixed `src`+docs does **not** auto-enter. `--include-governance` still restores pathMode=all **weights**; docs mode is presentation. `--full` expands remaining couplings in human output |

### `scan --json` gitScan schema (0180)

Pure stdout (or file only when `--out` is set — no dual dump). **Does not**
modify on-disk `latest-scan.json` schema (`ScanReport` remains without
`schemaVersion`/`kind`). Top-level **`kind` is intentional and new** on this
surface (other numeric-schema envelopes omit `kind`).

```json
{
  "schemaVersion": 1,
  "kind": "gitScan",
  "headHash": "…",
  "branchName": "main",
  "isClean": true,
  "changes": [],
  "diffSummaries": []
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | number **1** (not the impact packet string `"v1"`) |
| `kind` | always **`"gitScan"`** — primary discriminator vs impact / PR reports |
| `headHash` / `branchName` / `isClean` / `changes` / `diffSummaries` | Same semantics as durable `ScanReport` (camelCase) |
| `--base-ref` without impact | OK; `diffSummaries` often **`[]`** (working-tree diffs skipped) |

**Escalate:** `scan --impact --json` → full impact packet (string
`schemaVersion: "v1"`, risk/blast/agentSummary, …). Never emitted as gitScan-only.

---

## Default human `verify` contract (hooks / binary-first, 0121)

Installed pre-push shells call `ledgerful verify --scope fast` **without**
`--json`. Human `verify --dry-run` without `--scope` previews that same
fast plan (executed `verify` stays full; `--json --dry-run` stays
refused). After a PATH upgrade alone:

| Outcome | Default (non-verbose) stdout |
|---|---|
| **Pass** | Per-step `[i/n] Running:` + compact `ok` + elapsed; trailing `Verification passed`; **no** SUCCESS banner / plan banner / Suggested Actions |
| **Fail** | Per-step `[i/n] Running:` → `FAILURE` lines → structured fail block → Suggested Actions (if any) → miette on stderr; exit non-zero |

`--verbose` / `-v` restores plan banner, per-step SUCCESS (as-is, no compact ok
elapsed), aggregate “Running N step(s)…” progress `info!`, and Suggested Actions
on green. Step-start `[i/n] Running:` still emits under verbose. `--json`
never emits step-start / compact ok (pure schema + existing `durationMs`).

### Structured fail block (stdout, non-json fail)

Printed **before** Suggested Actions and before the final miette error:

```text
[Ledgerful] verify failed
step: cargo fmt --all
command: cargo fmt --all -- --check
exitCode: 1
failureDetail: <tool summary>
failedPaths: path1 path2   # only when formatter path extract yields ≥1 path
```

Field names use camelCase to correlate with the JSON wire. Paths are
best-effort from known formatter output (`cargo fmt`/`rustfmt --check`:
`Diff in <path>:` / `Diff in <path> at line N:`; `ruff format --check`:
`Would reformat: <path>`). Never invented; `\` → `/`; cap 50.

---

## `verify --json` schema (v1)

Primary example — **MappingRefuse** (default when fast cannot scope; empty
`test_mapping`, no DB connection, auto-index fail without allow). Exit ≠ 0,
`ok: false`, empty steps. Do **not** treat empty mapping as `running full`.

```json
{
  "schemaVersion": 1,
  "ok": false,
  "scopeRequested": "fast",
  "scopeExecuted": "refused",
  "fallbackReason": "fast scope unavailable — test_mapping is empty; run `ledgerful index --incremental` or use `--auto-index`; refusing full suite (~5-8 min)",
  "steps": [],
  "timestamp": "2026-07-28T12:00:00+00:00",
  "txId": "optional-pending-tx-id"
}
```

Optional — **full fallback** only for SharedInfra (always) or when the operator
passed `--allow-full-fallback` (restores pre-0135 surprise-full for mapping miss):

```json
{
  "schemaVersion": 1,
  "ok": true,
  "scopeRequested": "fast",
  "scopeExecuted": "full",
  "fallbackReason": "fast scope unavailable — shared infrastructure touched; running full (~5-8 min)",
  "steps": [
    {
      "name": "cargo fmt --all",
      "command": "cargo fmt --all -- --check",
      "status": "pass",
      "exitCode": 0,
      "durationMs": 1234
    }
  ],
  "timestamp": "2026-07-28T12:00:00+00:00",
  "txId": "optional-pending-tx-id"
}
```

Failed step example with path enrichment (additive; **schemaVersion stays 1**):

```json
{
  "name": "cargo fmt --all",
  "command": "cargo fmt --all -- --check",
  "status": "fail",
  "exitCode": 1,
  "durationMs": 400,
  "failureDetail": "Diff in src/lib.rs:",
  "failedPaths": ["src/lib.rs"]
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | integer | Always `1` for this contract |
| `ok` | boolean | `true` iff every step has `exitCode == 0`; **always `false` when refused** |
| `scopeRequested` | string | `fast` or `full` as passed on the CLI |
| `scopeExecuted` | string | ∈ {`fast`, `full`, `refused`}: `refused` when plan refused mapping-cannot-scope; `full` when SharedInfra / `--allow-full-fallback` fallback; else equals requested |
| `fallbackReason` | string (omitted when null) | Passthrough from the plan; present on fast→full fallback **and** on MappingRefuse (refusing string) |
| `steps` | array | **Plan order** (not alphabetically sorted); **`[]` when `scopeExecuted` is `refused`** |
| `steps[].status` | string | `"pass"` if `exitCode == 0`, else `"fail"` |
| `steps[].failureDetail` | string (omitted on pass) | stderr summary preferred |
| `steps[].failedPaths` | string[] (omitted when empty/pass) | Best-effort formatter paths; same sources as human fail block |
| `timestamp` | ISO 8601 | From the run report |
| `txId` | string (omitted when null) | Bound pending transaction if any |

This payload is a **CLI wire contract**. It is built from
`VerificationReport` but does **not** extend the persisted
`.ledgerful/reports/latest-verify.json` artifact.

### Invocation

```powershell
ledgerful verify --json
ledgerful verify --json --scope fast
ledgerful verify --json --quiet   # quiet is redundant for agents; machine mode already wins
```

---

## `ledger status --json` / top-level `status --json` schema (v1)

Top-level `status --json` (track **0149**) routes into the **same**
`execute_ledger_status` path as `ledger status --json` — identical field set,
no second DTO. Top-level `status` accepts `--json` and `--compact` (not a full
alias of `ledger status`; no `--global` / `--all`). Combined `--help`
`after_help` names the ledger-only flags and points at
`ledgerful ledger status` (0282; envelope unchanged). Track **0200** adds
`workRoot` and `stateDir` (schemaVersion stays **1**). Linked worktree
(**0108**): `workRoot` is this worktree; `stateDir` is the main `.ledgerful`.

```json
{
  "schemaVersion": 1,
  "workRoot": "C:\\dev\\ledgerful",
  "stateDir": "C:\\dev\\ledgerful\\.ledgerful",
  "pendingCount": 1,
  "unauditedCount": 0,
  "pendingTxIds": ["aaaaaaaa-....", "bbbbbbbb-...."],
  "unauditedFileCount": 0,
  "promoteOrphan": false,
  "headUncovered": false
}
```

| Field | Notes |
|---|---|
| `schemaVersion` | `1` (added in 0093; **0200** additive, not a v2 bump) |
| `workRoot` | Absolute git worktree this command bound (same string as doctor `environment.workRoot`) |
| `stateDir` | Absolute `.ledgerful` directory (same string as doctor `environment.stateDir`; linked worktree shares main) |
| `pendingTxIds` | **Sorted** lexicographically for determinism |
| `promoteOrphanTxId` / `promoteError` | Omitted when absent |

Observe-mode would-block diagnostics go to **stderr** via `cli_summary`
`warn!`. Stdout remains parseable JSON alone.

### Invocation

```powershell
ledgerful status --json
ledgerful ledger status --json   # same payload
```

---

## `hotspots trend --json` schema (v1)

Track **0151**. Default is a **top-N file summary** (limit **20**), not the full
timestamp×file matrix. Pure stdout under `--json` (no human table). Mode
precedence: `--entity` > `-a/--all` > summary.

### Summary mode (default)

```json
{
  "schemaVersion": 1,
  "mode": "summary",
  "days": 30,
  "limit": 20,
  "truncated": true,
  "historyAvailable": true,
  "bootstrapHint": null,
  "totalFiles": 28,
  "totalEntries": 3690,
  "snapshotCount": 369,
  "files": [
    {
      "filePath": "src/commands/index/modes.rs",
      "latestScore": 0.044,
      "displayScore": 3.809,
      "priorDisplayScore": 3.790,
      "delta": 0.019,
      "sampleCount": 369,
      "lastRecordedAt": "2026-08-08T15:59:19.529795500+00:00",
      "commitHash": "85ebb481e9c882e0c7d90c5e6281fa739a9c7dc6"
    }
  ],
  "provenance": {
    "source": "trends",
    "limit": 20,
    "deltaUnit": "displayScore",
    "firstRecordedAt": "2026-01-01T00:00:00Z",
    "lastRecordedAt": "2026-08-08T15:59:19.529795500+00:00"
  }
}
```

`provenance` is always present (including empty `files` / `entries`). Summary includes `limit` + `deltaUnit: "displayScore"` (`delta` / `displayScore` stay ln; `latestScore` stays 0–1). Full/entity omit `limit`, `filter`, `head`, and `deltaUnit`. `firstRecordedAt` / `lastRecordedAt` are window extrema when `totalEntries > 0` and omitted when empty. Trend `days` stays top-level — do not echo `daysRequested` / `commitsRequested` / `filter` / `head` on trend provenance.

### Full mode (`--all`) and entity mode (`--entity`)

- **`mode`:** `"full"` or `"entity"`
- **`entries`:** row array with **snake_case** keys (`file_path`, `recorded_at`,
  `score`, `display_score`, `commit_hash`) — same as pre-0151 matrix dump
- **`limit` / `files`:** omitted
- **`truncated`:** always `false` (no file-rank cap)

### Per-mode field rules

| Field | summary | full (`--all`) | entity |
|---|---|---|---|
| `mode` | `"summary"` | `"full"` | `"entity"` |
| `totalEntries` | `rows.len()` | same | same (entity-filtered) |
| `snapshotCount` | distinct `recorded_at` | same | same |
| `totalFiles` | distinct `file_path` | same | **1** if any rows else **0** |
| `limit` | effective limit | omit | omit |
| `truncated` | `totalFiles > limit` | **false** | **false** |
| `files` | top-N array | omit | omit |
| `entries` | omit | full row array | entity row array |

| Field | Rules |
|---|---|
| `schemaVersion` | always **1** |
| `historyAvailable` | **true** when `totalEntries > 0` **or** bootstrap/history flags say history exists — **never** false while `files`/`entries` non-empty |
| `bootstrapHint` | null when history available; bootstrap command string when empty |
| `commitHash` | **full** hash (no abbreviate) |
| `priorDisplayScore` / `delta` | omit when `sampleCount < 2` (never invent Δ from 0) |

### Invocation

```powershell
ledgerful hotspots trend --json
ledgerful hotspots trend --limit 5 --json
ledgerful hotspots trend --all --json
ledgerful hotspots trend --entity src/lib.rs --json
```

---

## `hotspots explain --json` schema (v1)

Track **0349**. One object. Human remains the default. Overall stop copies
`AnalysisCompleteness` (`scope: overall`, `stage` slug). Couplings skipped for
budget use `couplingsWarning`, not a trusted empty list.

```json
{
  "schemaVersion": 1,
  "kind": "hotspotExplanation",
  "entity": "src/lib.rs",
  "complexity": 4,
  "frequency": 1.2,
  "score": 0.08,
  "displayScore": 4.39,
  "couplings": [],
  "couplingsWarning": "temporal couplings untrusted: overall budget",
  "completeness": {
    "stop": "budget",
    "scope": "overall",
    "stage": "coupling",
    "budgetSecs": 25
  }
}
```

```powershell
ledgerful hotspots --json explain src/lib.rs
ledgerful hotspots --timeout 5 --commits 20 explain src/lib.rs --json
```

---

## `hotspots budget --json` schema (versionless)

Track **0310** / **0353**. Bare object (do **not** add `schemaVersion`). Compares persisted
`hotspot_history.score` (0–1) to an explicit threshold. Informational default
is **0.5**. `--fail` exits **1** on `VIOLATION`, `NO_DATA`, and
`NOT_CONFIGURED`; without `--fail` those statuses still exit **0**. `--fail`
without `--threshold` and without `[hotspots] budget_threshold` is
`NOT_CONFIGURED` (do not invent 0.5 as a CI gate). Only `status == "OK"` is
in-budget. `next` is print-only; do **not** auto-run `hotspots --snapshot`
(writes `.ledgerful/`). Informational `--json` has no budget-owned stderr
tokens; a pre-existing parent `[STALE]` banner from `warn_if_stale` is not
budget-owned.

```json
{
  "status": "OK",
  "dataset": "hotspot_history",
  "scoreUnit": "score",
  "threshold": 0.5,
  "thresholdSource": "default",
  "evaluated": 12,
  "violations": [],
  "snapshotAt": "2026-01-01T00:00:00Z",
  "snapshotAgeSecs": 3600,
  "head": "8e0f41d7…"
}
```

| Key | Rules |
|---|---|
| `status` | always `OK` \| `VIOLATION` \| `NO_DATA` \| `NOT_CONFIGURED` |
| `dataset` | always `"hotspot_history"` (including `NOT_CONFIGURED`) |
| `scoreUnit` | always `"score"` — do **not** emit on list / session / trend / MCP / API |
| `threshold` / `thresholdSource` | omit on `NOT_CONFIGURED`; source is `cli` \| `config` \| `default` |
| `evaluated` | finite rows in the latest snapshot (0 on `NO_DATA`) |
| `violations` | always an array; empty unless `VIOLATION`; items `path`, `score`, `threshold` |
| `snapshotAt` / `snapshotAgeSecs` | latest `MAX(timestamp)` when present and parseable |
| `head` | omit when HEAD id unknown |
| `legacyScoreCount` / `skippedNonFinite` | omit when 0 |
| `emptyReason` | omit-empty; `noSnapshot` \| `allNonFinite` only when `status` is `NO_DATA` (camelCase like `noMatches` / `emptyIndex`) |
| `next` | omit-empty; only `ledgerful hotspots --snapshot` when `emptyReason` is `noSnapshot`. Print only; do not auto-run. Omit on `NOT_CONFIGURED` |
| `schemaVersion` / `provenance` | **never** |

Invalid `--threshold` is clap usage (exit **2**), not a status.

```powershell
ledgerful hotspots budget --json
ledgerful hotspots budget --threshold 0.5 --json
ledgerful hotspots budget --fail --threshold 0.5 --json
```

---

## `symbols --json` schema (v1)

Track **0163**. Pure stdout object — scoped, index-backed symbol inventory
(not search ranking). Default `--limit 200`, hard max **5000** (clap range).
`totalMatching` is a true `COUNT(*)` under the same filters (not overfetch).
Identity for consumers (e.g. AI-Brains T233): **`(path, name, kind)`** (+
`line` when present) — **`qualifiedName` is not globally unique**.

```json
{
  "schemaVersion": 1,
  "scope": {
    "path": "src/commands",
    "changed": false,
    "kind": "Function",
    "pubOnly": true
  },
  "limit": 200,
  "truncated": false,
  "resultCount": 12,
  "totalMatching": 12,
  "symbols": [
    {
      "name": "from_row",
      "kind": "Function",
      "path": "src/state/storage/timings.rs",
      "line": 204,
      "isPublic": true,
      "qualifiedName": "TimingSample.from_row"
    }
  ],
  "indexStatus": {
    "state": "missing",
    "remediation": "ledgerful index --incremental"
  }
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | always **1** |
| `scope.path` / `scope.kind` | **null** when unset; `kind` is **canonical** PascalCase (never raw `fn`) |
| `scope.changed` / `scope.pubOnly` | always present bools |
| `truncated` | `totalMatching > limit` |
| `resultCount` | `symbols.len()` (≤ limit) |
| `totalMatching` | **COUNT** before limit (same filters) |
| `symbols[].line` | **omit** when unknown — never JSON `null` |
| `symbols[].qualifiedName` | omit when empty, absent, or equal to `name`; not a global vault key |
| `indexStatus` | **optional**; omit when index usable. When present: `state` + optional `remediation`. Used when `ledger.db` is **missing** without `--auto-index` (exit **0**, empty `symbols`). Other open failures propagate as errors (not silent empty). |
| Empty | full envelope, `symbols: []`, exit **0** |

### Flags

| Flag | Notes |
|---|---|
| `--path` | **Prefix** (`file_path == prefix` OR starts with `prefix/`); not endpoints substring. Trailing `/` trimmed; empty-after-trim → error |
| `--changed` | WT change set ∩ indexed paths; empty change set → empty inventory exit 0; **includes Deleted** and rename **old_path** still in index until re-index; membership **case-insensitive on Windows only** |
| `--kind` | Single kind; aliases (`fn`→`Function`, `mod`/`module`→`Module`, …); Class populated (C++/TS); Interface populated (TS/Go; an index may have 0 rows). Rust inherent impl methods are Function + `Type.method`; trait methods are Method |
| `--pub` | `is_public = 1` |
| `-l/--limit` | default 200; range `1..=5000` |
| `--auto-index` | bootstrap missing DB + `try_auto_index`; fatal under `--json` emits no partial machine stdout |

### Invocation

```powershell
ledgerful symbols --path src/commands --pub --limit 50 --json
ledgerful symbols --changed --json
ledgerful symbols --kind fn --path src/cli --json
```

---

## `dependencies list --json` schema (v1)

Track **0153**. Pure stdout object (not a bare array). Default mode lists
**declared direct** dependencies from live `Cargo.toml` with locked versions
from the **root package’s** `Cargo.lock` `dependencies` array. Full lock is
`--all`. No Cozo / index required. No progress on stdout under `--json`.

```json
{
  "schemaVersion": 1,
  "mode": "direct",
  "ecosystem": "rust/cargo",
  "root": {
    "name": "ledgerful",
    "version": "0.2.7",
    "source": "manifest"
  },
  "directCount": 96,
  "lockPackageCount": 859,
  "packages": [
    {
      "name": "clap",
      "version": "4.6.1",
      "kind": "normal",
      "ecosystem": "rust/cargo",
      "source": "registry+https://github.com/rust-lang/crates.io-index",
      "optional": false,
      "req": "4.6.1"
    }
  ]
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | always **1** |
| `mode` | `"direct"` (default) or `"all"` (`--all`) |
| `ecosystem` | `"rust/cargo"` for this surface |
| `root` | Live `[package]` name + version; `source` is `"manifest"` when version is a plain string in the manifest, or `"lock"` when filled from the root lock package (e.g. `version.workspace = true`) |
| `directCount` | Declared direct rows after kind expansion (all kinds + target tables) |
| `lockPackageCount` | `[[package]]` count from live lock (0 if no lock) |
| `packages` | Direct rows (`mode=direct`) or full lock rows (`mode=all`); sorted kind then name (direct) or name/version (all) |
| `packages[].version` | Locked version string, or **omitted/null** when not selected / no lock |
| `packages[].kind` | `normal` \| `build` \| `dev` in direct mode; omit in `--all` |
| `packages[].source` | Lock source string when known; omit/null for path/workspace |
| `packages[].optional` / `req` / `target` | Present when known from manifest (direct mode) |
| `truncated` | **Omitted** (no truncation logic on this surface) |

### Invocation

```powershell
ledgerful dependencies list --json
ledgerful dependencies list --all --json
ledgerful dependencies list -v --json
```

---

## `dead-code --json` schema (v1)

Track **0149**. Single camelCase object on stdout. Spinners, human tables,
SUCCESS lines, and stale-index banners are off under `--json`.

```json
{
  "schemaVersion": 1,
  "threshold": 0.75,
  "limit": 50,
  "includeTraits": false,
  "includeTests": false,
  "includeVendor": false,
  "truncated": false,
  "findingCount": 1,
  "findings": [
    {
      "symbolName": "unused",
      "filePath": "src/u.rs",
      "confidence": 0.81,
      "factors": [
        "noTestCoverage",
        { "gitInactive": { "daysSinceLastCommit": 42 } }
      ],
      "recommendation": "…",
      "lineStart": 10,
      "lineEnd": 20
    }
  ],
  "heuristicNote": "Heuristic evidence — not proof of dead code. Factors include reachability, git activity, and test coverage."
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | number | Always **1** |
| `threshold` / `limit` / `includeTraits` | echo of CLI flags | |
| `includeTests` / `includeVendor` | bool | Echo of `--include-tests` / `--include-vendor` (always present; default false). Additive 0314; schemaVersion stays 1 |
| `truncated` | bool | **Honest overfetch:** `scan_repo(limit + 1)` then `truncated = len > limit`; display cap is `limit` |
| `findingCount` | number | `findings.len()` after cap |
| `findings` | array | Sorted confidence desc; reuses `DeadCodeFinding` Serialize |
| `findings[].factors` | **mixed shape** | Unit variants serialize as **camelCase strings** (`"noTestCoverage"`, `"unreachableFromEntrypoints"`); `GitInactive` is an object `{"gitInactive":{"daysSinceLastCommit":N}}`. **Do not** flatten to string-only. **0363:** labeled trait-impl callables omit `unreachableFromEntrypoints` when dispatch is Ambiguous/unresolved (unknown reachability). No new finding keys |
| `heuristicNote` | string | Always present; findings are heuristic, not proof |

**Empty results:** `findings: []`, `findingCount: 0`, exit **0**.

### Invocation

```powershell
ledgerful dead-code --json --threshold 0.75 --limit 50
```

---

## List `--json` envelope (0207)

Populated and empty **list** commands share one object family. Collection
field names stay command-specific (`results` / `impacted` / `files` /
`models` / `gates` / `mappings`). No `--json-raw`. Nested item keys such as
`file_path` / `slo_count` stay as-is.

```json
{
  "schemaVersion": 1,
  "results": [ { "method": "GET", "path": "/health" } ],
  "resultCount": 1
}
```

| Command | Collection key | Extra |
|---|---|---|
| `endpoints --json` | `results` | Empty keeps `emptyReason`/`message`. Always `includeFixtures` (bool) + `fixturesOmitted` (omitted `(method, path, framework)` tuple count; 0 when included or none omitted). Default product inventory omits `is_test_path` registration files and `route_source == "TEST"`. `--include-fixtures` restores both. Post-omit product-empty without `--changed` is `emptyReason: noMatches` (message names N omitted fixture routes; not `cleanDiff` / `noIndexedData`). `noMatches` means no rows survived default omit after the current SQL/filter — not that the catalog has zero product routes. `--changed` empty stays `cleanDiff`. Item additive camelCase (omit-empty): `registrationFile`, `handlerFile` (only when different from registration), `handlerUnresolvedReason` (`closure` \| `not_identifier` \| `missing_symbol` \| `unresolved`), `mountPrefix`, `mountedPath`, `mountProvenance` (`nest` \| `literal` \| `unknown`), `authSource: "inferred"` when `auth` is present, `authParse`/`consumersParse: "invalid"` on malformed stored JSON (no silent null). Stored `path` stays nest-relative. MCP `endpoints_changed` rides this envelope with default omit. |
| `config schema --json` | `results` | Empty keeps `emptyReason`/`message`. Additive omit-empty `filePath` / `requiredness`. |
| `config verify --json` (success) | bare array | Unwrapped `[{section, rows}]`. Four-section health catalog (Backend, Semantic, Ask, Gate) is **human-only**. Additive omit-empty row `origin` / `location`. Config-backed / Default semantic concurrency rows emit origin via `toml_key`; Auto/Cli/derived rows omit both. Inherited concurrency keeps `"source": "inherited"` when origin is file. Empty/whitespace TOML `base_url` origin agrees with `resolve_string` (whitespace is `file`, not `env` while the cell is TOML whitespace; empty after env/dotenv miss is `default`, not `file` for a placeholder). Empty `base_url` with no env/dotenv is omitted without `--verbose`. Not a schemaVersion object. |
| `config view --json` | Config dump | Redacted resolved `Config` object (or `--section` / `--key` slice). **No** `schemaVersion`. Secrets are `[REDACTED]` or `(not set)`. Injects `verify.effective_mode` + `rules_source`. Human unscoped Next names `--section` / `--key` and `config verify --verbose`; `--json` / `--section` / `--key` omit Next. Not Daily 5. |
| `config verify --json` (fail) | object | `{success: false, errors, schemaVersion: 1, kind: "configVerify", ok: false}`. |
| `security impact --json` | `impacted` | `indexedCount` (unfiltered 0208-C denominator) on empty and populated. Additive envelope: `scope` (`inventory` unfiltered, `changed` with `--changed`), `authorization: "declared"`, `coverage` (`SecurityCoverage` camelCase). `coverage.policies` **equals** `indexedCount`. `coverage.linkedEndpoints` = unique refined `protected_by` **endpoint** target ids (service/deploy/config/adr excluded). `coverage.indexedEndpoints` = raw Cozo `category == endpoint` node count (fixtures may be included; not the 0315 product-omit list). On success, `coverage.limitation` is exactly `Declared Cedar @id coverage only. Daemon auth is Bearer (0090), not a PDP.` (no 0186 pack sentence). When coverage probes fail after the 0208-C policy list is already in memory, the inventory still emits: `linkedEndpoints`/`indexedEndpoints` are 0 and `limitation` is that string plus ` Coverage probes failed; linkedEndpoints and indexedEndpoints are unavailable.` JSON `label` is the resolved operator id (`annotations` → raw `@id` → stored `cedar_id` → stored label). Item additive: `enforcement: "none"` on each object in non-empty `impacted[]` (absent when the array is empty); `declaredAction` omit-empty `"METHOD path"` from policy raw (never `"UNKNOWN"` / `null`). JSON `id` stays the URN. |
| `security boundaries --json` | `boundaries` | Unwrapped object (`meta` + `boundaries`). **0359 freeze lift:** additive `schemaVersion` **1** (was absent) + `kind: "securityBoundaries"`. Additive collection `links[]` (operator `@id` → route; `resultCount` counts **links**, not policies). Item camelCase: `id`, omit-empty `sourceFile` / `effect` / `method` / `path` / `sourceMissing`, always `enforcement: "none"` / `linkKind: "inferred"` / `relation` / `targetLabel` / `targetCategory`. CLI-only bespoke `freshness` `{status: "available"\|"empty", source: "cozoGraph"}` is **not** `SurfaceFreshness` (0313). Additive top-level `"pdp": false` on empty and populated. Additive CLI-only `authorization: "declared"` + the same four-key **success** `coverage` object as impact (`policies` / `linkedEndpoints` / `indexedEndpoints` / success `limitation`). Boundaries **hard-fails** on coverage probe `Err` and never emits the impact-only probe-fail limitation. Empty keeps `emptyReason`/`message`. MCP `security_boundaries` shells this CLI and inherits the additive keys. `GET /api/security/boundaries` stays `{meta, boundaries}` — no `pdp`, no `coverage`, no `schemaVersion`, no `links`; `boundary_edges[]` does not grow `source_file` / `effect`. **0208** `security impact --changed` is a different command. |
| `observability coverage --json` | `results` | Item `slo_count` / `metric_count` stay snake. Always `inputs` + `notWired: ["endpoints"]` (lowercase command-mode token). Item always `health`. `--preview` sets `preview: true` and never opens Cozo. Omit-empty `parseErrors[]`. |
| `observability diff --json` | `changed` | `kind: "observabilityDiff"`. Always `unchanged_count` / `indexedCount` / `resultCount`. Item omit-empty `sourceFile`. `--preview` sets `preview: true`. Omit-empty `parseErrors[]`. |
| `data-models list --json` | `models` | Item `file_path` stays snake. Always `includeFixtures` (bool) + `fixturesOmitted` (omitted `(name, language, kind, file_id)` identity count; 0 when included or none omitted). Always sorted `supportedExtractors` (`goJsonTaggedStruct` / `pythonModelPath` / `rustPersistenceDerive` / `typescriptEntityOrModelDir`) and `notWired` (`cppWalker` / `javascript` / `sqlMigrations`). Omit-empty `next` (`ledgerful data-models list --include-fixtures`) only when `!includeFixtures && fixturesOmitted > 0` and the collection is empty. Default product inventory omits `is_test_path`. `--include-fixtures` restores. Item `fieldImpact: "unsupported"` on each `models[]` object; **absent** when the array is empty (no envelope `fieldImpact`). Post-omit product-empty is `emptyReason: noMatches` (not `noIndexedData`); message names persistence derives and says SQL migrations are not extracted. Truly empty catalog stays the 0207 list envelope (no `emptyReason`). Human truly-empty prints a second extractor line. No `fields` / `table` / `evidence` keys. No MCP tool. |
| `data-models impact --json` | `impacted` | Same fixture flags, `supportedExtractors` / `notWired`, and item `fieldImpact` as list. `--changed` stays 0146 path match then omit. Empty `--changed` with no dirty product models is `cleanDiff` / `No changed data models found.` (no compatibility-risk claim). JSON `message` unchanged when `fixturesOmitted > 0`; **human** empty `--changed` also prints the omit footer (`N fixture models omitted. Pass --include-fixtures to show them.`) — not a JSON field. Post-omit empty without `--changed` is `noMatches`. `noIndexedData` copy names supported extractors and says SQL migrations are not extracted (does not claim SQL table/migration extract). Omit-empty `next`: include-fixtures command when product-empty and the flag is off; `ledgerful index --incremental` on `noIndexedData`; omit on `cleanDiff`. COUNT is queried on the outer `Result` before the empty helper. |
| `hotspots --json` (list + `--semantic`) | `files` | List and `--semantic` echo `limit`. No `truncated` (no extra overfetch). Live list always emits `provenance` (0309); `--semantic` omits it. Item `presence: "historical"` only when HEAD is resolvable and the path is absent from HEAD (omit when current, when HEAD is unborn/unresolvable, or on `--semantic`; not a field on shared `Hotspot`). CLI default omits test/example/bench paths; `--include tests` is the unfiltered audit view (0222, includes vendor). CLI default also omits `.md`; `--include docs` ranks markdown by frequency (`score` = `f_norm`, `complexity` 0). CLI default also omits vendored `deps_src`/`vendor`/`third_party`; `--include vendor` restores `f×c` (tests + docs still omitted). `--entity` into a vendored subtree needs `--include vendor`. `--semantic` ignores `--include`. Default and `--include tests` / `--include vendor` item `score` is 0–1 (`f_norm × c_norm`); `displayScore` is `ln_1p(score × 1000)` for humans. Item `complexity` is `MAX(MAX(cognitive, cyclomatic))` across current-index symbols (`project_symbols`; impact `symbols` only if the file is unindexed). C++ `function_definition` is scored on its `body`. AI-T252 must pin `score`, not `displayScore`. No `scoreUnit` key. |
| `ci list --json` / `ci diff --json` (alias) | `gates` | Empty catalog: `gates: []`, `resultCount: 0`, **no** `emptyReason`. `list` is primary. Always `scope` as above. Item `filePath` (JOIN `project_files`; never JSON `null`) + `triggers[]` (GHA event names at two-space `on:` indent, including inline-valued children; GHA `[]` may be an unrecognized `on:` shape). Omit-empty `jobIf` / `needs` / `uses`. Sorted `(filePath, job, platform)`. |
| `services list --json` / `services diff --json` (alias) | `results` | Gated empty keeps `emptyReason: "disabledByConfig"` + `message`. `list` is primary. Always `inferenceState` (`disabledGlobally` \| `disabledForServices` \| `enabled`). Omit-empty `declared[]` `{name, root}`. `--preview` sets `preview: true` (in-memory infer; does not persist `service_name` and does not write the 0300 cookie). Item `source`: `declared` if the name is in `[services]`, else `preview` on `--preview`, else `inferred`. Omit-empty `root` (config root or preview directory; omit on persisted inferred). `--full` omit-empty `files` (cap 200) + `filesTruncated`. |
| `tests --json` (mapped) | `mappings` | Additive `resolvedPath` (omit when none). Item additive `kind`, `location`, `selector` (omit-empty; compat `test` stays `path::symbol`; `selector` is `{runner, testFile, testName, stem?}` — never nextest `+` / `-E`). Top-level `freshness` is **one object** (`id: "mapping"`) when classifiable — not `freshness[]` (0313 `index --check` owns `surfaces[]`). Empty arms use the helper (reason follows `surfaces[]` honesty: “up to date with index head” only when both heads are present and equal). Missing entity (no `--entity` / positional) is a usage error (exit 2, empty stdout), not an empty `mappings` envelope. |

Empty helper arm: `emptyReason` + `message` present. Populated helper arm:
those keys **omitted** (never JSON `null`). `schemaVersion` stays **1**.
No top-level `kind` on this helper (0180 `kind` is gitScan-only).

**MCP honesty:** `endpoints_changed` text is the CLI envelope (re-exec
`endpoints --changed --json`). MCP `hotspots` is in-process and **remains a
hotspot array** — do not parse it as `{files:[…]}`. MCP and `/api/hotspots`
stay **unfiltered** (0222); only the CLI list/JSON list default excludes
tests/examples/benches, markdown, and vendored trees (`deps_src`/`vendor`/…).

### `hotspots --json` `files[]` score units (0222 / 0293 / 0297 / 0299)

| Field | Unit | Consumer |
|---|---|---|
| `score` | 0–1 (`f_norm × c_norm` on default / `--include tests` / `--include vendor`; `f_norm` on `--include docs`) | Agents — pin this (AI-T252) |
| `displayScore` | ln display (`ln_1p(score × 1000)`) | Human table / dashboard |
| `complexity` | max across symbols (`MAX(MAX(cognitive, cyclomatic))` on current index); C++ functions are body-scoped | Ranking input `c_norm`; not an arbitrary first symbol |

Do **not** add a third score field. SchemaVersion stays 1. Human CLI tables print `displayScore` under the **Display** column (not a bare `Score` header). Session `files[]` uses the same two fields; adding `displayScore` there is not a third field.

---

## `index --check --json` schema (0149 purity + 0207 camelCase)

CLI DTO at the print site. Domain `IndexStatus` / `IndexFreshnessAssessment`
stay snake internally (no `rename_all`). On **success**, human Info lines
(e.g. `Index is up to date.`) are **not** emitted on stderr (0149; previously
Info was routed to stderr under json, which broke `2>&1 | ConvertFrom-Json`).
On **failure**, Error diagnostics still go to **stderr** (including under
`--json`) so CI gates keep a human reason; JSON is printed first when the
check path still emits status before `process::exit`.

```json
{
  "schemaVersion": 1,
  "kind": "indexCheck",
  "totalFiles": 760,
  "totalSymbols": 19134,
  "staleFiles": 0,
  "lastIndexedAt": "2026-08-22T12:00:00Z",
  "assessment": {
    "state": "FreshPopulated",
    "staleFiles": 0
  },
  "surfaces": [
    {
      "id": "mapping",
      "status": "stale",
      "source": "indexHead",
      "reason": "index head_hash (250c7afe) ≠ compared head (96d46c10)",
      "refresh": "ledgerful index --incremental"
    }
  ]
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | number **1** |
| `kind` | always **`"indexCheck"`** |
| `totalFiles` / `totalSymbols` / `staleFiles` / `lastIndexedAt` | camelCase; `lastIndexedAt` omitted when absent |
| `assessment.state` | Enum **values** stay **PascalCase**: `FreshPopulated`, `ContentStalePopulated`, `NeverIndexed`, `StaleEmpty`, `StalePopulated`, `FreshEmpty`, `Indeterminate`. Nested `emptyReason` / `source` values also PascalCase (`AllIndexableCandidatesIgnored`, `RepositoryMetadata`, …) |
| Nested assessment fields | camelCase (`emptyReason`, `staleFiles`, `emptyDiagnostics`, `indexedFiles`, …). Absent optionals **omitted** (never JSON `null`) |
| `surfaces[]` | Additive (0313). Typed per-surface freshness (`files` / `symbols` / `routes` / `mapping` / `embeddings`). `status` is camelCase `available`/`stale`/`unavailable`. `assessment.source` stays `FreshnessSource` (PascalCase); row `source` is `SurfaceFreshnessSource` (`contentHash`, `indexHead`, …). Omit-empty. `--strict` / exit codes stay file-hash only. `symbols.reason` must not claim a content-hash match when `status` is `stale`. Empty derived `reason` appends “up to date with index head” only when both `indexedHead` and `comparedHead` are present and equal; empty stays `available` (0135). |

`assessment.state` already carries Fresh/Stale — do not require stderr Info
for machine consumers. **Ban:** `FreshPopulated` with top-level `staleFiles > 0`.

---

## `index --repair-metadata --dry-run --json` schema (0371)

CLI DTO at the print site. Domain `IndexFreshnessAssessment` stays snake
internally. This is **not** `kind: indexCheck`. Assessment is the **age-only**
freshness result (no content-drift walk). Pretty JSON (`to_string_pretty`)
plus one trailing newline.

```json
{
  "schemaVersion": 1,
  "kind": "indexRepairPreview",
  "executed": false,
  "dryRun": true,
  "assessment": {
    "state": "FreshPopulated",
    "source": "RepositoryMetadata",
    "indexedFiles": 1,
    "staleFiles": 0,
    "unindexedFiles": 0,
    "lastIndexedAt": "2026-09-17T12:00:00Z",
    "daysSinceIndexed": 0
  },
  "proposed": [
    "force full index",
    "replace metadata if successful"
  ]
}
```

| Field | Rules |
|---|---|
| `schemaVersion` | number **1** |
| `kind` | always **`"indexRepairPreview"`** — not `indexCheck` |
| `executed` | always **false** on this path (mirrors `verifyDryRun`) |
| `dryRun` | always **true** on this path (mirrors federate export preview) |
| `ok` | **omit** |
| `assessment` | Age-only. Always `state`, `source`, `indexedFiles`, `staleFiles`, `unindexedFiles`. Omit-empty: `emptyReason`, `lastIndexedAt`, `daysSinceIndexed`, `samplePaths`, `warnings`. **Omit `emptyDiagnostics`.** `staleFiles` / `unindexedFiles` are hard-coded **0** and are not a drift verdict. PascalCase enum values. |
| `proposed` | **sorted** string array; locked tokens `force full index` and `replace metadata if successful` |

Executed `--repair-metadata` (including `--yes` and `--json` without `--dry-run`)
does not emit this envelope.

---

## `search --json` schema (v1)

Track **0136**. Single camelCase object on stdout (same mental model as
`doctor` / `change-context` / `verify`). Whole-stdout parsers
(`ConvertFrom-Json`, one `serde_json::from_str`, MCP tool text) succeed on
multi-hit output.

```json
{
  "schemaVersion": 1,
  "query": "change-context",
  "mode": "bm25",
  "limit": 3,
  "truncated": false,
  "resultCount": 3,
  "results": [
    {
      "kind": "bm25_match",
      "path": "src/commands/change_context/mod.rs",
      "score": 16.9,
      "content": "plain snippet (no ANSI, no HTML entities)"
    }
  ]
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | number | Always **1** |
| `query` | string | Echo of the search query |
| `mode` | string | Requested/selected engine: `bm25` \| `regex` \| `semantic` \| `hybrid`. Fuzzy fallback **keeps parent mode**; per-hit source is `results[].kind` (`fuzzy_match`) |
| `limit` | number | Requested limit |
| `truncated` | bool | `true` when overfetch shows more hits than `limit` |
| `resultCount` | number | `results.len()` |
| `results` | array | Match hits only — **not** status/readiness meta |
| `results[].kind` | string | `bm25_match` \| `regex_match` \| `fuzzy_match` \| `insight` |
| `results[].path` | string | Repo-relative path with `/` (Windows `\\` is normalized on emit) |
| `results[].line` | number \| **omitted** | Present only when known — never JSON `null` |
| `results[].score` | number \| **omitted** | Same omit policy as `line` |
| `results[].content` | string | **Plain** preview snippet. When the window is cut mid-identifier, the dangling ident is walked back. Agents pin `path` + `line`; `content` is not the source of truth. |
| `searchIndexStatus` | object \| **omitted** | Empty-index / FTS-rebuild honesty (`state`, `documentCount`, optional `remediation` / `error`) |
| `semantic` | object \| **omitted** | On `--semantic` paths: readiness fields + optional `error` |
| `fallbackUsed` | string \| **omitted** | When hybrid empty path used identifier-literal AllPaths fallback and produced ≥1 hit: `"identifier_literal"`. `schemaVersion` stays **1**; kind vocabulary unchanged (`regex_match` for those hits) |

**Empty results:** full envelope with `results: []`, `resultCount: 0`, exit **0**.

**Fatal auto-index** (`try_auto_index` Err under `--auto-index`): **no** machine
stdout (no partial envelope), non-zero exit; diagnostics on stderr.

### Migration: `--json-lines`

Pre-0136 `--json` emitted **NDJSON** BridgeRecord lines (`record_kind`,
`bridge_version`, timestamps). That stream is opt-in:

```powershell
ledgerful search --json-lines --limit 5 -- "change-context"
```

Do **not** whole-parse `--json-lines` stdout. `--json` and `--json-lines`
conflict (clap reject).

### Invocation

```powershell
ledgerful search --json foo bar
ledgerful search --json --limit 5 -- "change-context" | ConvertFrom-Json
ledgerful search --json --semantic -- "blast radius"
```

Unquoted multi-word argv (`search --json foo bar`) joins to the same `query`
string as `search --json "foo bar"`. Flags may appear before or after words.
Keep `--` for hyphen-leading tokens (`search -- --json`). Shell quotes do not
hide a leading hyphen from clap. Envelope `query` remains one string
(schemaVersion 1; no `queryTokens`).

`--json` stdout is pretty (`output::json::emit` / `to_string_pretty`) with a
trailing newline. Whole-document parse still required. `--json-lines` stays
compact NDJSON (one BridgeRecord per line).

MCP tool `search` spawns `search --json` (envelope; never `--json-lines`).

---

## `search-trigrams --json` schema (v1)

Track **0352**. Hidden CLI. Single camelCase object (`output::json::emit`).
`totalMatching` is Count-backed and is **not** an 0136 `search --json` key.

```json
{
  "schemaVersion": 1,
  "kind": "searchTrigrams",
  "query": ["led", "ger"],
  "accepted": ["led", "ger"],
  "rejected": [],
  "limit": 3,
  "resultCount": 3,
  "totalMatching": 12,
  "truncated": true,
  "documentCount": 12345,
  "next": "ledgerful search led ger",
  "results": [
    { "path": "src/ledger.rs", "score": 1.23 }
  ]
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | number | Always **1** |
| `kind` | string | Always `"searchTrigrams"` |
| `query` | string[] | Raw argv tokens (not a joined 0136 `query` string) |
| `accepted` | string[] | Lowercased 3-character terms actually queried |
| `rejected` | string[] | Trimmed originals that were not 3 characters |
| `limit` | number | Requested `--limit` (clap `1..=5000`) |
| `resultCount` | number | `results.len()` |
| `totalMatching` | number | Tantivy `Count`; `0` when no search ran |
| `truncated` | bool | `resultCount < totalMatching` |
| `documentCount` | number | Tantivy `num_docs` |
| `emptyReason` | string \| **omitted** | `emptyQuery` \| `invalidTrigrams` \| `emptyIndex` \| `noMatches`. Omit when `resultCount > 0` |
| `next` | string \| **omitted** | `ledgerful index` on emptyIndex; `ledgerful search {accepted…}` when accepted is non-empty. Omit on emptyQuery/invalidTrigrams |
| `results[].path` | string | Repo-relative `/` (0298) |
| `results[].score` | number | IDF-only (`IndexRecordOption::Basic`); not a relevance ranking signal |
| `results[].line` / `content` | absent | Do not fabricate |

**Empty results:** full envelope, `results: []`, `resultCount: 0`, exit **0**.
**Success stderr:** empty (diagnostics live in the envelope).
**Clap `--limit` refuse:** empty stdout, exit 2.
**Engine `Err`:** no machine stdout, non-zero, diagnostic on stderr.

No MCP tool. Not Daily 5.

---

## `bridge query --json` schema (v1)

Track **0366**. Hidden CLI. Single camelCase object (`output::json::emit`).
Not BridgeRecord NDJSON (that remains `search --json-lines` / export).

```json
{
  "schemaVersion": 1,
  "kind": "bridgeQuery",
  "ok": true,
  "status": "disabled",
  "query": "configuration provenance",
  "next": "Bridge is disabled. Enable with `bridge.enabled = true` in config or set LEDGERFUL_BRIDGE=1."
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | number | Always **1** |
| `kind` | string | Always `"bridgeQuery"` |
| `ok` | bool | `true` iff `status` is `disabled` \| `empty` \| `populated`. Agrees with process exit |
| `status` | string | Closed: `disabled` \| `unavailable` \| `failed` \| `empty` \| `populated` |
| `query` | string | Original query string |
| `source` | string \| **omitted** | `ipc` \| `cli` when a provider path ran |
| `providerCommand` | string \| **omitted** | Config echo when the CLI path was attempted |
| `resultCount` | number \| **omitted** | Only on `populated` |
| `results[]` | array \| **omitted** | Only on `populated`. `{memoryId, relevance, content}` sorted by `memoryId` then `relevance` (`f64::total_cmp`). Non-finite `relevance` omitted (not JSON `null`) |
| `skippedLines` | number \| **omitted** | Unparseable lines + non-Insight records + non-finite Insights. Omit when 0 |
| `message` | string \| **omitted** | Failure detail (no secret, no raw argv) |
| `next` | string \| **omitted** | Opt-in / allowlist / provider next. Generic; no product name |

**`disabled` / `empty`:** exit **0**, omit `results` / `resultCount`. `--json` stderr empty.
**`unavailable` / `failed`:** JSON then exit **1**.
**Human `disabled`:** stderr enable hint (0065) plus stdout `Status: disabled`.

No MCP tool. Not Daily 5.

---

## `bridge export` Snapshot `datasets[]` (0367)

Hidden CLI. Body is **BridgeRecord 0.3** (`record_kind: snapshot`), not a
`kind: bridgeExport` envelope. `payload.datasets[]` items are camelCase.

Row set: always `impact`, plus one row per requested flag
(`--hotspots` / `--ledger` / `--madr`). Sorted by `name`.
`metadata.hotspot_count` / `ledger_count` equal the matching row `count`
(or `"0"` when that row is absent).

| Field | Notes |
|---|---|
| `name` | `hotspots` \| `ledger` \| `madr` \| `impact` |
| `requested` | clap flag; `impact` is always `false` |
| `included` | rows (or the impact packet) serialized |
| `count` | always. `impact` is **1**. Else vec length; `0` on empty / notWired / error |
| `source` | omit-empty: `live` \| `ledgerSqlite` \| `workingTreeImpact` |
| `commitsRequested` / `commitsWalked` | omit-empty; hotspots only |
| `stop` | omit-empty; hotspots only; `budget` \| `cancelled` |
| `limit` | omit-empty; hotspots + ledger |
| `filter` | omit-empty; hotspots; `"unfiltered"` or comma-joined prefixes |
| `emptyReason` | omit-empty: `noMatches` \| `historyError` \| `ledgerError` \| `notWired` |
| `next` | omit-empty; `--madr` is `ledgerful ledger adr export` |

`--json` with no `--out` implies stdout. `--json --out <path>` writes the file
and leaves stdout empty. `-o -` is stdout (not a file named `-`).
`--stdout` + `--out <path>` errors. Compact one-line is NDJSON-compatible.

No MCP tool. Not Daily 5.

---

## `change-context --json` schema (v1)

Track **0114**. Canonical agent-consumable change packet composing impact
structure, doctor readiness, open ledger work, and a budgeted `readSet`.

```json
{
  "schemaVersion": 1,
  "status": "ready",
  "summary": "…",
  "headHash": "…",
  "baseRef": "origin/main",
  "riskLevel": "medium",
  "riskReasons": ["…"],
  "readSet": [
    { "path": "src/foo.rs", "reason": "changed", "priority": 1 }
  ],
  "readSetCapped": false,
  "readSetTotalCandidates": 1,
  "blast": {
    "depth": 1,
    "mustTouchFileCount": 0,
    "mustTouchSymbolCount": 0,
    "confidenceSummary": {
      "scipBound": 0,
      "resolved": 0,
      "ambiguous": 0,
      "unresolved": 0,
      "capped": 0,
      "unknown": 0,
      "expandable": 0,
      "total": 0
    }
  },
  "testCoverage": {
    "status": "available",
    "sourceSeedCount": 12,
    "mappedCount": 7,
    "fileMappedCount": 2,
    "unmappedCount": 3,
    "unmappedCapped": false,
    "unmappedTotal": 3,
    "unmapped": [
      {
        "symbol": "execute_foo",
        "file": "src/commands/foo.rs",
        "qualifiedName": "commands::foo::execute_foo",
        "mappingKind": "none"
      }
    ],
    "mappedSample": [
      {
        "symbol": "bar",
        "file": "src/bar.rs",
        "coveringTestCount": 2,
        "mappingKind": "symbol"
      }
    ],
    "notes": [
      "Structural test_mapping only (IMPORT/NAMING_CONVENTION/SAME_FILE); not line coverage",
      "LCOV COVERAGE mapping kind does not currently persist (DDL NOT NULL on test_symbol_id)"
    ]
  },
  "affectedFlows": {
    "status": "available",
    "flowCount": 2,
    "flowCapped": false,
    "flowTotal": 2,
    "flows": [
      {
        "method": "GET",
        "pathPattern": "/api/health",
        "handlerSymbolName": "health_handler",
        "handlerFile": "src/handlers/health.rs",
        "framework": "Axum",
        "matchKind": "handler_impl_file",
        "routeConfidence": 1.0
      }
    ],
    "notes": [
      "Registered HTTP routes only (api_routes); not distributed traces or CRG-style call-chain flows."
    ]
  },
  "changeHints": {
    "kind": "greenfield",
    "mostlyAdded": true,
    "addedCount": 3,
    "totalChanged": 3,
    "newPackagePrefixes": ["src/newpkg"],
    "surfaceTags": ["cli_surface", "new_entrypoint", "new_module"],
    "suggestedTests": [
      {
        "path": "src/newpkg/cli_test.rs",
        "kind": "convention",
        "reason": "conventional test path (to be created)"
      },
      {
        "path": "tests/newpkg/mod.rs",
        "kind": "convention",
        "reason": "conventional test path (to be created)"
      }
    ],
    "notes": [
      "No structural test_mapping for new paths; suggestions are path conventions, not proven coverage."
    ]
  },
  "doctor": {
    "status": "ok",
    "readyForPublish": true,
    "block": 0,
    "warn": 0,
    "info": 0,
    "topFindings": []
  },
  "ledger": {
    "pendingCount": 0,
    "activeTx": []
  },
  "analysisWarnings": [],
  "freshness": [
    {
      "id": "mapping",
      "status": "stale",
      "source": "indexHead",
      "reason": "index head_hash (250c7afe) ≠ compared head (96d46c10)",
      "refresh": "ledgerful index --incremental"
    }
  ],
  "nextActions": [
    "ledgerful verify --scope fast",
    "review changeHints.suggestedTests and add covering tests for new surfaces"
  ],
  "impactSchemaVersion": "v1"
}
```

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | u32 | Always **`1`** for this packet (doctor/verify style) |
| `impactSchemaVersion` | string | Forwarded from `ImpactPacket.schema_version` (different field/type) |
| `status` | string | `ready` \| `empty` \| `not_ready` |
| `summary` | string | Freeform one-line human/JSON summary (0114+). **Unchanged** by 0173. |
| `agentSummary` | object | **0173** structured scannable header. **Coexists** with `summary` — does not replace it. Present on `ready`/`empty`; **omitted** on `not_ready`. Fields: `riskOneLiner`, `changed` (`total`/`code`/`governance`/`contract`), `topSymbols` (≤5), `mustTouchSample` (≤5), `suggestedTestsSample` (≤3), `demotedTemporalCount`, `pathMode` (`code`\|`all`), `analysisMode` (`working_tree`\|`base_ref`\|`prospective`). |
| `readSetCapped` / `readSetTotalCandidates` | bool / usize | **Required**; `true` / total when truncated by `--max-files` |
| `blast` | object | **Counts only** — not full edges. Always includes nested `confidenceSummary` (class counts: `scipBound`, `resolved`, `ambiguous`, `unresolved`, `capped`, `unknown`, `expandable`, `total`) at both `minimal` and `standard` detail. Same shape as ImpactPacket `blastRadius.confidenceSummary`. Full edges remain on `impact --json` only (0114 token-budget fence). |
| `testCoverage` | object | **0115 deepened** structural test-gap report (same schema as PR `testGaps`). Status: `available` \| `empty_mapping` \| `missing_table` \| `no_source_seeds` \| `unavailable`. **Never** bare `"empty"`. Empty `ImpactPacket.test_coverage` vec is **not** full cover — use these counts/status. Caps: unmapped ≤20, mappedSample ≤5. Notes always include structural + LCOV ceiling. |
| `affectedFlows` | object | **0118** nested affected HTTP-route summary (same schema as ImpactPacket / PR `affectedFlows`). Status: `available` \| `empty_map` \| `missing_table` \| `no_change_seeds` \| `unavailable`. **Never** bare `"empty"`. **Route map only** (registered `api_routes` + handler binds + optional blast edges) — **not** CRG-style call-chain / execution-path traces. Present at both `minimal` and `standard` detail. Sample caps: **`flows` take(5)** when `detail=minimal`, **take(10)** when `standard` (counts `flowCount`/`flowTotal` pass through full report; impact library cap is 20). `endpoints --changed` uses the **same match library with uncapped keys** (filter is not truncated at 20). `available` + `flowCount` 0 = all-clear (no registered routes touched). **Framework fence:** Rust Axum/Actix/Rocket; Go Gin/`net/http`; TS Express/Fastify; Python FastAPI/Flask — not all languages. **Agent metadata:** CRG may ship `context_savings` (token estimates); Ledgerful ships `affectedFlows` + `testCoverage`/`testGaps` + blast/`confidenceSummary` counts — **different signals**, not substitutes. Go route extractors exist; CRG-style path recall on Go is weaker (~33% on some peer fixtures) — registration map only here. See `docs/Call-Resolution.md` §Affected HTTP flows. |
| `changeHints` | object | **0127** greenfield / new-surface hints. Present only when file changes exist; **omitted** on `status=empty` / `not_ready` (no clean-tree noise). `kind`: `greenfield` \| `mixed` \| `none`. Pure-add = `status==Added` **and** no `old_path` (renames excluded). `mostlyAdded` when pure-added/source-like ≥ 0.6 (or all-added ≥2). `newPackagePrefixes` ≤5 (isolated pure-add dirs). `surfaceTags`: `new_module` / `new_entrypoint` / `cli_surface` / `new_test` (path/basename primary). `suggestedTests` ≤10 path-unique, ladder **mapped → convention → adjacent** (`kind` + honesty `reason`). Convention reasons encode exists-on-disk vs to-be-created — **not** proven coverage. Notes cap ≤5. Summary may append `greenfield-ish (N added / M total; prefixes: …)` (≤3 prefixes, last-2 segments if deep). |
| `doctor` / `ledger` | object | **Always present** on successful builds (including `status=empty`) |
| `completeness` | object | **0347** additive. Omitted when no overall stop and any history walk completed. Overall stop: `stop` `budget`\|`cancelled`\|`error`, `scope: "overall"`, `stage` slug, `budgetSecs`. Stable `stage` slugs: `federated`, `api`, `data_models`, `contracts`, `ci_gates`, `infrastructure`, `environment`, `observability`, `coupling`, `deploy`, `ci_self_awareness`, `ci_predictor`, `hotspots`, `coverage`, `services`, `runtime_usage`, `signature_delta`, `dead_code`, `kg`, `adr`, `knowledge`, `analysis` (post-score cancel, 0374). CLI `deploy impact` overall stop (0358) uses stage `deploy` (not an enrichment-provider walk). Unknown provider names fall back to `enrichment`. Unscoped `audit` pipeline slugs (0350, **not** enrichment providers): `storage` \| `velocity` \| `federated` \| `churn` \| `hotspots` \| `ci_trend` \| `recent`. Unscoped audit overall `cancelled` (0376) uses those slugs when cancel fires without Instant; a history-budget hotspot walk stays history-only (`scope` omitted). History-only 0308 objects omit `scope` and keep `filter`/`commitsRequested`. |
| `doctor.topFindings` | array | From sidecar `findings` after a successful `doctor` write (0129 + **0138**): **action-critical** only — severity `block` always, or `warn` when category ≠ `optional` (optional-category warns **excluded** so flaky backends do not crowd the cap-5 budget). Severity-first (block before warn), then code/message, **cap ≤5**. Each entry: `code`, `severity`, `message`, optional `remediation` when present (never `null`). **Empty is OK** when the only warns are optional (or only info) — `doctor.warn` may still be >0; inspect full `ledgerful doctor --json` for optional backends. Full `doctor --json` `findings[]` remains the complete SoT and **includes `category`** on each finding (agents can self-filter). Empty also when doctor not run / sidecar missing / pre-0129 count-only sidecar. |
| `analysisWarnings` | array | Ambient analysis health (not diff risk). Empty-tree federation schema-unavailable/invalid lands here (same greppable string as historical medium riskReasons): `Cross-repo impact: Sibling '…' schema is unavailable or invalid.` Clean tree with only those warnings → `riskLevel=low` and empty/non-medium sole riskReasons. Real `[FEDERATED]` modify / interface-removed stay on `riskReasons`. |
| `freshness[]` | array | Additive (0313). Derived surfaces + impact only (`mapping` / `routes` / `embeddings` / `impact`). Omit-empty. Same row shape as `index --check` `surfaces[]` (empty derived “up to date with index head” only when both heads are present and equal; empty stays `available`). Nested `testCoverage.notes` / `affectedFlows.notes` stay the SoT for the free-text “change head” strings. PermissionDenied `not_ready` omits the array (no Class C `refresh`). |
| Empty-tree risk | — | `status=empty` is independent of `analysisWarnings` (file changes + pending ledger only). Do **not** escalate solely because historical medium federation noise — product routes schema-miss to warnings (0129). |

| `status` | When |
|---|---|
| `ready` | Non-empty file changes, **or** clean tree with `ledger.pendingCount >= 1` |
| `empty` | No file changes **and** `ledger.pendingCount == 0` (doctor still present) |
| `not_ready` | Layout/impact hard failure; `reason` + `nextActions` set |

### Stream rules

- `--json` → **machine mode**: pure JSON on **stdout only** (0093).
- Human mode (no `--json`): print **`agentSummary` header first**, then status,
  freeform `summary`, risk, readSet count, readyForPublish, a short
  `freshness:` block when any row is not `available`, then next steps.

### Path mode (code vs governance) — 0173 / 0202

Default **`pathMode=code`**: process/governance temporal couplings (conductor /
deferred / process docs, and code↔governance pairs) are **demoted** from risk
weight/reasons and from `readSet` priority-3. **0202** also demotes
`CHANGELOG.md` temporal pairs (CHANGELOG stays **Contract** / p1-when-changed)
and ancestor-path (directory-prefix) pairs such as `packaging` ↔
`packaging/homebrew/ledgerful.rb`. Full `temporalCouplings` remain on
`impact --json` for audit; `demotedTemporalCount` is honest. Restore pre-0173
process demotion and the **pre-0202 CHANGELOG wall** with
**`--include-governance`** (`pathMode=all`). Other contract allowlist paths
(agent-output-contract, Engineering, SKILL.md, Cargo.toml, …) never demote.

### Prospective `--paths` — 0173

```powershell
ledgerful change-context --json --paths src/foo.rs
ledgerful change-context --json --paths src/a.rs,src/b.rs
ledgerful impact --paths src/foo.rs --summary
ledgerful scan --impact --json --paths src/foo.rs
```

- Synthetic snapshot: on-disk → Modified; missing → **Added** (greenfield).
- `analysisMode=prospective`; `is_clean=false` so empty-tree short-circuit does not fire.
- Mutually exclusive with `--base-ref`. Cap ≤ 50. Empty/whitespace → usage error.
- **Write policy:** prospective does **not** rewrite `latest-impact.json`
  (in-memory only). Working-tree impact without `--paths` keeps current write
  unless the overall analysis stops (`completeness.scope=overall` from a
  budget Instant **or** the cancel flag, including omitted `--timeout` /
  `--timeout 0`) — no truncated persist (0374).
- **Overall deadline (0347):** cooperative provider-boundary Instant (default
  25s on prospective; CLI `--timeout` / env `LEDGERFUL_PROSPECTIVE_BUDGET_SECS`
  / `[impact] prospective_budget_secs`). Not a thread kill. Scoring runs
  **once** after the enrichment loop or stop. Distinct from
  history-walk `[hotspots] history_budget_secs`. Review `--timeout` is an overall emit budget (0348).
  `hotspots` list/explain `--timeout` is an overall emit budget (0349).
  Unscoped `audit` `--timeout` is an overall emit budget (0350).
- **Risk honesty (0284):** prospective `riskReasons` use
  `Public API present (prospective): {path} ({n} public symbol(s))`
  (not `Public symbol modified:`). Per-symbol 30×N does not apply.
  HIGH still possible from protected path / centrality / volume /
  temporal. `schemaVersion` unchanged.

### Docs-mode impact lead — 0227

```powershell
ledgerful scan --impact --mode docs
ledgerful scan --impact --mode docs --json --paths docs/agent-output-contract.md
ledgerful scan --impact --json --paths src/lib.rs,docs/installation.md  # mixed: not docs mode
```

When `--mode docs` is set, or when **every** dirty/prospective path is
documentation-shaped, impact **presentation** leads with ≤5 actionable
couplings / test-gap rows instead of crate co-change trivia:

| Field | Rules |
|---|---|
| `schemaVersion` | string **`"v1"`** (unchanged; not numeric 1) |
| `actionableLead` | Additive array, cap 5, **sorted**. Omitted when empty (non-docs runs). Items: `kind` `coupling` \| `test_gap`; coupling rows have `fileA`/`fileB`/`score`; test-gap rows have `status` / `mappedCount` / `explain` |
| `glossary` | Object with `no_source_seeds` and `mapped=0` explanations. Present in docs mode. Omitted otherwise |
| `temporalCouplings` | **Full list remains** (honesty). Trivia is out of the lead only |
| `agentSummary` | **Not** on `ImpactPacket` — change-context only |
| `latest-impact.json` | **Not rewritten** on `--mode docs` or auto-detect (0221 freeze; in-memory / stdout only) |

`--include-governance` still sets `pathMode=all` weights. Docs mode does not
change coupling math.

### `--base-ref` present-tense rule

**`--base-ref` only time-travels structural impact / `readSet` / risk.** Doctor and
ledger always report **present-tense** local workspace/DB state. CI agents should
still run `doctor --json` first; change-context will surface a missing/stale
sidecar honestly when doctor has not run.

### Truncation

When `readSetCapped` is true, deep-dive with `ledgerful scan --impact --json`
for the full change set. Do not assume a capped `readSet` is complete.

### MCP

Tool name: `change_context` (params: `detail`, `max_files`, `base_ref`,
`blast_depth`, **`paths[]`**, **`include_governance`**). Same builder as the CLI;
impact is in-memory and does **not** rewrite `.ledgerful/reports/latest-impact.json`.

**MCP `scan`:** remains a full-impact dump **without** `paths` / `include_governance`
in v1 — use CLI or `change_context` for prospective / path mode.

### Invocation

```powershell
ledgerful doctor --json
ledgerful change-context --json
ledgerful change-context --json --detail minimal --max-files 5
ledgerful change-context --json --base-ref HEAD~1
ledgerful change-context --json --paths src/impact/analysis/temporal.rs
ledgerful change-context --json --include-governance
```

---

## `session --json` schema (v1)

Track **0224**. One-shot agent briefing. Human `ledgerful session` is a 10-line
summary that **must not** parse as JSON. Agents pass `--json`. Does **not**
rewrite `latest-impact.json`. CLI-only (MCP registry is not one-file additive).

```json
{
  "schemaVersion": 1,
  "kind": "session",
  "git": { "branch": "", "head": "", "dirtyCount": 0, "dirtyPaths": [] },
  "ledger": { "workRoot": "", "pendingCount": 0, "pending": [], "unauditedDrift": 0, "collisions": [] },
  "doctor": { "readyForPublish": true, "block": 0, "warn": 0, "info": 0 },
  "changeContext": {
    "status": "empty",
    "riskLevel": "low",
    "readSetCapped": false,
    "readSetTotalCandidates": 0,
    "readSet": []
  },
  "hotspots": {
    "files": [],
    "excludedTests": true,
    "provenance": {
      "source": "live",
      "commitsRequested": 50,
      "daysRequested": 30,
      "limit": 5,
      "filter": "session"
    }
  },
  "impactCache": { "present": false, "validForHead": false, "treeClean": false },
  "next": ["ledgerful change-context --json"],
  "configChecklist": []
}
```

Session `hotspots.completeness` (0308) is omitted when the 50/30/5 walk finished the requested window; present with `filter: "session"` when the walk stopped early. Session `hotspots.provenance` (0309) is always emitted (`source: live`, `commitsRequested ≤ 50`, `daysRequested: 30`, `limit: 5`, `filter: session`). It never includes `snapshotAt` / `snapshotAgeSecs` (no `hotspot_history` query). Per-file `presence: "historical"` when HEAD is resolvable and a ranked path is absent from HEAD (omit when current or when HEAD is unborn/unresolvable).

| Field | Notes |
|---|---|
| `schemaVersion` | number **1** |
| `kind` | `"session"` |
| `git.dirtyPaths` | cap 5; `dirtyCount` is the true total. Omits `watch.ignore_patterns` (same set as change-context); harness junctions such as `.claude` / `.agents` are not product dirt |
| `ledger.collisions` | 0223 `pending_entity_overlap` vs dirty paths; `[]` when none. **Not** on status v1 |
| `doctor` | sidecar `block`/`warn`/`info` + `readyForPublish`. **No** `warnAction`. Per-finding `sessionPriority` / `acknowledged` stay on `doctor --json` |
| `changeContext.readSetCapped` / `readSetTotalCandidates` | pass-through from `build_change_context` with `max_files=5` (not a post-slice of a 20-file packet) |
| `hotspots.files` | limit 5, `exclude_test_paths: true`, `exclude_vendor_paths: true` (0297; vendor ranks unlike markdown), git walk `commits ≤ min(config, 50)`, `days: 30`. Item keys: `path`, `score` (0–1, pin this), additive `displayScore` (ln), optional emit-time `presence` (`historical` only when HEAD is resolvable and the path is absent; omit when current or HEAD is missing). This is the **second** field on the session item (parity with `hotspots --json`), not a third field on either surface. No `scoreUnit`. No `excludedVendor` key (`excludedTests` stays). Additive `hotspots.completeness` (0308) only when the walk stops early (`filter: "session"`). Always-on `hotspots.provenance` (0309). Session vs CLI list windows differ by design — compare `provenance` before comparing ranks. MCP `hotspots` stays unfiltered |
| `impactCache` | new HEAD comparator: None → all false; Packet → `present`, `treeClean=false`, `validForHead` iff packet head equals live HEAD; CleanTree → `present`, `treeClean=true`, `validForHead` iff tombstone head equals live HEAD |
| `next` | sorted deterministic strings (change-context `next_actions` idiom plus cache/collision notes). **Must not** gain `config set` from the checklist |
| `configChecklist` | always present (may be `[]`). Applicable rows only, sorted by `id`. Keys: `id`, `status` (`ready`\|`gated`\|`empty`\|`optional`), `applicable` (always true), `next` (always a string; `""` when ready except 0185 reuse rows), `alreadyShown`, optional `applyArg`. No `sessionNotices` on this envelope |

When `validForHead` is false, do not read `.ledgerful/reports/latest-impact.json`.

```powershell
ledgerful session
ledgerful session --json
```

---

## Exit codes

### `verify` / signature path (`sig_exit`)

| Code | Meaning |
|---|---|
| `0` | OK |
| `1` | INVALID signature / chain break / verification failed |
| `2` | POLICY (reserved; not currently enforced) |
| `3` | UNSIGNED under `require_signing` or `--strict-signatures` |

For `verify --json`, **`ok` and the process exit must agree**: `ok: true` ⇒
exit `0`; validation rejection ⇒ `ok: false` and non-zero exit with JSON still
on stdout.

### `bridge query`

| Code | Meaning |
|---|---|
| `0` | `status` is `disabled`, `empty`, or `populated` (`ok: true`) |
| `1` | `status` is `unavailable` or `failed` (`ok: false`; JSON still on stdout when `--json`) |

`--json` success (exit 0) has empty stderr. Human `disabled` still prints the enable hint on stderr (0065 keep-green).

### `ledger status --exit-code`

| Mode | Would-block exit |
|---|---|
| enforce | `1` |
| observe (default) | `0` + stderr warning |
| observe + `--strict-observe-signal` or `LEDGERFUL_STRICT_OBSERVE_SIGNAL=1` | `2` |

---

## Three outcomes agents must distinguish

| Outcome | stdout | Exit | Meaning | Agent action |
|---|---|---|---|---|
| **Pass** | valid JSON, `ok: true` | `0` | checks passed | proceed |
| **Validation rejection** | valid JSON, `ok: false` | non-zero (`1`/`3`) | repo failed a check | read `steps[]`, fix code |
| **Fatal execution error** | empty / unparseable | non-zero (`1`, or **`101`** on panic) | tool could not complete a verification result | fix environment / flags; **do not** treat as a clean pass |

A non-zero exit with **no** JSON is **not** a verification result. Always check
**exit code and** that stdout parsed.

### What is (and is not) fatal under `verify --json`

| Case | Behaviour | Outcome class |
|---|---|---|
| Clap / invalid flags (e.g. bad `--scope`, rejected `--json` combos) | no payload; non-zero; message on stderr | **Fatal** |
| Panic in the main thread | exit **101**; no payload | **Fatal** |
| Hard `Err` before the payload is emitted (e.g. cwd unreadable, rejected combo) | no payload; non-zero | **Fatal** |
| Plan step failure after the run completes | **JSON present**, `ok: false`, non-zero | **Validation rejection** |
| **Config load failure** | **not fatal** — warn + defaults (post-0094 honesty path); verification still runs | continue; may see stderr WARN |
| **SQLite / packet open failure** | **not fatal** — prediction disabled with warn; plan still runs | continue; may see stderr WARN |

Do **not** assume "missing `.ledgerful/config.toml` or a soft config parse
error" means no JSON. Soft config and storage failures degrade; only hard
pre-payload `Err`s and panics produce the empty-stdout fatal class.

---

## PowerShell note

`NativeCommandError` under Windows PowerShell **5.1** requires **all three** of:

1. Windows PowerShell 5.1 (not PowerShell 7+)
2. `$ErrorActionPreference = 'Stop'`
3. Stream merge (`2>&1`)

PowerShell 7+ does not throw on stderr alone. Stream discipline cannot eliminate
legitimate warnings forever; the durable agent invocation is:

```powershell
# Supported agent invocation: machine mode (selected by --json alone).
ledgerful verify --json
# --quiet is optional and only collapses cli_summary per-entry detail;
# it is not required for empty-stderr success under --json.
```

Machine mode keeps human product lines off stdout **and** silences normal_layer
progress `INFO` on stderr. A successful plan run under `--json` with no
degradation warnings should write **empty stderr**. Soft config/storage
degradation, would-block observe warnings, and CRITICAL refusals still use
stderr by design — agents must not merge streams under Windows PowerShell 5.1
+ `$ErrorActionPreference='Stop'`.

---

## Related docs

- [`operator-surface-policy.md`](operator-surface-policy.md) §3 — stream authority
- [`verify-performance.md`](verify-performance.md) — what `--scope fast` actually runs
- [`pr-scan-schema.md`](pr-scan-schema.md) — `scan --pr --format json` schema
