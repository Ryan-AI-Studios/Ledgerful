---
name: ledgerful
description: Use Ledgerful for local-first change intelligence before, during, and after code edits. Trigger this skill whenever a repository contains Ledgerful, the user asks about impact analysis, blast radius, risk, verification planning, hotspots, temporal coupling, Gemini-assisted review, or wants an AI agent to make safer changes with evidence from `ledgerful scan`, `impact`, `verify`, or `ask`.
---

# Ledgerful

Use this skill to make code changes with Ledgerful's local risk, impact, and verification signals.

This file is intentionally portable:

- For Claude Code skills, copy it to a skill folder as `SKILL.md`.
- For Gemini CLI agent skills, copy it to an extension skill folder such as `skills/ledgerful/SKILL.md`.
- For plain agent instructions, paste the full body into the agent's repo instructions.

## Purpose

Ledgerful is a local-first CLI that turns repository changes into deterministic impact packets, risk summaries, hotspot rankings, targeted verification plans, and bounded Gemini context.

Use it as a safety and planning layer. It is not the source of truth for code correctness; it tells you what changed, what may be affected, and what should be verified.

## When To Use

Use Ledgerful when:

- Starting work in a repo that already has `.ledgerful/`.
- Planning a non-trivial code change.
- Reviewing staged or unstaged changes.
- Deciding which tests or checks to run.
- Estimating blast radius before editing shared code.
- Investigating risky files, hotspots, temporal coupling, or cross-repo dependencies.
- Preparing structured context for an AI coding assistant.
- Producing a handoff summary after implementation.

## First Checks

From the repository root, inspect whether Ledgerful is available:

```bash
ledgerful doctor
```

If the command is unavailable, do not invent Ledgerful output. Tell the user it is not installed or not on `PATH`, then continue with normal repository inspection.

If installation is allowed, install Ledgerful like a normal CLI:

```bash
curl -fsSL https://raw.githubusercontent.com/Ryan-AI-Studios/Ledgerful/main/install/install.sh | sh
```

On Windows PowerShell:

```powershell
iwr https://raw.githubusercontent.com/Ryan-AI-Studios/Ledgerful/main/install/install.ps1 -UseBasicParsing | iex
```

After installing, open a new terminal if needed and re-run:

```bash
ledgerful doctor
```

If the repo has not been initialized and the user wants Ledgerful used here:

```bash
ledgerful init
ledgerful doctor
```

## Core Capabilities

- **Search & Discovery**: High-performance regex (Tantivy), precise LSP navigation (SCIP), and conceptual semantic search (local embeddings) with parallel HNSW retrieval.
- **Code Symbol Index**: Tree-sitter parsing of Rust, TypeScript, and Python — extracts every public function, struct, enum, trait, module, and HTTP route into the Knowledge Graph.
- **Call Graph**: Tracks function call relationships (`Direct`, `MethodCall`, `TraitDispatch`, `Dynamic`, `External`) so you can answer "what calls this function?" and "what does this function depend on?".
- **Knowledge Graph**: Durable, billion-edge relational and vector storage (CozoDB-redux/Sled) with native code-aware tokenization (Tree-Sitter).
- **Impact Analysis**: Deep "blast radius" analysis across 20+ specialized providers (Infra, Contracts, Observability, Temporal).
- **Cryptographic Provenance**: Mathematical proof of intent via Ed25519 signing of every ledger entry. Offline verification via `verify --signatures`.
- **Intent Capture TUI**: Interactive terminal UI for auditing and refining LLM-drafted intent payloads during the git commit process.
- **Real-time Sync**: Incremental Knowledge Graph updates, AST re-parsing, and code-aware symbol indexing via the `watch` command.
- **Predictable Verification**: Bayesian test reordering and CI failure prediction.
- **Scoped Verification**: `ledgerful verify --scope fast` uses the `test_mapping` index to run only the tests covering changed files (nextest filtersets), falling back to the full suite when shared infrastructure is touched. The pre-push hook uses `--scope fast`; CI uses `--scope full`.
- **Documentation Generation**: Export Knowledge Graph data to Markdown/Mermaid passive documentation (`index --export-docs`).
- **Dead Code Detection**: Confidence-based dead code detection blending graph reachability, git activity, and test history (`dead-code` command). Use `dead-code --prune` for interactive opt-in removal.
- **Live Visualization**: WebSocket-based Arc Diagram for real-time Knowledge Graph updates (`viz-server`, `viz-server --stop`).
- **Endpoints**: Indexed endpoint graph with auth, schemas, consumers, and owner links. `ledgerful endpoints --json` / `--changed`.
- **Services Diff**: Declared service map with queue/topic/RPC edges and PR-style boundary diff. `ledgerful services diff`.
- **Data Models**: Durable data model, table, migration, and compatibility-class relations with impact rules for destructive changes. `ledgerful data-models impact --changed`.
- **Config Schema & Diff**: Explicit env var schema metadata and change diff. `ledgerful config schema` / `config diff`.
- **Dependency & Advisory Graph**: Cargo/npm/Python lockfile ingestion with cargo-audit/osv advisory matching.
- **Test Mapping**: Durable test nodes linked to endpoints, symbols, services, and data models. `ledgerful verify --explain --entity <path>`.
- **Observability Graph**: SLO, metric, alert, and signal nodes from OpenSLO YAML. `ledgerful observability diff` / `observability coverage`.
- **Hotspot Trends**: Persistent hotspot and temporal coupling snapshots with trend deltas. `ledgerful hotspots trend` / `hotspots explain`.
- **Ledger Graph**: Per-transaction entity neighborhood view. `ledgerful ledger graph <tx-id>`.
- **Security Boundaries**: Cedar policy parsing with cross-surface links. `ledgerful security boundaries` / `security impact --changed`.
- **Team Sync**: Decentralized team ledger synchronization via `ledgerful sync` (optional `sync` feature).
- **AI-Brains Bridge**: Exports hotspots, ledger entries, and MADR data to AI-Brains via `ledgerful bridge export --hotspots --ledger [--madr] [--stdout]`.

## Code Symbol Queries — Use These First

Before searching the web or reading files manually, query Ledgerful's symbol index.

```bash
# Always refresh the index first (incremental, fast)
ledgerful index --incremental

# Use automated SCIP indexing for compiler-grade precision (Rust, TS, Python)
ledgerful index --auto-scip

# Find a function, struct, or type by name
ledgerful search "handleGetUser"
ledgerful search "AuthMiddleware"

# Find HTTP routes
ledgerful search "POST /auth"
ledgerful ask "list all HTTP GET route handlers"

# Find what calls a function
ledgerful ask "what calls validateToken"
ledgerful ask "show callers of UserRepository::find_by_id"

# Find all public endpoints
ledgerful ask "find all Axum route handlers"

# Dead code
ledgerful dead-code --threshold 0.75
ledgerful dead-code --include-traits  # include standard traits (Eq, Clone, Debug, …)
```

> **Heuristic note**: Dead code analysis blends graph reachability, git inactivity, and test coverage. Results are probabilistic, not definitive. Common false-positive patterns: traits derived via `#[derive(...)]` (suppressed by default), types ending in `Provider`/`Chunk`/`Record`/`Result` (receive a confidence penalty).

## Core Workflow

Before making a meaningful edit:

```bash
ledgerful scan --impact
```

For quick triage:

```bash
ledgerful impact --summary
```

Read the generated report at `.ledgerful/reports/latest-impact.json`. Use the report to identify:

- `riskLevel`
- `riskReasons`
- changed files
- public symbols and imports
- runtime usage (environment variables, config keys)
- temporal couplings
- hotspots
- federated/cross-repo impact if present

After making edits:

```bash
ledgerful scan --impact
ledgerful verify
```

Read `.ledgerful/reports/latest-verify.json` and use it as the primary evidence for whether planned validation passed.

## Persistent Verification Plans

Ledgerful supports project-specific verification plans in `.ledgerful/config.toml`:

```toml
[verify]
default_timeout_secs = 300

[[verify.steps]]
description = "Run project tests"
command = "cargo test -j 1 -- --test-threads=1"
timeout_secs = 300

[[verify.steps]]
description = "Check formatting"
command = "cargo fmt --check"
```

When `ledgerful verify` runs without `-c`, it follows this priority:

1. **`-c` flag**: Single manual command (highest priority)
2. **Config steps**: Steps defined in `[verify]` config section
3. **Predictive mode**: Impact packet + rules + predictor
4. **Hardcoded default**: `cargo test -j 1 -- --test-threads=1`

Steps that omit `timeout_secs` inherit `default_timeout_secs`. Invalid steps (empty commands, zero timeouts) are warned and skipped rather than failing the entire config load.

## Command Guide

```bash
# Default workflow
ledgerful scan --impact
ledgerful verify
ledgerful hotspots
ledgerful federate status

# Targeted variants
ledgerful impact --all-parents
ledgerful impact --summary
ledgerful verify --no-predict
ledgerful verify -c "cargo clippy -- -D warnings"
ledgerful verify --scope fast          # scoped to changed files
ledgerful verify --scope full          # full suite
ledgerful hotspots --limit 20 --commits 500
ledgerful hotspots --json
ledgerful hotspots trend
ledgerful hotspots explain
ledgerful federate export
ledgerful federate scan
ledgerful endpoints --changed --json
ledgerful services diff
ledgerful data-models impact --changed
ledgerful config schema
ledgerful config diff
ledgerful observability diff
ledgerful observability coverage
ledgerful security boundaries
ledgerful security impact --changed
ledgerful ledger graph <tx-id>
ledgerful ledger status
ledgerful dead-code --threshold 0.75

# Gemini-assisted reporting (when configured)
ledgerful ask "What should I verify next?"
ledgerful ask --mode suggest "What checks should I run?"
ledgerful ask --mode review-patch "Review the current diff."
ledgerful ask --narrative
```

## Strategic Reasoning for AI Agents

When acting as a coding agent, use Ledgerful signals to adjust your strategy:

1. **Temporal Coupling**: If `latest-impact.json` shows high affinity (e.g., >70%) between a changed file and an unchanged file, you **must** read the unchanged file. Assume there is a logical dependency that imports alone do not show. Coupling scores use recency weighting — recent shared commits count more.
2. **Hotspots**: Files with high hotspot scores are "brittle." If you must edit a hotspot, prioritize refactoring or extremely high test coverage. Avoid adding complexity to an already complex hotspot.
3. **Federated Impact (Cross-Repo)**: If `federated_impact` warnings appear, your change might break a sibling repository. Explain this risk and suggest an `export-schema` to verify the contract.
4. **Predictive Verification**: If `verify` suggests tests that seem unrelated to your change, trust the predictor. It is likely based on historical failure correlations that aren't obvious from the code alone.
5. **Stale Data**: If you see a `data_stale` warning, run `ledgerful scan` and `ledgerful impact` immediately to refresh the local cache.

## How To Interpret Results

Treat `riskLevel` as a routing signal:

- `Low`: small or isolated change. Run Ledgerful's suggested verification and any obvious local tests.
- `Medium`: inspect affected files, imports, risk reasons, and predicted verification targets before choosing tests.
- `High`: slow down. Inspect temporal couplings, hotspots, public API changes, protected paths, runtime/config usage, and cross-repo links before finalizing.

Treat `prediction_warnings` in `latest-verify.json` as important. If prediction inputs degraded, explain that the verification plan may be incomplete.

## Ledger Provenance

For tracked manual edits:

```bash
ledgerful ledger start <entity> --category <CAT> --message "Intent"
# edit files
ledgerful ledger commit <tx-id> --summary "Done" --reason "Why"
```

For surgical one-command provenance:

```bash
ledgerful ledger atomic <entity> --category <CAT> --summary "Task" --reason "Goal"
```

For lightweight notes:

```bash
ledgerful ledger note <entity> "Note content"
ledgerful ledger note <entity> --message "Note content"
```

### Git Hook Lifecycle

Ledgerful uses a two-phase commit lifecycle to ensure zero phantom records:

1. **`commit-msg`**: Launches the TUI to capture intent. Creates a `PENDING` transaction and a sidecar file.
2. **`post-commit`**: Automatically promotes the `PENDING` transaction to `COMMITTED` once the Git commit is finalized. If the Git commit fails, the record remains pending or is safely rolled back.

### Cryptographic Security

If `intent.require_signing = true` in `.ledgerful/config.toml`, all ledger entries must be signed by the developer's local Ed25519 key (generated during `init`).

```bash
ledgerful verify --signatures
```

This performs an offline mathematical validation of every record against its signature and public key.

## Repository Configuration

Ledgerful's `.ledgerful/rules.toml` and `.ledgerful/config.toml` are repo-local policy, not portable defaults. When installing or copying this skill into another repository, review and update:

- `required_verifications`: use commands that actually exist in that repo.
- `verify.default_timeout_secs`: set a timeout that fits the repo's slowest expected verification command.
- `protected_paths`: keep enforcement scoped to paths that make sense for the repository.

If `ledgerful verify` fails with "Command not found" or times out while the same command passes manually, fix the repo-local config before treating it as a code failure.

`ledgerful init` sanitizes every starter template before creating `.ledgerful/config.toml`. Secret-bearing keys and credentialed connection URLs are omitted. Keep credentials in the environment or an ignored repo-local `.env`.

## Dependency Alert Workflow

For Dependabot or audit findings:

- Identify whether the vulnerable crate is direct or transitive with `cargo tree -i <crate>@<version>`.
- If the vulnerable crate is transitive through a direct dependency, prefer upgrading the direct dependency.
- If the vulnerable path enters through a git dependency, verify whether the upstream fix is visible to downstream consumers.
- Record external remediation handoffs in a development task when another repo owns the durable fix.
- After dependency changes, run focused dependency checks plus `ledgerful verify`.

## Maintenance & Upgrades

```bash
# Safely migrate repository state (clears indices, preserves ledger)
ledgerful update --migrate --force

# Rebuild indices after migration
ledgerful index --semantic
```

## When To Skip

Skip Ledgerful only for trivial formatting, simple dependency lockfile updates, binary/media changes, temporary scratch files, or when the user explicitly says to bypass it.

## If Commands Fail

- If `ledgerful` is unavailable, continue with normal repo tools and tell the user Ledgerful signals were unavailable.
- If `ledger status` shows unaudited drift, reconcile or adopt before continuing unless the user directs otherwise.
- If `scan --impact` cannot complete, continue cautiously and include the error in the final report.
- If a command reports that the index is `[STALE]`, append `--auto-index` to commands like `search`, `ask`, `hotspots`, or `dead-code` to automatically refresh it.
- Do not edit `.ledgerful/` state files directly.

## Safety Notes

Ledgerful is local-first, but its `ask` command invokes Gemini CLI or a local model. Before using `ledgerful ask`, confirm the user is comfortable sending sanitized, truncated repository context to the configured backend.

Never paste secrets from `.env`, config files, reports, or terminal output into prompts or final responses. If Ledgerful reports redaction or prompt truncation, mention that it occurred without revealing the redacted value.

## Reasoning Rules

- If temporal coupling is above 70% for an unchanged file, inspect that file.
- If hotspots are reported, bias verification toward those files first.
- If KG reachability identifies downstream nodes, inspect them before finalizing.
- Treat hooks and CI gates as enforcement. Treat this skill as guidance.

## Final Response Template

When reporting work that used Ledgerful, include:

```text
Ledgerful:
- impact: <low|medium|high>, with key risk reasons
- affected areas: <important files/modules/symbols>
- hotspots/couplings: <notable findings or "none material">
- verification: <commands run and pass/fail result>
- warnings: <prediction/degradation warnings or "none">
```

Keep the summary factual. If Ledgerful could not run, say why and name the fallback verification you performed.