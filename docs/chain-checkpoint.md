# Chain-head checkpoint retention

Local `ledgerful verify --signatures --chain` proves integrity of the **presented**
chain. It cannot detect a full rollback if an adversary controls the only copy of
the database **and** the signed chain head. Independent retention of a prior head
closes that gap for operators.

This is **not** local immutability, Rekor/CT, or a public transparency log.

## 5-step recipe

1. **Export a thin head** (or a full SOC2 zip — both work):

   ```powershell
   ledgerful export head --out .\checkpoints\head.json
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

## Remote URL (compose with curl)

There is no `verify --against-url` (egress/SSRF surface). Compose:

```powershell
curl -fsSL -o head.json https://example.invalid/ledgerful-chain-head.json
ledgerful verify --signatures --against-export .\head.json
```

## Honesty

- Detection requires a head retained **outside** the compromised machine.
- Signing-key compromise + re-sign remains a separate ceiling.
- Team-sync peers share trust assumptions and are **not** a substitute for
  offline retention.
- See GitHub issue #6 for the original limitation; public always-on publish for
  Ledgerful itself is a separate track (Tier C).

Further reading: `docs/Features.md` (Chain Integrity), `docs/golden-path.md`.
