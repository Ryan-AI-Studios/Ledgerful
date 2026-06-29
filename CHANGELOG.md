# Changelog

All notable changes to Ledgerful are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
