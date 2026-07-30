# Team Sync (Experimental)

**Status:** Experimental foundation (track **0110**). Not multi-device ready.

Team Sync moves **encrypted ledger entry bundles** between developer devices so
gitignored `.ledgerful/` ledgers can consolidate without putting secrets or
device keys in Git.

Git **never** consolidates `.ledgerful/` — only opt-in team sync can.

## Opt-in forever

| Control | Default | Notes |
|---|---|---|
| Cargo feature `sync` | **on** in default builds (0110) | MCP-npm job still builds without it |
| `[sync].enabled` | **`false`** | `sync init` never sets this true |
| Pair accept / peer store | **not implemented** | track **0111** |
| Two-device apply polish | deferred | track **0112** |
| Wizard / Available marketing | deferred | track **0113** |

```toml
[sync]
enabled = false
target = ""                 # e.g. "dir:///path/to/shared-folder"
# device_id = "device-…"    # optional config mirror of SoT
batch_size = 500
archive_retention_days = 90
max_clock_drift_seconds = 300
schedule = "0 3 * * *"      # display-only; not auto-installed
```

## Readiness ladder

| Track | Owns |
|---|---|
| **0110** (this) | Feature posture, fail-closed pair, layout-aware init, SoT `device_id`, disabled-run honesty, Experimental docs/skill, light doctor |
| **0111** | Real pair accept, peer store/revoke, pair-code crypto review (blake3 construction) |
| **0112** | Encrypted transport E2E, apply bounds, quarantine UX, two-device golden path |
| **0113** | Wizard/status UX, Available label decision |

## Identity isolation

| Artifact | Location | Rule |
|---|---|---|
| `device.key` / `device.pub` | `{state_dir}/sync/` (layout `.ledgerful/sync`) | **Never** in bundle payload; never imported from peers |
| **Local `device_id` SoT** | SQLite `sync_state.device_id` (row `id=1`) | Written by `sync init` / `--force`. pair, status, verify, **run** all read this SoT |
| `config.sync.device_id` | Optional **mirror** only | Written via `toml_edit` helpers; must agree with SoT after init |
| Remote `manifest.device_id` | Bundle metadata | Identifies **sender** for peer key lookup; **must never** overwrite local SoT |

Apply updates `last_apply_hlc` only (`ON CONFLICT DO UPDATE SET last_apply_hlc = …`) and does **not** set `device_id` from the remote manifest.

## Algorithms (honest names)

- **KDF:** Argon2id (pin: argon2 **0.5.x** — no 0.6 RC in 0110)
- **AEAD:** XChaCha20-Poly1305 (chacha20poly1305 **0.11**)
- **Device identity:** Ed25519 (ed25519-dalek **2.x** — **no** 3.0 bump in 0110)
- **Pair provisional code:** `blake3::hash(team_secret || device.pub)` — non-standard MAC-like use; **0111** reviews construction (not rewritten here)
- **Bundle file extension:** `.zip.gpg` is a **misnomer** — ciphertext is XChaCha20-Poly1305 over a zip, not OpenPGP. Rename deferred (likely 0112).

Secrets: keep the team secret out of git, logs, and success banners. Prefer a password manager + `LEDGERFUL_SYNC_SECRET` for automation.

## Shared-folder v1 transport

v1 target scheme: `dir://<absolute-path>` — a shared folder (SMB, Dropbox-style, USB, etc.) with per-device outboxes under `devices/<device_id>/`. No cloud SaaS, no WebRTC, no “sync over GitHub” in this ladder.

## Word collisions (do not confuse)

| Phrase | Means |
|---|---|
| **Team Sync** / `ledgerful sync` | This feature — encrypted ledger entry bundles between devices |
| **Real-time Sync** / `watch` IncrementalSync | Knowledge-graph / index refresh while watching the worktree |
| **Ledger Federation** / `federate` | Sibling-repository ledger export/import for cross-repo provenance |
| OpenAPI `/api/sync/status` | Dashboard DTO (`device_id` + HLC timestamps). **CLI-first** for enabled/init (0110 left DTO unchanged) |
| **bridge** | AI-Brains NDJSON interchange — not team sync |
| **schedule** | Nightly index jobs — not team sync |

## CLI (Experimental)

```bash
ledgerful sync --help          # marked Experimental
ledgerful sync init            # keys + SoT device_id; enabled stays false
ledgerful sync status          # Experimental banner; peers not available
ledgerful sync pair            # provisional code (needs LEDGERFUL_SYNC_SECRET)
ledgerful sync pair BOGUS      # fail-closed NYI (0111)
ledgerful sync run --once      # clear message when disabled; no secret prompt
```

Doctor: if `enabled=true` without init or empty target → **warn** / optional. Mere “sync disabled” never sole-sets `readyForPublish=false`.

## Threat sketch (foundation)

- **No silent merge:** default `enabled=false`; disabled `run` does not write outbox.
- **No fake pairing:** accept fails closed until peer store exists.
- **Local identity isolation:** keys FS-only; SoT device_id never imported from bundles.
- **Secret hygiene:** not written by init; not printed on success.
- **Shared-folder trust:** anyone with folder write can drop ciphertext; apply must verify signatures against known peers (0111+) and quarantine failures (engine path exists; polish 0112).
- **Not a complete threat model:** re-evaluate before Available (0113).

## Performance

Team sync must **not** invoke graph indexing, SCIP, or full reindex on the sync path. Bundle extract/apply are ledger-row operations only.
