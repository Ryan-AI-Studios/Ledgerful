## Release notes

> Reference template for drafting release notes. Copy this into the GitHub
> Release body, replacing `<version>` (e.g. `v0.1.7`) and `<target>` (e.g.
> `ledgerful-x86_64-unknown-linux-gnu`) placeholders with the actual release
> values. Not used as an automated `body_path` (placeholders would be
> published literally).

### Verify this release

Ledgerful releases are signed and attested so a downloaded binary can be checked
without trusting the release page alone.

#### cosign keyless signature

```bash
cosign verify-blob \
  --bundle ledgerful-<target>.tar.gz.bundle \
  --certificate-identity 'https://github.com/Ryan-AI-Studios/Ledgerful/.github/workflows/release.yml@refs/tags/<version>' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ledgerful-<target>.tar.gz
```

Use the same pattern for `.zip` archives and for `.cdx.json` SBOM files.

#### GitHub artifact attestation

```bash
gh attestation verify ledgerful-<target>.tar.gz --owner Ryan-AI-Studios
gh attestation verify ledgerful-mcp-server-<version>.tgz --owner Ryan-AI-Studios
```

Attestation requires the repository to be public or on GitHub Enterprise Cloud.
Verifying the archive also verifies SBOM attestations bound to it.

#### Embedded dependency list

```bash
cargo audit bin ledgerful
```

### Honest gaps

- `cozo-redux` is a git dependency; scanners matching crates.io coordinates will not CVE-match it automatically.
- Native C code (vendored SQLite via `rusqlite`/`cozo`) is not enumerated as its own SBOM component.
- These artifacts are signed and attested, not tamper-proof, immutable, or blockchain-grade.
