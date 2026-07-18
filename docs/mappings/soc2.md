# SOC 2 Control-Evidence Mapping

Ledgerful can optionally scope its SOC2 evidence export to a control lens.
The mapping lives in `mappings/soc2.toml` and is embedded at compile time.
This mapping identifies evidence a control assessment typically wants. It is NOT a certification or compliance attestation. The tool produces audit-ready evidence; the customer's auditor maps it to controls and renders an opinion. This is a mapping aid, NOT a certification or compliance attestation.

## Using the control lens

```bash
ledgerful export evidence --profile soc2 --control CC8.1 --control "CC7.*" --out soc2-cc8.zip
```

* `--control` is repeatable.
* Family wildcards are supported: `CC7.*` matches every control whose ID starts with `CC7.`.
* Unknown or empty selectors are rejected with a clear error.
* The export always contains the full signed evidence bundle; the control lens only adds `control-lens/cover.md` and `control-lens/index.json`, and the manifest/sig are regenerated to cover them.

## Mapping file format

The TOML file has two top-level sections:

```toml
[meta]
framework = "soc2"
version = "1"
source = "2017 AICPA Trust Services Criteria, Revised Points of Focus (2022)"
disclaimer = "This mapping identifies evidence a control assessment typically wants. It is NOT a certification or compliance attestation. The tool produces audit-ready evidence; the customer's auditor maps it to controls and renders an opinion."
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

## Per-entry evidence keywords

These keywords return `true` for an individual ledger entry when the described predicate is satisfied.

* `signed_ledger_entry` — entry has a non-blank `signature` and any present `public_key` is non-blank.
* `signature_verification` — entry has a non-blank `signature` AND the signature verifies against the entry's `public_key` using the 5-field signing basis (tx_id, category, summary, reason, committed_at).
* `verification_result` — entry has a non-empty `verification_status`.
* `continuous_verification_runs` — entry has a non-empty `verification_status`.
* `risk_score` — entry has a non-empty `risk` field.
* `risk_impact_analysis` — entry has a non-empty `risk` field.
* `blast_radius` — entry category is Feature, Architecture, Bugfix, or Refactor.
* `impact_analysis` — entry category is Feature, Architecture, Bugfix, or Refactor.

## Framework-wide evidence keywords

These keywords represent bundle/system-level evidence rather than per-entry predicates, so the per-entry matcher returns `false` for all individual entries.

* `tamper_evident_chain` — the tamper-evident chain covers all entries; the chain as a whole is the evidence, not individual entries. Every entry is included because removing any entry would break continuity. `chain_head.json` is included in the bundle when a chain head exists.
* `scan_impact` — Framework-level evidence category (not included in this bundle — produced by `ledgerful scan --impact`).
* `config_diff` — Framework-level evidence category (not included in this bundle — produced by `ledgerful config diff`).
* `security_surface_diff` — Framework-level evidence category (not included in this bundle — produced by `ledgerful security surface diff`).
* `hotspots` — Framework-level evidence category (not included in this bundle — produced by `ledgerful hotspots`).
* `temporal_couplings` — Framework-level evidence category (not included in this bundle — produced by `ledgerful temporal couplings`).
* `drift_detection` — Framework-level evidence category (not included in this bundle — produced by `ledgerful drift detect`).
* `no_unsigned_entries_gate` — Ledgerful capability (not a bundle artifact); enforced across the whole ledger / export bundle.
* `verify_command` — Ledgerful capability (not a bundle artifact); evidence produced by running `ledgerful verify` over the bundle.
* `drift_reconciliation` — Framework-level evidence category (not included in this bundle — produced by `ledgerful drift reconcile`).

## Control provenance and honest limits

### CC8.1 — Change Management

* **Provenance:** Signed ledger entry per change (category, intent/reason, committed_at) = documentation; verification-run outcome (tests_run, pass/fail) = tested; risk/blast-radius = impact-assessed; tamper-evident chain = integrity/continuity of the presented chain, with an independently retained head needed for rollback detection.
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
* **Honest limit:** Evidence of verification runs and drift reconciliation across the observation period; does not by itself establish Type II operating-period strength or validate cadence/observation window.

## Important limitations

* This mapping is a **default starting point** based on the 2017 AICPA Trust Services Criteria, Revised Points of Focus (2022).
* Every customer environment and auditor interprets controls differently. Validate and customize `mappings/soc2.toml` before relying on it for an audit.
* The tool produces evidence; the auditor renders the opinion.
* Existing evidence payloads (ledger.csv, verification_history.csv, adr/*.md, chain_head.json) are preserved byte-identical. The manifest.json and manifest.sig files are regenerated so their signature covers the additive lens files.
