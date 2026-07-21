# Installing Ledgerful

Ledgerful is meant to be available as a normal CLI command, similar to `gh`.
The backward-compatible `ledgerful` command remains supported, and installs also provide `ledgerful` and `ldg`.
Once installed, AI agents and developers can run:

```bash
ledgerful doctor
ledgerful scan
ledgerful impact
ledgerful verify
```

## One-Line Install

Windows PowerShell:

```powershell
iwr https://raw.githubusercontent.com/Ryan-AI-Studios/Ledgerful/main/install/install.ps1 -UseB | iex
```

macOS or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Ryan-AI-Studios/Ledgerful/main/install/install.sh | sh
```

The installer tries to download a prebuilt GitHub release binary first. If no release asset exists for the platform, it falls back to `cargo install --git`.

## Package managers

### cargo-binstall (live when release assets match)

Ledgerful ships `[package.metadata.binstall]` in `Cargo.toml` mapping to the same GitHub release archives as the one-line installer. This installs a **prebuilt** binary (no full workspace compile) and does **not** require a crates.io publish:

```bash
cargo binstall --git https://github.com/Ryan-AI-Studios/Ledgerful
```

Requires [`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall) on the machine. If no matching prebuilt asset exists, binstall can fall back to compiling from source (quick-install mirrors are disabled in metadata).

### Homebrew

```bash
brew install Ryan-AI-Studios/tap/ledgerful
```

### Scoop

```powershell
scoop bucket add ledgerful https://github.com/Ryan-AI-Studios/scoop-bucket
scoop install ledgerful
```

### winget

winget is pending Microsoft review (PR open against `microsoft/winget-pkgs`). No install command is published yet. Until approved, use Homebrew, Scoop, the one-line installer, or `cargo binstall` above. Architecture and secrets: [package-distribution.md](package-distribution.md).

### macOS Gatekeeper / quarantine (interim)

Release macOS binaries are **not** currently Apple-codesigned or notarized. A plain Homebrew *formula* install usually avoids browser quarantine, but if Gatekeeper reports "developer cannot be verified" on first run of a downloaded binary:

```bash
xattr -d com.apple.quarantine "$(which ledgerful)"
```

The durable fix is codesign + notarize in the release pipeline; the `xattr` path is an interim workaround only.

## Requirements

For release binaries:

- `git` should be installed for normal Ledgerful operation.
- `gemini` is optional and only needed for `ledgerful ask`.

For source fallback:

- Rust/Cargo must be installed from <https://rustup.rs>.

## Install Location

Windows default:

```text
%USERPROFILE%\.ledgerful\bin
```

macOS/Linux default:

```text
~/.local/bin
```

The installer updates the user PATH when possible. Open a new terminal after installation if `ledgerful` is not immediately found.

## PATH / version FAQ (multiple install channels)

Ledgerful can land in more than one directory depending on how you install it (one-line installer, Homebrew, Scoop, `cargo install` / `cargo binstall`). When several copies exist, `PATH` order picks a winner — often a **stale shim** older than the release you just installed.

### Diagnose which binary runs

Check the resolved version first:

```bash
ledgerful --version
```

Then list every `ledgerful` on `PATH`:

**macOS / Linux:**

```bash
which -a ledgerful
type -a ledgerful
```

**Windows (PowerShell / cmd):**

```powershell
Get-Command ledgerful -All
where.exe ledgerful
```

Typical install directories (examples, not exhaustive):

| Channel | Common install location |
|---|---|
| One-line installer | `~/.local/bin` (Unix) · `%USERPROFILE%\.ledgerful\bin` (Windows) |
| Homebrew | Homebrew's `bin` (e.g. `/opt/homebrew/bin`, `/usr/local/bin`) |
| Scoop | Scoop's `shims` directory |
| Cargo / cargo-binstall | `~/.cargo/bin` |

### Fix stale or shadowed installs

1. **Prefer a single install channel.** Pick one (Homebrew, Scoop, installer, or cargo) and stick with it.
2. **Remove or rename the rest** so only one `ledgerful` remains on `PATH` (delete/rename the older binary, or uninstall the unused channel).
3. **Open a new shell** after any PATH change — existing terminals keep the old lookup cache.
4. Re-check with `ledgerful --version` and the diagnose commands above until the version and path match the channel you intended.

## Verifying your install

After installation, verify it works and the telemetry identifies your platform correctly:

**Windows**:
```powershell
> ledgerful --version
ledgerful <version>

> ledgerful doctor
Ledgerful Doctor - Environment Health Check
==================================================
Environment:         Windows
Active Shell:        Powershell
LEDGERFUL_PLATFORM: os=windows, arch=x86_64, family=windows, target_triple=x86_64-pc-windows-msvc
...
```

**macOS**:
```bash
$ ledgerful --version
ledgerful <version>

$ ledgerful doctor
Ledgerful Doctor - Environment Health Check
==================================================
Environment:         MacOS
Active Shell:        Zsh
LEDGERFUL_PLATFORM: os=macos, arch=aarch64, family=unix, target_triple=aarch64-apple-darwin
...
```

**Linux**:
```bash
$ ledgerful --version
ledgerful <version>

$ ledgerful doctor
Ledgerful Doctor - Environment Health Check
==================================================
Environment:         Linux
Active Shell:        Bash
LEDGERFUL_PLATFORM: os=linux, arch=x86_64, family=unix, target_triple=x86_64-unknown-linux-gnu
...
```

## Options

Windows:

```powershell
.\install\install.ps1 -BuildFromSource
.\install\install.ps1 -InstallDir "$HOME\.local"
.\install\install.ps1 -Daemon
.\install\install.ps1 -NoPathUpdate
```

macOS/Linux:

```bash
LEDGERFUL_BUILD_FROM_SOURCE=1 ./install/install.sh
LEDGERFUL_INSTALL_DIR="$HOME/.ledgerful" ./install/install.sh
LEDGERFUL_DAEMON=1 ./install/install.sh
LEDGERFUL_NO_PATH_UPDATE=1 ./install/install.sh
```

## Agent Bootstrap

If an AI agent is asked to use Ledgerful in a repository, it should:

1. Check availability:

   ```bash
   ledgerful doctor
   ```

2. If unavailable and installation is allowed, run the platform installer above.
3. Re-run:

   ```bash
   ledgerful doctor
   ```

4. Initialize the repository only when the user wants Ledgerful state in that repo:

   ```bash
   ledgerful init
   ```

5. Run the normal workflow:

   ```bash
   ledgerful scan
   ledgerful impact
   ledgerful verify
   ```

## Default Init Template

The installers seed a user-level default config template at:

```text
~/.ledgerful/default-config.toml
```

When `ledgerful init` creates a new repository config, it uses that template if
present. Set `LEDGERFUL_DEFAULT_CONFIG` to point at another template for
one-off initialization.

Template precedence is the explicit environment path, then the user-level
template, then the built-in template. Init parses the selected TOML before
publication and omits secret-bearing assignments and credentialed connection
URLs. A malformed custom template fails closed and does not create a partial
repo config. Existing repo configs are never replaced.

Store credentials in the process environment or an ignored repo-local `.env`.
Supported names include `GEMINI_API_KEY`, `OLLAMA_CLOUD_API_KEY`, and the
legacy `OLLAMA_API_KEY`. Ledgerful does not expand `${VAR}` syntax in TOML.

## Compatibility alias repair

`ledgerful` is the canonical installed executable; `ledgerful` is a
same-directory compatibility alias. Alias refresh stages and verifies a new
copy before atomically publishing it. It does not modify similarly named
binaries in other `PATH` directories.

If Windows has `ledgerful.exe` open without compatible sharing permissions,
close the process and retry:

```powershell
ledgerful update --binary
```

`ledgerful init` treats alias repair as best-effort. An explicit
`ledgerful update --binary` returns a failure if canonical installation
succeeds but the alias still cannot be repaired.

## Release Assets

Tagged releases publish these binary assets:

- `ledgerful-x86_64-pc-windows-msvc.zip`
- `ledgerful-x86_64-unknown-linux-gnu.tar.gz`
- `ledgerful-x86_64-apple-darwin.tar.gz`
- `ledgerful-aarch64-apple-darwin.tar.gz`

Create a release by pushing a tag:

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```
If `LEDGERFUL_DEFAULT_CONFIG` names a file that does not exist, initialization
preserves the historical contract and uses the built-in starter template. It
does not fall through to the home template. A readable explicit template that
is malformed or invalid fails closed.

Alias repair serializes mutations in each installation directory and never
overwrites an alias that appeared concurrently. If publication and rollback
both fail, the error identifies the operation-owned backup and prints the
PowerShell `Move-Item` recovery command; do not delete that backup.
Serialization uses a held OS advisory lock. The marker path is intentionally
persistent: process termination releases the lock, so stale or malformed marker
contents do not block a later repair. Windows publishes with no-replace
`MoveFileExW`; Linux/glibc uses `renameat2(RENAME_NOREPLACE)`. Other Unix
targets use same-directory hard-link create-if-absent semantics. If that
filesystem does not support hard links, Ledgerful fails closed with an
`atomic no-clobber` diagnostic instead of risking replacement of a concurrent
file. The same limitation applies to starter-config creation on those targets.
Operation-owned old executables are retried only when a regular working alias
exists. A still-running old executable remains deferred and its exact quoted
path is reported; a later repair retries cleanup after the process exits.
