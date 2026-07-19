# Changelog

All notable changes to Ledgerful are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Local self-timing facility: `ledgerful timings` surfaces which of *your*
  commands is slow (outer summaries, inner spans, collapsed flame stacks,
  `--explain`), with default-on capture, opt-out via `timings --opt-out`, and
  strict privacy (no paths/argv values/network). See
  [docs/self-timing.md](docs/self-timing.md).

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
