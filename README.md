# Ledgerful

Ledgerful is a local-first Rust CLI for change intelligence and Gemini-assisted development. It turns repository edits into deterministic impact packets, risk summaries, hotspot rankings, targeted verification plans, and bounded Gemini context.

The tool is designed to stay local and explain its work. It does not act as an autonomous coding agent.

Ledgerful: existing `ledgerful` commands, hooks, and `.ledgerful/` state directories keep working unchanged. New installs also provide `ledgerful` and the short `ldg` alias.

## Install

**Windows** (PowerShell):
```powershell
iwr https://raw.githubusercontent.com/Ryan-AI-Studios/Ledgerful/main/install/install.ps1 -UseB | iex
```

**macOS and Linux**:
```bash
curl -fsSL https://raw.githubusercontent.com/Ryan-AI-Studios/Ledgerful/main/install/install.sh | sh
```

**cargo-binstall** (prebuilt GitHub release binary; no crates.io, no full compile when assets match):
```bash
cargo binstall --git https://github.com/Ryan-AI-Studios/Ledgerful
```

**Package managers:**
- **Homebrew:** `brew install Ryan-AI-Studios/tap/ledgerful`
- **Scoop:** `scoop bucket add ledgerful https://github.com/Ryan-AI-Studios/scoop-bucket` then `scoop install ledgerful`
- **winget:** pending Microsoft review (no install command yet)
- Apt and other distro packages are not planned. Manifests and release-time bump automation live under [`packaging/`](packaging/); channel details in [`docs/package-distribution.md`](docs/package-distribution.md).

If `ledgerful --version` is older than the latest published release, you may have multiple installs on `PATH` — see the [PATH / version FAQ](docs/installation.md#path--version-faq-multiple-install-channels).

Manual install from a checkout (compiles from source):

```powershell
cargo install --path .
```

The LSP daemon is behind an optional feature:

```powershell
cargo install --path . --features daemon
```

See [docs/installation.md](docs/installation.md) for installer options, release assets, package managers, and agent bootstrap instructions.

## Quickstart

```powershell
ledgerful init
ledgerful doctor
ledgerful index
ledgerful scan
ledgerful impact
ledgerful verify
ledgerful hotspots
ledgerful ask "What should I verify next?"
```

## Commands

- `init`: create `.ledgerful/`, starter config, starter rules, and `.gitignore` wiring.
- `doctor`: report platform, shell, path, and tool health.
- `index`: parse source code to build structural, entrypoint, call-graph, data-model, observability, and semantic vector indices. Supports SCIP ingestion.
- `scan`: summarize staged and unstaged git changes.
- `watch`: debounce file-system events into persisted batches.
- `impact`: generate `latest-impact.json` with symbols, imports, runtime usage, complexity, temporal coupling, hotspots, CI predictions, and federated impact.
- `verify`: build and run a deterministic verification plan using structural impact, temporal coupling, CI predictions, and Bayesian failure probability ordering. Includes `--explain` for LLM failure rationales.
- `ask`: send sanitized impact context to Gemini or a local LLM. Supports natural-language `--semantic` codebase search.
- `search`: sub-millisecond regex search via Tantivy trigrams and ranked BM25 codebase queries.
- `hotspots`: rank files by temporal change frequency multiplied by complexity.
- `mcp`: run the Model Context Protocol stdio server for AI agent integration.
- `viz`: export an interactive HTML Knowledge Graph visualization of codebase dependencies and risk heatmaps.
- `federate`: export public interfaces, scan sibling repositories, and show known federated links.
- `ledger`: transactional architectural memory (start, commit, rollback, audit, search, adr).
- `daemon`: optional LSP server with diagnostics, Hover, CodeLens, stale-data handling, and lifecycle management.
- `reset`: remove derived local state. Preserves `ledger.db` by default; use `--include-ledger` to remove provenance data.
- `demo`: generate a synthetic invoice-service repo with real signed ledger entries and a SOC2 evidence export. Fully offline, ~15-30s; cleans up by default (`--keep` to inspect).
- `export evidence`: export a SOC2 evidence zip from the CLI (same artifact as the dashboard button). `--profile soc2 --out <path> [--force]`.
- `timings`: local-only command self-timing (which of *your* commands is slow, and why). See [docs/self-timing.md](docs/self-timing.md).

## Common Workflows

Generate an impact report using first-parent git history:

```powershell
ledgerful impact
```

Include all parent traversal for merge-heavy repositories:

```powershell
ledgerful impact --all-parents
```

Run predictive verification:

```powershell
ledgerful verify
```

Disable prediction and use rule-only verification:

```powershell
ledgerful verify --no-predict
```

Inspect risk hotspots:

```powershell
ledgerful hotspots --limit 20 --commits 500 --dir src --lang rs
ledgerful hotspots --json
```

Use Gemini narrative reporting:

```powershell
ledgerful ask --narrative
```

After the first tagged release and npm publish, use Ledgerful through
MCP-compatible coding agents:

```powershell
npx @ledgerful/mcp-server
```

The npm wrapper downloads a checksummed GitHub release binary and launches
`ledgerful mcp`. Set `LEDGERFUL_MCP_BIN_OVERRIDE` to a local binary for
development or CI smoke tests.

Use federated intelligence across sibling repositories:

```powershell
ledgerful federate export
ledgerful federate scan
ledgerful federate status
ledgerful impact
```

Track changes with transactional provenance:

```powershell
# Start a transaction before editing
ledgerful ledger start src/main.rs --category FEATURE --message "Add auth module"

# After editing and verifying
ledgerful ledger commit --tx-id <id> --summary "Added auth" --reason "API needs authentication"

# Quick single-file change
ledgerful ledger atomic src/config.rs --category REFACTOR --summary "Extract config validation" --reason "SRP"

# Lightweight note for docs changes
ledgerful ledger note docs/api.md "Update endpoint docs"

# Check status and reconcile drift
ledgerful ledger status
ledgerful ledger reconcile --all --reason "Intentional local changes"

# Search and audit
ledgerful ledger search "auth logic" --category FEATURE --days 30
ledgerful ledger audit --include-unaudited
ledgerful ledger adr --output-dir docs/adr
```

```powershell
cargo run --features daemon -- daemon
```

## Configuration

Ledgerful stores repo-local state in `.ledgerful/`.

- `.ledgerful/config.toml`: runtime configuration, watch debounce, Gemini timeout/context, temporal traversal, hotspot defaults, and ledger settings (enforcement, auto-reconcile, verification gating).
- `.ledgerful/rules.toml`: policy rules, protected paths, and required verification commands.

Examples live in [docs/examples/config.toml](docs/examples/config.toml), [docs/examples/rules.toml](docs/examples/rules.toml), and [docs/examples/LEDGERFUL.md](docs/examples/LEDGERFUL.md).

## Reports And State

Generated state is rebuildable and stays inside `.ledgerful/`.

- `.ledgerful/reports/latest-scan.json`
- `.ledgerful/reports/latest-impact.json`
- `.ledgerful/reports/latest-verify.json`
- `.ledgerful/reports/fallback-impact.json`
- `.ledgerful/state/ledger.db`
- `.ledgerful/state/schema.json`
- `.ledgerful/state/current-batch.json`

Impact packets are redacted before SQLite persistence. Gemini prompts are sanitized and truncated before subprocess execution.

## Gemini

Ledgerful shells out to the `gemini` CLI. Ensure it is on `PATH` before using `ledgerful ask`.

- `GEMINI_API_KEY` can be supplied from the process environment or a repo-local `.env` file. `.env` is ignored by git; use `.env.example` as the template.
- By default, routine `analyze`, `suggest`, and narrative requests use `gemini-3.1-flash-lite` for lower latency and cost.
- High-risk packets and `review-patch` requests use `gemini-3.1-pro` for deeper reasoning and code review.
- Set `gemini.model` in `.ledgerful/config.toml` only when you want one explicit model for every ask mode.
- `--mode analyze`: blast-radius and risk reasoning
- `--mode suggest`: targeted verification recommendations
- `--mode review-patch`: patch review with live diff context
- `--narrative`: senior-architect risk narrative generated from one structured prompt

If Gemini fails after an impact packet is available, Ledgerful writes a fallback impact artifact or reports why it could not.

## Windows / WSL

- Windows 11 + PowerShell is the primary environment.
- Mixed Windows/WSL filesystem setups can be slower and may produce different tool availability.
- Keep `git` and `gemini` installed in the environment where you run Ledgerful.

## Architecture

See [docs/architecture.md](docs/architecture.md) for module boundaries and current data flow.

## Contributing

- Questions and setup help: [GitHub Discussions](https://github.com/Ryan-AI-Studios/Ledgerful/discussions).
- Bug reports: [GitHub Issues](https://github.com/Ryan-AI-Studios/Ledgerful/issues).
- Security reports: see [SECURITY.md](SECURITY.md) — do not file public issues for security vulnerabilities.
- Keep changes phase-bounded and deterministic.
- Run `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features -j 1 -- --test-threads=1` before pushing.

## License

Ledgerful is source-available under the
[PolyForm Noncommercial License 1.0.0](LICENSE), with additional permissions
for qualified small entities in [COMMERCIAL-EXCEPTION.md](COMMERCIAL-EXCEPTION.md).

Required Notice: Copyright 2026 Ledgerful, LLC; additional permissions are stated in COMMERCIAL-EXCEPTION.md.
