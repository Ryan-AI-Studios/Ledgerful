# Security Policy

## Supported Versions

Ledgerful is pre-1.0 software. Security fixes are applied to the latest `main`
branch and the most recent tagged release.

| Version | Supported |
|---------|----------|
| latest `main` | yes |
| latest tag (currently `v0.1.6`) | yes |
| older tags | no |

When a release branch policy is introduced, this table will be updated.

## Reporting a Vulnerability

**Thank you for responsible disclosure.** We take security reports seriously and
will respond as quickly as we can.

### How to report

Choose **one** of these channels:

1. **Email** (preferred): send your report to **security@ledgerful.dev**.
   - *Provisioning note:* this mailbox is **pending activation** — Cloudflare
     Email Routing must be configured by the maintainer. Until it is live, use
     GitHub private vulnerability reporting (below) or contact the maintainer
     directly via a public GitHub issue referencing "security" (do not include
     vulnerability details in the public issue).
2. **GitHub Private Vulnerability Reporting:** open the repo's **Security tab →
   "Report a vulnerability"**. This uses GitHub's built-in private advisory
   channel (available once the repo is public). This is the most reliable
   channel while the email mailbox is being provisioned.

### What to include

Please include as much of the following as you can:

- A clear description of the vulnerability and its potential impact.
- The affected component (engine CLI, web daemon, MCP server, GitHub Action,
  sync bundle handling, etc.) and the version/commit you tested.
- Steps to reproduce, or a proof-of-concept.
- Any suggested remediation if you have one.

### Large or complex PoCs

**Do not attach large files, binaries, videos, or scripts directly to your
email.** Cloudflare Email Routing silently drops oversized or
attachment-heavy messages — your report may vanish without notice. Instead,
upload the PoC to a cloud storage service (Google Drive, Dropbox, S3, etc.)
with an **unlisted or access-controlled link**, and include the link in your
report body. For GitHub private reports, attach files directly (GitHub does not
have the same email-routing limitation).

### Encryption (PGP)

PGP-encrypted reports are **not currently supported**. This is a deliberate
choice: we do not yet have a published PGP key, and offering encryption without a
verifiable key would be worse than no encryption. If encrypted reporting
becomes important for your report, contact us through one of the channels above
and we will arrange a secure channel.

## Response Expectations

| Stage | Target |
|-------|--------|
| Acknowledgement of receipt | within **3 business days** |
| Initial triage + severity assessment | within **7 business days** |
| Fix or mitigation for high-severity issues | within **30 days** (target; may be longer for complex fixes) |
| Fix or mitigation for medium/low issues | next release cycle |
| Coordinated public disclosure | after a fix is released, or **90 days** from initial report (whichever is sooner), unless an extension is mutually agreed |

These are **targets, not guarantees.** Ledgerful is maintained by a small team.
If you need a status update during the process, follow up via the channel you
used to report.

## Scope

**In scope:**

- The Ledgerful engine (`src/`) — CLI commands, web daemon, MCP server, ledger
  validation/process runner, sync bundle handling.
- The Ledgerful GitHub Action.
- The Ledgerful web frontend (`ledgerful-frontend`) and marketing site
  (`ledgerful-web`) — XSS, auth bypass, data exposure.
- Cryptographic operations (Ed25519 device keys, ChaCha20-Poly1305 AEAD,
  Argon2id KDF, session tokens).

**Out of scope:**

- Vulnerabilities in third-party dependencies — report those to the upstream
  maintainer. We track dependency advisories via `cargo audit` and `cargo deny`
  and will update affected deps, but the upstream issue is theirs.
- Self-inflicted issues from running the daemon bound to a non-loopback
  interface (the daemon binds to `127.0.0.1` by default; exposing it is a user
  configuration error).
- Attacks requiring physical access to the user's machine.
- Social engineering, phishing, or attacks against the maintainer's personal
  accounts.
- Denial of service against the local loopback daemon from the local machine
  (it is a single-user local tool; local DoS is not a meaningful threat model).
- Automated scanner reports without a working proof-of-concept.
- Theoretical vulnerabilities without a demonstrated attack path.

## Safe Harbor

> **DRAFT — pending counsel review.** Ledgerful, LLC is not yet formed. The
> following safe-harbor statement is based on standardized disclose.io-style
> language and is provided in draft form. It will be finalized when legal
> counsel reviews it alongside the LICENSE, consistent with the launch gates.
> Until then, researchers should rely on the statement in principle but
> understand it has not been formally adopted.

Ledgerful supports responsible security research. To encourage good-faith
security research, we commit to the following:

- **Good-faith research is authorized.** Researchers who discover and report
  vulnerabilities in accordance with this policy, acting in good faith and
  without malicious intent, are welcome to do so. We will not pursue legal
  action against researchers who:
  - Respect the scope defined above.
  - Avoid privacy destruction, disruption of services, and degradation of user
    experience.
  - Do not access or modify data that does not belong to them.
  - Provide reasonable time for remediation before public disclosure.
- **No CFAA claims.** We will not invoke the Computer Fraud and Abuse Act
  (CFAA) or equivalent computer-misuse statutes against good-faith researchers
  acting within this policy.
- **No DMCA anti-circumvention claims.** We will not invoke the Digital
  Millennium Copyright Act (DMCA) anti-circumvention provisions against
  good-faith security research conducted within this policy.
- **No bug bounty.** Ledgerful does not currently offer a paid bug bounty
  program. We are grateful for responsible reports and will acknowledge
  contributors in release notes (with permission) unless they prefer to remain
  anonymous.

This safe-harbor statement does not override applicable law. Researchers are
responsible for ensuring their activities comply with all applicable laws and
regulations.

## Coordination and Disclosure

- We prefer **coordinated disclosure** — work with us to fix the issue before
  publishing details.
- We will credit researchers in release notes (with permission) unless
  anonymity is requested.
- If we are unresponsive past the response targets above, or if 90 days have
  elapsed since your initial report, you may publish your findings.
- We will publish fixed vulnerabilities in release notes and, for significant
  issues, a dedicated security advisory on GitHub.