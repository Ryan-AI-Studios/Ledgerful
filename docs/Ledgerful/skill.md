---
name: ledgerful
description: Use Ledgerful for local-first change intelligence before, during, and after code edits. Trigger this skill whenever a repository contains Ledgerful, the user asks about impact analysis, blast radius, risk, verification planning, hotspots, temporal coupling, Gemini-assisted review, or wants an AI agent to make safer changes. Default pre-edit evidence is `doctor --json` then `change-context --json`; escalate to `scan --impact` only for high-risk / readSetCapped / multi-module cases. Also use for `verify`, `ask`, ledger provenance, and drift handling. Prefer `--json` for machine-readable agent output.
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

## Git worktrees

Linked worktrees share ledger state with the primary worktree's `.ledgerful` (same pending TX and `ledger.db`). Run `ledgerful` commands from the worktree cwd; do not copy state into the linked tree. Submodules keep their own `.ledgerful`. Set absolute `LEDGERFUL_STATE_DIR` to override.

## Independent / Cross-Model Review (read-only)

For high-risk diffs, a read-only independent review (Codex `codex exec -s
read-only`, restricted subagent, second model) can ground DoD audit without a
writable implementer tree. Full durable matrix: [docs/reviewer-readonly.md](../reviewer-readonly.md).

### Honesty ceiling

Full `verify` / cargo / nextest / `index` rebuild / `ledger start|commit`
**require a writable environment**. Never claim full gates in pure zero-write RO.
On storage failure: report unavailable — do not invent impact.

### Command matrix (agent-critical)

| Class | Examples | Pure RO |
|---|---|---|
| A Git | `git status` / `diff` / `log` | Always |
| B Read-heavy | `ledger status`, `audit` | Prefer existing `ledger.db` |
| C Write-open | `doctor` (always); `change-context` soft-opens when DB exists | Doctor: **skip** on pure RO |
| D Write/exec | `index`, `scan`, `verify`, ledger start/commit | **Not** reviewer job |
| E Network | ask/embed probes, caches | Separate from FS RO |

**Hosts:** Codex `-s read-only` (native Windows OK) = pure RO. Codex
`--sandbox workspace-write` (not deprecated `--full-auto`) for Class C/D when
orchestrator authorizes. Claude Bash sandbox = **cwd + `$TMPDIR` writable by
default** — **≠** Codex pure RO.

### Reviewer ladder

1. `git status` + `git diff` (always).
2. If `ledgerful` on PATH and populated `.ledgerful` (or absolute
   `LEDGERFUL_STATE_DIR` → populated state):
   - `ledgerful ledger status --json` (or `--compact`)
   - `ledgerful audit` when provenance matters
   - `ledgerful change-context --json` (optional `--base-ref`)
   - **Skip** `doctor --json` on pure RO unless workspace-write / pre-written
     `doctor-results.json`
3. If change-context fails RO/permission: git-only + note grounding unavailable
   under pure RO (do not use that phrase for Claude cwd-writable without evidence).
4. **Never** run `verify` / `index` / `scan --impact` as the reviewer unless
   workspace-write (or stronger) **and** the orchestrator authorized write-class
   gates (`codex-review` skill: orchestrator owns gates).

### Env footgun

`LEDGERFUL_STATE_DIR` must be absolute and point at an **existing populated**
`.ledgerful`. Empty temp → empty index false confidence. Worktrees share main
state by default; do not copy state into the linked tree.

### Codex invocation hygiene

```powershell
# -s read-only only. Do NOT invent -a never or --full-auto.
cmd /c "codex exec -C ""C:\dev\Ledgerful"" -s read-only -m gpt-5.4 -o output\review.md ""Review the current diff for regressions. Do not modify files."" < NUL"
```

If the command appears stuck, inspect the output file before waiting longer; the
review may already have written useful findings.

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
# Prefer --json when parsing; branch on readyForPublish (zero block findings).
ledgerful doctor --json
# Human: ledgerful doctor
```

**`readyForPublish == true`** means the publish-environment path is fit to enter
(no lifecycle/tool **block** findings). It does **not** mean `verify` passed,
tests are green, or CI is green. Optional backends (embedding/completion/SCIP/
sccache/gemini) never set `readyForPublish=false`. See `docs/doctor-severity.md`.

**Signing hygiene:** when doctor reports `sig-pin` / `sig-version`, follow the
structured `remediation` commands. Pinning is an identity allowlist — not free-text
proof of intent. Use `ledger re-sign --all` to upgrade legacy v1 rows before raising
`min_sig_version=2`; `--all-invalid` is key-repair only.

If the command is unavailable, do not invent Ledgerful output. Tell the user it is not installed or not on `PATH`, then continue with normal repository inspection.

### MCP host wiring (optional)

To register Ledgerful as an MCP server in a supported agent host (Top-N only:
`claude-code`, `cursor`, `codex`, `copilot`), prefer:

```bash
ledgerful mcp install
# or: ledgerful mcp install --platform cursor --dry-run --json
```

Do **not** invent host-specific `mcp add` commands. Full registration, tools
(including `change_context` first), and limitations:
[`references/mcp.md`](references/mcp.md).
Bare `ledgerful mcp` still serves stdio.

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
ledgerful doctor --json
```

If the repo has not been initialized and the user wants Ledgerful used here:

```bash
ledgerful init
ledgerful doctor --json
```

## Core Capabilities

- **Search & Discovery**: High-performance regex (Tantivy), optional SCIP edge augment (`index --auto-scip`, off by default), and conceptual semantic search (local embeddings) with parallel HNSW retrieval.
- **Code Symbol Index**: Tree-sitter parsing of Rust, TypeScript, and Python — extracts every public function, struct, enum, trait, module, and HTTP route into the Knowledge Graph.
- **Call Graph**: Tracks function call relationships (`Direct`, `MethodCall`, `TraitDispatch`, `Dynamic`, `External`) so you can answer "what calls this function?" and "what does this function depend on?".
- **Knowledge Graph**: Durable, billion-edge relational and vector storage (CozoDB-redux/Sled) with native code-aware tokenization (Tree-Sitter).
- **Impact Analysis**: Deep impact analysis across 20+ specialized providers (Infra, Contracts, Observability, Temporal). Structural **`blastRadius`** is depth-1 by default (`--blast-depth 2` only high-confidence + transitive); must-touch punchlist — not a complete call graph; ≠ deploy high-blast resources. Edges carry `confidenceClass` (`SCIP_BOUND`/`RESOLVED`/…); change-context and blast expose `confidenceSummary` counts (not full edges on change-context).
- **Cryptographic Provenance**: Mathematical proof of intent via Ed25519 signing of every ledger entry. Offline verification via `verify --signatures`. Chain continuity via `verify --signatures --chain`. Independent rollback detection: `export head` + off-machine retention + `verify --against-export` (checkpoint extends-or-equals; `--exact` for freeze). See `docs/chain-checkpoint.md`.
- **Intent Capture TUI**: Interactive terminal UI for auditing and refining LLM-drafted intent payloads during the git commit process.
- **Real-time Sync (watch)**: Incremental Knowledge Graph updates, AST re-parsing, and code-aware symbol indexing via the `watch` command — **not** team ledger sync.
- **Predictable Verification**: Bayesian test reordering and CI failure prediction.
- **Scoped Verification**: `ledgerful verify --scope fast` uses the `test_mapping` index to run only the tests covering changed files (nextest filtersets). Shared infrastructure still runs full; mapping-cannot-scope **refuses** (not surprise full) unless `--allow-full-fallback`. Empty changes → cheap path (Rust: fmt+clippy; non-Rust: zero steps, exit 0). The pre-push hook uses `--scope fast`; CI uses `--scope full`.
- **Documentation Generation**: Export Knowledge Graph data to Markdown/Mermaid passive documentation (`index --export-docs`).
- **Dead Code Detection**: Confidence-based dead code detection blending graph reachability, git activity, and test history (`dead-code` command). Use `dead-code --prune` for interactive opt-in removal.
- **Live Visualization**: WebSocket-based Arc Diagram for real-time Knowledge Graph updates (`viz-server`, `viz-server --stop`).
- **Endpoints**: Indexed endpoint graph with auth, schemas, consumers, and owner links. `ledgerful endpoints --json` / `--changed` (matches handler symbol, impl file, registration file, or blast edges — not registration-only). Change-set `affectedFlows` on impact / change-context / PR is the same route-map signal (sample-capped on reports; filter uncapped).
- **Services Diff**: Declared service map with queue/topic/RPC edges and PR-style boundary diff. `ledgerful services diff`.
- **Data Models**: Durable data model, table, migration, and compatibility-class relations with impact rules for destructive changes. `ledgerful data-models impact --changed`.
- **Config Schema & Diff**: Explicit env var schema metadata and change diff. `ledgerful config schema` / `config diff`.
- **Dependency & Advisory Graph**: Cargo/npm/Python lockfile ingestion with cargo-audit/osv advisory matching.
- **Test Mapping**: Durable test nodes linked to endpoints, symbols, services, and data models. `ledgerful verify --explain --entity <path>`.
- **Observability Graph**: SLO, metric, alert, and signal nodes from OpenSLO YAML. `ledgerful observability diff` / `observability coverage`.
- **Hotspot Trends**: Persistent hotspot and temporal coupling snapshots with trend deltas. `ledgerful hotspots trend` / `hotspots explain`.
- **Ledger Graph**: Per-transaction entity neighborhood view. `ledgerful ledger graph <tx-id>`.
- **Security Boundaries**: Cedar policy parsing with cross-surface links. `ledgerful security boundaries` / `security impact --changed`.
- **Team Sync [Available — opt-in shared-folder v1]**: Opt-in encrypted ledger entry bundles via `ledgerful sync` (default feature; `[sync].enabled = false` forever until you opt in). Pairing (`LF-PAIR-1` + `sync pair`), secure shared-folder transport/apply (`.lfbundle`, verify-then-apply), and low-friction ops (`sync setup` checklist, gated `setup --enable`, status next-action) are real (0110–0113). Not default-on, not cloud, not CRDT. Never auto-enables; setup/status never prompt for secret. See `docs/team-sync.md`. Not the same as watch “Real-time Sync”.
- **Bridge Export/Import**: Local versioned NDJSON interchange for hotspots, ledger entries, and MADR data via `ledgerful bridge export --hotspots --ledger [--madr] [--stdout]` and `ledgerful bridge import --input <records.ndjson>`. The bridge is **off by default**; run it only after opting in.

## Bridge Opt-In

The Ledgerful bridge is **disabled by default** so a fresh install performs zero external/implicit activity. To enable it, add this to `.ledgerful/config.toml`:

```toml
[bridge]
enabled = true
provider_command = "ai-brains"
```

Or set the environment variable for the current session:

```bash
export LEDGERFUL_BRIDGE=1
```

```powershell
$env:LEDGERFUL_BRIDGE = "1"
```

When `enabled = false`:

- `ledgerful ask` skips bridge enrichment.
- `ledgerful verify` does not push `verify_outcome` records.
- `ledgerful watch` does not emit `risk_alert` records.
- `ledgerful bridge query` prints an enable hint.
- `ledgerful bridge export` and `ledgerful bridge import` remain usable because they are pure-local I/O.

## Code Symbol Queries — Use These First

Before searching the web or reading files manually, query Ledgerful's symbol index.

```bash
# Always refresh the index first (incremental, fast)
ledgerful index --incremental

# Optional: SCIP edge augment after native call graph (OFF by default).
# Use for precision work / impact prep — not a universal quality KPI.
# Requires a capable indexer (capability probe). Adds structural_edges with
# evidence=scip:ref onto native symbols only — does not replace the native index.
# Under --json, read scip.status, scip.edges_added, scip.edges_updated.
# ledgerful index --auto-scip --json
# ledgerful index --scip path/to/index.scip --json

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

## Index freshness

**Index freshness (short card):** full policy in `docs/index-freshness-policy.md`.

- Prefer `--auto-index` on **search / ask / hotspots / dead-code** when stale.
- **`verify --auto-index` only fixes `test_mapping` for `--scope fast`** — not general bootstrap.
- **`scan` / `scan --impact` have no `--auto-index`** — refresh first if freshness matters.
- Doctor green ≠ index fresh (Graph Index Health is age + content when age-fresh; `index --check` remains readiness JSON SoT).
- Light continuous: `ledgerful watch`. Heavy: `schedule setup-nightly` / `index --full` / explicit `--auto-scip`.
- Never idle SCIP. `init` installs no watcher/schedule.

## Daily 5 (agent default path)

Scannable day-to-day subset — not a replacement for the full Core Workflow below.
Prefer **`--json`** on doctor / change-context / ledger status when parsing.
`ask` answers Daily 5 / product-docs intents from this skill card (deterministic),
not free-form LLM invention.
Packet + colour env: [`docs/agent-output-contract.md`](../agent-output-contract.md).
Agent command sheet (local pack path): `.agents/skills/ledgerful/references/commands.md`
(or `references/commands.md` when the skill pack is installed as a unit).

| # | Command | Role |
|---|---|---|
| 1 | `ledgerful doctor --json` | Session/env readiness (`readyForPublish`); if `binary-behind-tree`, reinstall (`cargo install --path . --force`) before trusting `--help` / new flags |
| 2 | `ledgerful change-context --json` | Default pre-edit packet |
| 3 | `ledgerful ledger status --compact` or `--json` | Provenance / pending / drift |
| 4 | `ledgerful search …` (prefer `--auto-index` when stale) | Discovery (not full impact) |
| 5 | `ledgerful verify --scope fast` | Local gate (pre-push style); **≠** full CI; may **refuse** when mapping cannot scope |

**Step 5 may refuse** (exit ≠ 0, greppable `refusing full suite`) when
`test_mapping` is empty/stale/unusable — it will **not** surprise-run a multi-minute
full suite. Remediation:

```bash
ledgerful index --incremental
ledgerful verify --scope fast --auto-index
# or deliberate full / old fallback:
ledgerful verify --scope full
ledgerful verify --scope fast --allow-full-fallback
```

Empty tree (no file changes) uses a cheap plan (Rust: fmt+clippy, no nextest;
non-Rust: zero steps, exit 0). Shared infra still runs full with an announcement.

**Escalate (not Daily 5):**

- `scan --impact --json` — B2 only (readSetCapped / high risk multi-module / unclear public API / user DoD / change-context not_ready)
- `index --incremental` / `--full` / search `--auto-index` — freshness
- `verify --scope full` / CI — not the local fast gate

**Honesty:** doctor ≠ verify ≠ full CI. Empty-tree packets stay low risk with
`analysisWarnings` (0129). Index/search freshness: prefer `--auto-index` or
`index --incremental` (0128/0126) — not bare full impact as a refresh step.

## Core Workflow (Default)

**Default preflight ladder** = doctor → audit → ledger status → **change-context --json**.
Full `scan --impact` is **escalate-only** (B2), never a peer default of change-context.

1. Session start / first tool use:

```bash
ledgerful doctor --json
```

2. Provenance / drift (keep audit — distinct from doctor):

```bash
ledgerful audit
ledgerful ledger status --compact
# or: ledgerful ledger status --json
```

Skip status only for pure docs/conductor prose with no ledger work.

3. **DEFAULT pre-edit** for meaningful code/config/policy — budgeted agent change
   packet before bulk file reads (schema: `docs/agent-output-contract.md`):

```bash
ledgerful change-context --json
# CI / fixed base for structure only: --base-ref origin/main
# (doctor + ledger always report present workspace state)
# Cap: --max-files 20 (default)
```

Use `readSet` paths first. The packet includes `riskLevel`, `riskReasons`, doctor
`readyForPublish`, open ledger transactions, blast **counts** including nested
`confidenceSummary` (class counts such as `scipBound`/`resolved` — not full edges),
deepened `testCoverage` (structural test-gap status/counts/capped unmapped —
**not** line coverage; LCOV COVERAGE rows do not currently persist), and
`changeHints` (greenfield / new-surface classification + budgeted
`suggestedTests` when the change set is mostly pure-adds or a new package
prefix; omit on empty/not_ready; convention suggestions are path heuristics,
not proven coverage). Empty or missing mapping is **not** "fully covered"; use
the status enum (`available` \| `empty_mapping` \| `missing_table` \|
`no_source_seeds` \| `unavailable`) — never treat bare empty lists as complete
coverage.

4. **Escalate** to full impact only when a B2 trigger fires:

```bash
ledgerful scan --impact --json
```

| Trigger | Why |
|---|---|
| `readSetCapped == true` | Budget hid files |
| `riskLevel` high (or medium **and** multi-module / shared infra in readSet) | Accountability |
| Diff spans many packages/languages or public API unclear | depth-1 default |
| User/DoD requires full impact | Process |
| change-context `status` error/not-ready (**not** merely `empty`) | Fallback |

**De-escalate:** After an escalated `scan --impact`, return to **change-context**
for subsequent edits unless a B2 trigger re-fires. Do not pin full impact as the
session default after one escalation.

`.ledgerful/reports/latest-impact.json` is an **escalate-tier cache only** — never
a default before step. Prefer live `change-context` (computes impact in-memory;
does not rewrite that file).

5. **Skip / lighten** preflight when:

| Case | Guidance |
|---|---|
| Trivial format/lockfile/binary/scratch/explicit bypass | Skip Ledgerful |
| Pure conductor/docs prose, no product code | doctor optional; no change-context required |
| `status: "empty"` | Expected when no file changes + no pending ledger — **not** failure; do **not** escalate solely for empty |
| status empty + `riskLevel` ≠ low | Do **not** escalate solely because riskLevel ≠ low when status==empty |
| status empty + only federation schema warnings | Schema-unavailable siblings land under `analysisWarnings` with `riskLevel=low` (0129) — ambient federation health, not diff risk; do **not** treat as medium escalation |
| `search-empty` | Documented in commands reference; not a reason for full impact |

For human triage:

```bash
ledgerful impact --summary
```

For entity-scoped deep-dives use `ledgerful tests <entity>`. On CI,
`scan --pr --format json` always includes `testGaps`; without a local index the
status is honest **`unavailable`** (not a merge failure).

After making edits:

```bash
ledgerful change-context --json
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
ledgerful doctor --json          # branch on .readyForPublish
ledgerful change-context --json  # budgeted readSet + risk + doctor + ledger + testCoverage gaps
# if readSetCapped: ledgerful scan --impact --json
# entity deep-dive: ledgerful tests <path-or-symbol>
# PR sticky contract: ledgerful scan --pr origin/main...HEAD --format json  # testGaps always present
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

1. **Temporal Coupling**: Prefer live `change-context --json` (or an escalated
   `scan --impact` packet when B2 fires) for coupling signals. If affinity is
   high (e.g. >70%) between a changed file and an unchanged file, you **must**
   read the unchanged file — imports alone often miss the dependency. Coupling
   scores use recency weighting (recent shared commits count more).
   `.ledgerful/reports/latest-impact.json` is an **escalate-tier cache only** —
   never a default before-step source.
2. **Hotspots**: Files with high hotspot scores are "brittle." If you must edit a hotspot, prioritize refactoring or extremely high test coverage. Avoid adding complexity to an already complex hotspot.
3. **Federated Impact (Cross-Repo)**: If `federated_impact` warnings appear, your change might break a sibling repository. Explain this risk and suggest an `export-schema` to verify the contract.
4. **Predictive Verification**: If `verify` suggests tests that seem unrelated to your change, trust the predictor. It is likely based on historical failure correlations that aren't obvious from the code alone.
5. **Stale Data / `data_stale`**: First refresh with `ledgerful index --incremental`
   and/or re-run `ledgerful change-context --json` (prefer `search --auto-index`
   / ask / hotspots / dead-code when those commands report stale). Escalate to
   `ledgerful scan --impact --json` **only** on B2 — never bare `scan` + `impact`
   as the first move.

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

1. **`commit-msg`**: Captures intent (agent ledger SoT first; else TUI / conventional / silent LLM). Creates or links a `PENDING` transaction and a sidecar file.
2. **`post-commit`**: Automatically promotes the `PENDING` transaction to `COMMITTED` once the Git commit is finalized. If the Git commit fails, the record remains pending or is safely rolled back.

**Provenance source of truth (0122):** agent `ledger start` / `ledger commit` is intentional SoT. The commit-msg hook must not invent a parallel silent LLM intent or open a second TX when the agent already owns intent. Greppable lines use prefix `[Ledgerful] Provenance SoT:` (target `cli_summary`).

| Agent action | Hook behavior |
|---|---|
| `ledger commit` + git msg with `Ledger: {tx}` | AlreadyCommitted (skip) |
| `ledger start` only (one PENDING) | LinkPending |
| N>1 PENDING, no `Ledger:` (incl. multi-worktree shared DB) | Ambiguous → HookFallback |
| No ledger activity | HookFallback (LLM/silent/TUI) |

**Message binding:** include `Ledger: {tx_id}` on its own line (default `--with-git` template), or optional `Ledger-Tx: {tx_id}`. Bare UUIDs in prose are ignored. Linked worktrees share one `.ledgerful` DB — concurrent multi-worktree agents with two open PENDINGs and no `Ledger:` line hit Ambiguous → HookFallback; always include the TX ref when disambiguation matters.

### Cryptographic Security

If `intent.require_signing = true` in `.ledgerful/config.toml`, all ledger entries must be signed by the developer's local Ed25519 key (generated during `init`).

```bash
ledgerful verify --signatures
ledgerful verify --signatures --chain
```

This performs an offline mathematical validation of every record against its signature and public key, plus chain linkage of the presented chain.

**Independent head retention (operator hygiene):**
```bash
mkdir -p ./checkpoints   # parent must exist (or use default ./ledgerful-chain-head.json)
ledgerful export head --out ./checkpoints/head.json
# copy off-machine, then later:
ledgerful verify --signatures --against-export ./checkpoints/head.json
```
Local `--chain` alone cannot detect full rollback when the adversary controls DB + head. See `docs/chain-checkpoint.md`.

**Ledgerful-itself public head (0120):** thin checkpoint at `https://www.ledgerful.dev/ledger/chain_head.json` — download then `verify --signatures --against-export` (no `--against-url`). Customer repos use `export head` + off-machine retention (0119).

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

## Publish Hygiene (dual green)

Do **not** collapse these signals:

| Signal | Means | Does **not** mean |
|---|---|---|
| `doctor` / `readyForPublish` | Zero **block** doctor findings; env fit to enter publish path | Verify/tests/CI green |
| Pre-push hook | `verify --scope fast` + ledger cleanliness (quiet success; structured fail block on stdout — binary-first after PATH upgrade) | Full fmt/clippy/nextest/CI |
| `verify --scope full` / CI | Repo full gate | Doctor readiness |

Doctor green ≠ pre-push green ≠ full CI. Full definition: `docs/doctor-severity.md`.

## When To Skip

Skip Ledgerful only for trivial formatting, simple dependency lockfile updates, binary/media changes, temporary scratch files, or when the user explicitly says to bypass it.

## If Commands Fail

- If `ledgerful` is unavailable, continue with normal repo tools and tell the user Ledgerful signals were unavailable.
- If `ledger status` shows unaudited drift, reconcile or adopt before continuing unless the user directs otherwise.
- If `scan --impact` cannot complete, continue cautiously and include the error in the final report.
- If a command reports that the index is `[STALE]`, append `--auto-index` to **`search`, `ask`,
  `hotspots`, `dead-code`** (prefer passing it proactively when unsure). Time-stale **and**
  content-hash drift-stale both trigger refresh; never-indexed runs a full bootstrap. Never idle
  SCIP. Check machine readiness with `ledgerful index --check --json` or `doctor --json`.
- **`verify --auto-index` is different:** it only refreshes stale/empty `test_mapping` for
  `--scope fast` (changed-files incremental + retry). It does **not** perform general
  time/drift/bootstrap symbol refresh — use search/ask/hotspots/dead-code or `index` for that.
- **`scan` / `scan --impact` have no `--auto-index`** — refresh with `index` / doctor / check first.
- Continuous session: `ledgerful watch`. Overnight heavy: `schedule setup-nightly` (opt-in;
  `index --analyze-graph`, no default `--auto-scip`).
- Doctor green ≠ index fresh (Graph age + content; check remains SoT). Full policy: `docs/index-freshness-policy.md`.
- Prefer **`--json`** when an agent must parse command output (including `doctor --json`).
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