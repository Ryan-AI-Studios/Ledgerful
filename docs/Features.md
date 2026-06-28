# Ledgerful Features

Ledgerful is a local-first engineering intelligence engine. It combines structural code analysis, historical provenance, and probabilistic modeling to provide deep insight into repository changes.

## 1. Transactional Change Intelligence (The Ledger)

Ledgerful treats architectural changes as atomic transactions, maintaining a permanent record of design decisions and intent.

*   **Transaction Lifecycle**: Start, commit, rollback, or execute atomic changes with metadata (`category`, `summary`, `reason`). Rollbacks are auditable and require an explicit intent note.
*   **Garbage Collection**: Identify and prune orphaned PENDING transactions via `ledger gc --orphans`.
*   **Drift Detection**: Automatic detection of "unaudited drift" (changes made outside of a transaction).
*   **Reconciliation & Adoption**: Transition drift into formal ledger entries or adopt it as part of an active transaction.
*   **Token-Level Provenance**: Attribution of specific symbol modifications (functions, classes) to ledger transactions.
*   **ADR Generation**: Export architectural decisions directly from the ledger into MADR-format Markdown documents.
*   **Ledger Search**: Full-text search (FTS5) across all historical transactions and design notes.
*   **Ledger Federation**: Securely export and sync ledger entries across sibling repositories for cross-repo provenance.

## 2. Impact Analysis & Risk Assessment

Understand the "blast radius" of any change before it is committed.

*   **Modular Enrichment**: 20+ specialized providers analyze changes across different dimensions:
    *   **Structural**: Symbol, import, and call-graph impact.
    *   **Temporal**: Coupling patterns derived from Git history (who changes with whom).
    *   **Complexity**: Cognitive and cyclomatic complexity hotspots.
    *   **Contracts**: OpenAPI/Swagger contract risk matching.
    *   **Infrastructure**: Docker, Kubernetes, Terraform, and Helm manifest awareness.
    *   **Observability**: Trace config drift and SDK dependency detection.
*   **Knowledge Graph (KG)**: CozoDB-backed graph of structural and semantic links with Datalog reachability queries.
*   **Dependency Visualization**: `viz` command exports interactive HTML dependency maps with risk heatmaps.

## 3. High-Performance Code Search & Navigation

Compiler-grade search and conceptual discovery.

*   **Trigram Regex Search**: Sub-millisecond regex discovery using Tantivy and custom Trigram pre-filters.
*   **LSP Integration (SCIP)**: Ingest SCIP indices for exact, compiler-precise navigation and symbol mapping.
*   **Semantic Discovery**: AST-based chunking and local vector embeddings for conceptual/natural-language code search.

## 4. Predictable Verification

Move beyond blind test runs with intelligent, data-driven verification.

*   **Predictive CI Gate Analysis**: Predict Continuous Integration failures locally before pushing, leveraging semantic similarity to historical failures.
*   **Probabilistic Reordering**: A Bayesian engine reorders local tests descending by their failure probability, minimizing the time to first failure.
*   **Scoped Test Selection**: `verify --scope fast` uses the `test_mapping` index to run only the tests covering changed files via nextest filtersets, falling back to the full suite when shared infrastructure is touched. The pre-push hook uses `--scope fast` (~33s vs ~6m20s for full).
*   **Failure Explanation Engine**: Generates concise, technical rationales for predicted failures using a local LLM backend.
*   **Dynamic Verification Plans**: Deterministic plans generated from a blend of explicit configuration (`mode = "explicit"`), stack-aware automatic policy (`mode = "auto"`), structural impact, and historical outcomes.
*   **Stack-Aware Auto-Policy**: When in `auto` mode, Ledgerful scans the workspace for supported stacks (Rust, Node, Deno) and seamlessly builds a robust verification plan. It infers test runners (npm, pnpm, yarn, bun, deno, cargo) and scripts without manual configuration.

## 5. Engineering Coverage & Self-Awareness

Deep visibility into the engineering context of the repository.

*   **Service-Map Derivation**: Infers service boundaries and cross-service dependencies from route/data-model topology.
*   **Data-Flow Coupling**: Flags call chains where route handlers and their data models co-change.
*   **CI Pipeline Awareness**: Detects and surfaces risk when CI configuration itself changes or co-changes with source code.
*   **ADR Staleness**: Flags retrieved architectural decisions that exceed age thresholds or lack recent updates.

## 6. AI & LLM Integration

Ledgerful is "Gemini-ready," providing high-signal, sanitized context to Large Language Models.

*   **Local-First Backend**: OpenAI-compatible completions client for running models locally (e.g., via llama-server).
*   **Semantic Context Assembly**: Budget-aware assembly of structural, semantic, and historical context for prompts.
*   **Modes of Assistance**:
    *   `analyze`: Detailed blast-radius and risk reasoning.
    *   `suggest`: Targeted verification and fix recommendations.
    *   `review-patch`: Deep reasoning code review with live diff context.
    *   `narrative`: Senior-architect risk narrative from structured analysis.
*   **Secret Redaction**: Automated sanitization of diffs and code snippets before they are sent to an LLM.

## 7. Platform & Tooling

Built for the modern developer's environment.

*   **Local-First & Offline**: All core features (including embeddings and search) work without external services.
*   **LSP Daemon**: Optional background server providing diagnostics, Hover, and CodeLens directly in your IDE.
*   **Windows & WSL Resilience**: First-class support for Windows PowerShell and WSL environments.
*   **Health Diagnostics**: `doctor` command verifies toolchain health and environment readiness, including the active ask backend (Gemini Cloud vs Local).
*   **Dead Code Pruning**: `dead-code --prune` interactively removes high-confidence dead code with `inquire` prompts, wrapped in a pending ledger transaction for verifiable safety.
*   **Nightly Scheduler**: `schedule setup-nightly` installs a cross-platform nightly task (Windows schtasks / Unix crontab) that runs `git fetch` + `index --analyze-graph` to keep the search/observability cache warm without workday file-lock contention.
