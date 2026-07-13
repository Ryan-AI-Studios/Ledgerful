# Security Policy

## Supported Versions

Ledgerful is pre-1.0 software. Security fixes are applied to the latest `main`
branch and the most recent tagged release.

| Version | Supported |
|---------|----------|
| latest `main` | yes |
| latest tag (currently `v0.1.8`) | yes |
| older tags | no |

When a release branch policy is introduced, this table will be updated.

## Reporting a Vulnerability

**Thank you for responsible disclosure.** We take security reports seriously and
will respond as quickly as we can.

### How to report

Choose **one** of these channels:

1. **Email** (preferred): send your report to **security@ledgerful.dev**.
2. **GitHub Private Vulnerability Reporting:** open the repo's **Security tab â†’
   "Report a vulnerability"**. This uses GitHub's built-in private advisory
   channel.

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
attachment-heavy messages â€” your report may vanish without notice. Instead,
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

- The Ledgerful engine (`src/`) â€” CLI commands, web daemon, MCP server, ledger
  validation/process runner, sync bundle handling.
- The Ledgerful GitHub Action.
- The Ledgerful web frontend (`ledgerful-frontend`) and marketing site
  (`ledgerful-web`) â€” XSS, auth bypass, data exposure.
- Cryptographic operations (Ed25519 device keys, ChaCha20-Poly1305 AEAD,
  Argon2id KDF, session tokens).

**Out of scope:**

- Vulnerabilities in third-party dependencies â€” report those to the upstream
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

## Supply-chain posture

> **Status:** The signing and attestation pipeline is active in
> `.github/workflows/release.yml`. The v0.1.8 release was built with cosign
> keyless signing, SLSA build provenance, SBOM attestation, and `cargo auditable`
> embedded dependency lists. GitHub artifact attestations (SLSA provenance + SBOM
> attestation) are active for the public repository.

Ledgerful releases are **signed and attested** so a downloaded binary can be
verified without trusting the release page alone. This section describes what is
covered, the exact commands to run, and the honest gaps.

### What is signed and attested

- **Engine SBOM** (`ledgerful-<version>.cdx.json`): a CycloneDX bill of
  materials generated from the Cargo lockfile with `--all-features`. It is
  signed with cosign keyless and attested via GitHub artifact attestation.
- **MCP npm package SBOM** (`ledgerful-mcp-server-<version>.cdx.json`):
  CycloneDX output from `npm sbom`, signed with cosign keyless.
- **Release archives** (`ledgerful-<target>.tar.gz` / `.zip`): signed with
  cosign keyless (Sigstore Fulcio, GitHub Actions OIDC identity) and carry a
  SHA-256 checksum. SLSA build provenance is emitted inside each matrix build
  job by GitHub artifact attestation.
- **Embedded dependency list**: release binaries are built with `cargo auditable`
  so the dependency graph is embedded in the binary and can be inspected offline
  with `cargo audit bin <path>`.

Artifact signing is **independent** of the product's ledger signing. The
Ed25519 ledger signing basis in `src/ledger/crypto.rs` is unchanged; artifact
signing proves provenance of the download, not validity of ledger transactions.

### Verification commands

Replace `<version>` with the release tag (e.g. `v0.1.8`) and `<target>` with the release values.

#### cosign keyless signature

```bash
cosign verify-blob \
  --bundle ledgerful-<target>.tar.gz.bundle \
  --certificate-identity 'https://github.com/Ryan-AI-Studios/Ledgerful/.github/workflows/release.yml@refs/tags/<version>' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ledgerful-<target>.tar.gz
```

Use the same pattern for `.zip` archives and `.cdx.json` SBOM files.

#### GitHub artifact attestation (binary provenance + SBOM)

```bash
gh attestation verify ledgerful-<target>.tar.gz --owner Ryan-AI-Studios
gh attestation verify ledgerful-mcp-server-<version>.tgz --owner Ryan-AI-Studios
```

Attestation requires the repository to be public or on GitHub Enterprise Cloud.
The repository is public, so GitHub attestations are active for all release
artifacts. Verifying the release archive also verifies any SBOM attestations
bound to it (the SBOM is the attestation predicate).

#### Embedded dependency list

```bash
cargo audit bin ledgerful
```

This works offline because `cargo-auditable` embeds the dependency graph in the
binary itself.

### Honest gaps

- **`cozo-redux` is a git dependency** (pinned by rev, not a registry version).
  The SBOM records the git URL + commit, but downstream vulnerability scanners
  that match by crates.io coordinates will not CVE-match it automatically.
- **Native C code is not enumerated as its own component.** `rusqlite` with the
  `bundled` feature and `cozo` statically link a vendored SQLite / native C
  library. The SBOM lists `libsqlite3-sys` as a Rust crate, not the vendored C
  library as a separate component.
- **macOS codesign / notarization is out of scope.** Sigstore/SLSA attestation
  is not a substitute for Apple Gatekeeper notarization, which is tracked
  separately.
- These artifacts are **signed and attested**, not tamper-proof, immutable, or
  blockchain-grade. Verification reduces trust assumptions; it does not remove
  them.

## Coordination and Disclosure

- We prefer **coordinated disclosure** â€” work with us to fix the issue before
  publishing details.
- We will credit researchers in release notes (with permission) unless
  anonymity is requested.
- If we are unresponsive past the response targets above, or if 90 days have
  elapsed since your initial report, you may publish your findings.
- We will publish fixed vulnerabilities in release notes and, for significant
  issues, a dedicated security advisory on GitHub.
