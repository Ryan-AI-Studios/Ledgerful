# Public Ledger Bundle

This document explains the Ledgerful public ledger bundle: what it is, how it is generated, what it contains, and how to verify it.

---

## 1. What it is

The public ledger bundle is the engine's own signed change ledger, published as a static, redaction-controlled, cryptographically verifiable bundle. It is the development ledger of the Ledgerful project itself — a broadcast artifact, not a service. The bundle lets anyone inspect the project's history of intentional changes without exposing internal context, and lets them verify that every published entry was signed by the same Ed25519 keypair that the original author used when committing the change.

Stable public surfaces (Ledgerful-itself, track **0120**):

| Artifact | Stable URL |
|---|---|
| Full redacted bundle (browse + verifier) | `https://www.ledgerful.dev/ledger/` |
| **Thin signed chain head** | `https://www.ledgerful.dev/ledger/chain_head.json` |

The thin head uses the same JSON shape as `ledgerful export head` / manifest `chainHead`. It is an independent retention point for **this** project’s ledger — not a multi-tenant public log for customer repos.

---

## 2. How to generate it

Generate a bundle with the CLI:

```bash
ledgerful ledger export-public --output <dir> [--sign [--key <path>]]
```

* `--output <dir>` — destination directory for the bundle files.
* `--sign` — sign the manifest with the `ledgerful-ledger-bot` key.
* `--key <path>` — directory containing the bot keypair and the author-pseudonym secret. Defaults to `~/.ledgerful/keys/` when omitted.

Thin head only (operator / checkpoint shape):

```bash
ledgerful export head --out ./chain_head.json
```

---

## 3. What's in the bundle

The export writes the following files to the output directory:

* `manifest.json` — publisher identity (`ledgerful-ledger-bot`), entry count, time range, signature algorithm, Ed25519 signature and public key fingerprint, allowlist version, honest-ceiling text, an SHA-256 digest of `entries.ndjson`, and (when present) the signed chain head.
* `entries.ndjson` — one JSON object per line, with one committed ledger entry per line, limited to the allowlisted fields.
* `index.html` — static, no-JavaScript browse page listing the published entries.
* `verifier.html` — standalone offline verifier using the browser's WebCrypto API. No network resources are loaded.
* `README.md` — a self-contained explanation of the bundle, the allowlist, verification instructions, and the honest ceiling.

When signed with `--sign`, the bundle also contains:

* `manifest.sig` — raw 64-byte Ed25519 signature over the canonical `manifest.json` bytes.
* `manifest.pub` — raw 32-byte Ed25519 verifying key for the bot signature.

Public hosting additionally publishes a standalone **`chain_head.json`** (same fields as `export head` / `manifest.chainHead`) at `/ledger/chain_head.json` so operators can download a thin checkpoint without the full NDJSON bundle.

---

## 4. The allowlist

Each published entry contains only these fields:

* `tx_id`
* `category`
* `summary`
* `reason`
* `committed_at`
* `author_pseudonym`
* `verification_result`
* `risk_level`
* `entry_hash`
* `sig_version` (1 = legacy five-field; 2 = full provenance — non-sensitive)
* `signature`
* `public_key`

The following fields are intentionally redacted because they carry internal-only context that is not needed for public accountability:

* `entity` and `entity_normalized` — the affected file path or symbol; too granular for a public broadcast.
* `change_type`, `is_breaking`, `entry_type` — internal change taxonomy.
* `outcome_notes` — developer-level verification commentary that may reference internal systems.
* `origin`, `trace_id`, `related_tickets` — internal provenance links.
* `author` (raw) — replaced by `author_pseudonym` to protect identity while preserving per-author correlation.
* `observed` — internal observe-mode bookkeeping, not part of the signed basis.
* `prev_hash` — internal chain linkage; only the entry-specific `entry_hash` is published.
* Internal IDs: `id`, `operation_id`, `snapshot_id`, `tree_hash`, `issue_ref`.
* `verification_basis` and raw `verification_status` — replaced by the mapped `verification_result` value (`PASS`, `FAIL`, `PARTIAL`).

---

## 5. Author pseudonym

`author_pseudonym` is computed as `HMAC-SHA256(secret_key, author)`, encoded as lowercase hex. The same author always yields the same pseudonym for a given secret, so long-running contribution patterns remain correlated without revealing the author's identity. The secret key is generated once per bot keys directory and is never published in the bundle.

---

## 6. The honest ceiling

This bundle proves the manifest signature and the integrity of `entries.ndjson`.

* **v1 entries** (`sig_version` missing or `1`): offline verifiers can re-check Ed25519 over the published five-field payload (`tx_id`, `category`, `summary`, `reason`, `committed_at`).
* **v2 entries** (`sig_version >= 2`): the signature binds redacted provenance fields (entity, author, origin, change_type, …) that are intentionally not published. Offline entry-signature re-verify is **not** claimed for v2; use `ledgerful verify --signatures` against the local ledger.
* Chain head (when present) is a rollback checkpoint. Full `prev_hash` walks are not re-verified offline (prev_hash is redacted).
* Key identity still requires out-of-band fingerprint comparison.

**Not claimed:**

* “Immutable forever” or multi-party transparency (Rekor/CT).
* That every customer repo has public retention — customer operators use **0119** (`export head` + off-machine retention + `verify --against-export`).
* That a compromised publish pipeline or bot key is safe against split-view.

---

## 7. Separate bot key

The bundle manifest is signed by the `ledgerful-ledger-bot` key, separate from the engine's main signing key. If the bot key is compromised, the impact is limited to the bundle signature; it does not implicate the engine's own ledger signing identity. Bot-key rotation only requires re-signing future bundles, not re-signing historical ledger entries.

---

## 8. Chain head

If the ledger has a chain head (track 0046), the manifest carries it as a rollback checkpoint. The chain head fields are serialized in `manifest.json` under `chainHead`. The public site also exposes a standalone thin file:

`https://www.ledgerful.dev/ledger/chain_head.json`

Verifiers can compare a local ledger against that checkpoint with **checkpoint** semantics (local must extend or equal the public head). There is **no** `verify --against-url` (SSRF fence); compose:

```powershell
curl -fsSL -o head.json https://www.ledgerful.dev/ledger/chain_head.json
ledgerful verify --signatures --against-export .\head.json
```

This detects **local rollback/rewrite relative to the published head** for a Ledgerful engine workspace that shares that genesis — not “the public site proves an arbitrary private product history.” See `docs/chain-checkpoint.md`.

---

## 9. No-network claim

The `export-public` command imports no network crates. The public export module (`src/ledger/public_export.rs`) contains only offline cryptographic, file-system, and serialization code. Two CI guards protect this:

* The allowlist guard (see `tests/ci/allowlist.rs`) ensures sensitive fields are not published without a documented exception.
* The no-network guard (see `.github/workflows/ci.yml`, `no-network-public-export` job) greps the module for network-related dependency names and fails the build if any are introduced.

---

## 10. Verification

You can verify a bundle in two ways:

1. Open `verifier.html` in a modern browser. It loads `manifest.json` and `entries.ndjson` from the same directory, verifies the manifest signature with WebCrypto, checks that the SHA-256 of `entries.ndjson` matches the `entriesSha256` field in the manifest, and dual-paths entry signatures by `sig_version` (full offline Ed25519 for v1; honesty fence for v2). It works offline.
2. Use the CLI against the source ledger for full v2 entry + chain verification:

   ```bash
   ledgerful verify --signatures --chain
   ```

   This checks the source ledger's chain and entry signatures (including redacted provenance fields), which the public export is derived from.

3. Checkpoint against the published thin head (Ledgerful-itself):

   ```powershell
   curl -fsSL -o head.json https://www.ledgerful.dev/ledger/chain_head.json
   ledgerful verify --signatures --against-export .\head.json
   ```

---

## 11. Publishing (export-then-commit)

The engine is responsible for exporting a signed, redacted bundle (`ledgerful ledger export-public`) and a thin head (`ledgerful export head`). The actual publishing step — committing those artifacts into the web repository and deploying them — is owned by the **web** slice, not the engine.

### Model

1. **Export** on a machine that holds engine `.ledgerful` state (and the bot key when signing with `--sign`). GitHub-hosted CI cannot invent a live export from a clean checkout: `.ledgerful/` is gitignored.
2. **Commit** artifacts into `ledgerful-web` `public/ledger/` (PR preferred), including `chain_head.json` consistent with `manifest.chainHead`, plus `manifest.sig` / `manifest.pub` when claiming signed.
3. **CI validation** on the web repo checks committed files (presence, head/manifest coherence, signatures). CI **validates** artifacts; it does **not** produce them from live dogfood history on pure ubuntu-latest runners.
4. After deploy, public git + CDN retain the head **outside** a single local database.

### Web enable flag (not an engine command)

The web publish helper is gated by environment variable:

```text
LEDGERFUL_PUBLISH_LEDGER_ENABLED=1
```

That flag enables the **web-repo** publish helper script. There is **no** engine command `ledgerful ledger publish-public` (and none is planned). Historical web docs that referenced `publish-public --enable` were incorrect; the engine surface remains `ledger export-public` + `export head` only.

### Cadence honesty

Refresh is **export-then-commit** (manual PR or a runner that actually has ledger access). Do not market a silent “published daily” cron unless a runner with live ledger state is proven.
