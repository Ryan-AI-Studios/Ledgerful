# Changelog

All notable changes to Ledgerful are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

- **SCIP augments the native index (0095, user-visible):** `--auto-scip` /
  `--scip <PATH>` no longer early-return or replace the native pipeline. SCIP
  runs after `build_call_graph` (and again inside `--analyze-graph` before
  centrality/KG) and adds reference edges onto **native** symbol ids with
  `evidence=scip:ref`. Detection is a base-exe `--version` capability probe
  (rustup shims resolve unavailable). Process policy uses the configured
  verify allow/deny list. `index --json` always includes a `scip` object with
  an explicit status (`did_not_run` / `success` / `failed` / `skipped_stale`).
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
  can hide them while the aggregate stays at `info!`; **default interactive
  verbosity is unchanged** (summary filter remains `DEBUG`). Double-emitted
  observe would-block / CRITICAL messages now emit once via `cli_summary` only.
- **Machine mode silences normal_layer progress INFO (0093 R1):** under `--json`
  / machine mode the non-`cli_summary` EnvFilter is raised to `WARN`, so a
  successful `verify --json` has empty stderr. `WARN`/`ERROR` still pass.
  `verify --json` rejects combination with `--signatures` / `--chain` /
  `--against-export` (same pattern as `--health` / `--dry-run`). CI prediction
  `println!` tables are gated on `suppress_human_output`.

### Fixed

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

## [0.2.2] - 2026-07-27

### Fixed

- **`--open` handoff reliability (0090 follow-up):** bind the web listener **before**
  opening the browser so the SPA can load and call `POST /api/session/exchange`
  immediately. On Windows, open the handoff URL via `ShellExecuteW` so `#c=<code>`
  is not stripped (cmd `start` treats `#` as a batch comment). Optional
  `LEDGERFUL_WEB_OPEN_URL_FILE` writes the open URL for integration harnesses
  instead of launching a browser.

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
