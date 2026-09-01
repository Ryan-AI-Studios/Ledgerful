---
name: ledgerful
description: Use this skill for Daily 5 and whenever a repository contains Ledgerful, or the user asks about impact analysis, blast radius, risk, verification planning, hotspots, temporal coupling, change-context, verify, search, ledger provenance, or drift. Default pre-edit: `doctor --json` then `change-context --json`. Escalate to `scan --impact` only for high-risk / readSetCapped / multi-module cases. Prefer `--json` for machine-readable agent output.
---

# Ledgerful

AI agents only. Humans use README, `ledgerful --help`, and `docs/`. Run / skip / fallback / do not.

## When to load

Repo has Ledgerful, or the user asked for impact, blast radius, risk, ledger, verify, search, or change-context.

## Daily 5

Prefer `--json` when parsing. Packet schema: `docs/agent-output-contract.md`. Command sheet: `references/commands.md`.

| # | Command | Role |
|---|---|---|
| 1 | `ledgerful doctor --json` | Env readiness (`readyForPublish`). Skip phantom / sig-pin / v1 ceremony unless the task is signing or `require_signing`. |
| 2 | `ledgerful change-context --json` | Default pre-edit packet. Does **not** rewrite `latest-impact.json`. Plan: `--paths src/foo.rs`. |
| 3 | `ledgerful ledger status --compact` or `--json` | Pending / drift; names `workRoot`. Other repo: `-C` / `--directory`. |
| 4 | `ledgerful search …` (prefer `--auto-index` when stale) | Discovery, not full impact. |
| 5 | `ledgerful verify --scope fast` | Local gate (≠ full CI). |

Escalate `scan --impact --json` only on B2: `readSetCapped`, high risk + multi-module, unclear public API, user/DoD requires full impact, change-context `not_ready` (not merely `empty`).

## SCIP honesty

Optional call-edge augment: `ledgerful index --auto-scip --json` (off by default). Not a SCIP tutorial.

Requires a capable indexer. Adds `structural_edges` with `evidence=scip:ref` onto native symbols only.

On `--json` Success read `scip.status`, `edges_added`, `references_seen`, and skip/recovery tallies `edges_skipped_enclosing_disagreement`, `edges_recovered_nest_prefer`. Rate remaining disagreement: `edges_skipped_enclosing_disagreement` / `references_seen`.

O(1) WARN on stderr when disagreements or invalid ranges are > 0.

## Skip

Format-only, lockfile-only, binary/media, scratch, or explicit bypass. Read-only onboard may skip verify. Do not `scan --impact` over a sibling pending TX. Do not edit `.ledgerful` state files.

## Collision

If `ledger status` shows pending and dirty paths overlap that entity: do not ledger start; do not `scan --impact`; prefer `change-context --json`.

## Fallback

- Binary missing: continue with native checks; report missing signals.
- Status drift: reconcile or adopt before continuing unless the user says otherwise.
- change-context `not_ready` / `error`: `scan --impact --json` (B2).
- `verify --scope fast` MappingRefuse (empty mapping; not a surprise full suite):

```
ledgerful index --incremental
ledgerful verify --scope fast --auto-index
ledgerful verify --scope full
ledgerful verify --scope fast --allow-full-fallback
```

## Search

This engine, not a generic tutorial:

```
ledgerful search execute_change_context --auto-index
```

Unquoted multi-word joins; `--` for hyphen-leading queries.

## Hotspots

Default CLI `hotspots` / `hotspots --json` exclude tests/examples/benches; `--include tests` restores the audit view. Pin JSON `score` (0–1), not `displayScore` (ln). MCP and `/api/hotspots` stay unfiltered.

## Windows

Do not overlap `cargo` / `verify` jobs.
