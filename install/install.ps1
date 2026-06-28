param(
    [string]$Repo = "Ryan-AI-Studios/Ledgerful",
    [string]$Version = "latest",
    [string]$InstallDir = "$HOME\.ledgerful",
    [switch]$NoPathUpdate,
    [switch]$BuildFromSource,
    [switch]$Daemon
)

$ErrorActionPreference = "Stop"

function Write-Step($Message) {
    Write-Host "==> $Message"
}

function Test-Command($Name) {
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Add-UserPath($PathToAdd) {
    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @()
    if ($current) {
        $parts = $current -split ';' | Where-Object { $_ }
    }

    if ($parts -notcontains $PathToAdd) {
        $newPath = if ($current) { "$current;$PathToAdd" } else { $PathToAdd }
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        $env:Path = "$env:Path;$PathToAdd"
        Write-Step "Added $PathToAdd to the user PATH. Open a new terminal for other sessions."
    }
}

function Install-FromRelease {
    $binDir = Join-Path $InstallDir "bin"
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null

    $asset = "ledgerful-x86_64-pc-windows-msvc.zip"
    $tagPath = if ($Version -eq "latest") { "latest/download" } else { "download/$Version" }
    $url = "https://github.com/$Repo/releases/$tagPath/$asset"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) "ledgerful-$([System.Guid]::NewGuid()).zip"
    $extractDir = Join-Path ([System.IO.Path]::GetTempPath()) "ledgerful-$([System.Guid]::NewGuid())"

    Write-Step "Downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
    Expand-Archive -Path $tmp -DestinationPath $extractDir -Force

    $exe = Get-ChildItem -Path $extractDir -Recurse -Filter "ledgerful.exe" | Select-Object -First 1
    if (-not $exe) {
        throw "Release archive did not contain ledgerful.exe"
    }

    foreach ($name in @("ledgerful.exe")) {
        Copy-Item -Path $exe.FullName -Destination (Join-Path $binDir $name) -Force
    }
    Remove-Item -Path $tmp -Force -ErrorAction SilentlyContinue
    Remove-Item -Path $extractDir -Recurse -Force -ErrorAction SilentlyContinue
}

function Install-FromCargo {
    if (-not (Test-Command "cargo")) {
        throw "Rust cargo was not found. Install Rust from https://rustup.rs or publish a Ledgerful release asset, then rerun this installer."
    }

    $features = @()
    if ($Daemon) {
        $features = @("--features", "daemon")
    }

    if ((Test-Path "Cargo.toml") -and ((Get-Content "Cargo.toml" -Raw) -match 'name\s*=\s*"ledgerful"')) {
        Write-Step "Installing Ledgerful from the current checkout"
        cargo install --path . --locked --root $InstallDir @features
    } else {
        Write-Step "Installing Ledgerful from https://github.com/$Repo"
        cargo install ledgerful --git "https://github.com/$Repo" --branch main --locked --root $InstallDir @features
    }
}

function Install-DefaultConfig {
    $configDir = Join-Path $HOME ".ledgerful"
    $defaultConfig = Join-Path $configDir "default-config.toml"

    New-Item -ItemType Directory -Force -Path $configDir | Out-Null

    if (Test-Path $defaultConfig) {
        Write-Step "Default init config already exists at $defaultConfig"
        return
    }

    $checkoutConfig = Join-Path (Get-Location) ".ledgerful\config.toml"
    $exampleConfig = Join-Path (Get-Location) "docs\examples\config.toml"

    if (Test-Path $checkoutConfig) {
        Copy-Item -Path $checkoutConfig -Destination $defaultConfig -Force
        Write-Step "Seeded default init config from current checkout: $defaultConfig"
        return
    }

    if (Test-Path $exampleConfig) {
        Copy-Item -Path $exampleConfig -Destination $defaultConfig -Force
        Write-Step "Seeded default init config from docs example: $defaultConfig"
        return
    }

    @'
[core]
strict = false
auto_fix = false

[watch]
debounce_ms = 1000
ignore_patterns = ["target/**", ".git/**", "node_modules/**"]

[temporal]
max_commits = 1000
max_files_per_commit = 50
coupling_threshold = 0.75
min_shared_commits = 3
min_revisions = 5
decay_half_life = 100

[hotspots]
max_commits = 500
limit = 10

# [verify]
# default_timeout_secs = 300
# Steps to run when `ledgerful verify` is invoked without -c.
# Each step has a description, command, and optional timeout_secs (defaults to 300).
# [[verify.steps]]
# description = "Run project tests"
# command = "cargo test -j 1 -- --test-threads=1"
# timeout_secs = 300
# [[verify.steps]]
# description = "Check formatting"
# command = "cargo fmt --check"

[gemini]
# Prefer GEMINI_API_KEY in the environment or local .env.
# api_key = "..."
# Optional override for every ask mode:
# model = "gemini-3.1-pro"
fast_model = "gemini-3.1-flash-lite"
deep_model = "gemini-3.1-pro"
timeout_secs = 120
context_window = 128000
'@ | Set-Content -Path $defaultConfig -Encoding UTF8

    Write-Step "Seeded starter default init config: $defaultConfig"
}

$binDir = Join-Path $InstallDir "bin"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

if ($BuildFromSource) {
    Install-FromCargo
} else {
    try {
        Install-FromRelease
    } catch {
        Write-Step "Release install failed: $($_.Exception.Message)"
        Write-Step "Falling back to cargo install"
        Install-FromCargo
    }
}

Install-DefaultConfig

if (-not $NoPathUpdate) {
    Add-UserPath $binDir
}

Write-Step "Verifying installation"
foreach ($name in @("ledgerful.exe")) {
    $command = Join-Path $binDir $name
    if (-not (Test-Path $command)) {
        throw "Installed command was not found: $command"
    }
    & $command --version | Select-Object -First 1
}

Write-Host ""
Write-Host "Ledgerful installed. AI agents can now run: ledgerful doctor"
Write-Host "Default init config: $HOME\.ledgerful\default-config.toml"
