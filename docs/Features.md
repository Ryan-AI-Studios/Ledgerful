# Ledgerful Features

Ledgerful is a local-first engineering intelligence engine. It combines structural code analysis, historical provenance, and probabilistic modeling to provide deep insight into repository changes.

## 1. Transactional Change Intelligence (The Ledger)

Ledgerful treats architectural changes as atomic transactions, maintaining a permanent record of design decisions and intent.

*   **Transaction Lifecycle**: Start, commit, rollback, or execute atomic changes with metadata (`category`, `summary`, `reason`). Rollbacks are auditable and require an explicit intent note.
*   **Enforce lifecycle integrity (0074)**: Under `gate.mode = enforce`, post-commit promote failures retain PENDING + `pending_hook_tx` (`promote_failed`) instead of destroying the trail; next commit-msg / `ledger status --exit-code` / `doctor` surface `PROMOTE_ORPHAN` / `HEAD_UNCOVERED` until recovery. Adaptive trivial bypass and TUI Skip write durable `[SKIPPED]` coverage rows (Unverified, never Verified). Recovery: `ledger recover-orphan --promote` or `--abandon --reason "..."`. Promote always sets Unverified (or None for TRIVIAL) — never phantom Verified. **Client-hook ceiling:** local hooks are outrun by `git commit --no-verify` and `core.hooksPath`; green hooks ≠ unbypassable shared control (CI/remote still required). See `docs/lifecycle-integrity.md`.
*   **Hook / ledger provenance SoT (0122)**: Agent `ledger commit` (or a single open PENDING) is intentional SoT; commit-msg skips LLM draft / second TX when `Ledger: {tx}` is verified or one global PENDING is linked. Binary-only PATH upgrade. Multi-pending / shared-worktree without a message ref falls back to hook intent capture.
*   **Garbage Collection**: Identify and prune orphaned PENDING transactions via `ledger gc --orphans` / `--stale`. Promote-failed and HEAD-matching hook sidecars are GC-ineligible; use `recover-orphan` instead of silent delete.
*   **Drift Detection**: Automatic detection of "unaudited drift" (changes made outside of a transaction).
*   **Reconciliation & Adoption**: Transition drift into formal ledger entries or adopt it as part of an active transaction.
*   **Token-Level Provenance**: Attribution of specific symbol modifications (functions, classes) to ledger transactions.
*   **ADR Generation**: Export architectural decisions directly from the ledger into MADR-format Markdown documents.
*   **Ledger Search**: Full-text search (FTS5) across all historical transactions and design notes. Human search tables are width-aware (short committed timestamp, column bounds).
*   **Windows-safe human tables (0181)**: Premium human tables (`hotspots`, `timings`, `ledger search`, impact summaries, …) auto-select **ASCII** borders and non-PUA icons when Windows console OutputCP ≠ 65001 (or stdout is non-TTY); UTF-8 rounded borders otherwise. Overrides: `LEDGERFUL_TABLE_STYLE=ascii|utf8|auto`, `LEDGERFUL_TABLE_ASCII=1`, `LEDGERFUL_TABLE_UTF8=1` (`STYLE` wins over simple flags; if both simple flags set, ASCII wins). `NO_COLOR` is color-only and does not force table ASCII. JSON/machine paths unchanged.
*   **Ledger Federation**: Securely export and sync ledger entries across sibling repositories for cross-repo provenance. **Not** the same as Team Sync (`ledgerful sync`).
*   **Team Sync [Available — opt-in shared-folder v1]**: Opt-in encrypted ledger entry bundles between developer devices (`ledgerful sync`). Default feature surface; runtime `[sync].enabled = false` forever until you opt in. Pairing (0111), secure transport/apply (0112), and setup checklist / status next-action / gated enable (0113) are real. Not default-on, not cloud SaaS, not CRDT. See `docs/team-sync.md`.
*   **Cryptographic provenance (v2)**: New commits sign a domain-separated 14-line payload (`sig_version:2`) that binds entity, author, risk, origin, change_type, entry_type, is_breaking, and related_tickets in addition to the legacy five fields. Historical `sig_version=1` rows dual-verify. `intent.trusted_public_keys` pins keys; empty pin list yields `VALID (unknown key)`. `require_signing` / `--strict-signatures` fail closed on unsigned LOCAL rows.
*   **Signing hygiene (0125)**: `doctor` warns on empty pin / `min_sig_version=1` with structured `remediation` (exact next commands). Follow those lines: pin = identity allowlist (not free-text truth). `ledger re-sign --all` upgrades LOCAL legacy `sig_version` rows (and repairs invalids); `ledger re-sign --all-invalid` is key-repair only. Preview with `--dry-run`, mutate with `--yes` (WAL backup + maintenance audit).
*   **Signature / chain verify exit codes** (`verify --signatures` / `--chain`): **0** all valid · **1** INVALID signature, version reject, entity_normalized mismatch, or chain break · **2** reserved (trusted-key policy) · **3** UNSIGNED under require/strict. Federated rows count as `SKIP (federated)` and are not failures.
*   **Chain Integrity**: Each committed ledger entry carries a `prev_hash` linking it to the prior chain head, and a signed `chain_head` row binds the latest entry hash, genesis boundary, and chain length. `ledgerful verify --signatures --chain` validates the chain end-to-end and reports the exact break location. The chain verifies the integrity and continuity of the presented chain; detection of rollback to an earlier valid state requires an independently retained chain head. **Operator path:** `ledgerful export head` writes a thin `chain_head.json` checkpoint; `ledgerful verify --signatures --against-export <path>` compares against a SOC2 zip **or** bare JSON with **checkpoint (extends-or-equals)** semantics by default (`--exact` restores full snapshot equality). See `docs/chain-checkpoint.md`. This is not local immutability.
*   **Demo Command**: `ledgerful demo [--keep] [--output <dir>]` creates a synthetic invoice-service repo and drives it through the real hook flow (init, 5 commit cycles, verify, export) producing real Ed25519-signed entries from an ephemeral keypair, ending with a self-identifying DEMO SOC2 evidence export. Fully offline, ~15-30s warm path, cleans up by default (`--keep` to inspect). **Honesty:** demo runs in observe mode; promoted DEMO entries are **Unverified** until a bound `verify --tx-id` run — `CRYPTO VALID` proves signatures/chain integrity, not ledger verification_status Verified.
*   **CLI Evidence Export**: `ledgerful export evidence --profile soc2 [--out <path>] [--force]` produces the identical SOC2 evidence zip as the dashboard button, callable without the `web` feature. Path-safety applies (refuse `src/`, `.ledgerful/state/`, `Cargo.toml`; refuse overwrite without `--force`; symlink re-check).
*   **Chain head checkpoint export**: `ledgerful export head [--out <path>] [--force]` writes the live chain head as bare JSON for periodic off-machine retention (default `./ledgerful-chain-head.json`). Refuses unsigned heads when `intent.require_signing` is true; when `require_signing` is false may write an unsigned head with a warning. Same path-safety as evidence export. See `docs/chain-checkpoint.md`.
*   **Control-Scoped Evidence Export**: `ledgerful export evidence --profile soc2 --control CC8.1 --control "CC7.*"` appends an additive control lens (`control-lens/cover.md` + `index.json`) to the signed bundle without changing existing evidence payloads. Exact IDs and family wildcards are supported; unknown selectors are rejected. The manifest and signature are regenerated so they cover the additive lens files. See `docs/mappings/soc2.md`.

## 2. Impact Analysis & Risk Assessment

Understand the "blast radius" of any change before it is committed.

*   **Modular Enrichment**: 20+ specialized providers analyze changes across different dimensions:
    *   **Structural**: Symbol, import, and call-graph impact.
    *   **Temporal**: Coupling patterns derived from Git history (who changes with whom).
    *   **Complexity**: Cognitive and cyclomatic complexity hotspots.
    *   **Contracts**: OpenAPI/Swagger contract risk matching.
    *   **Infrastructure**: Docker, Kubernetes, Terraform, and Helm manifest awareness.
    *   **Observability**: Trace config drift and SDK dependency detection.
    *   **Affected HTTP flows (0118)**: Change-set `affectedFlows` over indexed route
        registrations (handler symbol / impl file / registration file / optional blast
        edges). Surfaces: impact, change-context, `scan --pr`, and
        `endpoints --changed` (shared match library; report samples cap at 20,
        filter keys uncapped). Frameworks: Axum/Actix/Rocket, Gin/`net/http`,
        Express/Fastify, FastAPI/Flask — **route map, not** CRG call-chain traces.
*   **Knowledge Graph (KG)**: CozoDB-backed graph of structural and semantic links with Datalog reachability queries.
*   **Dependency Visualization**: `viz` command exports interactive HTML dependency maps with risk heatmaps.

## 3. High-Performance Code Search & Navigation

Compiler-grade search and conceptual discovery.

*   **Trigram Regex Search**: Sub-millisecond regex discovery using Tantivy and custom Trigram pre-filters.
*   **Index freshness policy (three tiers)**: light continuous (`watch` + mega-batch safety), light
    on-demand (`--auto-index` on `search` / `ask` / `hotspots` / `dead-code` with time-stale
    **and** content-hash drift-stale, full bootstrap when never indexed), heavy scheduled/explicit
    (`schedule setup-nightly` → `index --analyze-graph`, or user `index --full` / `--auto-scip`).
    `verify --auto-index` is a **separate** `--scope fast` `test_mapping` refresh (not the shared
    drift/bootstrap path). `scan --impact` has **no** `--auto-index`. `ledgerful daemon` is an
    LSP **reader**, not an indexer. No idle SCIP / silent always-on reindex. See
    **`docs/index-freshness-policy.md`**.
*   **SCIP augment (optional)**: `index --auto-scip` / `index --scip <path>` run **after** the native
    call graph and add precise cross-file **reference edges** onto native `project_symbols` ids
    (`structural_edges` with `evidence=scip:ref`). SCIP does **not** write symbols, does not replace
    the native index, and is off by default. Requires a capable per-language indexer (capability
    probe, not PATH alone). Empty probe → `scip.status = unavailable` (not failed). See
    `docs/Call-Resolution.md`.
*   **Native language matrix**: tree-sitter structural indexing for **Rust**, **TypeScript/JavaScript**,
    **Python**, **Go**, and **C/C++** (single `Language::Cpp` via `tree-sitter-cpp` for `.c`/`.h`/
    `.cpp`/`.cc`/`.cxx`/`.hpp`/`.hh`/`.hxx`/`.h++`). C/C++ floor: symbols (declarator-aware names),
    `#include` with stripped delimiters, same-file call edges (callee unwrap for templates /
    members; unique same-file name → resolved, overloads stay unresolved), complexity/hotspots.
    **Not** stack-graph / clangd fidelity; scip-clang is manual `--scip` only (not auto-generate).
    **Auto-on (no separate CLI flag):** registering those D2 extensions on `Language::from_extension`
    also admits C/C++ into daemon complexity diagnostics (`daemon/handlers.rs`) and the federated
    scanner language gate (`federated/scanner.rs`) — the same extension map, not an opt-in toggle.
*   **Semantic Discovery**: AST-based chunking and local vector embeddings for conceptual/natural-language code search.

## 4. Predictable Verification

Move beyond blind test runs with intelligent, data-driven verification.

*   **Predictive CI Gate Analysis**: Predict Continuous Integration failures locally before pushing, leveraging semantic similarity to historical failures.
*   **Probabilistic Reordering**: A Bayesian engine reorders local verification steps descending by failure probability when sufficient history exists, minimizing the time to first failure (fmt stays before clippy; scoped nextest shares history via a stable step key).
*   **Scoped Test Selection**: `verify --scope fast` uses the `test_mapping` index to run only the tests covering changed files via nextest filtersets. When mapping cannot scope, it **refuses** (exit ≠ 0) rather than silently running the full suite; shared infrastructure still falls back to full; operators can opt in with `--allow-full-fallback`. The pre-push hook uses `--scope fast` (~33s vs ~6m20s for full).
*   **Transaction Provenance Binding**: Bind a verification run to a ledger transaction via `verify --tx-id <id>`, or let auto-binding attach it when running inside a `commit-msg` hook (detected via `COMMIT_EDITMSG` + `.git/index.lock`). Once bound, `/api/ledger/:txId` surfaces real `tests_run` and `flakes` counts for that transaction — not zeros.
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
*   **LSP Daemon**: Optional background server providing diagnostics, Hover, and CodeLens directly in your IDE. Ships with `--all-features` release builds; it **reads** the existing index and does **not** run as a background indexer (see `docs/index-freshness-policy.md`).
*   **Windows & WSL Resilience**: First-class support for Windows PowerShell and WSL environments.
*   **Health Diagnostics**: `doctor` command verifies toolchain health and environment readiness, including the active ask backend (Gemini Cloud vs Local). Doctor green ≠ index fresh.
* **Dead Code Pruning**: `dead-code --prune` interactively removes high-confidence dead-code *candidates* (heuristic evidence, not proof) with `inquire` prompts, wrapped in a pending ledger transaction for verifiable safety.
* **Nightly Scheduler**: `schedule setup-nightly` installs a cross-platform nightly task (Windows schtasks / Unix crontab) that runs `git fetch` + `index --analyze-graph` (no `--auto-scip` by default) to keep the search/observability cache warm without workday file-lock contention. Opt-in only — not installed by `init`.
* **PR Scan Surface**: `scan --pr <range> --format json` emits a stable, versioned, deterministic PR diff report for CI integration. See `docs/pr-scan-schema.md` for the schema contract.
