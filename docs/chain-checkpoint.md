# Chain-head checkpoint retention

Local `ledgerful verify --signatures --chain` proves integrity of the **presented**
chain. It cannot detect a full rollback if an adversary controls the only copy of
the database **and** the signed chain head. Independent retention of a prior head
closes that gap for operators.

This is **not** local immutability, Rekor/CT, or a public transparency log.

## 5-step recipe

1. **Export a thin head** (or a full SOC2 zip — both work):

   ```powershell
   New-Item -ItemType Directory -Force -Path .\checkpoints | Out-Null
   ledgerful export head --out .\checkpoints\head.json
   ```

   (Or omit `--out` to write `./ledgerful-chain-head.json` in the current directory.)

   For pure-stdout / pipe workflows (RO reviewers, shell redirect), use
   `--stdout` or `-o -` — same pretty `ChainHead` JSON, no SUCCESS banner, no
   file created. Still prefer a **file** retained off-machine for long-term
   checkpoint hygiene:

   ```powershell
   ledgerful export head --stdout > .\checkpoints\head.json
   ```

2. **Copy the file off this machine** (USB, object store, another host, CI
   artifact). Do **not** keep the only copy under `.ledgerful/` on the same disk.

3. Continue normal work (commits advance the chain).

4. **Verify against the retained checkpoint** (default = ancestor / prefix):

   ```powershell
   ledgerful verify --signatures --against-export .\checkpoints\head.json
   ```

   Exit **0** if the live chain **extends or equals** the retained head
   (genesis matches; hash at `export.length` matches `latest_entry_hash`).

5. Optionally re-export a newer checkpoint on a cadence that matches your risk
   tolerance.

## Checkpoint vs `--exact`

| Mode | Flag | Pass when |
|---|---|---|
| **Checkpoint** (default) | (none) | Live chain extends or equals the export head |
| **Exact** | `--exact` | Live latest hash, genesis, and length equal the export (freeze / forensic snapshot) |

Legitimate advance past a weekly export **passes** checkpoint and **fails**
`--exact`. Golden-path demos that export then immediately verify still pass both.

## Loader formats

`--against-export <path>` accepts:

- A **SOC2 evidence zip** containing `chain_head.json`
- A **bare JSON** file (`export head` output / extracted `chain_head.json`)

## Public head for Ledgerful itself (Tier C)

For **Ledgerful’s own** development ledger, a CI-anchored thin head is published at:

`https://www.ledgerful.dev/ledger/chain_head.json`

There is **no** `verify --against-url` (egress/SSRF surface). Compose download then
local path verify:

```powershell
curl -fsSL -o head.json https://www.ledgerful.dev/ledger/chain_head.json
ledgerful verify --signatures --against-export .\head.json
```

**What this means:** the **local workspace ledger** must **extend or equal** the
downloaded public checkpoint (same genesis; hash at public `length` matches
`latest_entry_hash`). It detects **local rollback/rewrite relative to the
published head** — not “the public site proves your private product history.”

Customer / other repos still need **Tier B** off-machine retention of **their**
heads (`export head` + USB/object store/peer host — track 0119). Public always-on
publish for Ledgerful-itself is this URL (track **0120**); it does **not** replace
operator retention for arbitrary customer workspaces.

Full redacted public bundle (entries + manifest + verifier): see
`docs/public-ledger.md` and `https://www.ledgerful.dev/ledger/`.

## Honesty

- Detection requires a head retained **outside** the compromised machine.
- Signing-key compromise + re-sign remains a separate ceiling.
- Team-sync peers share trust assumptions and are **not** a substitute for
  offline retention.
- Public publish for Ledgerful-itself is a static CI-anchored artifact, not
  Rekor/CT multi-party logging and not universal retention for every repo that
  uses Ledgerful.
- See GitHub issue #6 for the original limitation (Tier B = 0119; Tier C = 0120).

Further reading: `docs/Features.md` (Chain Integrity), `docs/golden-path.md`,
`docs/public-ledger.md`.
