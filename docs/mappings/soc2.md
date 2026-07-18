# SOC 2 Control-Evidence Mapping

Ledgerful can optionally scope its SOC2 evidence export to a control lens.
The mapping lives in `mappings/soc2.toml` and is embedded at compile time.
It is a mapping aid, **not** a certification or compliance attestation. This mapping identifies evidence a control assessment typically wants. It is NOT a certification or compliance attestation. The tool produces audit-ready evidence; the customer's auditor maps it to controls and renders an opinion.

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
source = "2017 AICPA Trust Services Criteria, Revised Points of Focus (2022)"
disclaimer = "..."
status = "draft-pending-validation"

[[control]]
id = "CC8.1"
title = "Change Management"
evidence = ["signed_ledger_entry", "verification_result", "risk_impact_analysis", "tamper_evident_chain"]
provenance = "..."
limit = "..."
```

Status: draft-pending-validation.

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
* `verification_result` — entry has a non-empty `verification_status`.
* `risk_score` — entry has a non-empty `risk` field.
* `risk_impact_analysis` — entry has a non-empty `risk` field.
* `tamper_evident_chain` — the tamper-evident chain covers all entries; the chain as a whole is the evidence, not individual entries. Every entry is included because removing any entry would break continuity.
* `blast_radius` / `impact_analysis` / `scan_impact` — currently matched by change category (Feature, Architecture, Bugfix, Refactor) because per-entry risk/impact fields are not populated in the current schema; future releases will narrow these to dedicated fields when available.
* `config_diff`, `security_surface_diff`, `hotspots`, `temporal_couplings`, `drift_detection`, `signature_verification`, `no_unsigned_entries_gate`, `verify_command`, `continuous_verification_runs`, `drift_reconciliation` — currently match entries with verification or risk metadata; future releases will narrow these to dedicated fields.

## Control provenance and honest limits

### CC8.1 — Change Management

* **Provenance:** Signed ledger entry per change (category, intent/reason, committed_at) = documentation; verification-run outcome (tests_run, pass/fail) = tested; risk/blast-radius = impact-assessed; tamper-evident chain = complete & not backdated.
* **Honest limit:** Covers documentation, testing, risk-assessment, completeness. Does not by itself prove authorization/approval (who approved) — that comes from PR reviews / branch protection, which Ledgerful can reference but doesn't provide.

### CC3.4 — Assessing Changes to Internal Control

* **Provenance:** Per-change risk score + blast-radius + impact analysis from scan --impact.
* **Honest limit:** Assesses technical change impact; not enterprise-wide control-environment change.

### CC7.1 — Detect Configuration Changes / New Vulnerabilities

* **Provenance:** scan --impact, config diff, dependency/security-surface diff.
* **Honest limit:** Detects config/surface changes; actual CVE scanning is cargo audit/deny (adjacent, not this).

### CC7.2 — Anomaly Monitoring

* **Provenance:** Hotspots, temporal couplings above threshold, drift detection.
* **Honest limit:** Scoped to change-pattern anomalies — not runtime/SIEM security-event monitoring.

### CC6.8 — Detect Unauthorized / Altered Software

* **Provenance:** Signature verification, no-unsigned-entries gate, verify --signatures.
* **Honest limit:** Detects tampering with the record + unexpected code changes; not anti-malware.

### CC4.1 — Ongoing Evaluation of Controls

* **Provenance:** Continuous verification runs + drift reconciliation across the observation period.
* **Honest limit:** Evidence the change-control operated throughout the window (Type II strength).

## Important limitations

* This mapping is a **default starting point** based on the 2017 AICPA Trust Services Criteria, Revised Points of Focus (2022).
* Every customer environment and auditor interprets controls differently. Validate and customize `mappings/soc2.toml` before relying on it for an audit.
* The tool produces evidence; the auditor renders the opinion.
* The signed bundle itself is unchanged by `--control`. Only the additive `control-lens/` files are added.
