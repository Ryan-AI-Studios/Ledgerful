# Team Sync (Experimental)

**Status:** Experimental (tracks **0110** foundation + **0111** pairing/peer trust +
**0112** secure transport/apply). Not Available marketing; wizard / Available decision is
**0113**.

Team Sync moves **encrypted ledger entry bundles** between developer devices so
gitignored `.ledgerful/` ledgers can consolidate without putting secrets or
device keys in Git.

Git **never** consolidates `.ledgerful/` — only opt-in team sync can.

## Opt-in forever

| Control | Default | Notes |
|---|---|---|
| Cargo feature `sync` | **on** in default builds (0110) | MCP-npm job still builds without it |
| `[sync].enabled` | **`false`** | `sync init` and **`sync pair` never** set this true |
| Pair accept / peer store | **real (0111)** | `LF-PAIR-1…` invite + `sync/peers/{device_id}.pub` |
| Secure transport / apply | **real (0112)** | extract cursor integrity, `.lfbundle`, verify-then-apply, golden path |
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
| **0110** | Feature posture, fail-closed pair stub, layout-aware init, SoT `device_id`, disabled-run honesty, Experimental docs/skill, light doctor |
| **0111** | Real pair invite/accept, peer store/list/revoke, keyed blake3 MAC, status/doctor peer honesty |
| **0112** (this) | Extract cursor integrity, `.lfbundle` + dual-read, peer-scoped transport identity, same-volume put, clock-drift quarantine, full crypto-chain golden path |
| **0113** | Wizard/status UX, Available label decision, secret rotation UX |

## Two-device enable → run (after mutual pairing)

1. **Both devices:** `ledgerful sync init` (keys + SoT `device_id`; `enabled` stays false).
2. **Share team secret** out-of-band (password manager). Set `LEDGERFUL_SYNC_SECRET` for automation.
   Prefer **not** pasting the secret into the same chat as the invite.
3. **Device A:** `ledgerful sync pair` → prints one-line invite
   `LF-PAIR-1.<device_id>.<b64url_pub>.<b64url_tag>`
4. **Device B:** `ledgerful sync pair '<invite-from-A>'` → writes `sync/peers/<A-id>.pub`
5. **Mutual:** B generates its invite; A accepts. Two-way trust requires **both** accepts.
6. **List / revoke:** `ledgerful sync pair --list` · `ledgerful sync pair --revoke <device_id>`
7. **Enable only when ready** (never auto):
   ```bash
   ledgerful config set sync.target="dir:///path/to/shared"
   ledgerful config set sync.enabled=true
   ```
8. **Run on each device** (order: exporter then importer for a one-way first pass):
   ```bash
   ledgerful sync run --once   # extract→encrypt→put then list→decrypt→parse→apply
   ```
9. Importer ledger rows for foreign `tx_id`s appear with `origin='PEER'`. Local SoT
   `device_id` is never overwritten by remote manifests.

### `init --force` re-pair

`sync init --force` mints a **new** local `device_id` + keypair. Peers that already trusted the
**old** id will reject new bundles as unknown device until you **re-pair mutually** (new invites
both ways). Document this before force-reinit on a live mesh.

## Identity isolation

| Artifact | Location | Rule |
|---|---|---|
| `device.key` / `device.pub` | `{state_dir}/sync/` (layout `.ledgerful/sync`) | **Never** in bundle payload; never imported from peers |
| **Local `device_id` SoT** | SQLite `sync_state.device_id` (row `id=1`) | Written by `sync init` / `--force`. pair, status, verify, **run** all read this SoT |
| `config.sync.device_id` | Optional **mirror** only | Written via `toml_edit` helpers; must agree with SoT after init |
| Peer trust store | `{state_dir}/sync/peers/{device_id}.pub` | FS-only SoT for apply; only paired pubs + explicit self key may verify |
| Remote `manifest.device_id` | Bundle metadata | Identifies **sender** for peer key lookup; **must never** overwrite local SoT |

Apply updates `last_apply_hlc` only (`ON CONFLICT DO UPDATE SET last_apply_hlc = …`) and does **not** set `device_id` from the remote manifest.

Extract advances `last_extract_hlc` / `last_run_at` / `device_id` only — it **never** nulls
`last_apply_hlc` (partial upsert; not full-row `SyncState::save`).

## Algorithms (honest names)

- **Bundle KDF:** Argon2id (pin: argon2 **0.5.x** — no 0.6 RC)
- **AEAD:** XChaCha20-Poly1305 (chacha20poly1305 **0.11**)
- **Device identity:** Ed25519 (ed25519-dalek **2.x** — **no** 3.0 bump); accept requires
  `VerifyingKey::from_bytes` (curve check), not mere length
- **Pair invite MAC (0111):**
  - `key = blake3::derive_key("ledgerful team-sync pair v1", secret)`
  - `msg = b"pair-invite-v1\0" || device_id || pub32`
  - `tag = keyed_hash(key, msg)[0..16]` verified with constant-time `ct_eq`
  - Encoding: `base64` **URL_SAFE_NO_PAD**, `.`-delimited `LF-PAIR-1…`
- **KDF honesty:** invite MAC uses a **fast** blake3 derive (captured invite ≈ offline oracle on
  the team secret). Bundle encryption still uses **Argon2id**. Accepted for v1 under the
  high-entropy secret assumption; re-trigger slow invite KDF if weak secrets are ever allowed.
- **Bundle file extension (0112):** new writes use **`.lfbundle`** (honest name — not OpenPGP).
  Transport dual-reads last-dot **`lfbundle` OR `gpg`** (legacy filter was any `*.gpg`, not only
  `*.zip.gpg`). Temp put names use `.part` and never match the bundle filter.

Secrets: keep the team secret out of git, logs, and success banners. Prefer a password manager +
`LEDGERFUL_SYNC_SECRET` for automation. Run/verify wrap the secret in `Zeroizing`.

## Shared-folder v1 transport (0112)

v1 target scheme: `dir://<absolute-path>` — a shared folder (SMB, Dropbox-style, USB, etc.) with
per-device outboxes under `devices/<device_id>/`. No cloud SaaS, no WebRTC, no “sync over GitHub”
in this ladder.

| Behavior | Policy |
|---|---|
| Outbox | `devices/<local_id>/` |
| Inbox | peer dirs under `devices/*/` (skips self) |
| Incoming identity | `(peer_id, name)` threaded through get → apply → move (no bare-name re-search) |
| Put | same-volume temp under `devices/<id>/.tmp/` then rename (**no** OS-temp→share / EXDEV) |
| Processed / quarantine | under local device dir; names prefixed `{peer_id}__` for disambiguation |
| Clock drift | quarantine if `bundle_hlc.physical_ms > now + max_clock_drift_seconds*1000` (ahead-only) |
| Empty extract | `NoNewEntries` — no watermark advance, no empty ciphertext |
| Batch | at most `batch_size` entries per extract; multi-run drains backlog |
| Zip path | extract returns finalized signed zip once; run only encrypts + puts (no rebuild) |

### HLC watermark compare

SQL uses string compare of Display form `{physical_ms}-{:04}-{node_id}`. Era-safe only while
`physical_ms` stays fixed-width (13-digit epoch ms) and logical is zero-padded to 4 digits. Same
discipline for entry and tombstone watermark filters.

## Word collisions (do not confuse)

| Phrase | Means |
|---|---|
| **Team Sync** / `ledgerful sync` | This feature — encrypted ledger entry bundles between devices |
| **Real-time Sync** / `watch` IncrementalSync | Knowledge-graph / index refresh while watching the worktree |
| **Ledger Federation** / `federate` | Sibling-repository ledger export/import for cross-repo provenance |
| OpenAPI `/api/sync/status` | Dashboard DTO (`device_id` + HLC timestamps). **CLI-first** for peers/enabled (DTO unchanged in 0112) |
| **bridge** | AI-Brains NDJSON interchange — not team sync |
| **schedule** | Nightly index jobs — not team sync |

## CLI (Experimental)

```bash
ledgerful sync --help          # marked Experimental
ledgerful sync init            # keys + SoT device_id; enabled stays false
ledgerful sync status          # peer count/ids from local trust store
ledgerful sync pair            # print LF-PAIR-1 invite (needs LEDGERFUL_SYNC_SECRET)
ledgerful sync pair '<invite>' # accept; writes peers/{device_id}.pub; never enables
ledgerful sync pair --list
ledgerful sync pair --revoke <device_id>
ledgerful sync pair --force '<invite>'  # re-key same device_id
ledgerful sync run --once      # clear message when disabled; no secret prompt
ledgerful sync verify <path>   # decrypt + Ed25519 verify via load_peer_keys + self
```

Doctor: if `enabled=true` without init, empty target, or **zero peers** → **warn** / optional
(`sync-enabled-no-peers` never sole-blocks publish). Mere “sync disabled” never sole-sets
`readyForPublish=false`.

## Threat sketch

- **No silent merge:** default `enabled=false`; disabled `run` does not write outbox; **pair never enables**.
- **Invite proves shared secret + (device_id, pubkey) binding** — not interactive liveness or MitM
  resistance if the secret already leaked.
- **Team secret alone** is not enough to inject PEER rows: ciphertext may decrypt, but parse requires
  a paired Ed25519 pubkey (or self). Forged/unknown/bad-sig bundles **quarantine**.
- **Hostile shared folder:** plant/truncate/rename/replay possible. Mitigations: AEAD, peer sign,
  peer-scoped get+move identity (prevents archiving the wrong peer’s same-named file), same-volume
  put, clock-drift ahead reject. Replay of older HLC is skipped (idempotent); not a full anti-replay log.
- **Local identity isolation:** keys FS-only; SoT device_id never imported from bundles; peer
  `.pub` files never enter bundles.
- **Path-safe device_id** on accept (no `.` `/` `\` traversal) before any peer write; temp files
  do not match `*.pub`.
- **Argon2 cost:** one KDF per encrypt and per decrypt (64 MiB). Accepted for v1 small-team volumes.
- **Not a complete threat model:** re-evaluate before Available (0113).

## Performance

Team sync must **not** invoke graph indexing, SCIP, or full reindex on the sync path. Bundle
extract/apply are ledger-row operations only. Empty runs return cheaply (`NoNewEntries`).
`batch_size` bounds each extract cycle.
