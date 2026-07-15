# AI-Code Adversarial Review Protocol

> **Purpose:** The Ledgerful codebase is entirely AI-generated. Research shows AI-generated code
> carries an elevated vulnerability base rate that automated scanners (SAST/secret-scan) miss
> ~98% of the time. This protocol is the **high-value manual gate** that compensates.

## When this review is required

- **Every PR** that touches security-sensitive code (crypto, auth, daemon API, path handling,
  deserialization, dependency manifests, CI/CD workflows).
- **Before merging** any change to: `src/ledger/crypto.rs`, `src/sync/crypto.rs`, `src/commands/web/`,
  `src/sync/bundle.rs`, MCP tool inputs, `deny.toml`, `Cargo.toml`,
  `package.json`, or any `.github/workflows/` file.
- **On every dependency add** (new crate or npm package) — the §3 provenance check must run.
- **Standing rule:** security-sensitive code is not iterated by AI across multiple rounds
  without a human or cross-model review between iterations (iterative AI "fixes" measurably
  *add* vulnerabilities).

## What to review (by surface)

### Crypto / auth (`src/ledger/crypto.rs`, `src/sync/crypto.rs`, session-token code, `src/commands/web/auth.rs`)
- Ed25519 signing/verification uses the vetted crate API correctly; no custom signature logic.
- ChaCha20-Poly1305 AEAD: unique nonces per message (no reuse), AAD covers what it must,
  decryption failures are hard-rejected (no partial-plaintext use).
- Argon2id parameters meet current OWASP guidance (memory/iterations/parallelism).
- Session token: generated from a CSPRNG, sufficient entropy, compared in **constant time**
  (`subtle`), never logged.

### Validator / process runner (shell-execution path)
- `{entity}` and any user/config-derived substitution cannot inject shell commands; arguments
  are passed as an argv array, not a shell string, wherever possible.
- `ProcessPolicy` is enforced (allowed executables, no arbitrary command escalation).
- Timeouts and resource bounds hold; failure modes are explicit.

### Path handling
- Path normalization rejects traversal (`..`), absolute-path escapes, and symlink escapes
  from the intended repo/state root. Windows + POSIX both covered.

### Deserialization / untrusted input (`src/sync/bundle.rs`, daemon JSON, MCP stdio, Action inputs)
- Peer sync bundles are signature-verified **before** deserialization is trusted.
- Malformed input cannot panic-crash or allocate unbounded memory.
- Daemon/API request bodies are size-limited and schema-validated.
- MCP tool inputs and GitHub Action inputs are treated as untrusted.

### Web / SSRF / secret-exposure (`ledgerful-web`, `ledgerful-frontend`)
- No service-role keys, Ed25519 private keys, daemon tokens, or `.env` reach the browser bundle
  (`NEXT_PUBLIC_*` audit).
- Telemetry uses only the official opt-in Supabase path.
- Mock data is never presented as live. `fallback.ts` must NOT convert 401/403 (or 4xx
  generally) into mock data — auth failures surface as errors; only 404 may map to an explicit
  empty state. Returned values carry data-source provenance (live / mock / stale / unavailable).

## How to review

1. **Cross-model review:** a different model than the author reviews the diff. The orchestrator
   delegates to a review subagent (e.g. `final-verifier`, `codex-review`, or equivalent
   cross-model tool).
2. **Owner merge decision:** the owner receives a plain-language summary of the cross-model
   findings, adjudicates any exceptions, and decides whether to merge. The owner is not expected
   to provide a code-level approval.
3. **Provenance check:** for any new dependency, verify it's a real, maintained, correctly-named
   package (run `scripts/slopsquat-sweep.ps1` or check the registry manually).

## Enforcement (CI gate)

Branch protection requires a pull request and the `ai-reviewed` status check before merge. Human
code-review approvals are not required (`required_approving_review_count = 0`); independent
adversarial AI review is the default technical approval gate. The owner retains the product,
risk-acceptance, and final merge decision. Required CI and security checks remain mandatory.

The `ai-reviewed` check is set by the orchestrator **only after** the cross-model review subagent
passes. The gate is:

- **Status check name:** `ai-reviewed`
- **Set by:** the orchestrator (manager agent) after the review subagent reports clean.
- **Implementation:** a GitHub Action workflow (`ai-review-gate.yml`) that creates a
  `pending` status on PR open, and the orchestrator pushes a `success` status via
  `gh api` when the review passes.

Passing AI review does not merge a PR. It confirms the technical review gate; the owner still
chooses whether and when the change is merged.

## Standing rules (every PR)

- Security-sensitive code is not iterated by AI across multiple rounds without a
  human/cross-model review between iterations.
- No AI-suggested dependency is merged without the §3 provenance check.
- SAST is a floor, not proof — this protocol is the higher-value gate.
