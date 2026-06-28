# Ledgerful MCP Server

`@ledgerful/mcp-server` wraps the Ledgerful CLI's existing `ledgerful mcp`
stdio server so AI coding agents can install it through npm without building
Rust locally.

```sh
npx @ledgerful/mcp-server
npm install -g @ledgerful/mcp-server
ledgerful-mcp
```

The package downloads the matching `ledgerful-{target}` release asset,
verifies the adjacent `.sha256` checksum file, caches the binary, and launches:

```sh
ledgerful mcp
```

Existing `ledgerful` binary naming is intentional: Ledgerful keeps the
backward-compatible CLI alias while the npm package carries the Ledgerful brand.

## Agent Configuration

Use the installed bin as the MCP command:

```json
{
  "mcpServers": {
    "ledgerful": {
      "command": "ledgerful-mcp"
    }
  }
}
```

## Tools

The tool table is cross-checked against `src/commands/mcp/manifest.rs`.

| Tool | Description |
|---|---|
| `scan` | Assess the impact and risk of uncommitted changes in the repository. |
| `search` | High-precision regex and text discovery for code symbols. |
| `ask` | Conceptual and semantic natural language queries about the codebase. |
| `ledger_status` | Get the current provenance status, including pending transactions and unaudited drift. |
| `ledger_search` | Search the architectural history and transaction ledger. |
| `hotspots` | Identify brittle files with high change frequency or complexity. |
| `endpoints_changed` | List API endpoints affected by current changes. |
| `security_boundaries` | Inspect security policy boundaries and their risk status. |
| `dead_code` | Identify likely unused functions and types based on graph reachability. |
| `verify_plan` | Predict the verification plan for current changes. |

## Environment

| Variable | Purpose |
|---|---|
| `LEDGERFUL_MCP_BIN_OVERRIDE` | Use a local Ledgerful/Ledgerful binary instead of downloading a release asset. Useful for CI and development. |
| `LEDGERFUL_MCP_CACHE_DIR` | Override the binary cache directory. |
| `LEDGERFUL_MCP_RELEASE_TAG` | Download from a specific GitHub release tag instead of `latest`. |
| `LEDGERFUL_MCP_RELEASE_BASE_URL` | Override the complete GitHub release asset base URL. |
| `LEDGERFUL_MCP_SKIP_DOWNLOAD=1` | Skip the best-effort postinstall download. First run will still need a binary or override. |

## Troubleshooting

- Unsupported platform: the loader supports `linux:x64`, `win32:x64`,
  `darwin:x64`, and `darwin:arm64`.
- Download failure: check that the GitHub release has the expected archive and
  adjacent `.sha256` file, or set `LEDGERFUL_MCP_BIN_OVERRIDE`.
- Checksum mismatch: the loader refuses to execute the downloaded archive. Clear
  the cache and verify the release asset was not replaced.
