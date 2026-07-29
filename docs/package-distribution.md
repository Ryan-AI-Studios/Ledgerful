# Package distribution

How Ledgerful reaches package managers. Distribution only — no engine runtime, signing-basis, or network-posture changes.

## Channels

| Channel | Status | Notes |
|---|---|---|
| One-line installer (`install/install.{ps1,sh}`) | Live | Downloads release zip/tar.gz + checksum verify |
| `cargo binstall --git …` | Engine-ready | `[package.metadata.binstall]` in `Cargo.toml`; uses release assets |
| Homebrew tap | Live | Formula (CLI), not cask; tap-first (not homebrew-core); auto-bumped on each release |
| Scoop bucket | Live | 64-bit portable `.zip` only; auto-bumped on each release |
| winget (`Ledgerful.Ledgerful`) | Pending Microsoft review | First submission PR open; subsequent bumps via SHA-pinned `winget-releaser` |
| crates.io `cargo install ledgerful` | **Not pursued** for distribution | Heavy native graph; prebuilt path preferred |

## Release artifacts (canonical)

Published by `.github/workflows/release.yml` on tags `v*`:

- `ledgerful-x86_64-pc-windows-msvc.zip` — portable zip; binary at archive root
- `ledgerful-x86_64-unknown-linux-gnu.tar.gz` — nested `ledgerful-{target}/ledgerful`
- `ledgerful-x86_64-apple-darwin.tar.gz` — nested binary
- `ledgerful-aarch64-apple-darwin.tar.gz` — nested binary
- Matching `*.sha256` sidecars (authoritative hashes for manifests)

URL scheme:

```text
https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v{VERSION}/{name}-{target}.{tar.gz|zip}
```

## In-engine packaging layout

```text
packaging/
  homebrew/ledgerful.rb   # formula template (version + per-arch sha256)
  scoop/ledgerful.json    # scoop manifest (64bit zip + autoupdate)
scripts/
  bump-manifests.ps1      # Primary local path on Windows (pwsh)
  bump-manifests.sh       # release CI + macOS (Bash 3.2+ compatible)
tests/fixtures/package-manifests/v0.1.8/
  *.sha256                # real published v0.1.8 hashes (fixture)
```

**Script runtimes:** On Windows, use `pwsh -File scripts/bump-manifests.ps1` as the primary local path. `scripts/bump-manifests.sh` is **Bash 3.2+** compatible (macOS `/bin/bash`) and is the path used by Ubuntu release CI.

## Release gates (0098)

Two gates keep half-executed releases from going unnoticed:

| Gate | Script / workflow | When it fails |
|---|---|---|
| **A** (preflight) | `scripts/check-release-tag.sh` as first job in `release.yml` | Tag disagrees with `Cargo.toml` version, dated CHANGELOG section, or `mcp-server` `ledgerfulEngineTag` — **before any build** |
| **B** (drift) | `scripts/check-release-state.sh` via `release-state.yml` (weekday schedule + `workflow_dispatch`) | `Cargo.toml` version has a dated CHANGELOG section but no matching remote tag |

Gate B is scheduled, not a PR gate — a short red window between a merged release commit and the tag push is expected. **Where to look for a red run:** Actions → *Release state*. A scheduled red has no pager; someone must watch that workflow.

**Schedule operational facts** (normal, not bugs):

- Delivery can **lag under load** (GitHub high-load includes the start of every hour — the cron is non-top-of-hour for that reason).
- A **missed day is harmless**: the next run sees a larger `[Unreleased]` (or the same half-executed state if still untagged).
- On a **public** repo, scheduled workflows **auto-disable after ~60 days of repository inactivity**. Re-enable with a `workflow_dispatch` run or any push.
- Parser unit matrix: `bash scripts/test-release-changelog.sh` (also run as the first step of `release-state.yml`).

`ci.yml` / `smoke.yml` frontend `ref:` drift is **reported only** by Gate B (warnings do not fail the job). Do not refresh those pins without resolving the Node 24 + Linux CSP gap (see track 0098 / `deferred.md`).

### Ops recovery — re-run a release without retagging

```bash
gh workflow run release.yml --ref vX.Y.Z
```

`GITHUB_REF_NAME` becomes the tag for Gate A and every `${GITHUB_REF_NAME#v}` step. There is **no** separate `tag` input (removed 0098 — a second source of truth would fight the preflight). Optional `frontend_ref` pins the embedded SPA for a recovery rebuild; empty resolves `ledgerful-frontend` `main` at run time (SHA is recorded in the release body).

### Post-publish smoke

- `verify-assets` (`needs: publish`): expected archives / checksums / SBOMs / signatures exist; downloads the Linux tarball and asserts `./ledgerful --version` matches the tag.
- `verify-manifests` (`needs: bump-manifests`): live `gh api` read of `homebrew-tap/ledgerful.rb` and `scoop-bucket/ledgerful.json` (not the job's own `bumped/` output).

## Bump automation

On each release, job `bump-manifests` (after `publish`):

1. `gh release download` of `*.sha256` for the tag
2. `scripts/bump-manifests.sh --version … --checksums-dir …` (always — validates script)
3. `scripts/require-secret.sh MANIFEST_PUSH_TOKEN` — **hard-fails** if the secret is empty (0098; was silent `exit 0`)
4. On success, commit + push:
   - `Ryan-AI-Studios/homebrew-tap` → `ledgerful.rb`
   - `Ryan-AI-Studios/scoop-bucket` → `ledgerful.json`
5. Step summary names both repos

**Failure after publish is loud and recoverable by design** — a published release with unbumped manifests can be fixed by re-running the push (or a hand `bump-manifests` + push). Do not change the hard-fail back to `exit 0`.

**Invariant:** the bump script reads hashes **only** from published `.sha256` files. It never recomputes hashes from archives.

`WINGET_TOKEN` remains an intentional skip when unset (`::notice::` + step summary); that is deliberate until `Ledgerful.Ledgerful` exists on winget-pkgs.

### Local / CI fixture test

```powershell
pwsh -File scripts/bump-manifests.ps1 `
  -Version 0.1.8 `
  -ChecksumsDir tests/fixtures/package-manifests/v0.1.8 `
  -PackagingDir packaging `
  -OutDir $env:TEMP\bump-out

cargo nextest run --test integration -E 'test(bump_manifests)'
```

```bash
scripts/bump-manifests.sh \
  --version 0.1.8 \
  --checksums-dir tests/fixtures/package-manifests/v0.1.8 \
  --packaging-dir packaging \
  --out-dir /tmp/bump-out
```

## winget

- Identifier: `Ledgerful.Ledgerful`
- Action: `vedantmgoyal9/winget-releaser@4ffc7888bffd451b357355dc214d43bb9f23917e` (tag v2, SHA-pinned)
- Installer regex: portable `ledgerful-x86_64-pc-windows-msvc.zip`
- Secret: `WINGET_TOKEN` (PAT that can open PRs against `microsoft/winget-pkgs` via fork)
- **First-time package:** the action requires ≥1 version already in winget-pkgs. Bootstrap with `wingetcreate` / a manual PR; subsequent tags use this job when `WINGET_TOKEN` is set.

## Secrets checklist

| Secret | Used by | Purpose |
|---|---|---|
| `MANIFEST_PUSH_TOKEN` | `bump-manifests` | Push formula/manifest to homebrew-tap + scoop-bucket |
| `WINGET_TOKEN` | `winget-release` | Submit winget-pkgs update PR |
| `GITHUB_TOKEN` | release download of checksums | Default; contents read on public releases |

## cargo-binstall metadata

See `Cargo.toml` `[package.metadata.binstall]` (+ Windows zip override). Template variables: `{ repo }`, `{ version }`, `{ name }`, `{ target }`, `{ bin }`, `{ binary-ext }`. `disabled-strategies = ["quick-install"]` keeps compile as fallback without third-party quickinstall mirrors.

### DoD-4b verification

Live smoke (prebuilt path, compile disabled) on Windows x86_64 against published `v0.1.8`:

```powershell
cargo binstall --manifest-path Cargo.toml --version 0.1.8 `
  --install-path $env:TEMP\ledgerful-binstall-smoke --force --no-confirm `
  --disable-strategies compile,quick-install ledgerful
& "$env:TEMP\ledgerful-binstall-smoke\ledgerful.exe" --version
# → ledgerful 0.1.8  (downloaded from github.com, not compiled)
```

After this metadata lands on the default branch, the one-liner is:

```bash
cargo binstall --git https://github.com/Ryan-AI-Studios/Ledgerful
```

CI regression: `tests/integration/binstall_metadata.rs` locks the template shape to the release archive layout.

## Explicit non-goals

- homebrew-core submission (later optional)
- Linux distro packages (apt/dnf/AUR/nix)
- crates.io publish for install UX
- Changing release signing (cosign) or ledger crypto
