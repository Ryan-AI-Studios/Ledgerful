# Ledgerful

Ledgerful is a local-first Rust CLI for deterministic change intelligence, transactional provenance, and codebase governance. It turns repository edits into impact packets, risk summaries, hotspot rankings, and targeted verification plans.

The tool is designed to stay local and explain its work. It does not act as an autonomous coding agent. Optional `ask` can use a Gemini HTTP backend (`GEMINI_API_KEY`) or a local model; Gemini is an optional Ask backend, not the product name.

Existing `ledgerful` commands, hooks, and `.ledgerful/` state directories keep working unchanged. New installs also provide `ledgerful` and the short `ldg` alias.

## Daily 5

The same default path `--help` prints for agents:

```powershell
ledgerful doctor --json
ledgerful change-context --json
ledgerful ledger status --compact
ledgerful search init --auto-index
ledgerful verify --scope fast
```

First run on an empty `git init` repo is honest: doctor may create `.ledgerful/`, change-context can be `empty`, search can rebuild an empty index, and verify can pass with nothing to map. That sequence is clap-valid; it is not a setup wizard (`init` / `setup` / `demo` live later in this file).

Captured 2026-09-26 with PATH `ledgerful 0.2.15 (4fc51f263901)` on `git init` only (execute tree `a8ced425`). Paths normalized to `C:\dev\my-project`. JSON is a real excerpt (`…` marks omitted keys and extra findings).

`ledgerful doctor --json`:

```json
{
  "schemaVersion": 1,
  "readyForPublish": true,
  "summary": { "block": 0, "warn": 5, "info": 4, "…": "…" },
  "findings": [
    { "code": "graph-empty", "severity": "warn", "message": "Graph state: Empty (never indexed)" },
    { "code": "search-empty", "severity": "warn", "message": "Search index: present but empty (0 documents); full-text search unusable until populated" },
    { "code": "tool-gemini", "severity": "info", "message": "gemini NOT FOUND (optional CLI; not the Cloud Ask backend)" },
    "…"
  ],
  "environment": {
    "workRoot": "C:\\dev\\my-project",
    "binaryVersion": "0.2.15",
    "buildSha": "4fc51f263901"
  },
  "…": "omitted keys"
}
```

`ledgerful change-context --json`:

```json
{
  "schemaVersion": 1,
  "status": "empty",
  "summary": "No file changes and no pending ledger transactions.",
  "riskLevel": "low",
  "…": "omitted keys"
}
```

`ledgerful ledger status --compact`:

```text
Ledger [C:\dev\my-project]: 0 pending, 0 unaudited drift.
```

`ledgerful search init --auto-index` (stdout status lines):

```text
WARN Search index empty after rebuild (0 documents). No indexable content found — check ignore patterns; empty repo or filters may leave the index empty.
```

`ledgerful verify --scope fast`:

```text
Verification passed
```

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
- **winget:** `winget install Ledgerful.Ledgerful` (accepted 2026-07-30; community package version may lag engine releases)
- Apt and other distro packages are not planned. Manifests and release-time bump automation live under [`packaging/`](packaging/); channel details in [`docs/package-distribution.md`](docs/package-distribution.md).

If `ledgerful --version` is older than the latest published release, you may have multiple installs on `PATH` — see the [PATH / version FAQ](docs/installation.md#path--version-faq-multiple-install-channels).

Manual install from a checkout (compiles from source):

```powershell
cargo install --path .
```

The LSP `daemon` command and `usage` are optional Cargo features (not on default `--help`):

```powershell
cargo install --path . --features daemon
```

See [docs/installation.md](docs/installation.md) for installer options, release assets, package managers, and agent bootstrap instructions.

## First-time repo setup

Daily 5 is the day-to-day door. To create `.ledgerful/` wiring, index, and a proof loop, see [`docs/golden-path.md`](docs/golden-path.md) and `ledgerful demo`. A one-time checkout sequence:

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

`ledgerful setup` is a guided wizard (welcome → init → doctor → first scan). It is not Daily 5.

## Commands

Default-features `--help` names, in clap order. Hidden variants (`bridge`, `search-trigrams`, `internal`) stay off this table. `services`, `deploy`, and `observability` stay callable and are marked `(gated)`.

| Command | Role |
|---|---|
| `init` | Initialize Ledgerful in the current repository |
| `gate` | Gate mode configuration |
| `policy` | Evaluate declared repository policy (CI merge gate) |
| `release` | Diff GitHub Latest vs packaging templates, brew/scoop remotes, and npm engine pin |
| `setup` | Guided onboarding wizard (welcome → init → doctor → first scan → success) |
| `scan` | Scan git changes and identify affected symbols |
| `impact` | Analyze impact of current changes |
| `change-context` | Budgeted agent change packet (impact + doctor + ledger + readSet) |
| `session` | One-shot agent session briefing (git + ledger + doctor + change-context + hotspots) |
| `configure` | Applicable config gaps and named `--apply` after HITL (no TUI) |
| `review` | Range review packet for agents (`kind: review`) |
| `index` | Index the project for search and discovery |
| `search` | Search the codebase using high-performance regex or semantic search |
| `hotspots` | Rank files by change frequency and complexity (Hotspots) |
| `endpoints` | List and filter API endpoints |
| `symbols` | List indexed symbols (scoped path/changed/kind/pub inventory; not search) |
| `surfaces` | Inventory of advanced surfaces (ready / empty / gated); alias `tour` |
| `federate` | Manage cross-repo federation |
| `data-models` | Manage data models and schema migrations |
| `ci` | CI configuration and gate commands |
| `dependencies` | Manage project dependencies and security advisories |
| `security` | Manage security boundaries and policies |
| `tests` | List tests validating a specific entity |
| `ledger` | Manage project ledger and transactional provenance |
| `verify` | Run verification plan (predictive Bayesian testing) |
| `ask` | Ask Gemini or a local model for assistance based on the current context |
| `intent` | Manage Ledgerful intent capture and TUI interaction |
| `reset` | Reset Ledgerful state or configuration |
| `doctor` | Health check for Ledgerful and local model stack |
| `status` | Ledger pending/drift status (`--json` / `--compact`; not a full alias of `ledger status`) |
| `config` | Configuration management |
| `dead-code` | Detect likely dead code across the repository |
| `audit` | Perform a holistic project audit or history for an entity |
| `timings` | Local-only per-command timing analysis (Track 0043; `--global` is Track 0044) |
| `viz` | Generate an interactive visualization of the knowledge graph |
| `update` | Update the Ledgerful binary or migrate repository state |
| `watch` | Watch repository for changes and run incremental graph sync |
| `sync` | Team ledger synchronization [Available — opt-in shared-folder v1] |
| `schedule` | Schedule nightly indexing and graph analysis tasks |
| `viz-server` | Knowledge graph visualization server |
| `export` | Export evidence artifacts (SOC2, etc.) |
| `web` | Launch the Ledgerful local web dashboard |
| `mcp` | Run the MCP server (stdio) or install/uninstall host platform config |
| `openapi` | Print the canonical OpenAPI JSON spec for this build to stdout |
| `demo` | Generate a disposable demonstration repo with signed ledger entries, cryptographic VALID proof, and a DEMO evidence export (see `docs/golden-path.md`) |
| `services` (gated) | Service boundary and topology commands |
| `deploy` (gated) | Deployment manifest and surface commands |
| `observability` (gated) | Manage runtime observability and SLOs |

`daemon` and `usage` are absent from default `--help` (`--features daemon` / `usage-metrics`; install snippet above).

## MCP agent install (Top-N platforms)

Wire Ledgerful into supported agent hosts with one command (merge-only; never
clobbers foreign MCP servers):

```powershell
# Detect hosts, or pick one: claude-code | cursor | codex | copilot
ledgerful mcp install
ledgerful mcp install --platform cursor --dry-run --json
ledgerful mcp status --json
ledgerful mcp uninstall --platform cursor
```

| Platform id   | Default scope | Config key / notes |
|---------------|---------------|--------------------|
| `claude-code` | user          | top-level `mcpServers` in `~/.claude.json` (not `projects[cwd]`) |
| `cursor`      | user          | `mcpServers` in `~/.cursor/mcp.json` |
| `codex`       | user          | `[mcp_servers.ledgerful]` in `~/.codex/config.toml` |
| `copilot`     | project       | top-level **`servers`** + `"type":"stdio"` in `.vscode/mcp.json` |

Launcher: **`--launcher auto`** (default) prefers a PATH `ledgerful` binary with
args `["mcp"]`; falls back to `npx -y @ledgerful/mcp-server` (Windows prefers
`npx.cmd`) with a pin-lag warning when the binary is missing. Prefer PATH over
npx when both exist — published npm may lag the engine.

**Written ≠ connected:** a successful install updates config files only. Hosts may
still require trust/approve prompts (Codex project trust, Claude Code approve,
VS Code MCP trust). Reload the host after install.

Bare `ledgerful mcp` / `ledgerful mcp serve` still start the stdio server.

Manual / npm wrapper (optional):

```powershell
npx @ledgerful/mcp-server
```

The npm wrapper downloads a checksummed GitHub release binary and launches
`ledgerful mcp`. Set `LEDGERFUL_MCP_BIN_OVERRIDE` to a local binary for
development or CI smoke tests.

## Federation

Use federated intelligence across sibling repositories:

```powershell
ledgerful federate export
ledgerful federate scan
ledgerful federate status
ledgerful impact
```

`federate status` lists **live peers** (one row per path; name = folder
basename). Run `federate scan` to refresh discovery and prune dead/self
cache rows.

## Provenance

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

# Search and audit (ROLLBACK rows omitted by default; --include-rollback restores)
ledgerful ledger search "auth logic" --category FEATURE --days 30
ledgerful ledger audit --include-unaudited
ledgerful ledger adr --output-dir docs/adr
```

## Configuration

Ledgerful stores repo-local state in `.ledgerful/`.

- `.ledgerful/config.toml`: runtime configuration, watch debounce, Ask timeout/context, temporal traversal, hotspot defaults, and ledger settings (enforcement, auto-reconcile, verification gating).
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

Impact packets are redacted before SQLite persistence. Ask prompts are sanitized and truncated before they leave the process.

## Ask (optional Gemini HTTP backend)

Cloud Ask talks to the Gemini API over HTTP using `GEMINI_API_KEY` (process environment or a repo-local `.env`; `.env` is gitignored — use `.env.example` as the template). The `gemini` CLI is an optional local tool (`doctor` code `tool-gemini`); it is not the Cloud Ask backend.

- By default, routine `analyze`, `suggest`, and narrative requests use `gemini-3.1-flash-lite`.
- High-risk packets and `review-patch` requests use `gemini-3.1-pro`.
- Set `gemini.model` in `.ledgerful/config.toml` only when you want one explicit model for every ask mode.
- `--mode analyze`: blast-radius and risk reasoning
- `--mode suggest`: targeted verification recommendations
- `--mode review-patch`: patch review with live diff context
- `--narrative`: senior-architect risk narrative generated from one structured prompt

If Ask fails after an impact packet is available, Ledgerful writes a fallback impact artifact or reports why it could not.

## Windows / WSL

- Windows 11 + PowerShell is the primary environment.
- Mixed Windows/WSL filesystem setups can be slower and may produce different tool availability.
- Keep `git` installed in the environment where you run Ledgerful. The `gemini` CLI is optional (see Ask above).

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
