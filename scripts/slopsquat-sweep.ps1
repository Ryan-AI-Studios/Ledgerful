# Slopsquatting Sweep — provenance check for all direct dependencies
# Queries crates.io and npm registries for existence, age, downloads, maintenance.
# Flags suspects for manual adjudication. Output: JSON report + console summary.
# Usage:  .\scripts\slopsquat-sweep.ps1

[CmdletBinding()]
param(
    [string] $EngineCargoToml = "C:\dev\ledgerful\Cargo.toml",
    [string] $FrontendPkg = "C:\dev\ledgerful-frontend\package.json",
    [string] $WebPkg = "C:\dev\ledgerful-web\package.json",
    [string] $McpPkg = "C:\dev\ledgerful\mcp-server\package.json",
    [string] $ActionPkg = "C:\dev\ledgerful\action\package.json",
    [string] $OutPath = "C:\Users\RyanB\AppData\Local\Temp\opencode\slopsquat-report.json"
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$results = [System.Collections.Generic.List[object]]::new()
$suspects = [System.Collections.Generic.List[object]]::new()

function ConvertFrom-TomlSimple {
    param([string]$Path)
    $content = Get-Content -LiteralPath $Path -Raw
    $deps = [System.Collections.Generic.List[object]]::new()
    $inDeps = $false
    $section = ''
    foreach ($line in ($content -split "`n")) {
        if ($line -match '^\[([^\]]+)\]') {
            $section = $matches[1]
            if ($section -match '^dependencies') { $inDeps = $true } else { $inDeps = $false }
            continue
        }
        if ($inDeps -and $line -match '^\s*([a-zA-Z0-9_-]+)\s*=\s*"([^"]+)"') {
            $deps.Add(@{ name = $matches[1]; version = $matches[2]; section = $section })
        }
        elseif ($inDeps -and $line -match '^\s*([a-zA-Z0-9_-]+)\s*=\s*\{') {
            $name = $matches[1]
            $ver = ''
            if ($line -match 'version\s*=\s*"([^"]+)"') { $ver = $matches[1] }
            $deps.Add(@{ name = $name; version = $ver; section = $section })
        }
    }
    return $deps
}

function Get-CrateInfo {
    param([string]$Name)
    $url = "https://crates.io/api/v1/crates/$Name"
    try {
        $resp = Invoke-RestMethod -Uri $url -Headers @{ 'User-Agent' = 'ledgerful-slopsquat-sweep/1.0' } -TimeoutSec 15
        $ver = $resp.crate.max_stable_version
        $created = $resp.crate.created_at
        $downloads = $resp.crate.downloads
        $recent = $resp.crate.recent_downloads
        return @{ exists = $true; latest = $ver; created = $created; downloads = $downloads; recent_downloads = $recent }
    } catch {
        return @{ exists = $false; latest = ''; created = ''; downloads = 0; recent_downloads = 0 }
    }
}

function Get-NpmInfo {
    param([string]$Name)
    $url = "https://registry.npmjs.org/$Name"
    try {
        $resp = Invoke-RestMethod -Uri $url -TimeoutSec 15
        $latest = $resp.'dist-tags'.latest
        $created = $resp.time.created
        $allVers = $resp.versions.PSObject.Properties.Name
        $verCount = $allVers.Count
        $lastMod = $resp.time.modified
        $maintCount = $resp.maintainers.Count
        return @{ exists = $true; latest = $latest; created = $created; versions = $verCount; lastModified = $lastMod; maintainers = $maintCount }
    } catch {
        return @{ exists = $false; latest = ''; created = ''; versions = 0; lastModified = ''; maintainers = 0 }
    }
}
function Add-Result {
    param(
        [string]$PkgName, [string]$Ecosystem, [string]$Repo,
        [string]$DeclaredVer, [hashtable]$Info
    )
    $ageDays = 0
    if ($Info.created) {
        try { $ageDays = [int]((Get-Date) - [DateTime]::Parse($Info.created)).TotalDays } catch { $ageDays = 0 }
    }
    $flags = [System.Collections.Generic.List[string]]::new()
    if (-not $Info.exists) { $flags.Add('NOT_FOUND') }
    if ($ageDays -lt 90 -and $Info.exists) { $flags.Add('YOUNG(<90d)') }
    $recentDl = 0; if ($Info.ContainsKey('recent_downloads')) { $recentDl = $Info.recent_downloads }
    $verCount = 0; if ($Info.ContainsKey('versions')) { $verCount = $Info.versions }
    $maintCount = 0; if ($Info.ContainsKey('maintainers')) { $maintCount = $Info.maintainers }
    if ($Ecosystem -eq 'crates.io' -and $recentDl -lt 1000) { $flags.Add('LOW_DOWNLOADS(<1k_recent)') }
    if ($Ecosystem -eq 'npm' -and $verCount -lt 3) { $flags.Add('FEW_VERSIONS(<3)') }
    if ($Ecosystem -eq 'npm' -and $maintCount -lt 1) { $flags.Add('NO_MAINTAINERS') }

    $downloads = 0; if ($Info.ContainsKey('downloads')) { $downloads = $Info.downloads }
    $entry = [ordered]@{
        package     = $PkgName
        ecosystem   = $Ecosystem
        repo        = $Repo
        declared    = $DeclaredVer
        latest      = $Info.latest
        exists      = $Info.exists
        created     = $Info.created
        age_days    = $ageDays
        downloads   = $downloads
        recent_dl   = $recentDl
        versions    = $verCount
        maintainers = $maintCount
        flags       = ($flags -join ',')
    }
    $results.Add($entry)
    if ($flags.Count -gt 0) { $suspects.Add($entry) }
}

# Engine crates
Write-Host 'Engine (crates.io)...' -ForegroundColor Cyan
$crateDeps = ConvertFrom-TomlSimple -Path $EngineCargoToml
$skipCrates = @('ledgerful')
foreach ($d in $crateDeps) {
    if ($skipCrates -contains $d.name) { continue }
    $info = Get-CrateInfo -Name $d.name
    Add-Result -PkgName $d.name -Ecosystem 'crates.io' -Repo 'ledgerful' -DeclaredVer $d.version -Info $info
    Write-Host "  $($d.name): $(if ($info.exists) { "OK $($info.latest)" } else { "NOT FOUND" })"
}

# npm packages
$npmFiles = @(
    @{ path = $FrontendPkg; repo = 'ledgerful-frontend' },
    @{ path = $WebPkg; repo = 'ledgerful-web' },
    @{ path = $McpPkg; repo = 'mcp-server' },
    @{ path = $ActionPkg; repo = 'action' }
)

foreach ($f in $npmFiles) {
    if (-not (Test-Path -LiteralPath $f.path)) {
        Write-Host "Skip $($f.repo) (no package.json)" -ForegroundColor Yellow
        continue
    }
    Write-Host "$($f.repo) (npm)..." -ForegroundColor Cyan
    $pkg = Get-Content -LiteralPath $f.path -Raw | ConvertFrom-Json
    $hasDeps = $pkg.PSObject.Properties.Name -contains 'dependencies'
    $hasDevDeps = $pkg.PSObject.Properties.Name -contains 'devDependencies'
    if ($hasDeps) {
        foreach ($dep in $pkg.dependencies.PSObject.Properties) {
            $info = Get-NpmInfo -Name $dep.Name
            Add-Result -PkgName $dep.Name -Ecosystem 'npm' -Repo $f.repo -DeclaredVer $dep.Value -Info $info
            Write-Host "  $($dep.Name): $(if ($info.exists) { "OK $($info.latest)" } else { "NOT FOUND" })"
        }
    }
    if ($hasDevDeps) {
        foreach ($dep in $pkg.devDependencies.PSObject.Properties) {
            $info = Get-NpmInfo -Name $dep.Name
            Add-Result -PkgName $dep.Name -Ecosystem 'npm' -Repo $f.repo -DeclaredVer $dep.Value -Info $info
            Write-Host "  $($dep.Name) (dev): $(if ($info.exists) { "OK $($info.latest)" } else { "NOT FOUND" })"
        }
    }
}

# Write report
$report = [ordered]@{
    generated  = (Get-Date -Format 'o')
    total      = $results.Count
    suspects   = $suspects.Count
    findings   = $results
    flagged    = $suspects
}
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutPath -Encoding UTF8

Write-Host ''
Write-Host "Total deps scanned: $($results.Count)" -ForegroundColor White
Write-Host "Flagged suspects:   $($suspects.Count)" -ForegroundColor $(if ($suspects.Count -gt 0) { 'Yellow' } else { 'Green' })
if ($suspects.Count -gt 0) {
    Write-Host 'Suspects:' -ForegroundColor Yellow
    foreach ($s in $suspects) {
        Write-Host "  [$($s.ecosystem)] $($s.package) (@$($s.repo)) - $($s.flags)"
    }
}
Write-Host "Report: $OutPath"