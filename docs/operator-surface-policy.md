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
   - Dogfood fixtures must be isolated from default production-facing scans unless the operator explicitly opts into the fixture path.

9. Provenance should show exactness
   - Provenance surfaces must distinguish exact links from derived or heuristic links.
   - Users should be able to tell whether a transaction-to-entity relationship came from token provenance, changed files, directory derivation, or other fallback logic.

10. Default output should be concise
    - Interactive defaults should optimize for operator signal, not raw exhaustiveness.
    - Full graph, duplicate-heavy, or verbose outputs should be opt-in.

11. Bounded work by default
    - Operator conveniences such as retries, graph expansion, and bootstrap helpers must be capped so a default invocation stays responsive on large repos.
    - Expensive deep dives should require explicit opt-in flags, limits, or pagination.

## Current Repo Policy Decisions

- `services diff` remains config-aware and intentionally follows the repo's current `coverage.enabled` policy. This document does not require enabling service inference by default.
- Dogfooding optional surfaces should use checked-in fixtures, focused tests, or explicit smoke recipes rather than silently changing repo policy for unrelated workflows.

## Enforcement Direction

New conductor tracks should reference this policy when they touch:

- command-discovery or operator-facing `ask`
- health and status commands
- JSON or stdout/stderr contracts
- sparse surfaces with prerequisites
- cache readers
- CLI argument conventions
- provenance and audit surfaces

## Dogfood Fixture Smoke Recipes

To manually smoke-test optional observability and security surfaces using the provided dogfood fixtures:

**Run these commands from the repository root of this Ledgerful/Ledgerful checkout.** The fixture
source paths below (`tests/fixtures/...`) are relative paths resolved against the current working
directory, not the repo root — if you run them from any other directory (including a separate clean
test repo used to validate this recipe), `Copy-Item` will throw a clear `PathNotFound`-style error for
the missing source. Follow that error rather than ignoring it: a silently-skipped copy leaves
`observability/`/`policies/` empty, which surfaces later only as a confusing "no coverage data" /
`noIndexedData` result from steps 2-3, with no obvious link back to the real cause.

1. Copy the dogfood fixtures to the active scanning directories:
   - For observability (OpenSLO):
     `New-Item -ItemType Directory -Force -Path observability; Copy-Item -Path tests/fixtures/observability/dogfood_slo.yaml -Destination observability/dogfood_slo.yaml`
   - For security policies (Cedar):
     `New-Item -ItemType Directory -Force -Path policies; Copy-Item -Path tests/fixtures/policies/dogfood_policy.cedar -Destination policies/dogfood_policy.cedar`

2. Re-index and build the knowledge graph with graph analysis enabled:
   `ledgerful index --analyze-graph`

3. Verify the surfaces are populated and print correct coverage/boundaries:
   - Run observability coverage:
     `ledgerful observability coverage`
   - Run security boundaries:
     `ledgerful security boundaries`

4. Clean up the dogfood fixtures and re-index to restore the clean repository state:
   `Remove-Item -Force -Path observability/dogfood_slo.yaml; Remove-Item -Force -Path policies/dogfood_policy.cedar; ledgerful index --analyze-graph`

