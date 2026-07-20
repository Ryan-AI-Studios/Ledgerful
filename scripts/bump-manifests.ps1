#Requires -Version 5.1
<#
.SYNOPSIS
  Bump Homebrew formula + Scoop manifest version/hashes from published .sha256 files.

.DESCRIPTION
  Reads checksums ONLY from published/fixture *.sha256 files (never recomputes from
  archives). Updates packaging/homebrew/ledgerful.rb and packaging/scoop/ledgerful.json.

  Required assets (must have a matching <asset>.sha256 in -ChecksumsDir):
    - ledgerful-x86_64-pc-windows-msvc.zip
    - ledgerful-x86_64-unknown-linux-gnu.tar.gz
    - ledgerful-x86_64-apple-darwin.tar.gz
    - ledgerful-aarch64-apple-darwin.tar.gz

.EXAMPLE
  pwsh -File scripts/bump-manifests.ps1 -Version 0.1.8 `
    -ChecksumsDir tests/fixtures/package-manifests/v0.1.8 `
    -PackagingDir packaging -OutDir $env:TEMP\bump-out
#>
[CmdletBinding()]
param(
    # Accepts X.Y.Z or vX.Y.Z
    [Parameter(Mandatory = $true)]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [string]$ChecksumsDir,

    [Parameter(Mandatory = $false)]
    [string]$PackagingDir = "packaging",

    [Parameter(Mandatory = $false)]
    [string]$OutDir = "",

    [Parameter(Mandatory = $false)]
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Normalize-Version([string]$v) {
    $t = $v.Trim()
    if ($t.StartsWith("v") -or $t.StartsWith("V")) {
        return $t.Substring(1)
    }
    return $t
}

function Get-Sha256FirstToken([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "Missing checksum file: $path"
    }
    $raw = Get-Content -LiteralPath $path -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) {
        throw "Empty checksum file: $path"
    }
    # First non-empty line, first whitespace-separated token; strip BOM/CRLF.
    $line = ($raw -split "(`r`n|`n|`r)" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -First 1)
    if (-not $line) {
        throw "No hash line in checksum file: $path"
    }
    $token = ($line.Trim() -split "\s+")[0].Trim().ToLowerInvariant()
    if ($token -notmatch '^[0-9a-f]{64}$') {
        throw "Invalid sha256 token in ${path}: '$token'"
    }
    return $token
}

function Resolve-ExistingDir([string]$path, [string]$label) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "$label does not exist: $path"
    }
    return (Resolve-Path -LiteralPath $path).Path
}

$versionNorm = Normalize-Version $Version
if ($versionNorm -notmatch '^\d+\.\d+\.\d+') {
    throw "Version must look like X.Y.Z (got '$Version' → '$versionNorm')"
}

$checksumsRoot = Resolve-ExistingDir $ChecksumsDir "ChecksumsDir"
$packagingRoot = Resolve-ExistingDir $PackagingDir "PackagingDir"

$requiredAssets = @(
    "ledgerful-x86_64-pc-windows-msvc.zip",
    "ledgerful-x86_64-unknown-linux-gnu.tar.gz",
    "ledgerful-x86_64-apple-darwin.tar.gz",
    "ledgerful-aarch64-apple-darwin.tar.gz"
) | Sort-Object

$hashes = [ordered]@{}
foreach ($asset in $requiredAssets) {
    $shaPath = Join-Path $checksumsRoot ($asset + ".sha256")
    $hashes[$asset] = Get-Sha256FirstToken $shaPath
}

$homebrewSrc = Join-Path $packagingRoot "homebrew\ledgerful.rb"
$scoopSrc = Join-Path $packagingRoot "scoop\ledgerful.json"
if (-not (Test-Path -LiteralPath $homebrewSrc)) {
    throw "Homebrew formula missing: $homebrewSrc"
}
if (-not (Test-Path -LiteralPath $scoopSrc)) {
    throw "Scoop manifest missing: $scoopSrc"
}

$formula = Get-Content -LiteralPath $homebrewSrc -Raw
$scoop = Get-Content -LiteralPath $scoopSrc -Raw

# Capture previous version from formula if present (for URL rewrite).
$prevVersion = $null
if ($formula -match 'version\s+"([^"]+)"') {
    $prevVersion = $Matches[1]
}

# --- Homebrew ---
$formulaNew = $formula
# version field
$formulaNew = [regex]::Replace($formulaNew, 'version\s+"[^"]+"', "version `"$versionNorm`"")
# URL version segments: /vX.Y.Z/ → /v{new}/
if ($prevVersion) {
    $formulaNew = $formulaNew.Replace("/v$prevVersion/", "/v$versionNorm/")
} else {
    $formulaNew = [regex]::Replace(
        $formulaNew,
        '/v\d+\.\d+\.\d+(?:-[^/"]+)?/',
        "/v$versionNorm/"
    )
}

# Per-asset sha256: line after a url that contains the asset filename.
foreach ($asset in @($requiredAssets | Where-Object { $_ -notlike "*.zip" } | Sort-Object)) {
    $hash = $hashes[$asset]
    $escaped = [regex]::Escape($asset)
    $pattern = "(url\s+`"[^`"]*$escaped[^`"]*`"\s*\r?\n\s*)sha256\s+`"[0-9a-fA-F]+`""
    if (-not [regex]::IsMatch($formulaNew, $pattern)) {
        throw "Homebrew formula has no url/sha256 pair for asset: $asset"
    }
    $replacement = "`${1}sha256 `"$hash`""
    # Note: content may be unchanged when template already has the target hash.
    $formulaNew = [regex]::Replace($formulaNew, $pattern, $replacement)
}

# --- Scoop ---
$scoopNew = $scoop
$winAsset = "ledgerful-x86_64-pc-windows-msvc.zip"
$winHash = $hashes[$winAsset]
$scoopNew = [regex]::Replace($scoopNew, '"version"\s*:\s*"[^"]+"', "`"version`": `"$versionNorm`"")
if ($prevVersion) {
    $scoopNew = $scoopNew.Replace("/v$prevVersion/", "/v$versionNorm/")
} else {
    $scoopNew = [regex]::Replace(
        $scoopNew,
        '/v\d+\.\d+\.\d+(?:-[^/"]+)?/',
        "/v$versionNorm/"
    )
}
# Only replace the architecture hash (not license urls). Match hash after the windows zip url.
$scoopHashPattern = '("url"\s*:\s*"[^"]*' + [regex]::Escape($winAsset) + '[^"]*"\s*,\s*\r?\n\s*"hash"\s*:\s*")[0-9a-fA-F]+(")'
$scoopFallbackPattern = '("architecture"[\s\S]*?"hash"\s*:\s*")[0-9a-fA-F]+(")'
if ([regex]::IsMatch($scoopNew, $scoopHashPattern)) {
    $scoopNew = [regex]::Replace($scoopNew, $scoopHashPattern, "`${1}$winHash`${2}")
} elseif ([regex]::IsMatch($scoopNew, $scoopFallbackPattern)) {
    $scoopNew = [regex]::Replace($scoopNew, $scoopFallbackPattern, "`${1}$winHash`${2}")
} else {
    throw "Scoop manifest: failed to locate hash for $winAsset"
}

# Summary
Write-Host "bump-manifests: version=$versionNorm"
Write-Host "checksums-dir=$checksumsRoot"
foreach ($asset in $requiredAssets) {
    Write-Host ("  {0} = {1}" -f $asset, $hashes[$asset])
}

$changed = @()
if ($formulaNew -ne $formula) { $changed += "homebrew/ledgerful.rb" }
if ($scoopNew -ne $scoop) { $changed += "scoop/ledgerful.json" }
if ($changed.Count -eq 0) {
    Write-Host "No content changes (already at target version/hashes)."
} else {
    Write-Host ("Changed: {0}" -f ($changed -join ", "))
}

if ($DryRun) {
    Write-Host "DryRun: not writing files."
    exit 0
}

$destRoot = if ($OutDir -and $OutDir.Trim().Length -gt 0) { $OutDir.Trim() } else { $packagingRoot }
$hbDestDir = Join-Path $destRoot "homebrew"
$scDestDir = Join-Path $destRoot "scoop"
New-Item -ItemType Directory -Force -Path $hbDestDir | Out-Null
New-Item -ItemType Directory -Force -Path $scDestDir | Out-Null

$hbDest = Join-Path $hbDestDir "ledgerful.rb"
$scDest = Join-Path $scDestDir "ledgerful.json"

# Deterministic line endings: LF for formula (Ruby on Unix CI), LF for JSON.
$formulaOut = ($formulaNew -replace "`r`n", "`n" -replace "`r", "`n")
$scoopOut = ($scoopNew -replace "`r`n", "`n" -replace "`r", "`n")
# Ensure trailing newline
if (-not $formulaOut.EndsWith("`n")) { $formulaOut += "`n" }
if (-not $scoopOut.EndsWith("`n")) { $scoopOut += "`n" }

[System.IO.File]::WriteAllText($hbDest, $formulaOut, [System.Text.UTF8Encoding]::new($false))
[System.IO.File]::WriteAllText($scDest, $scoopOut, [System.Text.UTF8Encoding]::new($false))

Write-Host "Wrote: $hbDest"
Write-Host "Wrote: $scDest"
exit 0
