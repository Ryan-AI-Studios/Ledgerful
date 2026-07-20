# Package distribution

How Ledgerful reaches package managers. Distribution only — no engine runtime, signing-basis, or network-posture changes.

## Channels

| Channel | Status | Notes |
|---|---|---|
| One-line installer (`install/install.{ps1,sh}`) | Live | Downloads release zip/tar.gz + checksum verify |
| `cargo binstall --git …` | Engine-ready | `[package.metadata.binstall]` in `Cargo.toml`; uses release assets |
| Homebrew tap | Templates + bump automation in-engine; tap repo seeded separately | Formula (CLI), not cask; tap-first (not homebrew-core) |
| Scoop bucket | Templates + bump automation in-engine; bucket repo seeded separately | 64-bit portable `.zip` only |
| winget (`Ledgerful.Ledgerful`) | CI job wired; first submission is manual | Subsequent bumps via SHA-pinned `winget-releaser` |
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

## Bump automation

On each release, job `bump-manifests` (after `publish`):

1. `gh release download` of `*.sha256` for the tag
2. `scripts/bump-manifests.sh --version … --checksums-dir …` (always — validates script)
3. If secret `MANIFEST_PUSH_TOKEN` is set, commit + push:
   - `Ryan-AI-Studios/homebrew-tap` → `ledgerful.rb`
   - `Ryan-AI-Studios/scoop-bucket` → `ledgerful.json`
4. If the secret is empty: print skip; dry-run/validation still must pass

**Invariant:** the bump script reads hashes **only** from published `.sha256` files. It never recomputes hashes from archives.

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
