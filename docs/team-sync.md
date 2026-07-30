# Team Sync (Experimental)

**Status:** Experimental (tracks **0110** foundation + **0111** pairing/peer trust). Not
Available marketing; multi-device apply polish is **0112**.

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
| **0110** | Feature posture, fail-closed pair stub, layout-aware init, SoT `device_id`, disabled-run honesty, Experimental docs/skill, light doctor |
| **0111** (this) | Real pair invite/accept, peer store/list/revoke, keyed blake3 MAC, status/doctor peer honesty |
| **0112** | Encrypted transport E2E, apply bounds, quarantine UX, two-device golden path |
| **0113** | Wizard/status UX, Available label decision, secret rotation UX |

## Two-device mutual pairing (golden path)

1. **Both devices:** `ledgerful sync init` (keys + SoT `device_id`; `enabled` stays false).
2. **Share team secret** out-of-band (password manager). Set `LEDGERFUL_SYNC_SECRET` for automation.
   Prefer **not** pasting the secret into the same chat as the invite.
3. **Device A:** `ledgerful sync pair` → prints one-line invite
   `LF-PAIR-1.<device_id>.<b64url_pub>.<b64url_tag>`
4. **Device B:** `ledgerful sync pair '<invite-from-A>'` → writes `sync/peers/<A-id>.pub`
5. **Mutual:** B generates its invite; A accepts. Two-way trust requires **both** accepts.
6. **List / revoke:** `ledgerful sync pair --list` · `ledgerful sync pair --revoke <device_id>`
7. **Enable only when ready:** set `[sync].enabled = true` and a shared-folder `target` yourself.
   Pairing **never** enables sync.

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
| OpenAPI `/api/sync/status` | Dashboard DTO (`device_id` + HLC timestamps). **CLI-first** for peers/enabled (DTO unchanged in 0111) |
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
```

Doctor: if `enabled=true` without init, empty target, or **zero peers** → **warn** / optional
(`sync-enabled-no-peers` never sole-blocks publish). Mere “sync disabled” never sole-sets
`readyForPublish=false`.

## Threat sketch

- **No silent merge:** default `enabled=false`; disabled `run` does not write outbox; **pair never enables**.
- **Invite proves shared secret + (device_id, pubkey) binding** — not interactive liveness or MitM
  resistance if the secret already leaked.
- **Team secret alone** is not enough to inject a peer without an invite string; holders of the
  secret can still forge invites for **new** device_ids (accepted shared-secret threat). Mitigate
  with revoke + secret rotation (rotation UX → 0113).
- **Local identity isolation:** keys FS-only; SoT device_id never imported from bundles; peer
  `.pub` files never enter bundles.
- **Path-safe device_id** on accept (no `.` `/` `\` traversal) before any peer write; temp files
  do not match `*.pub`.
- **Shared-folder trust:** anyone with folder write can drop ciphertext; apply verifies signatures
  against known peers and quarantines failures (polish → 0112).
- **Not a complete threat model:** re-evaluate before Available (0113).

## Performance

Team sync must **not** invoke graph indexing, SCIP, or full reindex on the sync path. Bundle extract/apply are ledger-row operations only.
