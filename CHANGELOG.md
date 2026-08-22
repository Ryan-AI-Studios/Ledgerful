# Changelog

All notable changes to Ledgerful are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Empty-tree `policy check` scare + `ci diff` inventory (0214):** On a
  clean working tree with no pending transactions (or `--pr` with an empty
  change set), `verification_must_pass` with no bound verification run is a
  note, not a violation. Unbound runs still never satisfy the rule; a bound
  fail still violates even when idle. `ci diff` (alias `ci list`) is an
  indexed CI-gate inventory, not a working-tree workflow diff. JSON array
  shape unchanged. No Cargo bump.

- **Doctor header vs Index Health + gemini CLI vs Cloud Ask (0209):** Human
  header “warning(s)” now counts action-critical warns (what Index Health
  expands), with ` · {n} optional` when optional warns exist. JSON
  `summary.warn` stays the all-severity total; additive `warnAction` /
  `warnOptional` (schemaVersion stays 1). Tools table labels `gemini` /
  `gemini-cli` as `gemini CLI` and qualifies NotFound as optional CLI, not
  the Cloud Ask backend. Hygiene trailer names optional warns when present.
  Does not require Gemini CLI install.

- **Doctor `Environment: Wsl` inside Docker Desktop containers (0204):**
  `ledgerful doctor` no longer labels a Linux container as `Wsl` when the
  kernel string is `*-microsoft-standard-WSL2`. Container markers
  (`/.dockerenv`, `/run/.containerenv`, named cgroup) make
  `environment.platform` / human `Environment:` **Linux**. A real WSL
  distro (no container markers) stays **Wsl**. No new OS enum, JSON
  field, exhibit recapture, or crate.

- **Security impact `--changed` CleanDiff (0208):** Count indexed Cedar
  policy nodes before the git-status filter. A clean tree with indexed
  policies now reports CleanDiff (`0 of N policies match changed files`)
  instead of NoIndexedData / “Add Cedar policy files … `index
  --analyze-graph`”. Disk-present uningested Cedar still points at
  `index --analyze-graph` without the add-Cedar lie. Filter-only (0146);
  no JSON array wrap.

- **Hotspots explain complexity (0210):** Metrics `Complexity` now copies
  the list/JSON scoring complexity when a git-history hotspot row exists
  (including zero). When no hotspot row exists, complexity is
  max-across-symbols (`MAX(MAX(cognitive, cyclomatic))`), not the first
  `project_symbols` row. Ranking / `query_file_complexities` unchanged.

- **Clippy 1.98 `chunks_exact_to_as_chunks` (CI):** six blob/UTF-16
  decode sites use `as_chunks` instead of `chunks_exact`. Unrelated to
  0210 scoring; required for ubuntu `-D warnings` on rustc 1.98.

### Changed

- **Engine dogfood Action+engine pin (0198):** Workflow A pins
  `ledgerful-action@bacf400797142884c46e97c6ce755b7ef7433a53` (Action PR #11
  merge SHA) with engine **v0.2.10** and the published Linux gnu sidecar
  checksum. Workflow B uses the same action SHA, report-only (no version /
  checksum). No Cargo bump.

- **Packaging templates + winget pin language (0199):** Homebrew
  formula and Scoop manifest templates pin published **v0.2.10** with
  sidecar sha256 hashes. Installation and package-distribution
  present-tense language floors winget at community-index **0.2.10**
  matching GitHub Latest (PR #421115). Leftover open 0.2.8/#415913 and
  0.2.9/#416853 PRs are skipped/WDSI, not current lag of Latest. 0164
  dual-glob install path unchanged. No Cargo bump.

## [0.2.10] - 2026-08-20

### Security

- **Transitive `h2` 0.4.15 → 0.4.16 (RUSTSEC-2026-0258):** lockfile-only
  bump so required `audit`/`deny` checks pass. Unbounded empty DATA
  frames in the HTTP/2 stack used by hyper/axum. Not an 0193 or 0194
  behavior change.

### Changed

- **Packaging templates + install pin language (0188):** Homebrew
  formula and Scoop manifest templates pin published **v0.2.9** with
  sidecar sha256 hashes. Installation and package-distribution
  present-tense language no longer floors the live engine at v0.2.4;
  winget community-index lag stays honest (last merged 0.2.7; 0.2.9
  is not claimed live). 0164 dual-glob install path unchanged. No
  Cargo bump.

- **Ask routing module split + compile-once regex (0197):**
  `commands/ask_routing.rs` is now a barrel (`parse.rs` + `answers.rs` + tests out).
  Remaining per-call parse regexes compile once via `OnceLock`. Ask early-exit
  banners, parse order, wire order, and SQL are unchanged.

- **LLM client module split + named Rust chunker keep/skip (0196):**
  `local_model/client.rs` is now a barrel (`complete.rs` cascade + tests out).
  The Rust semantic chunker uses exhaustive `is_standalone_chunk_kind`
  (Method stays skip). Timeout numbers, ureq, and completion error
  strings are unchanged.

- **Ledger crypto errors + staged commit (0195):** key I/O and sign
  helpers return `CryptoError` (`thiserror` + miette `Diagnostic`) with
  `#[source]`. `normalize_trusted_public_key` returns `TrustedKeyError`.
  `SignatureVerifyError` / `ChainHeadVerifyError` keep their Display
  text and attach hex / slice / dalek sources. `classify_entry_signature`
  names every option-pair arm; mixed pairs stay `Unsigned`.
  `commit_change` is validate → sign → persist; rollback and reconcile
  share a parameterized sign-or-warn helper. Ed25519 / chain-hash bytes,
  `CURRENT_LEDGER_SIG_VERSION`, and `SignatureTrustStatus::as_str` are
  unchanged. `get_connection` stays pub.

- **Storage / impact encapsulation (0194):** `StorageManager.cozo` is
  reached through `cozo()` / `cozo_mut()` / `set_cozo()`. Impact packet
  schema goldens live in `packet/tests.rs`. Enrichment assignment sites
  use named `set_*` methods. Fields stay pub. `get_connection` stays
  pub. Impact JSON `schemaVersion` remains `"v1"`.

- **Call graph / semantic-index module split (0193):**
  `index/call_graph` is a types/builder/persist barrel.
  `commands/index/semantic` stages parse/embed/persist with
  typed errors and a ProgressStyle `default_bar()` fallback.
  No persist-contract, UNIQUE, or `--semantic --json` change.

- **Ask / change-context / search command-body split (0192):**
  `execute_ask` takes named `ExecuteAskOpts`; gather + legacy complete
  live beside the orchestrator. `change_context` is a directory barrel.
  `search` retrieve/present helpers sit outside the 0128
  FTS-before-semantic orchestrator. No flag, packet, or search-envelope
  change.

- **Verify / doctor / plan module split (0191):** `commands/verify`,
  `commands/doctor` check families, and `verify/plan` are domain barrels.
  Restores the 0137 engine fingerprint (`src/cli/args/mod.rs`) and
  SharedInfra globs (`src/cli/args/**`, `src/cli/dispatch/**`) after the
  0190 CLI move. No new doctor finding, no verify JSON/`--scope fast`
  policy change, no clap flag change.

- **CLI/MCP registration split (0190):** `src/cli/args` and `src/cli/dispatch`
  are domain modules with thin `Commands` / `run_with` barrels. MCP tool
  failures use an internal `thiserror` type, then the same
  `{ content: [{ type: text, text }], isError: true }` tools/call result.
  No new flags, aliases, 0179 parent defaults, MCP tool names, or
  `schema_json` changes.

### Fixed

- **Index pipeline native stacking (0189):** `--analyze-graph` no longer
  re-extracts after main already extracted. `--incremental` no longer stacks
  native `structural_edges`. Stored count on `--full --analyze-graph` is one
  builder pass, not 2×. Stopped inserting another copy of the native pass
  (not a UNIQUE / de-dupe of intra-pass groups). SCIP rows (`evidence` like
  `scip:%`) are left in place. No CLI flag change; SCIP still default-off.

## [0.2.9] - 2026-08-13

### Fixed

- **Federate status hygiene (0184):** Path is peer identity; display/store
  name is the on-disk directory basename (not stale `schema.repo_name` or
  case-variant leftovers). `federate status` is read-only: shows collapsed
  **Live** peers only, omits self/husk/duplicate rows, and prints one honesty
  line suggesting `federate scan` to prune. Scan/refresh upsert by path +
  basename, prune Dead/Self only (not “absent from this scan”), clear deps
  before link delete, and migrate SIBLING `ledger_entries.trace_id` on rename
  (dedupe when the new name already has the `tx_id`). Impact walks the same
  Live set using `layout.root` for self-class (not CWD). Web sibling
  `ProjectResponse.name` uses basename; `id` stays `schema.repo_name`. No
  schema migration, no forced dep bumps, no product version cut. Bare
  `federate` → status (0179) unchanged.

- **Entity path identity residual (0183):** Shared unique-only file resolve
  (`exact` → Rust `X.rs`↔`X/mod.rs` / extensionless alias → unique suffix) in
  `util::path_entity`. **`symbols --path`** applies it as a **zero-match
  fallback** only (successful dir prefixes unchanged; ambiguous refuses with
  candidates). **`hotspots explain`** uses the same resolve for
  **complexity** against `project_files` only — list/trend stay git-history
  prefix / exact (no rename-frequency coalesce). `tests` / `verify --explain
  --entity` already shipped the same rules via **0156** (regression-guarded).
  Windows suffix equality uses `LOWER` to match exact lookup. No dep bumps.

- **Windows-safe human tables (0181):** Auto-detect non-UTF-8 console output
  code pages (e.g. CP437 under Windows Terminal) and render **ASCII** table
  borders + non-PUA icons by default. UTF-8 capable consoles (OutputCP 65001)
  and non-Windows keep premium rounded borders. Env overrides (first match):
  `LEDGERFUL_TABLE_STYLE=ascii|utf8|auto`, then `LEDGERFUL_TABLE_ASCII=1` /
  `LEDGERFUL_TABLE_UTF8=1` (ASCII wins if both simple flags set). `NO_COLOR`
  remains color-only and does not force table ASCII. Ledger `search` human
  output is width-aware (Dynamic arrangement, short committed timestamp,
  column upper bounds, ASCII truncation `...`). JSON/machine paths unchanged.
  Stays on comfy-table **7.x** (no 8.x migration).

### Added

- **Unquoted multi-word `search` (0187):** `ledgerful search foo bar` joins
  tokens to the same query as `search "foo bar"`. Flags (`--json`, `--limit`,
  …) parse before or after query words. Empty `search` stays required
  (`<QUERY>...`). Hyphen-leading tokens still need `--` (quotes do not hide
  a leading hyphen); unknown flags stay clap errors. Envelope `query` remains
  one string. MCP search
  still passes one string after `--`. Not a query-language rewrite; `ask`
  still swallows post-query flags. No crate or product bump.

- **Engine dogfood graph pack (0186):** Committed `.env.example` (operator-facing
  env schema; secrets empty) and `policies/daemon-api.cedar` (8 core `/api`
  permits, not a live PDP). After `index --incremental`, `config schema` is
  ready; after `index --analyze-graph`, `security boundaries` is ready.
  `surfaces` then shows 2 gated · 1 empty · 3 ready without flipping product
  `coverage.enabled`. `[services]` stays a local-only recipe. No OpenSLO, no
  deploy default, no crate bumps.

- **`surfaces` / `tour` inventory (0185):** Read-only map of six advanced
  surfaces (services, deploy, security, observability, config schema,
  data-models) as ready / empty / gated. `--json` envelope is
  `schemaVersion` 1 / `kind: "surfaces"`. Ready is live-command index or
  config data; repo-root files only choose empty `next`. Doctor emits
  Info/Optional `surfaces-gated` when coverage gates are off (collapsed
  unless `--full`; does not change `readyForPublish`). Does not enable
  coverage or add dogfood content. No dep bumps.

- **`export head --stdout` / `-o -` (0182):** Pure pretty `ChainHead` JSON on
  stdout (same bytes as file path write; no SUCCESS banner; no file created).
  Fixes the footgun where `-o -` wrote a file named `-`. Default file path and
  `--force` unchanged. `--stdout` + non-dash `--out path` hard-errors
  (stricter than bridge export). Machine mode on for stdout path. No product
  version bump; chain/crypto shape unchanged.

- **`scan --json` / `scan --out` gitScan envelope (0180):** Bare machine flags
  no longer require `--impact`. Emit a pure-stdout (or file-only for `--out`)
  summary with `schemaVersion: 1`, top-level `kind: "gitScan"`, and the same
  change fields as the durable scan report. Full impact packet remains
  **`scan --impact --json`** (no silent auto-impact). `--summary` still requires
  `--impact` (impact brief). On-disk `latest-scan.json` schema unchanged.

- **Bare parent CLI defaults (0179):** Six feedback parents no longer fail with
  clap missing-subcommand usage when invoked alone. Each defaults to a safe
  **read-only** subcommand (same path as the explicit form with default flags):

  | Parent | Default |
  |---|---|
  | `dependencies` | `list` |
  | `policy` | `check` |
  | `gate` | `mode` (**show** only; does not set observe/enforce) |
  | `ci` | `diff` |
  | `deploy` | `impact` |
  | `federate` | `status` (not `export`, which writes schema) |

  Soft: bare `services` → `diff`. Parent help documents
  `Default when omitted: …`. **Flags still require an explicit subcommand**
  (e.g. `dependencies list --json` — bare `dependencies --json` remains a
  clap error). Bare `federate` routes to `status` (known status display
  hygiene → track **0184**, not a regression of this default).

## [0.2.8] - 2026-08-11

### Security

- **Release signing cosign v3.1.3 + GHA pin hygiene (0169):** Bump
  `cosign-release` to v3.1.3 (fixes GHSA-fx35-mq7g-6g98 verify bypass);
  pin cosign-installer v4.1.2 (tagged), actions/attest v4.2.2, install-action
  v2.85.11.

### Fixed

- **Impact / scan(--impact) RO report write no longer hard-fails (0174):** Soft-open
  when `ledger.db` exists; soft-skip `latest-impact.json` / scan report / tombstone
  writes under RO or RO-class permission errors. Greppable honesty
  `report write unavailable under RO` on human stdout and in `analysisWarnings`;
  never claims “Wrote impact report” when skipped. Writable trees still write.
- **Ledger `--category` aliases + case-insensitive canonical (0175):** CLI
  `--category` on `ledger start` / `atomic` / `adopt` (and filters on `search` /
  `stack`) accepts track-language and conventional-commit aliases (`feat`, `fix`,
  `ux`/`ui`→FEATURE, `doc`→DOCS, `perf`/`style`→REFACTOR, `test`→CHORE, `ci`/`build`→INFRA,
  `dx`→TOOLING, …) and case-insensitive canonical names (`feature` as well as
  `FEATURE`). Stored value is always canonical SCREAMING. Unknown-category errors
  list all 9 variants **including SECURITY** plus short alias examples and up to
  three “did you mean” suggestions.

### Changed

- **toml 1.1.4 (0176):** Bump direct `toml` to 1.1.4 (writer overflow fix in 1.1.3; serde `Value::Datetime` preserve in 1.1.4). Supersedes Dependabot #175.
- **Engine cargo majors (0171):** httpmock 0.8.3 (test-only API renames:
  `hits`→`calls`, `assert_hits`→`assert_calls`, `body_contains`→`body_includes`,
  `json_body_partial`→`json_body_includes`). ed25519-dalek 3 + rand 0.10 co-bump
  (API only; Ed25519 wire format unchanged). scip 0.9 Dependabot major ignored
  (classic-only consumer; typed_range future track). Keep exact pins
  rayon/fs4/url.
- **Cargo minor/patch hygiene (0170):** Refresh engine lock/floors for the
  Dependabot minor-and-patch group (clap, serde, tokio, cedar-policy,
  tree-sitter, zeroize, …). Keep `rayon = "=1.10"` (cozo/graph_builder).
  Dependabot ignores exact pins rayon/fs4/url.
- **Doctor human progressive disclosure (0174):** Default human expands Block +
  action-critical warn only; Optional/Info hygiene collapses with greppable
  `N hygiene finding(s) collapsed — run doctor --full`. `--full` expands hygiene;
  `-q`/`LEDGERFUL_QUIET` (`resolve_quiet`) suppress remediations + VRAM.
  `doctor --json` unchanged (schemaVersion 1, full findings).
- **Non-interactive unknown category fails closed (0175):** Near-miss tokens no
  longer silently auto-select the closest fuzzy match outside an interactive
  Select; agents get an error with suggestions instead of wrong provenance labels.

### Added

- **Prospective `--paths` on change-context / impact / scan --impact (0173):** Analyze
  explicit paths as if changed without a dirty working tree (`analysisMode=prospective`).
  On-disk → Modified; missing → Added (greenfield). Cap ≤50; mutually exclusive with
  `--base-ref`. **Does not** rewrite `latest-impact.json` (in-memory only). MCP
  `change_context` gains `paths[]` + `include_governance`; MCP `scan` stays unscoped in v1.
- **`agentSummary` on change-context (0173):** Structured scannable header
  (`riskOneLiner`, class counts, capped samples, `demotedTemporalCount`, `pathMode`,
  `analysisMode`) coexists with freeform `summary`. Human mode prints it first.

### Changed

- **Default pathMode=code demotes process/governance temporal (0173):** Code↔governance
  and governance↔governance temporal couplings no longer dominate risk weight/reasons
  or `readSet` p3 under the default agent path. Full `temporalCouplings` remain on
  impact JSON for audit; `demotedTemporalCount` is honest. Restore pre-0173 behavior
  with `--include-governance` (`pathMode=all`). Process co-evolution is demoted for
  agent budget — not claimed as CodeScene “noise.”
- **ImpactPacket honesty fields (0173):** `pathMode`, `demotedTemporalCount`,
  `analysisMode`, optional `prospectivePaths` (legacy JSON still deserializes).

- **`ask --timeout` is optional and backend-aware (0158):** Omitted timeout no longer
  forces 15s for every backend. Local uses `local_model.timeout_secs` (default **300**);
  Gemini uses `gemini.timeout_secs` / starter **120**-class; OllamaCloud/OpenRouter stay
  short (**15**). Explicit `--timeout N` always wins. Cloud fallback after a local attempt
  does not inherit the 300s local budget when CLI timeout is omitted.
- **Default `dependencies list` is direct deps from live Cargo.toml + Cargo.lock (0153):**
  Shows declared direct dependencies (normal/build/dev + target tables) with locked
  versions resolved from the **root package’s own** lock `dependencies` array (not a
  multi-version name-join; not Cozo package nodes). Header no longer claims “from
  Knowledge Graph”. JSON is a schemaVersion **1** envelope (`mode`: `direct`|`all`,
  `root`, `directCount`, `lockPackageCount`, `packages`). Dual-kind names emit one
  row per kind. See `docs/agent-output-contract.md`.
- **Default `hotspots trend` is top-file summary (0151):** Human + `--json` default
  to a scannable top-**20** file rollup (Score / Prior / Δ / Samples / Last recorded),
  not the full timestamp×file matrix. Mode precedence: `--entity` > `-a/--all` >
  summary. JSON is schemaVersion **1** with per-mode counts (`summary` / `full` /
  `entity`); full matrix via `--all --json`. See `docs/breaking.md` and
  `docs/agent-output-contract.md`.
- **Warm default incremental for `index --semantic` (0161):** Bare
  `ledgerful index --semantic` is **incremental** when the vector store is non-empty
  (`vector_count > 0` after foreign purge — not hash presence alone). Cold/empty
  vectors full-bootstrap (`cold-store`); `--full` forces rebuild; `-i` is explicit
  incremental. Per-file skip only when hash-current **and** the file still has ≥1
  snippet row. Graph/`index` without `--semantic` still requires `-i` for incremental.
  See `docs/Semantic-Search.md` (Indexing mode & progress).

### Added

- **Native C/C++ language support (0165):** `Language::Cpp` via `tree-sitter-cpp` 0.23.4 for
  `.c`/`.h`/`.cpp`/`.cc`/`.cxx`/`.hpp`/`.hh`/`.hxx`/`.h++`. Symbols (shared declarator-name
  helper — C++ `function_definition` has no `name` field), `#include` → `imported_from` with
  quote/angle strip, same-file calls with template/qualified/member unwrap (**unique** local
  name → resolved; overloads / same-name multi-def stay unresolved), complexity
  (incl. `lambda_expression`). Routes/observability empty Ok. Semantic walk honors gitignore
  (`ignore::WalkBuilder`); sample SQL includes `Cpp`. Soft honesty: empty SCIP probe →
  `status = unavailable` (not failed); Cargo.lock WARN only when `Cargo.toml` exists;
  doctor `scip-clang-not-wired` (manual `index --scip` only — not auto-generate).
  **Auto-on:** D2 extensions on `Language::from_extension` also enable daemon complexity
  diagnostics and the federated scanner language gate (same map; no separate flag).
- **SCIP skip tallies + `references_seen` on `index --json` / human SCIP section (0157):**
  Success `scip` object always includes
  `edges_skipped_enclosing_disagreement`, `edges_skipped_unmapped`,
  `edges_skipped_invalid_occ_range`, `edges_skipped_duplicate`,
  `definitions_skipped_invalid_range`, `invalid_enclosing_fallback`,
  `references_seen`, and `definitions_seen` (snake_case). Human SCIP success
  prints non-zero skip/refs lines. Rate quality as
  `edges_skipped_enclosing_disagreement / references_seen`.
- **`ledgerful symbols` scoped inventory (0163):** Index-backed list of definitions
  under `--path` (prefix), `--changed` (WT ∩ indexed, includes Deleted still
  indexed), `--kind` (canonical PascalCase + aliases), `--pub`, with default
  `--limit 200` / hard max **5000**. Pure `--json` schemaVersion **1**
  (`totalMatching` = COUNT, honest `truncated`, optional `indexStatus` when the
  DB is missing). Not search; not dump-all. See `docs/agent-output-contract.md`.
- **`dependencies list --all` / `-a` (0153):** Full lock packages from live
  `Cargo.lock` (sorted name/version; Source column = lock source or `-` for
  path/workspace). `-v/--verbose` remains richer **direct** columns (Req, Source),
  not a synonym of `--all`.
- **`hotspots trend --limit` / `-a/--all` (0151):** `--limit N` (range `1..`, default
  20) caps summary files; `-a/--all` restores the full days-window matrix. Parent
  `hotspots --limit` remains list-scoped only.

### Fixed

- **Non-TTY semantic index progress flood (0167):** Mid-phase parse/embed counters
  no longer clamp the report stride to 25, so large totals emit ~`total/20`
  mid-lines (~20) plus the 20s wall-clock OR instead of hundreds of lines on
  dogfood-scale cold fulls. Also: "embedding done" prints only after a successful
  embed collect (not before a batch `Err`); ProgressBar/spinner (and steady tick)
  stay hidden under `--json` even on interactive TTY.
- **SCIP nest-prefer caller resolve (0166):** When `enclosing_range` and occurrence
  range map to different native symbol ids but one native span **strictly contains**
  the other, prefer the **innermost** id and emit the edge instead of skipping as
  enclosing disagreement (common Rust + rust-analyzer trivia → ancestor miss).
  Disjoint / equal-span / missing-span cases still disagree. Success `scip` JSON
  always includes `edges_recovered_nest_prefer` (incl. 0); human SCIP section prints
  non-zero nest recovery. Remaining quality rate is still
  `edges_skipped_enclosing_disagreement / references_seen`. See
  `docs/Call-Resolution.md`.
- **Ask cloud-fallback multi-cause honesty (0160):** When local completion fails and
  cloud fallback exhausts (or cloud-only fails), the operator no longer sees only
  `Cloud fallback exhausted. Last error: …` (which erased the local trigger). Errors
  now report a **primary class** (content-quality > auth > rate-limit > transport),
  retain the local cause when a local attempt ran, keep greppable 0159
  `reasoning only` / empty tokens and the literal `Cloud fallback exhausted`
  substring, sanitize each cause before embed, omit a false `Local:` section on
  cloud-only, size the hard-deadline budget for the full cascade when cloud
  credentials exist, and include actionable Next steps (warm/timeout, disable
  cloud keys / `LEDGERFUL_CLOUD_POLICY=forbidden`, `--backend gemini`). Terminal
  prints the full multi-line report once; miette / next-provider paths use a
  compact single-line form. Content-quality multi-cause reports stay
  non-degradable (no silent graph-only success).
- **Local / OpenAI-compatible thinking-only completions no longer dump CoT (0159):**
  Empty `content` with non-empty `reasoning` / `reasoning_content` (and Ollama
  `thinking`) no longer promotes chain-of-thought as the product answer. Known
  think tags (`<think>`, `<thinking>`, `<thought>`, `<reasoning>`,
  `<|begin_of_thought|>`) are stripped non-greedily (multi-block + unclosed
  open→EOF); post-tag or last line-anchored `Final answer:` / `Answer:` /
  `## Answer` may extract a real answer; otherwise fail-closed with greppable
  `reasoning only`. Shared pure helper covers local and cloud OpenAI-compatible
  parse paths (and Ollama-native).
- **Local `ask` cold-start false-fail (0158):** On-demand local routers no longer die
  solely on the CLI default 15s, a fixed **5s** HTTP probe, or a **500ms** Local TCP
  precheck while the model loads. Local TCP precheck/connect use `min(30, effective)`;
  closed-port refuse stays fail-fast. Wait/timeout/degrade messages distinguish
  unreachable vs timed-out; MCP `ask` child passes `--timeout` ~110 so product errors
  surface before the 120s parent kill.
- **SCIP enclosing_range WARN flood (0157):** Successful `--auto-scip` no longer
  emits ~tens of thousands of per-occurrence
  `enclosing_range … disagrees …` WARN lines. Skips are aggregated on
  `ScipEdgeStats` / `scip.*` JSON; **one** summary WARN when disagreement and/or
  invalid-range counts are non-zero (not for unmapped-only or duplicates). First
  ≤3 disagreement samples at `debug!` with path + enc/occ ids. Skip **policy**
  unchanged (disagreement → no edge). Typed-empty classic ranges → ≤1 process
  WARN (scip 0.8.1 detect-only; no typed consumer).
- **`dependencies list` stale self-version / Cozo coupling (0153, Cosmetic #3):**
  Default list no longer reads Cozo package nodes; root version comes from live
  `[package]` when it is a string (`source: "manifest"`), or falls back to the
  root package entry in `Cargo.lock` when the manifest uses non-string version
  (e.g. `version.workspace = true`; `source: "lock"`). Works without index/Cozo
  and on non-git cwd with a valid cargo project (`get_layout_or_cwd_if_not_git`).
  Missing Cargo.toml → clear non-zero error (no KG fallback). Missing lock →
  direct names with locked version `-`/null and an honest note.
- **`tests -e` / `verify --explain --entity` path resolve (0156):** Shared resolver
  in `explain_test_mappings` now does exact path (Windows `LOWER`) → module +
  extensionless aliases (`X.rs`→`X/mod.rs`; extensionless→`.rs`/`mod.rs`, accept
  iff exactly one hit) → unique-only full-input path suffix → symbol. Multi-match
  is **Ambiguous** (sorted candidates, “more specific path”) — not silent pick and
  not “run index”. Mapped/NoMappings headers prefer the resolved stored path.
- **`tests` empty-state help (0156):** Examples no longer cite non-indexed
  `src/commands/doctor.rs` (use `doctor/mod.rs` / `verify.rs`).
- **`data-models list` / `impact` duplicate rows (0155):** Emit-time dedupe keeps one
  row per `(name, language, kind, file_id)` (keep-best: higher confidence, else lower
  `id`). List SELECT joins `project_files` for path; human adds File column; JSON
  gains additive `file_path` (normalized `/`). Empty-state / CleanDiff still use raw
  table COUNT — not invent no-models from dedupe alone.
- **Data model extract stacking on re-index (0155):** Full `extract_data_models`
  mirrors routes: collect-then-atomic `DELETE` + insert; empty symbols clear the
  table; partial walk (`files_skipped` from I/O or extract `Err`, not `Ok([])`)
  leaves the table untouched and reports `partial: true` / `dm_files_skipped` on
  index JSON.
- **Unbounded `hotspots trend` default dump (0151):** Default human/JSON no longer
  emit ~MB timestamp×file matrices on long-lived repos; agents get a bounded
  summary. `historyAvailable` is honest when trend rows exist (not false with
  non-empty `files`/`entries`).
- **Non-TTY semantic index progress (0161):** Multi-minute agent/CI runs no longer
  look hung — product phase lines + throttled counters on stdout when non-interactive
  (mode early, parse/embed, HNSW total-size announce, complete / up-to-date). TTY
  keeps indicatif bars. Not tracing INFO (0154 quiet default).
- **`index --semantic --json` human-line purity (0161):** Suppresses all mid-run
  human stdout; success emits one final JSON object (`schemaVersion`, `mode`,
  `reason`, counts, `upToDate`, optional `purgedForeign` / `hnswRebuilt`).
- **Semantic search work-root isolation (0152):** Semantic keys are work-root-relative;
  `VectorStore::query_scoped` filters foreign absolute leftovers (filter-before-truncate
  + one-shot re-query); full/incremental `index --semantic` dual-purges
  `snippet_embedding` + `semantic_file_hash`; ask opens files via root join; semantic
  hotspots drop foreign-absolute pairs. Envelope optional `semantic.filteredForeignCount`
  (omit when zero; envelope-only — not on `--json-lines`). After upgrade or repo move,
  run `index --semantic --full`. Do not share `LEDGERFUL_STATE_DIR` across unrelated repos.

## [0.2.7] - 2026-08-08

### Added

- **CLI discoverability aliases (0150):** Dogfood wrong-guess synonyms and
  multi-word `ask` without renaming primary commands:
  - **Aliases (visible):** `config show` → `view`; `policy evaluate` → `check`;
    `gate status` → `mode`; `ledger list` → `status`; `ledger history` →
    `search`. Help tips for ledger list/history and honest `services` note
    (no inventory `list` subcommand — use `services diff` / search).
  - **Update:** bare / no-action path prints multi-line action menu
    (`--binary` / `--migrate` / `--repair-hooks`); `--check` is a visible
    alias of `--dry-run` (preview only — not a version-check).
  - **Ask:** unquoted multi-word queries via trailing varargs (e.g.
    `ask what is change-context`); flags must precede unquoted words or they
    become query text.
- **Uniform machine JSON mode (0149):** Agent-relevant CLI gaps closed for pure
  `--json` parsing (`ConvertFrom-Json` / whole-stdout):
  - **`status --json`** — top-level flag; same payload as `ledger status --json`
    (single `execute_ledger_status` path).
  - **`dead-code --json`** — schemaVersion 1 envelope (`findings[]`, honest
    `truncated` via overfetch, mixed `factors[]` shape); rejects
    `--json --prune` and `--json --explain` early; no spinners/stale banner.
  - **`index --check --json`** — success path stderr empty (Info suppressed;
    Error still on stderr).
  - **`scan --json`** without `--impact` — clearer error naming impact packet
    and PR-range tip (`scan --pr <range> --format json`).
  - Contract inventory + schemas: `docs/agent-output-contract.md`.

### Changed

- **Default human CLI quiet for tracing INFO (0154):** Non-verbose, non-machine
  runs use a `normal_layer` **WARN** floor when `RUST_LOG` is unset (was INFO).
  Dogfood-hot diagnostic probes (embed probe, semantic init/HNSW serve,
  federated Scanning progress) are demoted to `debug!`. Product bind/init/
  migration/watch notices use `println!`/`eprintln!` so they remain visible
  without `-v`. Escape hatches: `-v` (DEBUG) and ambient `RUST_LOG` on the
  human path; machine/`--json` still forces WARN. Contract:
  `docs/agent-output-contract.md`; policy: `docs/operator-surface-policy.md` §10.
- **`ConfidenceFactor::GitInactive` wire field (0149):** nested days field
  serializes as `daysSinceLastCommit` (camelCase) so mixed `factors[]` matches
  unit-factor camelCase strings. Affects `dead-code --json` and any
  `deadCodeFindings[].factors` surfaces that reuse the type.
- **Verify step-start progress + compact elapsed (0148):** Human `verify`
  prints greppable `[i/n] Running: <cmd>` before each step and, on the default
  (non-verbose) path, compact `[i/n] ok  <cmd>  (2.2s)` after each pass.
  Keeps 0121 quiet SUCCESS / plan banner / Suggested Actions discipline;
  `--verbose` keeps SUCCESS as-is (no compact ok); `--json` stays pure with
  existing `durationMs`. Contract: `docs/agent-output-contract.md`;
  Progress UX matrix: `docs/verify-performance.md`.

### Fixed

- **Release cut OpenAPI `info.version` (0162):** `prepare-release-cut.sh` rewrites
  `docs/api/openapi.json` `info.version` offline (info-block-anchored awk; not a
  full cargo/utoipa regen). Cut content set is **exactly five** files including
  openapi; Gate A and Gate B assert openapi version == tag/Cargo. Stops every cut
  PR going red on `openapi_drift_check` after 0146 tracked `CARGO_PKG_VERSION`.
- **Homebrew install binary resolution (0164):** Formula install finds `ledgerful`
  as a direct child of brew `buildpath` first (`Pathname.glob`), with nested
  `ledgerful-*/ledgerful` as fallback. Release archives still nest under
  `ledgerful-{target}/`; Homebrew stages that directory as buildpath, so the old
  CWD-relative `Dir["ledgerful-*/ledgerful"]` always missed. Live tap ships the
  same fix; engine template + bump fixture + `verify-manifests` install-body
  lockstep keep the next bump durable.
- **Stderr log-file noise on default human runs (0154):** Default `doctor`,
  `search --semantic`, and human `impact` no longer emit timestamped tracing
  `INFO` lines (embed probe, semantic VectorStore/HNSW chatter, federated
  Scanning progress). Aligns with clig.dev output guidance (do not treat
  stderr as a log file by default). Use `-v` or `RUST_LOG` for diagnostics.
- **Empty-tree impact fast path (0147):** When `treeClean && changes` empty,
  `impact` / `scan --impact` / `deploy impact` skip enrichment (AI reachability
  probe, all providers including federation walk, analysis registry) and apply
  low-risk defaults (`No changes detected`) without Cross-repo / changeguard
  warnings. CleanTree `latest-impact.json` no longer rewrites when HEAD is
  unchanged (stable content hash); human wording says "refreshed" only when a
  write occurred.

## [0.2.6] - 2026-08-05

### Added

- **Doctor binary currency (0137):** Inside the Ledgerful **engine** worktree,
  doctor emits greppable `binary-behind-tree` (warn / tools) when the executing
  PATH/binary lags worktree **Cargo.toml version** and/or **embedded build short-SHA
  vs HEAD** (same-version dogfood lag after merge without reinstall). Remediation is
  install-only (`cargo install --path . --force`, `ledgerful update --binary`) —
  **never** auto-install. Consumer repos stay silent. `doctor --json` `environment`
  adds `binaryVersion` + `buildSha` (schemaVersion stays 1). Long `--version` may
  show short SHA; short `-V` stays package version only.

### Changed

- **Verify dry-run scannability (0144):** Plan-first `verify --dry-run` human
  stdout — Verification Steps print **command + timeout only** (no pipe-merged
  description walls); `print_verify_plan` is skipped on the dry-run path (including
  `--verbose`). When Bayesian ordering ran, greppable
  `Bayesian ordering: matched_steps=N dataset_keys=K` is printed on **stdout**
  (independent of `RUST_LOG`); the former tracing `info!` lines are demoted to
  `debug!`. Predicted Impacts keep heading
  `Predicted Impacts (grouped by source):`; default path list is **top 3** per
  source (was 5) with overflow `… and K more (use --verbose for full list)`;
  CLI `--verbose` expands full paths (one path per line); `VERBOSE_DRY_RUN`
  remains an additive alias. Pipe-merged and nested-paren prediction segments
  are parsed correctly. `verify --json --dry-run` remains refused; schemaVersion
  / `matchedSteps` unchanged.

- **Search `--json` agent envelope (0136):** multi-hit `search --json` is now a
  **single** camelCase object (`schemaVersion: 1`, `results[]`) so whole-stdout
  parsers succeed (PowerShell `ConvertFrom-Json`, `JSON.parse`, MCP tool text).
  Closes dogfood Daily 5 search NDJSON parse friction. Migration: **`--json-lines`**
  keeps the pre-0136 BridgeRecord NDJSON stream for line-by-line consumers;
  `--json` and `--json-lines` conflict. MCP `search` stays on `--json` (gains
  envelope with no argv change). Fatal auto-index under machine mode emits no
  partial stdout. Contract: `docs/agent-output-contract.md`.

### Docs

- **Agent Daily 5 short card (0132):** Tracked docs skill + local agent packs
  (`.agents/.../SKILL.md`, `references/commands.md`) document the scannable agent
  default path (`doctor --json`, `change-context --json`, `ledger status`, `search`,
  `verify --scope fast`) with escalate footer and honesty one-liners. Closes
  report-card surface-sprawl / short-card residual (partial 0093); docs skill
  Strategic Reasoning no longer treats `latest-impact.json` as coupling SoT or bare
  `scan`+`impact` as first `data_stale` step (0123 residual); local pack install
  snippet polish (`-UseBasicParsing`); root `--help` `before_help` points at skill
  Daily 5.

### Fixed

- **Empty-surface latency hygiene (0146):** Filter-only CLIs
  (`data-models impact` [whole arm], `security impact --changed`,
  `observability diff`, `endpoints --changed`) use git status path membership
  instead of `execute_impact_silent` — no full federated impact, no
  `latest-impact.json` cache rewrite on empty/filter paths. OpenAPI
  `info.version` is `env!("CARGO_PKG_VERSION")` (artifact regenerated) so the
  daemon contract tracks the crate instead of a frozen `0.2.1` pin.

- **Verify fast mapping freshness (0145):** Live-clean working tree under
  `verify --scope fast` uses EmptyChanges (fmt+clippy / zero steps) even when a
  saved impact packet still lists changes — no phantom scoped nextest or false
  MappingRefuse from head lag alone. Populated `test_mapping` with head_hash lag
  auto-repairs once without requiring `--auto-index`; empty mapping still refuses
  without `index --incremental` / `--auto-index`. Repair no longer overwrites
  index `head_hash` with a possibly stale packet head. Classification-aware refuse
  messages (`empty` / `head_hash lags HEAD` / `freshness unverifiable`).

- **Doctor session latency (0143):** Real probe hard deadline via `thread::spawn` +
  `recv_timeout` (not `thread::scope` join-first); production per-attempt deadline
  `timeout_secs*1000+250` ms = **2250 ms** (doctor `timeout_secs=2`); hang unit test.
  Parallel embed + completion probes (network only; SQLite/Cozo stay main-thread).
  Parallel content-hash drift walk (rayon, same counts + sorted sample_paths).
  Warm session targets documented; hang-class ≤2.5s. Optional top-level
  `durationMs` on `doctor --json` (schemaVersion stays 1).

- **Ask locate/find-symbol local grounding (0142):** `ask "find the function …"` /
  `locate …` / short bare `find <identifier>` maps to symbol-definition routing
  and early-exits from local symbols (primary) or Tantivy TermQuery full-id
  search evidence (secondary), or an honest miss with `ledgerful search "X"
  --auto-index` next steps — never Gemini invent of “no codebase / not found in
  context” while local search hits exist. Residual empty-context prompts say
  “no retrieved snippets” (not “no project context” / “without codebase
  context”). Policy: `docs/operator-surface-policy.md` §2.

- **Search identifier hybrid / snake_case (0141):** `CodeIdentifierTokenizer`
  dual-emits full underscore identifiers (e.g. `verify_step_key`) plus `_`-parts
  so BM25 finds full snake_case and partials; camelCase splits unchanged. Hybrid
  empty path retries escaped identifier literal via `all_paths` candidates
  (cap 5000) with human honesty and envelope `fallbackUsed: "identifier_literal"`
  when hits are found. FTS format stamp `ledgerful_search_format` /
  `code_tokenizer_v2` forces one-time rebuild when the tokenizer format changes
  without a schema bump. `schemaVersion` stays 1.

- **Bayesian verify join (0140):** Canonical `verify_step_key` join for failure
  probability ordering — write path stores step keys in `test_outcome_history`,
  extract re-buckets legacy full-command rows, scoped nextest (`-E` / filterset)
  shares `nextest-scoped` history. Multi-band sort keeps fmt before clippy;
  vacuous apply (matched_steps=0) preserves plan order and logs honestly (no
  “applied N models” with dataset size alone). `verify --json` adds optional
  `matchedSteps` (schemaVersion stays 1). Window query uses
  `ORDER BY recorded_at DESC, diff_embedding_id DESC`. Semantic cold-start uses
  named `SEMANTIC_COLD_START_THRESHOLD` (5); inverted `/50` explain branches
  removed.

- **Ask Daily 5 / product-docs grounding (0139):** `ask` questions about Daily 5,
  agent default path, or session start commands early-exit with a skill-grounded
  five-command card (banner: `Product-docs query resolved via skill Daily 5.`)
  before CG-F31 command-discovery and before any LLM backend. Prevents free-form
  invention of “top 5 findings” framing or cargo-cult flags (`--machine-output`,
  ask-only `--narrative`/`--mode`/`--semantic`/`--auto-scan`). Policy:
  `docs/operator-surface-policy.md` §2 (Structured sources before LLM synthesis).

- **topFindings optional-category noise (0138):** Doctor sidecar `findings` and
  change-context `doctor.topFindings` no longer include `DoctorCategory::Optional`
  warns (e.g. dogfood `completion-unreachable`), so flaky optional backends do
  not crowd the cap-5 budget. Eligibility is shared with dashboard `failures`
  via `is_action_critical` (block always; non-optional warn; info never). Full
  `doctor --json` `findings[]` stays complete (with `category`). schemaVersion
  stays 1. Run `doctor` once after upgrade to flush pre-0138 sidecars.

- **PATH/binary lag vs engine tree (0137 dogfood):** After merge without reinstall,
  `~/.cargo/bin/ledgerful` could share `CARGO_PKG_VERSION` with the tree while
  help/flags lagged. Doctor now warns on commit-level (and version) lag with
  copy-paste reinstall; build embeds short SHA for honest long `--version`.

- **Verify fast-scope honesty (0135):** `verify --scope fast` is **fast-or-refuse**
  — never surprise multi-minute full hang. Closes dogfood A-paste verify-fast
  hang; **0061 residual** full-still-runs on mapping-cannot-scope (now
  MappingRefuse with `plan.refused`, exit ≠ 0, greppable reason + Next
  remediations, `scopeExecuted: "refused"`); **`index_metadata.head_hash` never
  written by `store_index_metadata`** (false-stale with populated mapping —
  now INSERT on resolvable HEAD / DELETE when unresolvable; stale matrix:
  missing index head + rows is not force-stale). Empty changes → cheap path
  (Rust: fmt+clippy only; non-Rust: zero steps, exit 0). Escape:
  `--allow-full-fallback` restores 0061 full path;
  SharedInfra full unchanged; `--auto-index` then still cannot → refuse.

- **MCP search auto-index (0134):** MCP `search` passes `--auto-index` (before
  `--`; never `--index`) so stale-index refresh matches CLI `search --auto-index`.
  Empty `document_count==0` rebuild remains complementary inside CLI. Closes
  **0128** MCP auto-index residual (**D6**); flips **0126** MCP no-auto-index
  unit (policy change). Manifest honesty: may take multi-seconds; 120s MCP
  spawn ceiling — large/cold repos prefer explicit index first.

- **Doctor Graph content honesty (0133):** Graph Index Health no longer reports
  success **Current** when the index is age-fresh but content-hash dirty.
  Age path (`graph-empty` / `graph-stale`) is unchanged and still STOP (no drift
  walk). Else one `count_content_hash_drift` on `layout.root` →
  `graph-content-stale` (N files) or `graph-drift-check-failed` on Err; clean
  keeps Current / empty-Cozo analyze-graph hint. Warn-only (does not flip
  `readyForPublish`). Closes **0128 doctor Graph residual (D5/B7)**; partial
  **0107** (doctor N from drift). See `docs/index-freshness-policy.md` and
  `docs/doctor-severity.md`.

- **CLI colour gate (0131):** Human product colour goes through `if_supports_color`
  (or the thin `paint` helper) with stream pairing (stdout vs stderr). Piped /
  non-TTY capture and `NO_COLOR` emit no ANSI; `FORCE_COLOR=1` (or
  `CLICOLOR_FORCE`) re-enables. Startup `init_color_support` applies override
  policy; the one-off `verification.rs` `no_color` bool is removed. Deferred
  0099 colour row closed for OwoColorize paths (miette pipeline residual).

- **config schema empty honesty (0131):** Zero env declarations print why + next
  step (`NoIndexedData` vs `NoMatches` via index staleness); empty `--json` uses
  the `format_json_empty_state` envelope (`results` + `emptyReason` + `message`);
  non-empty remains a bare declaration array.

- **Empty-tree federation risk honesty (0129):** Sibling schema unavailable/invalid
  is ambient federation health and lands on `analysisWarnings`, not `riskReasons`.
  Clean trees no longer report `riskLevel=medium` solely from missing sibling
  schemas. Real `[FEDERATED]` modify and interface-removed signals stay on
  `riskReasons`. `ImpactPacket::finalize` sorts/dedups `analysis_warnings`.

- **change-context `doctor.topFindings` population (0129):** `doctor-results.json`
  emits additive `findings` top-N (block+warn, no category filter, severity-first
  re-sort before cap 5, optional `remediation`). After `doctor`, change-context
  exposes usable codes/messages (and remediations when present) instead of always
  empty `topFindings` while warn/block counts are non-zero.

- **Pure-add public-symbol risk wording (0129):** Risk reasons use status-aware
  verbs — `Public symbol added:` / `deleted:` / `renamed:` / `modified:` from
  `ChangedFile.status`. Weights unchanged; still every public symbol in a touched
  file (0088 precision deferred).

- **endpoints list emit dedupe (0130):** `endpoints` (human + JSON, including
  `--changed`) collapses stacked identical routes to one row per
  `(method, path_pattern, framework)` with keep-best (confidence → non-empty
  handler → lex lower handler → lower id). Empty-state still uses raw SQL
  emptiness before dedupe.

- **route extract non-stacking on re-index (0130):** `RouteExtractor::extract`
  rebuilds `api_routes` (DELETE + insert in one outer transaction) so full
  re-extract and `index --incremental` route passes no longer multiply rows.

## [0.2.5] - 2026-08-02

### Fixed

- **Search index freshness honesty (0128):** `index --check` never reports
  `FreshPopulated` while content-hash drift is positive. Age-only assess no
  longer copies row count into `assessment.stale_files`; check runs one
  `count_content_hash_drift` and sets `ContentStalePopulated` when age-fresh
  + dirty. Top-level and assessment `stale_files` agree. `search --auto-index`
  full-rebuilds Tantivy after SQLite FullBootstrap/Incremental (not on every
  search when auto-index no-ops). See `docs/index-freshness-policy.md`.

- **change-context RO nextActions honesty (0124):** `not_ready` recovery actions
  are error-class-aware. Permission/RO failures no longer suggest
  `doctor --json` / `init` / `index`; they guide populated `LEDGERFUL_STATE_DIR`,
  workspace-write, or git-only review. Greppable reasons keep
  `storage unavailable:` and add `state directory not writable` for RO class.

- **Search index empty honesty (0126):** Doctor no longer reports an empty
  Tantivy search index as `OK (0 documents)`. When the index exists, integrity
  passes, and `document_count==0`, doctor emits structured finding
  `search-empty` (warn/index) with remediation `ledgerful index` and a non-OK
  Index Health line. `search` captures pre/post counts across auto-rebuild and
  surfaces empty-index status (`search_index_status` Insight under `--json`;
  human WARN) so empty index is not collapsed into silent no-matches.

### Added

- **Greenfield changeHints on change-context (0127):** Additive optional
  `changeHints` nested object on the agent change-context packet
  (`schemaVersion` stays **1**). Classifies pure-add / mostly-added change sets
  (`kind`: `greenfield` \| `mixed` \| `none`), surfaces `newPackagePrefixes` /
  `surfaceTags`, and budgeted `suggestedTests` via mapped → convention → adjacent
  ladder (cap 10; convention reasons encode exists-on-disk vs to-be-created —
  not proven coverage). Omitted on empty/not_ready. Summary appends
  `greenfield-ish …`; nextActions greppable review of `suggestedTests` when
  present. Docs: `docs/agent-output-contract.md`, dual skill field lists, MCP
  `change_context` description.

- **Public chain head docs (0120):** Document Ledgerful-itself thin head at
  `https://www.ledgerful.dev/ledger/chain_head.json` and the checkpoint compose
  recipe (`curl` + `verify --signatures --against-export`). Clarifies
  export-then-commit publish model, no `verify --against-url`, and that customer
  repos still need 0119 off-machine head retention. See `docs/chain-checkpoint.md`
  and `docs/public-ledger.md`.

- **Reviewer read-only sandbox path (0124):** Durable matrix in
  `docs/reviewer-readonly.md` (honesty ceiling, command classes A–E, host table
  Codex pure RO vs workspace-write vs Claude cwd-writable, reviewer ladder,
  empty `LEDGERFUL_STATE_DIR` footgun). Dual skill + `commands.md` Reviewer (RO)
  block + `codex-review` ledgerful grounding subsection. `change-context`
  soft-opens existing `ledger.db` via true `SQLITE_OPEN_READ_ONLY` (no WAL
  PRAGMA on RO path); layout `open_read_only*` matches rollup flags.



- **Doctor actionable remediations + `re-sign --all` (0125):** `DoctorFinding`
  gains optional structured `remediation` (schemaVersion stays 1). `sig-pin` and
  `sig-version` findings emit PowerShell-safe next commands (outer single quotes
  on pin `config set`; re-sign before min_sig when v1 rows remain). `ledger
  re-sign --all` upgrades LOCAL rows with `sig_version < current` (not only
  invalids; distinct from `--all-invalid` key-repair). Human doctor printer
  surfaces remediation; docs in `doctor-severity.md` / Features.

- **Hook / ledger provenance SoT (0122):** Agent `ledger commit` / open PENDING
  is intentional provenance SoT; commit-msg classifies after reading the raw
  message and **before** adaptive trivial bypass, conventional well-formed
  path, and LLM `draft_intent`. AlreadyCommitted (`Ledger: {uuid}` verified
  COMMITTED) skips draft and does not open a second TX; LinkPending (msg ref
  PENDING or exactly one global open PENDING) writes the sidecar for that id
  without `start_change`; N>1 pending without a verified ref warns and falls
  back. Binary-only PATH upgrade (no shell stamp / doctor SoT finding).
  Multi-pending shared-worktree honesty: without `Ledger: {tx}` the hook does
  not auto-pick among concurrent open PENDINGs.

- **Agent hook verify failure contract (0121):** Default human `verify` (what
  pre-push hooks run) is **quiet on success** (no per-step SUCCESS, no plan
  banner, no Suggested Actions; one `Verification passed` line) and **loud on
  fail** with a structured stdout block (`[Ledgerful] verify failed` + step /
  command / exitCode / failureDetail / optional failedPaths). Formatter path
  extraction for `cargo fmt`/`rustfmt --check` and `ruff format --check` (cap
  50, `\`→`/`, never invent). Additive optional `failedPaths` on
  `VerifyCliStepJson` (**schemaVersion stays 1**). Binary-first: PATH upgrade
  alone improves hook UX without shell rewrite. Product templates stamped
  `# ledgerful-*-gate:v2`; shared ensure supersedes init silent body-diff
  rewrite. Doctor Info finding `hook-template-stale` +
  `doctor --apply-hook-refresh` (+ `--dry-run`; reject with `--json`);
  `update --repair-hooks` runs product-refresh after legacy repair. Docs:
  agent-output-contract, installation fleet section, skill one-liner.

### Changed

- **Agent skill preflight alignment (0123):** Default agent preflight is `doctor` → `audit`/`ledger status` → `change-context --json`. Full `scan --impact` is escalate-only (readSetCapped, high risk, multi-module, explicit DoD, or change-context error). Dual skill frontmatter descriptions de-equate peer impact; AGENTS.md/CLAUDE.md harness aligned; commands.md promotes change-context. **Agents may need a new session to reload skill description.**

- **`verify --against-export` checkpoint default (0119 — behavior change):**
  Comparison is now **ancestor/prefix** (live chain must extend or equal the
  retained export head at `export.length`). Legitimate advance past a prior
  export **passes**. Use **`--exact`** to restore previous full head equality
  (latest/genesis/length) for freeze/forensic checks. Shared LOCAL ordering
  with `synthesize_chain_head` fixes multi-entry pre-chain export length.

### Added

- **Chain-head operator retention (0119):** Thin `ledgerful export head`
  (`./ledgerful-chain-head.json` by default) for periodic off-machine
  checkpoints; multi-format `verify --against-export` (SOC2 zip **or** bare
  `chain_head.json`); doctor info finding `chain-checkpoint-practice` when a
  signed head exists; `docs/chain-checkpoint.md` + dual-skill hygiene. Unsigned
  `export head` **refuses** when `intent.require_signing` is true. No migration;
  no version bump.
- **Change-set affected HTTP flows (0118 engine):** Shared `affected_flows`
  library (probe-first statuses `available` \| `empty_map` \| `missing_table` \|
  `no_change_seeds` \| `unavailable`; match kinds handler_symbol / handler_impl_file /
  route_file / blast_symbol / blast_file over **`blast.edges` only**; flows cap 20;
  registered-routes honesty note). Wired into:
  - **Impact** via CouplingProvider after blast (`Option` `affectedFlows` on
    ImpactPacket; Phase-3 truncate clears; finalize sorts).
  - **`change-context`** nested summary (schemaVersion stays **1**); detail-aware
    sample **take(5)** minimal / **take(10)** standard; prefer impact-attached report.
  - **`scan --pr`** always emits non-optional `affectedFlows` on schema **v2**
    (no v3); soft-open read-only only (`unavailable` without creating `.ledgerful`
    / `ledger.db`); file-path seeds only (no blast on PR path).
  - **Human CLI:** impact summary section + all-clear when available+0; brief
    `flows=N` when available and N>0.
  - **`endpoints --changed`:** filter widens via shared library (impl file +
    symbol + registration file + blast); **JSON keys unchanged**
    (`method`, `path`, `handler`, …). Filter uses **uncapped** match keys
    (`match_affected_route_keys`); report payloads still cap sample `flows` at 20.
  - Blast compute failure no longer skips `affectedFlows` attach (warn + continue).
  Docs: `docs/agent-output-contract.md`, `docs/pr-scan-schema.md`,
  `docs/Call-Resolution.md` (framework fence, Go honesty, CRG vs Ledgerful
  agent metadata), `docs/Features.md`, skill. No migration; no Cargo version bump.
  Action sticky consumer ships separately.
- **Impact edge confidence classes + summaries (0117):** Shared classifier over
  existing `resolution_status` + `evidence` yields product `confidenceClass`
  (`SCIP_BOUND` \| `RESOLVED` \| `AMBIGUOUS` \| `UNRESOLVED` \| `CAPPED` \|
  `UNKNOWN`) and always-present `confidenceSummary` counts on ImpactPacket
  `blastRadius` and change-context `blast` (minimal + standard; **no** edges on
  change-context). Hop expansion and pair-collapse share one helper; primary
  production blast split is SCIP_BOUND vs RESOLVED. Bound-callee ceiling:
  production hop-1 blast lists only non-null callees, so AMBIGUOUS/UNRESOLVED
  stay off the live punchlist (they remain index statuses). Docs:
  `docs/Call-Resolution.md`, `docs/agent-output-contract.md`. No migration; no
  Cargo version bump.
- **Team Sync low-friction ops (0113) — Available (opt-in shared-folder v1):**
  - `ledgerful sync setup` readiness checklist + Next command (never enables, never
    prompts for secret); `setup --enable` strict refuse matrix (init + ≥1 peer +
    `SyncTarget::parse` OK + bounded target reachable) with sibling `config.toml.bak`
    before mutate; pure camelCase `--json` (`schemaVersion: 1`).
  - `sync status` readiness + next-action + target reachable + **Quarantined (this
    device)**; all `dir://` paths via `SyncTarget::parse` (fixes Windows
    `dir:///C:/…` inbox/outbox lie).
  - Docs: ≤15 min two-person setup card (explicit two-way pairing), password-manager
    secret distribution + rotation, dual-purpose secret (MAC+AEAD), local FDE note,
    NAS hang honesty, schedule display-only + secret-manager recipe.
  - Doctor enabled-incomplete findings point at `sync setup`. Init secret uses
    `Zeroizing<String>`; run/init share `prompt_password` UX.
  - **Available decision:** opt-in shared-folder v1 only (not default-on, not cloud,
    not CRDT). Label flipped in docs/Features/dual skill/clap help.
- **MCP agent platform install (0116):** `ledgerful mcp install|uninstall|status`
  for Top-N hosts only (`claude-code`, `cursor`, `codex`, `copilot`). Merge-only
  host config wiring (JSONC + Codex TOML), launcher `auto|path|npx` (PATH binary
  preferred; Windows `npx.cmd`), sibling `.bak` + atomic write, dry-run/`--json`,
  host-trust honesty (written ≠ connected). Bare `ledgerful mcp` / `mcp serve`
  still starts the stdio server; install/uninstall/status are human unless
  `--json`. Skill/docs teach install and list `change_context` first among MCP
  tools. No version bump.
- **Change-set test gaps (0115 engine):** Shared structural test-gap library
  (`impact/enrichment/test_gaps`) with probe-first status vocabulary
  (`available` \| `empty_mapping` \| `missing_table` \| `no_source_seeds` \|
  `unavailable`), symbol vs file-mapped classification, caps (unmapped 20 /
  mappedSample 5), and always-on structural + LCOV ceiling notes. Wired into:
  - **Impact** via orchestrator (same seeds as `test_coverage`; optional
    `testGaps` on `ImpactPacket` — empty `test_coverage` vec ≠ full cover).
  - **`change-context`** deepened `testCoverage` (full report; **removed**
    “see track 0115” handoff; never bare `"empty"`).
  - **`scan --pr`** always emits `testGaps` on schema **v2** (no v3); soft-open
    read-only only (`unavailable` without creating `.ledgerful` / `ledger.db`);
    file-level path only (no `resolve_seeds` / no `init_with_layout`).
  Go `*_test.go` path heuristic in `is_test_path`. No DDL migration; no default
  fail-on-gap. Sample: `output/0115/sample-test-gaps.json`.

### Fixed

- **MCP release pin tests no longer hardcode a third engine tag:** `mcp-server/test/platform.test.js`
  uses Cargo.toml as the external SoT for `ledgerfulEngineTag` (with `releaseBaseUrl` wiring
  checks). Removes the hand-maintained `EXPECTED_ENGINE_TAG` that lagged prepare-release-cut’s
  four-file bump and failed the v0.2.4 release after full multi-OS builds. Release Gate A now
  runs `node --test test/platform.test.js` before native builds so pin drift fails in seconds.
  *(Also on the recovered `v0.2.4` tag via #103 retag.)*

## [0.2.4] - 2026-07-31

### Fixed

- **Release-cut prepare four-file invariant ignores chmod mode noise (0104 residual):**
  `prepare-release-cut.sh` counts **content** changes only (`git diff --name-only -G.`),
  so Linux CI `chmod +x` on scripts still stored as `100644` no longer fails the cut
  before push. Workflow sets `core.fileMode false` before chmod; release scripts are
  `100755` in git. Regression case in `test-prepare-release-cut.sh`.

### Added

- **Agent change-context packet (0114):** `ledgerful change-context --json` and MCP
  tool `change_context` emit a budgeted schemaVersion **1** packet composing
  in-memory impact (risk, blast **counts**, thin testCoverage), doctor sidecar
  readiness, pending ledger TXs, and a capped `readSet` with `readSetCapped` /
  `readSetTotalCandidates`. Does **not** call silent-persist impact (no
  `latest-impact.json` clobber). `--base-ref` time-travels structure only; doctor
  and ledger stay present-tense. Skill Default Workflow prefers change-context
  after doctor. See `docs/agent-output-contract.md`.
- **Team Sync secure transport and apply (0112):** extract cursor integrity (partial upsert never
  nulls `last_apply_hlc`; empty extract → `NoNewEntries` without watermark advance; tombstone
  delta filter; single signed zip from extract — run encrypts only). Transport writes **`.lfbundle`**
  with dual-read last-dot `lfbundle|gpg`; peer-scoped `IncomingBundle { peer_id, name }` identity
  through get/move; same-volume put under outbox `.tmp/` (no OS-temp EXDEV). Wired
  `max_clock_drift_seconds` (ahead-only quarantine); run/verify secrets via `Zeroizing`; verify uses
  `load_peer_keys` + self-insert. Two-layout full-crypto golden path + poison quarantine tests.
  Docs/skill honesty: consolidation works Experimental, not Available. No crypto dep majors.
  See track 0112.
- **Team Sync pairing and peer trust (0111):** real `LF-PAIR-1.<device_id>.<b64url_pub>.<b64url_tag>`
  pairing invites bound with `blake3::derive_key` + `keyed_hash` (16-byte tag, `ct_eq`); accept
  path-validates `device_id`, curve-checks with `VerifyingKey::from_bytes`, and persists
  `sync/peers/{device_id}.pub` via non-`*.pub` temp + rename. `sync pair --list` / `--revoke` /
  `--force` (re-key); mutual exclusion on invite vs list vs revoke. Fallible `load_peer_keys`
  (no panic on malformed peers); self-insert stays at apply call site. Status shows real peer
  count/ids; doctor optional `sync-enabled-no-peers` when enabled with zero peers. Pairing
  **never** sets `[sync].enabled = true`. Docs/skill mutual-pair + `init --force` re-pair honesty.
  No crypto dep majors (dalek 3 / argon2 RC / base64 0.23 declined). See track 0111.
- **Team Sync opt-in foundation (0110):** `sync` is now a **default** Cargo feature (release
  already shipped it via `--all-features`; MCP-npm remains `--no-default-features --features
  mcp,export`). Runtime stays forever opt-in (`[sync].enabled = false`; `sync init` never enables).
  Layout-aware init writes keys under `{state_dir}/sync/` and upserts SoT `sync_state.device_id`;
  disabled `sync run` explains opt-in before any secret prompt; light doctor warns only when
  enabled-but-misconfigured. Docs: `docs/team-sync.md`; skill honesty for Team Sync vs watch
  Real-time Sync. OpenAPI `SyncStatusResponse` left unchanged (CLI-first). See track 0110.
- **Index freshness policy (0107):** three-tier model documented in
  `docs/index-freshness-policy.md` — light continuous (`watch` + mega-batch safety), light
  on-demand (`--auto-index` on `search` / `ask` / `hotspots` / `dead-code` with **time-stale and
  content-hash drift-stale**, full bootstrap when never indexed / missing DB), heavy
  scheduled/explicit (`schedule` / `index --full` / `--auto-scip`). `verify --auto-index` remains
  a **scoped** `test_mapping` refresh for `--scope fast` (not the shared drift/bootstrap path).
  `scan` has no `--auto-index`. Daemon is an LSP reader, not an indexer. No idle SCIP. Watch
  skips batches ≥1000 unique paths, marks index STALE, and suggests `index --full`. Optional
  `watch.mega_batch_threshold` config.
- **Bounded structural blast radius (0106):** `impact` / `scan --impact` emit additive
  `blastRadius` (must-touch files/symbols, evidence-tagged edges, optional test hints from
  `test_mapping`). Default depth **1**; `--blast-depth 2` walks only high-confidence edges
  (RESOLVED or `scip:`) with transitive confidence (AMBIGUOUS hop-1 listed, never expanded).
  Seeds join by file+name / qualified_name (never bare name). `structural_couplings` derived
  from hop-1. Not a complete call graph; ≠ deploy `highBlastResources`. See
  `docs/Call-Resolution.md`.
- **Doctor severity + publish readiness (0109):** structured findings `block|warn|info` with
  required `category` and stable `code`. `ledgerful doctor --json` emits pure schema-v1 JSON
  (`schemaVersion` u32 `1`, `readyForPublish`, `summary`, `findings`, `environment`).
  `readyForPublish` is true iff zero **block** findings (docs/skill define dual-green:
  readiness ≠ `verify --scope fast` ≠ full CI). See `docs/doctor-severity.md`.
- **Scheduled release cut (0104):** weekday `release-cut.yml` proposes a Tier-2 release PR when
  `[Unreleased]` has content (`scripts/prepare-release-cut.sh` + pre-bump `changelog-unreleased.sh`).
  Opens `release/vX.Y.Z` with exactly four files (CHANGELOG, Cargo.toml/lock, mcp pin **and** wrapper
  patch), label `release-cut`. Merge tags the **merge commit** so `release.yml` fires. Empty Unreleased
  and an already-open cut PR are clean skips. Merge and `ai-reviewed` stay human; automation cannot
  set the review status (by design).

### Changed

- **Doctor dashboard `failures` formula (0109):** `failures = count(block) + count(warn WHERE
  category != optional)`. Optional backends (embedding/completion/SCIP/sccache/gemini) no longer
  soft-fail or penalize health. On a models-down machine this removes the historical **~−60**
  health points (`failures * 20`) while corrupt index/search still penalizes via non-optional warn.
  Additive `doctor-results.json` fields: `readyForPublish`, `block`, `warn`, `info`. Legacy
  `results: [{passed}]` readers still accepted. Human aggregate: red reserved for **block** only;
  warnings use yellow “ready for publish env · N warning(s)”.

### Fixed

- **scip-python install hint uses npm (0105):** doctor / capability messages now say
  `npm install -g @sourcegraph/scip-python` (package is not on PyPI; the old
  `pip install scip-python` hint 404'd).
- **Agent skill SCIP honesty (0105):** skill no longer advertises “compiler-grade”
  SCIP as universal. SCIP remains optional (`index --auto-scip` / `--scip`, default
  off); agents should read `scip.status` / `edges_added` under `--json`.
- **Linked worktrees share `.ledgerful` with the primary worktree (0108):** state (ledger DB,
  config, keys, reports, search index) resolves via git common-dir to the main checkout;
  submodules keep private state. Nested cwd no longer creates orphan `{subdir}/.ledgerful`.
  Override with absolute `LEDGERFUL_STATE_DIR`.
- **MCP npm channel engine pin (0101):** published `@ledgerful/mcp-server` was still shipping
  `ledgerfulEngineTag` `v0.1.9` (registry `0.1.11`, last modified 2026-07-21) while the engine
  release tag was `v0.2.3`. Republish as `0.1.12` with pin `v0.2.3`. The npm channel is now gated:
  Gate B asserts the published pin against the newest release tag; `release.yml` gains an
  `npm-publish` job (`needs: [publish]`, trusted publishing / OIDC, Node 24) so the registry
  cannot silently lag the engine again.

## [0.2.3] - 2026-07-29

> Note: 0.2.2 was prepared on 2026-07-27 but never tagged; rolled into 0.2.3.

### Added

- **Release-state gates (0098 Part A):** scheduled Gate B (`scripts/check-release-state.sh` +
  `release-state.yml`) fails when `Cargo.toml` has a dated CHANGELOG section but no matching
  remote tag; Gate A (`scripts/check-release-tag.sh`) blocks `release.yml` before any build when
  the tag disagrees with `Cargo.toml`, dated CHANGELOG, or `mcp-server` `ledgerfulEngineTag`.
  Post-publish `verify-assets` / `verify-manifests` smoke the published Linux binary `--version`
  and live tap/bucket manifests. Frontend SPA SHA is resolved at release time (record in notes).

### Changed

- **Release pipeline fail-loud (0098 Part A):** missing `MANIFEST_PUSH_TOKEN` hard-fails
  `bump-manifests` (was silent `exit 0`); intentional `WINGET_TOKEN` skip emits `::notice::`.
  Dead `workflow_dispatch` `tag` input removed — recover via
  `gh workflow run release.yml --ref vX.Y.Z`. Concurrency group on the release ref with
  `cancel-in-progress: false`.

### Fixed

- **MCP engine download pin (0098):** `mcp-server/package.json` `ledgerfulEngineTag` now tracks
  the engine release tag (`v0.2.3`). Previously pinned at `v0.1.9`, so `@ledgerful/mcp-server`
  tarballs published with engine releases `v0.2.0`/`v0.2.1` downloaded a `v0.1.9` engine on
  `postinstall` (failure only `console.warn`ed). Tests assert a literal tag and that the pin
  matches `Cargo.toml`.
- **`--open` handoff reliability (0090 follow-up):** bind the web listener **before**
  opening the browser so the SPA can load and call `POST /api/session/exchange`
  immediately. On Windows, open the handoff URL via `ShellExecuteW` so `#c=<code>`
  is not stripped (cmd `start` treats `#` as a batch comment). Optional
  `LEDGERFUL_WEB_OPEN_URL_FILE` writes the open URL for integration harnesses
  instead of launching a browser.

### Added

- **SCIP doctor hints (0095):** `doctor` reports per-language SCIP capability
  (probe, not PATH) and install commands; Go noted as upstream-exists / not
  wired. Optional accelerators are **never** put in `DoctorReport.tools` and
  do not count as doctor failures.
- **Semantic search honesty docs (0096):** `docs/Semantic-Search.md` documents
  backend/index states, `--semantic-dry-run`, and the ban on conflating
  “semantic did not run” with “no semantic matches.”
- **Agent CLI output contract (0093):** `verify --json` emits a versioned
  (`schemaVersion: 1`) machine-readable result with `ok`, `scopeRequested`,
  `scopeExecuted`, `fallbackReason`, and per-step status. Global `--quiet`/`-q`
  (and `LEDGERFUL_QUIET=1`) hide per-entry signature detail while keeping the
  aggregate. See `docs/agent-output-contract.md`.
- **`ledger status --json`:** adds `schemaVersion: 1` and sorts `pendingTxIds`
  for deterministic agent parsing.

### Changed

- **CLI output scannability (0100, user-visible defaults):**
  - `verify --signatures` is **summary-first** by default: per-entry `VALID`/`SKIP`
    lines are hidden (cli_summary layer `INFO`); use **`--verbose`** to restore
    the previous per-entry dump (`DEBUG`). `--quiet` / `LEDGERFUL_QUIET=1` stay
    equivalent to the new default for signatures. Machine mode still `WARN`.
    `INVALID` / required-`UNSIGNED` remain raw stderr and are never suppressed.
    Unknown-key per-entry status stays `VALID (unknown key)` (yellow/amber, not
    green); doctor `[sig-pin]` wording already uses the same terms.
  - `dead-code` keeps the title **Dead Code Analysis** and adds an honest-ceiling
    footer (heuristic evidence, not proof). Empty state: *"No findings above
    threshold (heuristic analysis)."* Command name and `--json` contract unchanged.
  - `doctor` prints an aggregate status line first (CRITICAL / issues / warnings /
    all-pass) and groups embedding, completion, SCIP, and sccache under
    **Optional Accelerators**. Exit still tracks `critical_count` only; partial-
    config failures stay failures (0096).
  - `search` human output emits `… and more results (use --limit N to see more)`
    when results are truncated (overfetch; no exact remaining count; no JSON fields).
  - `config --help` includes examples using real subcommands (`view`, `verify`,
    `set key=value`, `diff`).
- **SCIP augments the native index (0095, user-visible):** `--auto-scip` /
  `--scip <PATH>` no longer early-return or replace the native pipeline. SCIP
  runs once: after `build_call_graph` without `--analyze-graph`, or only inside
  `run_graph_analysis` (after `infer_services`, before centrality/KG) when
  `--analyze-graph` is set. It adds reference edges onto **native** symbol ids
  with `evidence=scip:ref`. Detection is a base-exe `--version` capability
  probe (rustup shims resolve unavailable). Process policy uses the configured
  verify allow/deny list. `index --json` always includes a `scip` object with
  an explicit status (`did_not_run` / `success` / `failed`).
  **Default remains off** (`auto_scip: false` on implicit index paths).
- **Semantic readiness wire shape (0096):** `SemanticReadiness` replaces
  `endpoint_available: bool` with `backend_status` (`not_configured` /
  `unreachable` / `ready`) plus `zero_vector_count`. JSON consumers of
  `semantic_readiness` BridgeRecords should update.
- **Stream discipline for `cli_summary` (0093, user-visible):** product `info!`
  lines route to **stdout**; diagnostic `warn!`/`error!` stay on **stderr**
  (level-split writer). Machine mode (`--json`, `scan --format json`, `mcp`)
  filters the layer to `WARN` so human lines cannot corrupt JSON on stdout.
  Per-entry signature `VALID`/`SKIP` lines are demoted to `debug!` so `--quiet`
  / default can hide them while the aggregate stays at `info!`. **0100** moved
  the product default summary filter from `DEBUG` to `INFO`; use `--verbose` for
  per-entry detail. Double-emitted observe would-block / CRITICAL messages now
  emit once via `cli_summary` only.
- **Machine mode silences normal_layer progress INFO (0093 R1):** under `--json`
  / machine mode the non-`cli_summary` EnvFilter is raised to `WARN`, so a
  successful `verify --json` has empty stderr. `WARN`/`ERROR` still pass.
  `verify --json` rejects combination with `--signatures` / `--chain` /
  `--against-export` (same pattern as `--health` / `--dry-run`). CI prediction
  `println!` tables are gated on `suppress_human_output`.

### Fixed

- **CLI output defects (0099):** four incorrect CLI surfaces:
  - `index --check` now prints a human status on success (was silent); the
    human report is no longer nested inside the `--json` branch, so
    `--json` stdout is a single parseable document and warnings go to stderr.
  - `index --check --strict` on a stale index prints its reason on **stderr**
    before exiting 1 (was a silent CI-gate failure).
  - Search snippets no longer round-trip through HTML: `&&` and quotes render
    as plain text (not `&amp;&amp;` / `&quot;`), raw `\x1b` literals are gone
    from the engine, and hybrid `search --json` `content` is plain (no
    escapes or entities). Human search still applies ungated `owo_colors`
    emphasis like its neighbours — **piped colour gating is a separate track**
    (spec §2.4; `if_supports_color` remains unused repo-wide).
  - `endpoints` Auth column parses `Option<Vec<String>>` and shows human text
    (e.g. `secured`) instead of raw JSON like `["secured"]`.
- **SCIP exclusive call sites + always re-apply (0095 review):** with
  `--analyze-graph`, SCIP runs only inside `run_graph_analysis` (not also in
  `execute_main_mode`). Requested augment always re-applies edges (idempotent);
  hash is audit-only, not a skip gate. Precedence matches `(caller, callee)`
  regardless of `call_kind` so native `METHOD_CALL` edges upgrade to
  `evidence=scip:ref` without duplicates.
- **SCIP no longer writes `project_symbols` (0095, user-visible):** the old
  ingest path wrote external/stdlib symbols into local files, used 0-based
  lines against 1-based native rows, and kept the last occurrence's range.
  SCIP now only contributes `structural_edges` via a definition→native
  resolver. `scip_indices` is cleared with other project tables so staleness
  cannot outlive wiped rows. Fresh-clone `--auto-scip` produces a complete
  native index (native floor first).
- **Semantic search honesty (0096, user-visible):** with no embedding backend
  configured, `index --semantic` **refuses** instead of fabricating and storing
  all-zero vectors. `vector_store.index_chunks` rejects zero-length and
  all-zero embeddings. `search --semantic` distinguishes not-configured /
  unreachable / ready+empty (and never recommends `index --semantic` when
  unconfigured). The broken interactive “run index --semantic?” prompt (which
  ran non-semantic incremental) is removed in favour of explicit warnings.
  `doctor` no longer reports a healthy backend for partial config (model name
  set, URL empty). Pre-existing zero-vector rows are detected and reported,
  never auto-deleted; query paths exclude zero-magnitude stored vectors.
  See `docs/Semantic-Search.md`.
- **Legacy migration completion (0094):** state-dir rename now also ensures
  `.ledgerful/` is gitignored (anchoring-aware equivalence so `/.ledgerful/`
  already counts), emits a one-line migration record, and no longer leaves
  state un-ignored. `hook_repair` normalizes legacy gate markers, covers
  `scan` invocations, two-tier de-duplicates dual-marker hooks, and never
  reports a hook as repaired while a retired-binary invocation remains.
  Hook discovery honours `core.hooksPath` and linked-worktree `commondir`,
  refuses outside-repo rewrites, and re-detects husky on the resolved path.
  Config load warns on parse failure (no silent defaults); unknown keys are
  reported via `serde_ignored` through `doctor` (no `deny_unknown_fields`).
  `doctor` reports all four residue surfaces as WARNINGs with remediation
  (`ledgerful update --repair-hooks`). See `docs/installation.md`
  ("Migrating from ChangeGuard").

### Added

- **Module + import binding resolution (0092 Part 1):** per-file `file_bindings`
  table (m54) stores bound names from Rust `use`/`mod`, TypeScript imports, and
  Python imports (aliases and list forms expanded; wildcards non-enumerable).
  Module paths are derived from source layout (`src/platform/urn.rs` →
  `crate::platform::urn`). Shared `resolve_callee` gains a higher-precedence arm
  for `crate::`/`self::`/`super::` and first-segment-local callees **only when an
  enumerable binding proves locality** — so `pub mod fs;` + `fs::write` may
  resolve locally while `use std::fs;` + `fs::write` stays `UNRESOLVED`. Full and
  incremental index paths share the arm (DoD-6). Package-root manifests (Part 2)
  declined by default (single-crate repo, no multi-package fixture). See
  `docs/Call-Resolution.md`. **Measured on this repo (clean full reindex):**
  `crate`/`self`/`super`-rooted UNRESOLVED **840→226 (−614)**; third-party bare
  names (`unwrap`/`join`/`to_string`/`clone`/`expect`) stay at **0 RESOLVED**;
  all observed `fs.*` edges stay UNRESOLVED. Net RESOLVED count can fall when
  DoD-4 demotes false name-only matches — that is correct, not a quality score.
  **Behaviour change:** path-qualified local edges and external-proven first
  segments both move `callee_symbol_id` / status, so **dead-code, centrality, and
  coupling outputs can move**.

- **TypeScript + Python function signature extraction (0091 Part B):** extractors
  write `metadata.signature` / `metadata.signatureShape` for `.ts`/`.tsx`/`.js`/
  `.jsx`/`.py`, so `SignatureDeltaProvider` no longer silently no-ops on those
  languages. Python covers the full parameter grammar (typed/default/variadic/
  `/`/`*` separators as modifiers, binding-decorator allowlist). TypeScript
  covers existing forms plus query widening: interface/abstract
  `method_signature`, `function_signature`, and **named** arrows only
  (`variable_declarator`, class field, `export default` → path-qualified
  `{path.dots}.default`, e.g. `src.foo.index.default`). Anonymous arrows are
  skipped. `.tsx`/`.jsx` symbol parse uses `LANGUAGE_TSX`. See
  `docs/Signature-Diff.md`.

### Changed

- **TypeScript symbol population (0091 behaviour change):** query widening adds
  interface methods, declare-function signatures, and named arrows to
  `project_symbols`. **Dead-code, centrality, and coupling outputs can move**
  for TypeScript/JavaScript trees. Python is metadata-only (no new symbols).

- **Call-graph resolution precision (0089 Parts A+B):** shared `resolve_callee`
  for full and incremental index paths. Candidates restricted to callable kinds
  (`Function`, `Method`); same-file preference on bare-name collisions; higher-
  precedence `qualified_name` match (`Foo::new` / `Foo.new` → `Foo.new`).
  Python/TypeScript methods emit `Class.method` QNs; member calls store dotted
  `receiver.field` so external forms like `json.loads` / `axios.get` no longer
  false-resolve to a unique local bare name. Package-root import mapping
  (Part C) is deferred to track 0092. See `docs/Call-Resolution.md`.
  **Behaviour change:** more (or fewer) edges get a non-null `callee_symbol_id`,
  so **dead-code, centrality, and coupling outputs move** — ambiguous edges that
  previously dropped out of those analyses may now resolve, and fabricated
  external→local edges are removed.

### Added

- **Function signature extraction + signature-changed impact risk (0088 Part A):**
  tree-sitter extractors for Rust and Go write `metadata.signature` /
  `metadata.signatureShape` (arity, ordered types, return, behavioural modifiers;
  names excluded from the shape). `signature_hash` is derived from the shape via
  blake3 in `symbol_to_project_symbol`. Coverage widened to Rust trait method
  declarations (`function_signature_item`) and Go interface methods
  (`method_elem` in pinned tree-sitter-go 0.25). New `SignatureDeltaProvider`
  compares working tree vs HEAD via **gix** (no raw `git` subprocess) and emits
  `signatureDeltas` on the impact packet. Shape changes raise a distinct risk
  reason (`Signature changed: …`); renames are cosmetic (recorded, not scored).
  TypeScript/Python extractors deferred to Part B. See `docs/Signature-Diff.md`.

## [0.2.1] - 2026-07-26

### Added

- **PR scan schema v2 + index-free history signals (0086):** `scan --pr`
  reports now emit `schemaVersion: 2` with per-change `churn`, optional
  `lastCommitAt`, and `isSensitive`, plus report-level `historyWindowCommits`
  / `historyTruncated` from a bounded first-parent walk (no index, no network,
  no author names). Optional `headHash` / `branchName` omit when unknown
  (never serialize as JSON `null`) so detached-HEAD CI checkouts validate
  cleanly. `analysisWarnings` is documented as reserved (still always `[]`).
- **Authenticated SSE push channel (0085 engine):** `GET /api/events` streams
  narrow `DaemonEvent` snapshots (`pendingTransactions`, `unauditedDrift`,
  `indexReady`, `graphReady`) over Server-Sent Events with Bearer auth (no
  query token). A change detector polls SQLite `PRAGMA data_version` every
  500 ms in autocommit and publishes only on cross-process ledger commits;
  streams terminate on graceful shutdown so Ctrl+C does not hang with a live
  dashboard tab. See `coordination.md` §3.2.
- **Embedded dashboard refresh:** release embeds `ledgerful-frontend` at
  `91dd039` (includes 0085 SSE client + SPA session/routing fixes from FE
  PRs #29/#30).

### Fixed

- **SPA `--spa-dir` static assets (PR #67):** missing `/_next/*` under a custom
  SPA directory no longer 404 incorrectly when serving an external export.

## [0.2.0] - 2026-07-25

### Added

- **Go structural language support (0084):** `Language::Go` reaches Python-equivalent
  structural-indexing depth — symbols (funcs, receiver-qualified methods, structs,
  interfaces), call graph (same-package resolved, cross-package `Unresolved`),
  complexity/hotspots scoring (including anonymous `func_literal` closures),
  `net/http`/Gin route detection, `json`-tagged struct data models, and
  `log/slog`/`errors.Is`/`errors.As` observability detection. Receiver methods
  carry a qualified `TypeName.MethodName` symbol name.
- **Crypto v2 provenance signing basis + trusted-key pin (0072):** new commits
  sign a frozen v2 payload binding full provenance (entity/author/risk/origin/
  change_type/entry_type/is_breaking/related_tickets, plus v1 fields).
  Dual-verify by stored `sig_version` (schema m53). Trusted-key pin,
  `min_sig_version`, `--strict-signatures`, new `doctor` checks, and
  `enforce-init --require-signing` auto-pin for existing installs. Distinct
  verify exit code `UNSIGNED=3`.
- **Daemon auth fail-closed + rate limiting (0078):** fail-closed token
  resolution/comparison, `--spa-dir` containment, peer allowlist, bounded
  per-peer rate limiting on `ConnectInfo`, reduced token exposure
  (`--print-token=false` file path). Auth failure frozen at `403`.
- **Default-strict process policy (0079):** `ProcessPolicy` now defaults to
  `strict: true` with a populated built-in toolchain allowlist (was
  permissive-by-default); config allowlist entries extend rather than replace
  the built-in set; `allow_shell_steps` gate scoped to config-declared steps
  only, with shell-chain inner-command inspection (not just the `cmd`/`sh`
  wrapper); shared `exec::grouped` process-group kill; `shlex`-based schedule
  quoting; `GIT_BINARY` absolute-path validation + git subprocess env
  hardening (`GIT_EXEC_PATH`/`GIT_CONFIG_*`/`GIT_SSH_COMMAND` stripped).
- **Daemon SPA security headers + hash CSP (0081 engine slice):** the local
  web daemon now serves a full security header set (hash-based CSP,
  `X-Content-Type-Options`, frame/referrer/permissions policy) on SPA
  document paths, mirroring the frontend's static-host headers; vendored
  script-hash manifest for the shipped SPA, optional `--spa-dir` sidecar
  manifest with a logged, testable `fallback_reason` when hashes are absent.
- **Telemetry ingest token header (0077):** usage-flush POSTs now carry
  `X-Ledgerful-Telemetry-Token` so the ingest edge function can stage
  optional→required auth without silently dropping telemetry from older
  clients. Default token is public/bar-raising (open CLI); override via
  `LEDGERFUL_USAGE_TOKEN`. Silent-fail and opt-out-sends-nothing preserved.
- **Golden-path demo cryptographic proof (0070):** the automated `demo` path
  now runs real signature+chain and against-export verification so a
  first-run skeptic sees `CRYPTO VALID`, not test-suite noise; `verify
  --scope fast` demoted to quiet optional checks; new
  [docs/golden-path.md](docs/golden-path.md) single-source narration.
- Local self-timing facility: `ledgerful timings` surfaces which of *your*
  commands is slow (outer summaries, inner spans, collapsed flame stacks,
  `--explain`), with default-on capture, opt-out via `timings --opt-out`, and
  strict privacy (no paths/argv values/network). See
  [docs/self-timing.md](docs/self-timing.md).
- **Enforce lifecycle integrity (0074):** promote failure under enforce retains
  PENDING+sidecar (`promote_failed`); shared GC policy never deletes promote-fail
  / HEAD-matching orphans; `ledger recover-orphan --promote|--abandon`; doctor
  CRITICAL codes `PROMOTE_ORPHAN`, `HEAD_UNCOVERED`, `INTENT_NEVER_UNDER_ENFORCE`;
  `ledger status --exit-code` HEAD-uncovered / promote-orphan signals with
  observe opt-in `--strict-observe-signal` (exit 2). See
  [docs/lifecycle-integrity.md](docs/lifecycle-integrity.md).
- **MCP cloud-egress hard-fail (0073):** `CloudPolicy::Forbidden` propagates via
  spawn env `LEDGERFUL_CLOUD_POLICY=forbidden` on every MCP tool child (unless
  host `LEDGERFUL_MCP_ALLOW_CLOUD_EGRESS=1|true`) plus `LEDGERFUL_NON_INTERACTIVE=1`.
  Under Forbidden: provider chain truncates to Local-only, `complete*` skips all
  cloud fallbacks, direct Gemini is blocked, structured error
  `cloud_policy_forbidden` names the opt-in. Universal `sanitize_for_egress`,
  DATA-fence for retrieved chunks, bridge `provider_command` allowlist
  (`ai-brains` only), MCP `search` `--` separator (RT-A4).

### Changed

- Post-commit promote no longer sets `Verified`/`ManualInspection` (phantom
  green). Non-TRIVIAL promote → `Unverified`; only bound `verify --tx-id` sets
  `Verified`. Export `Verified→PASS` mapping unchanged but no longer fed by
  promote phantoms.
- **Honesty correction (0031 residual):** Track 0031 claimed MCP `ask`
  `--backend local` made cloud egress "explicit/opt-in on every path including
  MCP." That was incomplete — `--backend local` only reordered the provider
  chain; cloud fallbacks inside local completion and priority tails still
  egressed. **0073** closes that gap with process-level Forbidden policy.
  Interactive CLI `ask --backend local` remains **Allowed-with-fallback**
  (human operator path); only MCP spawn / explicit Forbidden forces zero-cloud.
- MCP `ask` tool blurb documents Forbidden + host opt-in (not "forces local"
  alone).
- **Install docs truth (0068):** Homebrew/Scoop install docs and PATH FAQ
  corrected to match actual installer behavior; new `install_docs_truth`
  guard catches future drift. Engine's dogfood PR-scan workflow SHA-pinned
  to `ledgerful-action@2c8dacbc`.

### Security

- **Public ledger verifier XSS fix (0075):** the export-public verifier
  template's row/status rendering replaced `innerHTML` sinks with
  `createElement`/`textContent`, closing a DOM-XSS path via malicious
  free-text ledger fields. Signature verification basis (`VERIFY-ON-RAW`)
  is unaffected — this is a rendering fix, not a crypto change.
- **Offline network honesty + gitleaks pin + PAT-out-of-URL (0082 engine
  slice):** `LEDGERFUL_NO_NETWORK` is now honored *before* AI-reachability
  probes run (previously only gated the request itself); CI's `gitleaks`
  step digest-pinned to match the frontend's pin (`v8.30.1@sha256:...`);
  release automation's package-manager-manifest push now authenticates via
  `gh auth setup-git` instead of embedding `MANIFEST_PUSH_TOKEN` directly in
  a git remote URL.
- **0073 residuals (not closed here):** RT-A5 tool *results* are not
  secret-redacted (MCP can still leak repo secrets to the agent); RT-A7 full
  semantic-extract cloud closure beyond Forbidden-env paths; RT-A8 tool DoS /
  limit clamps; Forbidden blocks known cloud providers, not arbitrary
  non-loopback HTTP to a misconfigured local `base_url`.

## [0.1.8] - 2026-07-12

### Added

- `ledger re-sign` command: audited key-repair for invalid ledger signatures
  (`--dry-run`, `--yes`, `--tx`, `--all-invalid`), WAL-safe backup with
  integrity check, batch MAINTENANCE ledger entry.
- `export evidence --profile soc2` CLI command: SOC2 evidence ZIP export
  outside the `web` feature (new `export` cargo feature).
- `demo` command: synthetic repo driven through the real hook flow with
  ephemeral demo-local keypair, DEMO-marked on every surface.
- Ledger chain hash: additive `prev_hash` linkage + signed `chain_head` table.
  `verify --signatures --chain` validates end-to-end, fails-closed on
  downgrade. `verify --against-export <path>` detects rollback/tail-truncation.
  SOC2 export includes `chain_head.json`.
- Observe/enforce gate mode: `gate.mode = observe | enforce` in config
  (default observe). `init` → observe, `init --enforce` → enforce, `gate mode`
  transitions write signed MAINTENANCE entry. `observed` metadata marker on
  entries with warned conditions.
- `GET /api/trends?days=N` endpoint: cached daily rollup of hotspot scores
  (`project_trend_days` table, migration m49), populated incrementally by
  post-commit hook.
- Per-file diff stats: `changed_files.additions`/`deletions` populated at
  commit time via `git show --numstat` (rename-aware, committed-diff basis).
  `is_binary` flag for binary files. `ChangedFile` wire type now nullable.
- OpenAPI contract gap closure: `/api/sync/status` in schema (unconditional,
  501 no-sync fallback), `snapshot.recent_changes` typed as `Vec<ChangeResponse>`,
  `snapshot.top_hotspots` typed as full `HotspotResponse`.
- Supply-chain attestation pipeline: CycloneDX SBOM (engine + MCP npm),
  cosign keyless signing, SLSA build-provenance via `actions/attest`, SBOM
  attestation via `actions/attest-sbom`, `cargo auditable` embedded deps.
  Phase 3 attestations gated on public/Enterprise repo.
- `--tx-id` flag on `verify`: auto-bind via `COMMIT_EDITMSG` when inside a
  live commit hook. `/api/ledger/:txId` now returns real `tests_run`/`flakes`
  from bound verification runs.
- Hotspot DTO enrichment: `lastTouchedAt`, `contributor`, `changeCount`,
  `rank` via `project_files` (migration m47).
- Entity fallback: `"(uncategorized)"` substitution at JSON-serialization
  for federated/sibling entries with empty `entity`.
- `validation_warnings` on `/api/projects` for stale siblings.
- Unexposed commands wired: `ledger resume`, `ledger export-provenance`,
  `ledger hook-repair`, `ledgerful openapi` (CLI dev-tooling).
- CLI ergonomics: quiet-by-default commit path via `tracing` target routing
  (`cli_summary`), proactive sidecar GC, softer `ledger status` UI.
- Scan reliability config: `federation.scan_exclusions`, `sync_timeout_secs`,
  `scan_file_budget`, `scan_timeout_secs` (all serde-defaulted).
- AI/agent boundary security: MCP `ask` forces `--backend local` unless
  `LEDGERFUL_MCP_ALLOW_CLOUD_EGRESS=1`; `--` separator prevents flag injection;
  LLM JSON parsing verify-before-trust with bounds.
- God-file refactoring: 6 HIGH-severity files split into focused submodules
  (web/types, server/, api/, ask/, index/, config/, storage/).

### Changed

- `impact` graceful degradation: first-byte timeout (15s), bounded retries,
  deterministic output + `analysis_warnings` when model unavailable.
- `federate export` gets per-sibling timeout (30s) + process-group kill;
  `scan_dependency_dir` gets file-count budget (5000).
- `fetch_changed_files` now uses shared per-file numstat parser
  (`src/git/numstat.rs`), rename-aware, committed-diff basis.
- Hotspot risk thresholds use log scale (4.0/3.0/2.0), not 0–100.

### Fixed

- `cozodb_integrity` test serialized with `#[serial(test)]` (resource
  contention, not a concurrency bug).
- `impact` no longer stalls ~10 min when completion model is unreachable.
- Ed25519 private key file renamed from `private.pem` to `private.key`
  (active one-time migrate).
- `memmap2 0.9.10` → `0.9.11` patched (tantivy/gix path).
- `crossbeam-epoch 0.9.18` → `0.9.20` (RUSTSEC-2026-0204).

### Security

- Daemon hardening: host-header validation (DNS-rebinding defense), CORS
  tightened to exact loopback origin, token moved to `Authorization` header,
  CSP header added, rate-limit on auth-failure paths.
- Crypto adversarial review: XChaCha20-Poly1305 upgrade, AAD binding, token
  leak fix, shell-exec opt-in, symlink-aware path containment,
  verify-before-deserialize, input size caps, proptest path fuzz.
- `SECURITY.md` with responsible disclosure policy, supported versions,
  response timelines, scoped safe-harbor.
- `responsible disclosure` channels (email + GitHub private vuln reporting).
- Supply-chain posture section in `SECURITY.md` with cosign/attestation
  verify commands and honest gaps.

## [0.1.6] - 2026-06-28

### First release

Ledgerful v0.1.6 is the first tagged release. It is the first build under the
Ledgerful name (renamed and relicensed from the prior internal project).

### Added

- CLI binary `ledgerful` (with `ldg` alias) for local-first change intelligence
  and transactional provenance.
- Core command surface: `init`, `setup`, `doctor`, `status`, `config`, `scan`,
  `watch`, `index`, `search`, `ask`, `impact`, `verify`, `hotspots`, `audit`,
  `dead-code`, `endpoints`, `data-models`, `services`, `dependencies`,
  `observability`, `security`, `federate`, `bridge`, `ledger`, `viz`, `intent`,
  `schedule`, `reset`, `update`, and `tests`.
- Ledger subsystem for transactional architectural memory: `start`, `commit`,
  `rollback`, `atomic`, `status`, `register`, `stack`, `search`, `audit`, `adr`,
  `reconcile`, and `graph`.
- Deterministic impact packets and risk summaries via `impact`, including
  symbols, imports, runtime usage, complexity, temporal coupling, hotspots, CI
  predictions, and federated impact.
- Predictive verification via `verify` with Bayesian failure probability
  ordering, structural impact, temporal coupling, and CI predictions.
- Sub-millisecond regex codebase search via Tantivy trigrams and ranked BM25
  queries (`search`), plus optional semantic search through `index --semantic`.
- `ask` command for Gemini or local-LLM-assisted analysis, suggestions, patch
  review, and narrative reporting.
- Optional LSP daemon (`ledgerful daemon`) behind the `daemon` Cargo feature.
- Optional knowledge graph visualization server (`ledgerful viz-server`) behind
  the `viz-server` feature.
- Optional embedded local web dashboard (`ledgerful web`) behind the `web`
  feature, with token-authenticated access and background serving on Unix and
  Windows.
- MCP stdio server (`ledgerful mcp`) behind the `mcp` feature, plus the
  `@ledgerful/mcp-server` npm wrapper for AI-agent integration.
- Optional sync subsystem (`ledgerful sync`) behind the `sync` feature.
- Optional usage metrics collection (`ledgerful usage`) behind the
  `usage-metrics` feature.
- `.ledgerful/` local state layout with config, rules, reports, and SQLite/Cozo
  knowledge graph state.
- Multi-OS release pipeline building Linux x86_64, macOS Intel, macOS ARM64, and
  Windows x86_64 archives with SHA256 checksums, plus the MCP npm package.

### Known limitations

- The repository is currently private; public download paths will be proven in a
  follow-up track.
- The embedded web SPA is produced by a separate private repository and bundled
  at release build time.
- The MCP npm wrapper downloads its matching release binary from GitHub on first
  install.
