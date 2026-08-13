# Operator Surface Policy

This document defines the default quality bar for Ledgerful command surfaces that are used interactively by humans and programmatically by agents.

The goal is not novelty. The goal is to match the baseline operator expectations set by mature CLI tools: truthful status, predictable flags, parseable output, explicit prerequisites, and actionable next steps.

## Core Policies

1. Truthful over optimistic
   - A command must distinguish "feature exists" from "feature is currently populated and meaningful on this repo".
   - Health and status surfaces must separate hard failure, transient failure, disabled-by-config, missing prerequisite, stale cache, and genuinely empty result.

2. Structured sources before LLM synthesis
   - If a question can be answered from CLI help, indexed command metadata, the Knowledge Graph, or repo-local docs, that path wins over free-form completion.
   - Completion backends may summarize grounded results, but they should not replace deterministic retrieval for command-discovery or structural questions.

3. Stdout is the contract
   - Machine-readable output belongs on stdout only.
   - Diagnostics, progress, retries, and backend chatter belong on stderr or tracing.
   - A command advertised as parseable JSON must emit only JSON on stdout.
   - Warnings, prompts, cache notices, and retry messages must not precede or follow JSON on stdout.

4. Empty states must be classified
   - Sparse surfaces must classify at least these states where relevant:
     - clean diff / no changed entities
     - disabled by config
     - prerequisite files absent
     - index missing or stale
     - indexed but no matches
     - enabled but errored before data fetch or traversal completed
     - cache stale or corrupt
   - Empty output is not enough; the reason must be explicit.

5. Every empty state gets one real next step
   - When recovery is possible, print the exact command or file path that advances the state.
   - Do not recommend reindexing when configuration disables the surface.
   - Do not recommend configuration changes when the repo intentionally disables a feature by policy.

6. Flags are consistent across related surfaces
   - Entity-targeting commands should converge on `--entity` while preserving positional compatibility where already established.
   - Similar commands should not require users or agents to memorize one-off argument shapes.

7. Caches must advertise freshness
   - Cached artifacts such as `latest-impact.json` are useful, but the reader must be told whether they are current, stale, missing, or corrupt.
   - Consumers must not silently treat a stale cache as authoritative state.

8. Optional subsystems should still be exercised
   - Repo-local optional surfaces such as observability and security should have at least one checked-in fixture or smokeable path so the repo continuously exercises them.
   - If the main repo intentionally does not enable a subsystem, the repo should still provide fixture-backed verification coverage.
   - **Security (0186-E):** the committed pack at `policies/daemon-api.cedar` **is** default production-facing scan content. Policy 8’s older “isolated from default scans” clause is **superseded for security**. Hermetic fixtures under `tests/fixtures/policies/` remain test-only and are not ingested from that path.
   - **Observability:** stays fixture-isolated. There is no product OpenSLO under `observability/` (declined). Copy the OpenSLO fixture only for an explicit smoke, then delete it.

9. Provenance should show exactness
   - Provenance surfaces must distinguish exact links from derived or heuristic links.
   - Users should be able to tell whether a transaction-to-entity relationship came from token provenance, changed files, directory derivation, or other fallback logic.

10. Default output should be concise
    - Interactive defaults should optimize for operator signal, not raw exhaustiveness.
    - Full graph, duplicate-heavy, or verbose outputs should be opt-in.
    - Default (non-verbose, non-`RUST_LOG`) human runs must not emit timestamped
      tracing-style `INFO` on stderr (log-file posture). Product notices use
      `println!` / `eprintln!` / `cli_summary`; backend diagnostics require `-v`
      or an explicit `RUST_LOG` directive (0154).

11. Bounded work by default
    - Operator conveniences such as retries, graph expansion, and bootstrap helpers must be capped so a default invocation stays responsive on large repos.
    - Expensive deep dives should require explicit opt-in flags, limits, or pagination.

## Current Repo Policy Decisions

- `services diff` remains config-aware and intentionally follows the repo's current `coverage.enabled` policy. This document does not require enabling service inference by default.
- Dogfooding optional surfaces should use the committed pack (env schema + Cedar) plus focused tests. Do not silently flip product `coverage.enabled` (or deploy) for unrelated workflows. `[services]` remains a local-only recipe (`.ledgerful/` is gitignored).

## Enforcement Direction

New development tasks should reference this policy when they touch:

- command-discovery or operator-facing `ask`
- health and status commands
- JSON or stdout/stderr contracts
- sparse surfaces with prerequisites
- cache readers
- CLI argument conventions
- provenance and audit surfaces

## Engine dogfood pack (0186)

Clone-durable content (Phase A) lives in git:

- `.env.example` — operator-facing env schema. After `ledgerful index --incremental`,
  `ledgerful config schema` is ready. Secrets stay empty. Copy to a gitignored `.env`
  for real values.
- `policies/daemon-api.cedar` — 8 core `/api` permits (not a live PDP; daemon auth is
  still Bearer). After `ledgerful index --analyze-graph`, `ledgerful security boundaries`
  is ready. This file is **default scan content**. Do not copy
  `tests/fixtures/policies/dogfood_policy.cedar` into `policies/`.

Expected `ledgerful surfaces` on this checkout after Phase A index, without flipping
coverage: **2 gated · 1 empty · 3 ready** (services + deploy gated; observability empty;
schema + security + data-models ready).

### Phase B — local `[services]` recipe (not clone-durable)

`.ledgerful/` is gitignored, so declared services and coverage flags cannot ship with
the repo without flipping product defaults (declined). On this machine only:

```text
ledgerful config set coverage.enabled=true
ledgerful config set coverage.services.enabled=true
# then hand-edit .ledgerful/config.toml with the [[services.definitions]] block
# from docs/examples/config.toml — config set cannot write array-of-tables
ledgerful index --analyze-graph
```

`docs/examples/config.toml` is a fully-enabled **example** (coverage.global / services /
deploy already true). Copying it to `.ledgerful/config.toml` can skip the `config set`
steps. Do **not** treat `coverage.deploy.enabled=true` as an 0186 DoD. Product
`CoverageConfig` defaults stay false.

### Observability fixture smoke (still isolated)

OpenSLO remains declined as product content. To smoke the parser path only:

**Run from this repository root.** Fixture paths are relative to the current working
directory — a missed copy looks like `noIndexedData`, not a missing file.

1. Copy the OpenSLO fixture:
   `New-Item -ItemType Directory -Force -Path observability; Copy-Item -Path tests/fixtures/observability/dogfood_slo.yaml -Destination observability/dogfood_slo.yaml`
2. `ledgerful index --analyze-graph`
3. `ledgerful observability coverage`
4. Clean up:
   `Remove-Item -Force -Path observability/dogfood_slo.yaml; ledgerful index --analyze-graph`

Hermetic tests still parse `tests/fixtures/policies/dogfood_policy.cedar` and the
OpenSLO fixture in-place. Those files are **not** default security scan content.

