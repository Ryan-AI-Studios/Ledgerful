# Ledgerful Risk Report Action

A GitHub Action that posts a change-intelligence risk analysis comment on every pull request using [Ledgerful](https://github.com/Ryan-AI-Studios/Ledgerful).

## What it does

On every pull request event (opened, synchronize, reopened), the action:

1. Installs the `ledgerful` CLI alias from the `ledgerful` package (skips if already in `PATH`)
2. Runs `ledgerful scan --impact --json --base-ref <base-sha>` to analyse the PR's changed files
3. Parses the JSON output into a structured risk report
4. Posts or updates a single comment on the PR with:
   - Overall risk level (HIGH / MEDIUM / LOW / TRIVIAL)
   - Per-file risk breakdown
   - Temporal couplings (files that historically change together)
   - Predicted test failures
   - Pending ledger transactions
5. Optionally fails the workflow if risk meets or exceeds a configured threshold

## Inputs

| Input | Required | Default | Description |
|---|---|---|---|
| `github-token` | Yes | — | GitHub token for posting/updating comments. `GITHUB_TOKEN` is sufficient. |
| `project-path` | No | `.` | Workspace-relative path to the repository root |
| `base-ref` | No | PR base commit SHA | Git ref to compare against. Defaults to the PR's base SHA. |
| `risk-threshold` | No | `TRIVIAL` | Minimum risk level that triggers a comment: `TRIVIAL`, `LOW`, `MEDIUM`, `HIGH` |
| `fail-on-risk` | No | `` (disabled) | Fail the action if risk meets or exceeds this level: `LOW`, `MEDIUM`, `HIGH` |
| `post-on-clean` | No | `false` | Post a comment even when the scan detects no changed files |

## Outputs

| Output | Description |
|---|---|
| `overall-risk` | Overall risk level: `HIGH`, `MEDIUM`, `LOW`, or `TRIVIAL` |
| `changed-files-count` | Number of changed files detected |
| `comment-url` | URL of the created or updated PR comment |

## Minimal usage

```yaml
name: Ledgerful Risk Analysis
on:
  pull_request:
    types: [opened, synchronize, reopened]

jobs:
  risk:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pull-requests: write
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Ledgerful Risk Report
        uses: Ryan-AI-Studios/Ledgerful/action@main
        with:
          github-token: ${{ secrets.GITHUB_TOKEN }}
```

## Full example with cache and fail-on-risk

```yaml
name: Ledgerful Risk Analysis
on:
  pull_request:
    types: [opened, synchronize, reopened]

jobs:
  risk:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pull-requests: write
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Cache Cargo registry
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
          key: ${{ runner.os }}-cargo-ledgerful-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: |
            ${{ runner.os }}-cargo-ledgerful-

      - name: Ledgerful Risk Report
        id: risk
        uses: Ryan-AI-Studios/Ledgerful/action@main
        with:
          github-token: ${{ secrets.GITHUB_TOKEN }}
          base-ref: ${{ github.event.pull_request.base.sha }}
          risk-threshold: LOW
          fail-on-risk: HIGH
          post-on-clean: false

      - name: Show risk level
        run: echo "Overall risk ${{ steps.risk.outputs.overall-risk }}"
```

## Permissions

The action requires `pull-requests: write` to post comments. If you use a restricted GITHUB_TOKEN, ensure this permission is granted in your workflow:

```yaml
permissions:
  contents: read
  pull-requests: write
```

## Why `fetch-depth: 0`

Ledgerful's temporal coupling analysis needs full git history to identify which files historically change together. Without `fetch-depth: 0`, the shallow clone provided by default only includes recent commits and will miss co-change patterns.

## Cargo cache tip

The first run installs the `ledgerful` package from source via `cargo install`, which provides the backward-compatible `ledgerful` binary and can take several minutes. Cache `~/.cargo/registry` and `~/.cargo/git` between runs to speed up subsequent invocations significantly:

```yaml
- uses: actions/cache@v4
  with:
    path: |
      ~/.cargo/registry
      ~/.cargo/git
    key: ${{ runner.os }}-cargo-ledgerful-${{ hashFiles('**/Cargo.lock') }}
    restore-keys: |
      ${{ runner.os }}-cargo-ledgerful-
```
