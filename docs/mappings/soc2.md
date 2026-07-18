# SOC 2 Control-Evidence Mapping

Ledgerful can optionally scope its SOC2 evidence export to a control lens.
The mapping lives in `mappings/soc2.toml` and is embedded at compile time.
It is a mapping aid, **not** a certification or compliance attestation.

## Using the control lens

```bash
ledgerful export evidence --profile soc2 --control CC8.1 --control "CC7.*" --out soc2-cc8.zip
```

* `--control` is repeatable.
* Family wildcards are supported: `CC7.*` matches every control whose ID starts with `CC7.`.
* Unknown or empty selectors are rejected with a clear error.
* The export always contains the full signed evidence bundle; the control lens only adds `control-lens/cover.md` and `control-lens/index.json`.

## Mapping file format

The TOML file has two top-level sections:

```toml
[meta]
framework = "soc2"
version = "1"
disclaimer = "..."

[[control]]
id = "CC8.1"
title = "Change Management"
evidence = ["signed_ledger_entry", "verification_result", "risk_impact_analysis", "tamper_evident_chain"]
provenance = "..."
limit = "..."
```

Each control entry declares which Ledgerful evidence keywords it wants, why that evidence supports the control (`provenance`), and what the evidence does **not** prove (`limit`).

## Default controls

| ID | Title | Evidence keywords |
|---|---|---|
| CC8.1 | Change Management | `signed_ledger_entry`, `verification_result`, `risk_impact_analysis`, `tamper_evident_chain` |
| CC3.4 | Assessing Changes to Internal Control | `risk_score`, `blast_radius`, `impact_analysis` |
| CC7.1 | Detect Configuration Changes / New Vulnerabilities | `scan_impact`, `config_diff`, `security_surface_diff` |
| CC7.2 | Anomaly Monitoring | `hotspots`, `temporal_couplings`, `drift_detection` |
| CC6.8 | Detect Unauthorized / Altered Software | `signature_verification`, `no_unsigned_entries_gate`, `verify_command` |
| CC4.1 | Ongoing Evaluation of Controls | `continuous_verification_runs`, `drift_reconciliation` |

## Evidence keyword semantics

The mapping engine matches each keyword against ledger entry fields:

* `signed_ledger_entry` — entry has a non-empty `signature`.
* `verification_result` — entry has a non-empty `verification_status` / `verification_basis`.
* `risk_score` / `risk_impact_analysis` — entry has a non-empty `risk` field.
* `tamper_evident_chain` — entry has a non-empty `prev_hash` (chain continuity).
* `blast_radius` / `impact_analysis` / `scan_impact` — entry has a non-empty `risk` field.
* `config_diff`, `security_surface_diff`, `hotspots`, `temporal_couplings`, `drift_detection`, `signature_verification`, `no_unsigned_entries_gate`, `verify_command`, `continuous_verification_runs`, `drift_reconciliation` — currently match entries with verification or risk metadata; future releases will narrow these to dedicated fields.

## Important limitations

* This mapping is a **default starting point** based on the AICPA 2017 Trust Services Criteria and 2022 Points of Focus.
* Every customer environment and auditor interprets controls differently. Validate and customize `mappings/soc2.toml` before relying on it for an audit.
* The tool produces evidence; the auditor renders the opinion.
* The signed bundle itself is unchanged by `--control`. Only the additive `control-lens/` files are added.
