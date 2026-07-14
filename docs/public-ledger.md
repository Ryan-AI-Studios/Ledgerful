# Public Ledger Bundle

This document explains the Ledgerful public ledger bundle: what it is, how it is generated, what it contains, and how to verify it.

---

## 1. What it is

The public ledger bundle is the engine's own signed change ledger, published as a static, redaction-controlled, cryptographically verifiable bundle. It is the development ledger of the Ledgerful project itself — a broadcast artifact, not a service. The bundle lets anyone inspect the project's history of intentional changes without exposing internal context, and lets them verify that every published entry was signed by the same Ed25519 keypair that the original author used when committing the change.

---

## 2. How to generate it

Generate a bundle with the CLI:

```bash
ledgerful ledger export-public --output <dir> [--sign [--key <path>]]
```

* `--output <dir>` — destination directory for the bundle files.
* `--sign` — sign the manifest with the `ledgerful-ledger-bot` key.
* `--key <path>` — directory containing the bot keypair and the author-pseudonym secret. Defaults to `~/.ledgerful/keys/` when omitted.

---

## 3. What's in the bundle

The export writes the following files to the output directory:

* `manifest.json` — publisher identity (`ledgerful-ledger-bot`), entry count, time range, signature algorithm, Ed25519 signature and public key fingerprint, allowlist version, honest-ceiling text, and (when present) the signed chain head.
* `entries.ndjson` — one JSON object per line, with one committed ledger entry per line, limited to the allowlisted fields.
* `index.html` — static, no-JavaScript browse page listing the published entries.
* `verifier.html` — standalone offline verifier using the browser's WebCrypto API. No network resources are loaded.
* `README.md` — a self-contained explanation of the bundle, the allowlist, verification instructions, and the honest ceiling.

When signed with `--sign`, the bundle also contains:

* `manifest.sig` — raw 64-byte Ed25519 signature over the canonical `manifest.json` bytes.
* `manifest.pub` — raw 32-byte Ed25519 verifying key for the bot signature.

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

This bundle proves each entry's Ed25519 signature and the manifest signature. It does NOT prove the order/set of entries (that's the chain head's role) or the identity behind the key (out-of-band fingerprint comparison).

---

## 7. Separate bot key

The bundle manifest is signed by the `ledgerful-ledger-bot` key, separate from the engine's main signing key. If the bot key is compromised, the impact is limited to the bundle signature; it does not implicate the engine's own ledger signing identity. Bot-key rotation only requires re-signing future bundles, not re-signing historical ledger entries.

---

## 8. Chain head

If the ledger has a chain head (track 0046), the manifest carries it as a rollback checkpoint. The chain head fields are serialized in `manifest.json` under `chainHead`. Verifiers can compare the bundle's claimed latest entry hash and chain length against an independently obtained chain head.

---

## 9. No-network claim

The `export-public` command imports no network crates. The public export module (`src/ledger/public_export.rs`) contains only offline cryptographic, file-system, and serialization code. The CI no-network guard (see `tests/ci/allowlist.rs`) greps the module for network-related dependency names and fails the build if any are introduced.

---

## 10. Verification

You can verify a bundle in two ways:

1. Open `verifier.html` in a modern browser. It loads `manifest.json` and `entries.ndjson` from the same directory, verifies the manifest signature with WebCrypto, and verifies every entry's Ed25519 signature. It works offline.
2. Use the CLI against the source ledger:

   ```bash
   ledgerful verify --signatures
   ```

   This checks the source ledger's chain and entry signatures, which the public export is derived from.

---

## 11. Publishing

The publishing cron is shipped disabled. After launch, the user enables automated publishing with:

```bash
ledgerful ledger publish-public --enable
```

This command is part of the web slice and is not yet implemented.
