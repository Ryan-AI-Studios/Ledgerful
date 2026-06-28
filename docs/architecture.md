# Ledgerful Architecture

Ledgerful keeps command entry points thin and pushes behavior into focused subsystems. The repository is local-first: generated state lives under `.ledgerful/`, reports are deterministic where practical, and expensive or failure-prone analysis degrades visibly.

## Data Flow

```text
CLI
  -> commands/
    -> git/         repository discovery, status, diff, history
    -> index/       symbols, imports, runtime usage, complexity
    -> impact/      packet assembly, redaction, risk, temporal, hotspots
    -> verify/      predictive plan building and verification reports
    -> federated/   sibling schema discovery and cross-repo impact
    -> gemini/      prompt construction, sanitization, Gemini subprocess
    -> daemon/      optional LSP diagnostics, Hover, CodeLens
    -> state/       layout, reports, migrations, SQLite persistence
    -> watch/       debounced filesystem event batches
    -> util/        path normalization, clock, hashing, process
    ```

    ## Command Responsibilities

    - `init` creates repo-local configuration and ignore wiring.
    - `doctor` checks the host environment.
    - `scan` records git change summaries.
    - `impact` builds the main impact packet, runs temporal/hotspot/federated enrichment, redacts secrets, writes JSON, and persists to SQLite.
    - `verify` loads rules and latest packet data, recomputes missing temporal context when possible, scans current imports, predicts additional verification targets, runs commands, and writes `latest-verify.json`.
    - `ask` loads the latest impact packet, truncates and sanitizes context, then invokes Gemini.
    - `hotspots` computes risk density from git history and stored complexity.
    - `federate` exports public interfaces and scans sibling schemas.
    - `audit` (top-level) performs a holistic project audit or history for an entity.
    - `daemon` is optional and feature-gated behind `--features daemon`.
    - `reset` removes derived state without touching files outside `.ledgerful/`.

    ## Module Boundaries

    - `commands/`: command orchestration only. This layer handles CLI-visible messages, fallback reporting, and composition of lower-level modules.
    - `git/`: repository discovery, status, history, and platform-sensitive git behavior.
    - `index/`: language-aware extraction for symbols, imports/exports, runtime usage, complexity scoring, and orchestrator for Phase 2 data models.
    - `impact/`: packet assembly, secret redaction, temporal coupling, hotspot ranking, and modular risk scoring via enrichment providers.
    - `verify/`: deterministic verification plan generation, predictive verification (semantic and probability-based), subprocess execution, and explanation generation.
    - `federated/`: sibling schema parsing, path confinement, dependency discovery, and cross-repo impact checks.
    - `gemini/`: mode-specific prompts, narrative prompt construction, prompt sanitization, and subprocess invocation.
    - `local_model/`: OpenAI-compatible HTTP client for local LLMs, context assembly, and fallback logic.
    - `embed/`: unified HTTP client and SQLite vector storage for local embedding generation.
    - `semantic/`: AST chunking, logic extraction, and local vector search powered by CozoDB HNSW indices.
    - `search/`: streaming trigram indexing, Tantivy regex execution, and ranked BM25 search.
    - `scip/`: Protobuf ingestion, symbol mapping, and stale index detection for precise compiler-grade graph integration.
    - `docs/`: Markdown crawling, chunking, parsing, and indexing.
    - `contracts/`: OpenAPI/Swagger YAML/JSON parsing, semantic alignment, and public contract risk matching.
    - `coverage/`: domain extraction for CI configurations, Docker/Kubernetes manifests, service maps, data-flow coupling, and third-party SDK dependencies.
    - `daemon/`: LSP server, read-only state access, diagnostics, Hover, CodeLens, and lifecycle/PID handling.
    - `state/`: repo-local layout, JSON report writing, SQLite migrations, CozoDB Datalog management, and persistence APIs.
    - `watch/`: event filtering, normalization, batching, and callback dispatch.
    - `util/`: shared utilities including secure lexical path normalization (`normalize_relative_path`), clock abstraction, and process execution helpers.
    - `platform/`: host, shell, path, and process-policy seams.

    ## State Layout

    ```text
    .ledgerful/
      config.toml
      rules.toml
      daemon.pid
      logs/
      tmp/
      reports/
        latest-scan.json
        latest-impact.json
        latest-verify.json
        fallback-impact.json
      search_index/    (Tantivy directories)
      state/
        current-batch.json
        ledger.db
        ledger.db-wal
        ledger.db-shm
        ledger.cozo    (Knowledge Graph data)
        schema.json
    ```
All generated state is rebuildable. `reset` removes derived state by default and only removes config/rules or the full tree when explicitly requested.

## Impact Packet Pipeline

1. Read git status.
2. Extract symbols, imports/exports, runtime usage, and complexity for supported changed files.
3. Compute temporal coupling from git history. First-parent traversal is the default; `--all-parents` opts into full parent traversal.
4. Apply policy/risk analysis.
5. Redact secrets before persistence.
6. Refresh federated sibling links and dependency edges when possible.
7. Compute hotspots from stored complexity and temporal frequency.
8. Write `latest-impact.json` and persist the packet.

Unsupported files, parser failures, temporal failures, hotspot failures, and federation failures are surfaced as warnings rather than silently changing semantics.

## Verification Pipeline

`verify` combines three inputs:

- configured verification rules from `.ledgerful/rules.toml`
- the latest impact packet and packet history from SQLite
- current repository import data scanned at verification time

Prediction uses current structural imports first, historical packet imports as additional evidence, and temporal couplings when available. Missing or failed prediction inputs are written to `prediction_warnings` in `latest-verify.json`.

## Complexity And Hotspots

Complexity scoring uses the native tree-sitter implementation behind `ComplexityScorer`. The `arborist-metrics` spike decision is documented in [docs/architecture/arborist-metrics-decision.md](architecture/arborist-metrics-decision.md).

Hotspot score is normalized temporal frequency multiplied by normalized complexity. Sorting is deterministic by score descending and path ascending. SQLite row errors are propagated instead of dropped.

## Federation

Federation reads sibling `.ledgerful/state/schema.json` files and never writes to sibling repositories. Discovery:

- stays within direct siblings of the current repository root
- skips symlinks
- validates schema version and required fields
- caps sibling scans
- records local-to-sibling symbol dependency edges

`impact` refreshes known sibling links opportunistically before cross-repo impact checks. `federate scan` remains available for explicit refresh/status workflows.

## Gemini

Gemini integration is subprocess-based. Prompt flow:

1. truncate impact packet context to budget
2. construct the mode-specific prompt
3. sanitize secrets from user/context payload
4. select the Gemini model
5. invoke `gemini --model <model> --prompt ""`
6. write a fallback impact artifact on Gemini failure when possible

Model routing uses `gemini.model` as an explicit override. Without an override, routine `analyze`, `suggest`, and narrative prompts use `gemini-3.1-flash-lite` for low-latency, cost-sensitive assistance. `review-patch` and high-risk packets use `gemini-3.1-pro` for deeper reasoning over code and risk context. `GEMINI_API_KEY` is passed from the process environment, optional local `.env`, or repo-local config.

Narrative mode uses one structured narrative prompt instead of nesting that prompt under the generic question template.

## LSP Daemon

The daemon is optional and compiled with `--features daemon`. It uses `tower-lsp-server` and Tokio, opens SQLite read-only, retries busy reads, and surfaces stale data in diagnostics/CodeLens/Hover. It provides:

- text synchronization
- diagnostics from cached impact data plus real-time complexity checks
- Hover summaries for file risk and temporal coupling
- CodeLens for risk and complexity
- PID lifecycle management and parent-process liveness monitoring

## Engineering Constraints

- No production `unwrap()` or `expect()` in new logic.
- Prefer explicit warnings over silent fallback.
- Keep outputs deterministic for identical repository/config/SQLite state.
- Keep feature-gated daemon dependencies optional.
- Run all-feature tests and clippy before merging.
