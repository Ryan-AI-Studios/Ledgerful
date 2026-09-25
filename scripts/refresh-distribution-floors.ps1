#Requires -Version 5.1
<#
.SYNOPSIS
  Refresh dated winget-index / GitHub-Latest claim sites in distribution docs.

.DESCRIPTION
  Windows twin of refresh-distribution-floors.sh. Production reads remotes only.
  -FixturesDir is test-only. Never calls winget search. Never reads Cargo.toml
  or packaging/ as the current-claim source.

.EXAMPLE
  pwsh -File scripts/refresh-distribution-floors.ps1 -Write
  pwsh -File scripts/refresh-distribution-floors.ps1 -Check -DocsDir docs -FixturesDir tests/fixtures/distribution-floors
#>
[CmdletBinding()]
param(
    [switch]$Write,
    [switch]$Check,
    [string]$DocsDir = "docs",
    [string]$FixturesDir = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if (-not $Write -and -not $Check) {
    throw "error: -Write or -Check is required"
}
if ($Write -and $Check) {
    throw "error: pass only one of -Write / -Check"
}
if (-not (Test-Path -LiteralPath $DocsDir -PathType Container)) {
    throw "error: docs-dir does not exist: $DocsDir"
}

$installMd = Join-Path $DocsDir "installation.md"
$pkgMd = Join-Path $DocsDir "package-distribution.md"
if (-not (Test-Path -LiteralPath $installMd) -or -not (Test-Path -LiteralPath $pkgMd)) {
    throw "error: expected $installMd and $pkgMd"
}

function Get-NewestSemver([string[]]$names) {
    $best = [version]"0.0.0"
    foreach ($n in $names) {
        $t = ($n -as [string]).Trim()
        if ([string]::IsNullOrWhiteSpace($t)) { continue }
        try {
            $v = [version]$t
        } catch {
            continue
        }
        if ($v -gt $best) { $best = $v }
    }
    return $best.ToString()
}

function Get-PrClause([string]$prsCsv) {
    if ([string]::IsNullOrWhiteSpace($prsCsv)) { return "" }
    $nums = @($prsCsv.Split(",") | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '^\d+$' } | Sort-Object { [int]$_ } -Unique)
    if ($nums.Count -eq 0) { return "" }
    if ($nums.Count -eq 1) { return " Open version PR #$($nums[0])." }
    $joined = ($nums | ForEach-Object { "#$_" }) -join ", "
    return " Open version PRs $joined."
}

function Get-LagClause([string]$index, [string]$latestVer) {
    if ($index -eq $latestVer) { return "" }
    return " do not claim winget $latestVer until that directory exists."
}

function Get-Facts {
    $tag = $null
    $index = $null
    $prs = ""
    $asOf = [DateTime]::UtcNow.ToString("yyyy-MM-dd")
    if ($FixturesDir -and $FixturesDir.Trim().Length -gt 0) {
        if (-not (Test-Path -LiteralPath $FixturesDir -PathType Container)) {
            throw "error: fixtures-dir does not exist: $FixturesDir"
        }
        $tag = (Get-Content -LiteralPath (Join-Path $FixturesDir "latest.tag") -TotalCount 1).Trim()
        $dirs = Get-Content -LiteralPath (Join-Path $FixturesDir "winget-dirs.txt")
        $index = Get-NewestSemver $dirs
        $prFile = Join-Path $FixturesDir "open-prs.txt"
        if (Test-Path -LiteralPath $prFile) {
            $prLines = Get-Content -LiteralPath $prFile | Where-Object { $_.Trim() -match '^\d+$' }
            $prs = ($prLines | Sort-Object { [int]$_ } -Unique) -join ","
        }
        $asOfPath = Join-Path $FixturesDir "as-of.txt"
        if (Test-Path -LiteralPath $asOfPath) {
            $asOf = (Get-Content -LiteralPath $asOfPath -TotalCount 1).Trim()
        }
    } else {
        if (-not $env:GH_TOKEN -and $env:GITHUB_TOKEN) {
            $env:GH_TOKEN = $env:GITHUB_TOKEN
        }
        $tag = gh api repos/Ryan-AI-Studios/Ledgerful/releases/latest --jq .tag_name
        if ($LASTEXITCODE -ne 0) { throw "error: gh api Latest failed" }
        $dirJson = gh api --paginate "repos/microsoft/winget-pkgs/contents/manifests/l/Ledgerful/Ledgerful" --jq ".[].name"
        if ($LASTEXITCODE -ne 0) { throw "error: gh api winget-pkgs failed" }
        $index = Get-NewestSemver @($dirJson)
        $prJson = gh pr list --repo microsoft/winget-pkgs --search "Ledgerful.Ledgerful" --state open --limit 50 --json number --jq ".[].number"
        if ($LASTEXITCODE -ne 0) { throw "error: gh pr list failed" }
        if ($prJson) {
            $prs = (@($prJson) | Sort-Object { [int]$_ } -Unique) -join ","
        }
    }
    $tag = $tag.Trim()
    if ($tag.StartsWith("v") -or $tag.StartsWith("V")) { $tag = $tag.Substring(1) }
    if ([string]::IsNullOrWhiteSpace($tag) -or [string]::IsNullOrWhiteSpace($index) -or $index -eq "0.0.0") {
        throw "error: failed to load Latest tag or winget-pkgs index"
    }
    return @{
        Tag     = $tag
        TagV    = "v$tag"
        Index   = $index
        Prs     = $prs
        Date    = $asOf
    }
}

function Set-Site([string]$path, [string]$site, [string]$interior, [string]$unmarkedPattern) {
    $text = [IO.File]::ReadAllText($path)
    $start = "<!-- lf-floor:$site -->"
    $stop = "<!-- /lf-floor:$site -->"
    $escapedStart = [regex]::Escape($start)
    $escapedStop = [regex]::Escape($stop)
    if ($text -match "$escapedStart[\s\S]*?$escapedStop") {
        $marked = New-Object System.Text.RegularExpressions.Regex "$escapedStart[\s\S]*?$escapedStop"
        $text = $marked.Replace($text, { param($m) $start + $interior + $stop }, 1)
    } else {
        $re = New-Object System.Text.RegularExpressions.Regex $unmarkedPattern
        if (-not $re.IsMatch($text)) {
            throw "error: failed to apply floor site $site in $path"
        }
        $text = $re.Replace($text, { param($m) $start + $interior + $stop }, 1)
    }
    if ($text -notlike "*$start*") {
        throw "error: failed to apply floor site $site in $path"
    }
    [IO.File]::WriteAllText($path, $text)
}

function Get-SiteBody([string]$path, [string]$site) {
    $text = [IO.File]::ReadAllText($path)
    $start = "<!-- lf-floor:$site -->"
    $stop = "<!-- /lf-floor:$site -->"
    $s = $text.IndexOf($start)
    $e = $text.IndexOf($stop)
    if ($s -lt 0 -or $e -le $s) { return $null }
    return $text.Substring($s + $start.Length, $e - ($s + $start.Length))
}

$facts = Get-Facts
$prClause = Get-PrClause $facts.Prs
$lag = Get-LagClause $facts.Index $facts.Tag

$i57 = "community index is live at **$($facts.Index)** (``microsoft/winget-pkgs`` ``manifests/l/Ledgerful/Ledgerful``, $($facts.Date)). GitHub Release **$($facts.TagV)** is published;$lag$prClause"
$p13 = "Live at **$($facts.Index)** (manifests $($facts.Date)); GitHub **$($facts.TagV)** published, index may lag.$prClause"
$p191 = "live on winget at **$($facts.Index)** (``microsoft/winget-pkgs`` ``manifests/l/Ledgerful/Ledgerful``, $($facts.Date)). GitHub Release **$($facts.TagV)** is published;$lag$prClause"
$p215 = "live on winget at **$($facts.Index)**, manifests $($facts.Date)"
$p217Lag = if ($facts.Index -eq $facts.Tag) { "" } else { " do not claim $($facts.Tag) live until that directory exists." }
$p217 = "community index **$($facts.Index)**. GitHub Release **$($facts.TagV)** published $($facts.Date);$p217Lag$prClause"

if ($Write) {
    Set-Site $installMd "I-57" $i57 'community index is live at \*\*[^*]+\*\* \([^)]+\)\. GitHub Release \*\*v[^*]+\*\* is published; do not claim winget [0-9.]+ until [^.]*\.'
    Set-Site $pkgMd "P-13" $p13 'Live at \*\*[^*]+\*\* \([^)]+\); GitHub \*\*v[^*]+\*\* published, index may lag'
    Set-Site $pkgMd "P-191" $p191 'live on winget at \*\*[^*]+\*\* \([^)]+\)\. GitHub Release \*\*v[^*]+\*\* is published; do not claim [a-z0-9. -]+ until [^.]*\.'
    Set-Site $pkgMd "P-215" $p215 'live on winget at \*\*[^*]+\*\*, `winget search` [0-9-]+|live on winget at \*\*[^*]+\*\*, manifests [0-9-]+'
    Set-Site $pkgMd "P-217" $p217 'community index \*\*[^*]+\*\*\. GitHub Release \*\*v[^*]+\*\* published [0-9-]+; do not claim [0-9.]+ live until [^.]*\.'
    $prOut = if ([string]::IsNullOrWhiteSpace($facts.Prs)) { "none" } else { $facts.Prs }
    Write-Host "refresh-distribution-floors: write index=$($facts.Index) latest=$($facts.TagV) date=$($facts.Date) prs=$prOut"
    exit 0
}

function Test-Site([string]$site, [string]$path, [bool]$needTag, [bool]$needPrs) {
    $body = Get-SiteBody $path $site
    if ([string]::IsNullOrWhiteSpace($body)) {
        Write-Error "error: --check: missing markers for $site in $path (run --write first)"
        return $false
    }
    $ok = $true
    $idxMatch = [regex]::Match($body, '\*\*([0-9]+\.[0-9]+\.[0-9]+)\*\*')
    $gotIndex = if ($idxMatch.Success) { $idxMatch.Groups[1].Value } else { "" }
    if ($gotIndex -ne $facts.Index) {
        Write-Error "error: $site index '$gotIndex' != '$($facts.Index)'"
        $ok = $false
    }
    if ($needTag) {
        $tagMatch = [regex]::Match($body, '\*\*(v[0-9]+\.[0-9]+\.[0-9]+)\*\*')
        $gotTag = if ($tagMatch.Success) { $tagMatch.Groups[1].Value } else { "" }
        if ($gotTag -ne $facts.TagV) {
            Write-Error "error: $site tag '$gotTag' != '$($facts.TagV)'"
            $ok = $false
        }
    }
    if ($needPrs) {
        $gotPrs = @([regex]::Matches($body, '#([0-9]+)') | ForEach-Object { $_.Groups[1].Value } | Sort-Object { [int]$_ } -Unique) -join ","
        if ($gotPrs -ne $facts.Prs) {
            Write-Error "error: $site open PRs '$gotPrs' != '$($facts.Prs)'"
            $ok = $false
        }
    }
    if ($body -notmatch '\d{4}-\d{2}-\d{2}') {
        Write-Error "error: $site missing YYYY-MM-DD date token"
        $ok = $false
    }
    return $ok
}

$fail = $false
if (-not (Test-Site "I-57" $installMd $true $true)) { $fail = $true }
if (-not (Test-Site "P-13" $pkgMd $true $true)) { $fail = $true }
if (-not (Test-Site "P-191" $pkgMd $true $true)) { $fail = $true }
if (-not (Test-Site "P-215" $pkgMd $false $false)) { $fail = $true }
if (-not (Test-Site "P-217" $pkgMd $true $true)) { $fail = $true }
if ($fail) {
    Write-Error "refresh-distribution-floors: check FAILED"
    exit 1
}
Write-Host "refresh-distribution-floors: check ok index=$($facts.Index) latest=$($facts.TagV)"
exit 0
