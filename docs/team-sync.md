# Team Sync (Available — opt-in shared-folder v1)

**Status:** **Available** for the **opt-in shared-folder v1** topology only.
Tracks **0110–0113** are complete. Not default-on, not cloud SaaS, not multi-master
CRDT, not auto-sync on `git push`.

Team Sync moves **encrypted ledger entry bundles** between developer devices so
gitignored `.ledgerful/` ledgers can consolidate without putting secrets or
device keys in Git.

Git **never** consolidates `.ledgerful/` — only opt-in team sync can.

## Honesty ceiling (Available)

| Promise | Reality |
|---|---|
| Multi-device ledger consolidation | **Yes** — shared-folder `dir://` drop path |
| Default-on / silent merge | **No** — `[sync].enabled = false` forever until you opt in |
| Cloud / WebRTC / GitHub relay | **No** — out of scope permanently for this ladder |
| OS scheduler auto-install for `sync run` | **No** — schedule field is **display-only** |
| Wizard stores your secret | **No** — secret never in config.toml, git, logs, or metrics |

## Opt-in forever

| Control | Default | Notes |
|---|---|---|
| Cargo feature `sync` | **on** in default builds (0110) | MCP-npm job still builds without it |
| `[sync].enabled` | **`false`** | `sync init`, `sync pair`, and plain `sync setup` **never** set this true |
| Pair accept / peer store | **real (0111)** | `LF-PAIR-1…` invite + `sync/peers/{device_id}.pub` |
| Secure transport / apply | **real (0112)** | extract cursor integrity, `.lfbundle`, verify-then-apply, golden path |
| Setup checklist / status next-action | **real (0113)** | `sync setup`, gated `--enable`, readiness + quarantine (this device) |

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
| **0110** | Feature posture, layout-aware init, SoT `device_id`, disabled-run honesty, Experimental docs/skill, light doctor |
| **0111** | Real pair invite/accept, peer store/list/revoke, keyed blake3 MAC, status/doctor peer honesty |
| **0112** | Extract cursor integrity, `.lfbundle` + dual-read, peer-scoped transport identity, same-volume put, clock-drift quarantine, full crypto-chain golden path |
| **0113** | Setup checklist, status next-action, secret hygiene docs, **Available** (opt-in shared-folder v1) |

## ≤15-minute two-person setup card

Shared-folder v1 (Syncthing / Drive / OneDrive / NAS / SMB / USB) — one topology.

### 0. Prerequisites (once)

- Both machines have Ledgerful installed and a git worktree with `.ledgerful/` (run `ledgerful init` if needed).
- Agree on a **shared folder** path both can write (create it empty first).
- One person generates a **high-entropy team secret** (12+ word phrase or long random string).

### 1. Secret distribution (password manager)

1. Person A creates a **shared vault item** (1Password / Bitwarden / etc.) with the team secret.
2. Person B accepts the shared item.
3. On each device, for automation: set `LEDGERFUL_SYNC_SECRET` from the vault (shell env, secret-manager inject — **never** commit `.env` with the secret to the repo).
4. Prefer **not** pasting the secret into the same chat as a pairing invite.

### 2. Init on both devices

```bash
# Device A and Device B (separately)
ledgerful sync init
# or: LEDGERFUL_SYNC_SECRET=… ledgerful sync init
```

`enabled` stays **false**. Keys land under `.ledgerful/sync/`.

### 3. Mutual pairing — two full invite/accept cycles

Trust is **one-directional per accept**. You need **both** directions.

```bash
# Round trip 1 — A trusts B's identity? No: A publishes invite; B accepts A.
# Device A:
ledgerful sync pair
# → prints LF-PAIR-1.<device_id>.<b64url_pub>.<b64url_tag>
# Device B:
ledgerful sync pair 'LF-PAIR-1.…from-A…'
# → writes .ledgerful/sync/peers/<A-id>.pub

# Round trip 2 — reverse (required for two-way merge)
# Device B:
ledgerful sync pair
# Device A:
ledgerful sync pair 'LF-PAIR-1.…from-B…'
```

Check: `ledgerful sync pair --list` on each device shows the other device_id.

### 4. Shared folder + target

```bash
# Both devices — same logical share (paths may differ per OS)
ledgerful config set sync.target="dir:///absolute/path/to/shared"
# Windows example: dir:///C:/Shared/ledgerful-sync
# (triple-slash + drive letter is correct; SyncTarget::parse handles it)
```

### 5. Readiness checklist (no enable yet)

```bash
ledgerful sync setup
# or machine: ledgerful sync setup --json
```

All gates should be green except **Enabled**. Fix any Next: line first.

### 6. Enable (gated)

```bash
ledgerful sync setup --enable
# Refuses (non-zero) unless: initialized + ≥1 peer + parseable target + target reachable
# On success: writes sibling config.toml.bak then sets [sync].enabled=true
# Never writes the team secret. Never sets target.
```

### 7. First run both ways

```bash
# Both devices (exporter then importer order is fine for a first one-way pass)
ledgerful sync run --once
```

Importer ledger rows for foreign `tx_id`s appear with `origin='PEER'`. Local SoT
`device_id` is never overwritten by remote manifests.

### 8. Status anytime

```bash
ledgerful sync status
ledgerful sync status --json
```

Shows **Readiness**, **Next**, **Target reachable**, **Quarantined (this device)**.

---

## Secret hygiene

| Rule | Detail |
|---|---|
| Never in git | No `.env` with team secret committed; no secret in config.toml |
| Never logged | Setup/status/metrics never print secret material |
| Presence only | Setup/status check whether `LEDGERFUL_SYNC_SECRET` is set — **never prompt** |
| Init / run may prompt | Interactive TTY only (`prompt_password`); agents must set the env var |
| Distribution | Shared password-manager vault item is the **primary** path |
| Automation | Inject env from secret manager at process start |

### Dual-purpose team secret (document, do not redesign)

The **same** team secret feeds:

1. **Pair invite MAC** — fast `blake3::derive_key` (captured invite ≈ offline oracle on the secret if entropy is weak).
2. **Bundle AEAD** — Argon2id → XChaCha20-Poly1305.

Accepted under a **high-entropy** secret assumption. Do not use short or guessable secrets.

### Secret rotation procedure

1. Update the shared password-manager item with the **new** secret.
2. On **every** device: update `LEDGERFUL_SYNC_SECRET` (or be ready to type the new secret on run).
3. Peer **pubkeys remain valid** across rotation (no re-pair required solely for rotation).
4. **Old `.lfbundle` ciphertexts do not decrypt** under the new secret — expect quarantine/skip for leftover drop-folder blobs. Purge or leave them; re-run sync to produce new bundles.
5. If a device is compromised: `ledgerful sync pair --revoke <device_id>` on remaining devices (and re-pair if that device returns with new keys).

### Local state is plaintext

Team sync encrypts **drop-folder bundles** only. Local `.ledgerful/` (keys, ledger DB, peer pubs) is **plaintext on disk**. If device theft is a threat, use **OS full-disk encryption** (BitLocker / FileVault / LUKS).

## High-latency NAS / cloud-drive notes

- `sync setup` / `sync status` probe target reachability with a **bounded** wall-clock timeout (~3s). Timeout → not reachable + honest next-action (CLI does not hang).
- Full `sync run` may still **block** on slow `read_dir` / I/O for the OS connection timeout — no per-bundle FS timeout in v1. For unattended runs, use a process watchdog / OS job timeout.

## Schedule (display-only)

`[sync].schedule` (default `0 3 * * *`) is **display-only** — Ledgerful does **not** install OS cron or Task Scheduler for team sync.

**Why:** embedding `LEDGERFUL_SYNC_SECRET` in Task Scheduler XML, crontab, or shell history is an ops footgun.

**Manual recipe (operator-owned):**

1. Keep the secret in a password manager / OS secret store.
2. Create an OS scheduled task that **injects** the env var at runtime (secret-manager CLI, Windows Credential Manager, systemd `LoadCredential`, etc.).
3. Run `ledgerful sync run --once` from that task.
4. Do **not** put the raw secret in the task definition if you can avoid it.

Word collision: `ledgerful schedule setup-nightly` is the **index** nightly job — **not** team sync.

## Shared-folder v1 transport (0112)

v1 target scheme: `dir://<absolute-path>` — a shared folder with per-device outboxes under
`devices/<device_id>/`. No cloud SaaS, no WebRTC, no “sync over GitHub”.

| Behavior | Policy |
|---|---|
| Outbox | `devices/<local_id>/` |
| Inbox | peer dirs under `devices/*/` (skips self) |
| Incoming identity | `(peer_id, name)` threaded through get → apply → move |
| Put | same-volume temp under `devices/<id>/.tmp/` then rename |
| Processed / quarantine | under **local** device dir; quarantine label = **this device only** |
| Clock drift | quarantine if bundle HLC too far ahead |
| Empty extract | `NoNewEntries` — no watermark advance |
| Batch | at most `batch_size` entries per extract |

### `init --force` re-pair

`sync init --force` mints a **new** local `device_id` + keypair. Peers that already trusted the
**old** id will reject new bundles until you **re-pair mutually** (new invites both ways).

## Identity isolation

| Artifact | Location | Rule |
|---|---|---|
| `device.key` / `device.pub` | `{state_dir}/sync/` | **Never** in bundle payload |
| **Local `device_id` SoT** | SQLite `sync_state.device_id` (row `id=1`) | Written by `sync init` / `--force` |
| `config.sync.device_id` | Optional **mirror** only | Must agree with SoT after init |
| Peer trust store | `{state_dir}/sync/peers/{device_id}.pub` | FS-only; only paired pubs + self may verify |
| Remote `manifest.device_id` | Bundle metadata | Identifies **sender**; **must never** overwrite local SoT |

## Algorithms (honest names)

- **Bundle KDF:** Argon2id (pin: argon2 **0.5.x** — no 0.6 RC)
- **AEAD:** XChaCha20-Poly1305 (chacha20poly1305 **0.11**)
- **Device identity:** Ed25519 (ed25519-dalek **2.x** — **no** 3.0 bump)
- **Pair invite MAC (0111):**
  - `key = blake3::derive_key("ledgerful team-sync pair v1", secret)`
  - `msg = b"pair-invite-v1\0" || device_id || pub32`
  - `tag = keyed_hash(key, msg)[0..16]` verified with constant-time `ct_eq`
  - Encoding: `base64` **URL_SAFE_NO_PAD**, `.`-delimited `LF-PAIR-1…`
- **Bundle file extension:** new writes use **`.lfbundle`**; dual-read `lfbundle` OR legacy `gpg`.

## Word collisions (do not confuse)

| Phrase | Means |
|---|---|
| **Team Sync** / `ledgerful sync` | This feature — encrypted ledger entry bundles between devices |
| **Real-time Sync** / `watch` IncrementalSync | Knowledge-graph / index refresh while watching the worktree |
| **Ledger Federation** / `federate` | Sibling-repository ledger export/import for cross-repo provenance |
| OpenAPI `/api/sync/status` | Dashboard DTO (`device_id` + HLC timestamps). **CLI-first** for peers/enabled/readiness |
| **bridge** | AI-Brains NDJSON interchange — not team sync |
| **schedule setup-nightly** | Nightly **index** jobs — not team sync |

## CLI

```bash
ledgerful sync --help
ledgerful sync init                 # keys + SoT device_id; enabled stays false
ledgerful sync setup                # readiness checklist + Next (never enables)
ledgerful sync setup --json         # pure camelCase JSON, schemaVersion: 1
ledgerful sync setup --enable       # gated enable; bak then sync.enabled=true
ledgerful sync status               # readiness, next-action, target reachable, quarantine
ledgerful sync status --json
ledgerful sync pair                 # print LF-PAIR-1 invite (needs LEDGERFUL_SYNC_SECRET)
ledgerful sync pair '<invite>'      # accept; never enables
ledgerful sync pair --list
ledgerful sync pair --revoke <device_id>
ledgerful sync pair --force '<invite>'
ledgerful sync run --once           # clear message when disabled; no secret prompt if disabled
ledgerful sync verify <path>        # decrypt + Ed25519 verify
```

Doctor: if `enabled=true` without init, empty target, or **zero peers** → **warn** / optional
and points at `ledgerful sync setup`. Mere “sync disabled” never sole-sets
`readyForPublish=false`.

## Threat sketch (Available shared-folder v1)

- **No silent merge:** default `enabled=false`; disabled `run` does not write outbox; **pair and plain setup never enable**.
- **Invite proves shared secret + (device_id, pubkey) binding** — not interactive liveness or MitM resistance if the secret already leaked.
- **Dual-purpose secret:** MAC (fast blake3) + AEAD (Argon2id). High entropy required.
- **Team secret alone** is not enough to inject PEER rows: ciphertext may decrypt, but parse requires a paired Ed25519 pubkey (or self). Forged/unknown/bad-sig bundles **quarantine**.
- **Hostile shared folder:** plant/truncate/rename/replay possible. Mitigations: AEAD, peer sign, peer-scoped get+move identity, same-volume put, clock-drift ahead reject.
- **Local identity isolation:** keys FS-only; SoT device_id never imported from bundles.
- **Local `.ledgerful/` plaintext** — use OS full-disk encryption if device theft matters.
- **Quarantine is per device** under the share (`devices/<local_id>/quarantine/`); status reports **this device only**.
- **Argon2 cost:** one KDF per encrypt and per decrypt (64 MiB). Accepted for v1 small-team volumes.

## Performance

Team sync must **not** invoke graph indexing, SCIP, or full reindex on the sync path. Bundle
extract/apply are ledger-row operations only. Empty runs return cheaply (`NoNewEntries`).
`batch_size` bounds each extract cycle.
