$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. "$PSScriptRoot/Projection.ps1"

$Repository = 'Ryan-AI-Studios/Ledgerful'
$DiagnosticRef = 'refs/heads/diagnostics/engine-rss-697-v2'
$Main = '90a3a0a072f791495607a4e77c188b512a223092'
$Candidate = 'fa899a99b0a4275ddc07b75e90486c9eb050e9c3'
$CandidateHead = '624fc2bf0d110af78330ce927656ad5b80991284'
$FixtureTree = '4c569775bacb6dbd7dd3b3440799759bcedf1657'
$ProbeHash = 'f63d7b2362c24402f75c4bf1780d6bc495fd84b445d43eacd4ddf52def7a1412'
$Overlay = [ordered]@{
    'Cargo.toml' = 'e8337ecec30e157a0543d3c415e312172cb930fd12f756b265bb69820e3144e0'
    'src/commands/bench.rs' = 'bf2b6f5c5eef7d1fca520d25b5d12730d67a6c4d264b1086ade6720fe06846d4'
    'src/commands/bench_memory_attribution.rs' = '4c9b6afaeebcd03a843e481976665d853b7112fcfbcd75bd31d1115180ff0183'
}
$Runs = 20
$TimeLimits = [ordered]@{diagnosticSeconds=6480; buildSeconds=1500; workloadSeconds=360; budgetSeconds=60}

function Get-Digest([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Read-Capture([string]$Path, [int]$Limit = 1048576) {
    Require ((Get-Item -LiteralPath $Path).Length -le $Limit)
    return [IO.File]::ReadAllText($Path, [Text.UTF8Encoding]::new($false, $true))
}

function Invoke-Captured([string]$Directory, [string]$Executable, [string[]]$Arguments, [int]$MaxSeconds) {
    $remaining = [int][Math]::Floor(($Deadline - [DateTime]::UtcNow).TotalSeconds)
    if ($remaining -le 0) { throw 'diagnostic time limit reached' }
    $timeout = [Math]::Min($MaxSeconds, $remaining)
    $sequence = ++$script:Sequence
    $stdout = Join-Path $Raw "$sequence.stdout"
    $stderr = Join-Path $Raw "$sequence.stderr"
    # Start-Process joins its argument array. Inputs are literals or constructed paths.
    $quoted = foreach ($argument in $Arguments) {
        Require (!$argument.Contains('"') -and !$argument.EndsWith('\'))
        '"' + $argument + '"'
    }
    $started = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $options = @{
        FilePath=$Executable; WorkingDirectory=$Directory; ArgumentList=$quoted
        RedirectStandardOutput=$stdout; RedirectStandardError=$stderr; NoNewWindow=$true; PassThru=$true
    }
    $child = Start-Process @options
    try {
        $creation = $null
        try { $creation = [uint64]$child.StartTime.ToFileTimeUtc() } catch [InvalidOperationException] {}
        $timedOut = !$child.WaitForExit(1000 * $timeout)
        if ($timedOut) {
            try { $child.Kill($true) } catch [InvalidOperationException] {}
            $null = $child.WaitForExit(5000)
        }
        $exitCode = $null
        if ($child.HasExited) { $exitCode = $child.ExitCode }
        return @{
            sequence=$sequence; processId=$child.Id; creationFiletime=$creation
            startedMs=$started; endedMs=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            wallMs=$clock.ElapsedMilliseconds; timedOut=$timedOut; exitCode=$exitCode
            stdout=$stdout; stderr=$stderr
        }
    }
    finally { $child.Dispose() }
}

function Invoke-Git([string]$Directory, [string[]]$Arguments) {
    $run = Invoke-Captured $Directory 'git.exe' $Arguments 180
    Require (!$run.timedOut -and $run.exitCode -eq 0)
    return (Read-Capture $run.stdout).Trim()
}

function New-DetachedClone([string]$Source, [string]$Destination, [string]$Revision) {
    $null = Invoke-Git $Source @('clone','--no-hardlinks','--no-checkout','-c','core.autocrlf=false','-c','core.longpaths=true','--',$Source,$Destination)
    $null = Invoke-Git $Destination @('checkout','--detach',$Revision)
    Require ((Invoke-Git $Destination @('rev-parse','HEAD')) -ceq $Revision)
    Require ((Invoke-Git $Destination @('status','--porcelain','--untracked-files=all')) -ceq '')
    Require ((Invoke-Git $Destination @('rev-parse','--is-shallow-repository')) -ceq 'false')
}

function Assert-Fixture([string]$Fixture) {
    Require ((Invoke-Git $Fixture @('rev-parse','HEAD^{tree}')) -ceq $FixtureTree)
    Require ((Invoke-Git $Fixture @('rev-list','--count','HEAD')) -ceq '688')
    Require ((Invoke-Git $Fixture @('rev-list','--max-parents=0','HEAD')) -ceq $FixtureRoot)
    [string[]]$files = (Invoke-Git $Fixture @('ls-tree','-rz','--name-only','HEAD')).Split(
        [char]0, [StringSplitOptions]::RemoveEmptyEntries)
    [Array]::Sort[string]($files, [StringComparer]::Ordinal)
    $lf = [string][char]10
    $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData(
        [Text.Encoding]::UTF8.GetBytes(($files -join $lf) + $lf))).ToLowerInvariant()
    Require ("$($files.Count):$($hash.Substring(0,12))" -ceq $FixtureId)
}

function Save-Packet {
    $json = ConvertTo-Json -InputObject $Packet -Depth 16 -Compress
    $copy = ConvertFrom-Json -InputObject $json -AsHashtable -Depth 16
    Require ((ConvertTo-Json -InputObject $copy -Depth 16 -Compress) -ceq $json)
    $temporary = Join-Path $Raw 'projection.json'
    [IO.File]::WriteAllText($temporary, $json)
    [IO.File]::Move($temporary, (Join-Path $Public 'diagnostic.json'), $true)
}

function Invoke-Build([string]$Label, [string]$Revision, [string]$Patch) {
    $script:Stage = "build-$Label-overlay"
    $build = Join-Path $Raw $Label
    New-DetachedClone $Source $build $Revision
    $tree = Invoke-Git $build @('rev-parse','HEAD^{tree}')
    Require ($tree -cmatch '\A[0-9a-f]{40}\z')
    $lock = Get-Digest "$build/Cargo.lock"
    $before = [ordered]@{}
    foreach ($path in $Overlay.Keys) {
        $before[$path] = $null
        if (Test-Path -LiteralPath "$build/$path") { $before[$path] = Get-Digest "$build/$path" }
    }
    $null = Invoke-Git $build @('apply','--check',$Patch)
    $null = Invoke-Git $build @('apply',$Patch)
    foreach ($path in $Overlay.Keys) { Require ((Get-Digest "$build/$path") -ceq $Overlay[$path]) }
    $script:Stage = "build-$Label-compile"
    $target = Join-Path $Raw "target-$Label"
    $compileArgs = @('+1.98.1','build','--locked','--release','--bin','ledgerful','--target',$TargetTriple,'--target-dir',$target)
    $compilation = Invoke-Captured $build 'cargo.exe' $compileArgs $TimeLimits.buildSeconds
    $record = [ordered]@{
        revision=$Revision; tree=$tree; lockSha256=$lock; preOverlaySha256=$before; postOverlaySha256=$Overlay
        wallMs=$compilation.wallMs; exitCode=$compilation.exitCode; timedOut=$compilation.timedOut; binarySha256=$null
    }
    $Packet.builds[$Label] = $record
    Save-Packet
    Require (!$compilation.timedOut -and $compilation.exitCode -eq 0)
    Require ((Get-Digest "$build/Cargo.lock") -ceq $lock)
    $binary = Join-Path $target "$TargetTriple/release/ledgerful.exe"
    $record.binarySha256 = Get-Digest $binary
    Save-Packet
    return @{binary=$binary; cwd=$build}
}

function Invoke-Arm([string]$Arm) {
    $script:Stage = 'arm-fixture'
    $label, $state = $Arm.Split('-')
    $fixture = Join-Path $Raw "$Arm-fixture"
    New-DetachedClone $fixtureSeed $fixture $Candidate
    Assert-Fixture $fixture
    $output = Join-Path $Raw "$Arm-receipts"
    $null = New-Item -ItemType Directory -Path $output
    $build = $builds[$label]
    Require ((Get-Digest $build.binary) -ceq $Packet.builds[$label].binarySha256)
    $record = [ordered]@{
        revision=$revisions[$label]; fixtureId=$FixtureId; probe=$state
        binarySha256=$Packet.builds[$label].binarySha256; status='partial'
        receipts=[ordered]@{}; invocations=[ordered]@{}; contextFrames=@(); budget=$null
    }
    $Packet.arms[$Arm] = $record
    Save-Packet
    $receiptPaths = [System.Collections.Generic.List[string]]::new()
    foreach ($workload in $Workloads) {
        $script:Stage = 'bench-invoke'
        $probe = $state -ceq 'on' -and $workload -ceq 'context-warm'
        $receiptPath = Join-Path $output "$workload.json"
        $benchArgs = @('bench','--fixture',$fixture,'--workload',$workload,'--runs',"$Runs",'--host-class',$HostClass,'--output',$receiptPath,'--json')
        try {
            if ($probe) { $env:LEDGERFUL_BENCH_RSS_ATTRIBUTION = '1' }
            $run = Invoke-Captured $build.cwd $build.binary $benchArgs $TimeLimits.workloadSeconds
        }
        finally { $env:LEDGERFUL_BENCH_RSS_ATTRIBUTION = $null }
        $record.invocations[$workload] = [ordered]@{
            exitCode=$run.exitCode; timedOut=$run.timedOut; wallMs=$run.wallMs; status='validating'
        }
        Save-Packet
        $receipt = $null
        try {
            $script:Stage = 'receipt-validation-failed'
            if (!$run.timedOut -and $run.exitCode -eq 0) {
                $rawReceipt = Read-Capture $receiptPath
                Require ($rawReceipt.Trim() -ceq (Read-Capture $run.stdout).Trim())
                $receipt = Project-Receipt (Read-StrictJson $rawReceipt) $workload $revisions[$label] $Runs
            }
            $script:Stage = 'frame-validation-failed'
            $frames = Project-Frames (Read-Capture $run.stderr 4194304) $probe $run $receipt $Runs $identities
        } catch {
            $record.invocations[$workload].status = $Stage
            Save-Packet
            return $false
        }
        if ($workload -ceq 'context-warm') { $record.contextFrames = $frames }
        if ($null -eq $receipt) {
            $record.invocations[$workload].status = 'child-exit'
            if ($run.timedOut) { $record.invocations[$workload].status = 'timed-out' }
            Save-Packet
            return $false
        }
        $record.receipts[$workload] = $receipt
        $record.invocations[$workload].status = 'complete'
        $receiptPaths.Add($receiptPath)
        Save-Packet
    }
    $script:Stage = 'budget-invoke'
    $check = Invoke-Captured $build.cwd $build.binary (@('bench','--json','--check') + $receiptPaths.ToArray()) $TimeLimits.budgetSeconds
    $record.budget = [ordered]@{status='unvalidated'; exitCode=$check.exitCode; timedOut=$check.timedOut}
    Save-Packet
    $script:Stage = 'budget-validation'
    Require (!$check.timedOut)
    $null = Project-Frames (Read-Capture $check.stderr 4194304) $false $check $null $Runs $identities
    $record.budget = Project-Verdict (Read-StrictJson (Read-Capture $check.stdout)) $record.receipts $check.exitCode
    $record.status = 'complete'
    Save-Packet
    return ($check.exitCode -eq 0)
}

function Add-Comparisons {
    foreach ($label in $revisions.Keys) {
        $on = $Packet.arms["$label-on"]
        $off = $Packet.arms["$label-off"]
        if (!$on.receipts.Contains('context-warm') -or !$off.receipts.Contains('context-warm')) { continue }
        $a = $on.receipts['context-warm']
        $b = $off.receipts['context-warm']
        $peakDelta = $null
        if ($null -ne $a.peakRssBytes -and $null -ne $b.peakRssBytes) {
            $peakDelta = [bigint]$a.peakRssBytes - [bigint]$b.peakRssBytes
        }
        $frameAttribution = [long]0
        $frameRefresh = [long]0
        foreach ($frame in $on.contextFrames) {
            $frameAttribution += $frame.attributionWallUs
            $frameRefresh += $frame.refreshWallUs
        }
        $Packet.comparisons[$label] = [ordered]@{
            onMinusOffP50Ms=[long]$a.p50Ms - [long]$b.p50Ms
            onMinusOffP95Ms=[long]$a.p95Ms - [long]$b.p95Ms
            onMinusOffPeakBytes=$peakDelta; onMinusOffNonzeroTicks=[long]$a.rssTicks - [long]$b.rssTicks
            onMinusOffWallMs=$on.invocations['context-warm'].wallMs - $off.invocations['context-warm'].wallMs
            measuredFrameAttributionUs=$frameAttribution; measuredFrameRefreshUs=$frameRefresh
        }
    }
}

$Stage = 'guard'
$failure = $false
$Raw = $null
$Public = $null
$Packet = $null
$env:LEDGERFUL_BENCH_RSS_ATTRIBUTION = $null
$env:GIT_TERMINAL_PROMPT = '0'
$env:GIT_NO_REPLACE_OBJECTS = '1'
try {
    Require ($IsWindows -and $env:GITHUB_ACTIONS -ceq 'true' -and
        $env:GITHUB_REPOSITORY -ceq $Repository -and $env:GITHUB_REF -ceq $DiagnosticRef -and
        $env:GITHUB_EVENT_NAME -ceq 'push' -and $env:RUNNER_OS -ceq 'Windows' -and $env:RUNNER_ARCH -ceq 'X64' -and
        $env:GITHUB_SHA -cmatch '\A[0-9a-f]{40}\z')
    $Source = (Resolve-Path $env:GITHUB_WORKSPACE).Path
    $tempRoot = (Resolve-Path $env:RUNNER_TEMP).Path
    $Raw = Join-Path $tempRoot ("engine-rss-raw-" + [guid]::NewGuid().ToString('N'))
    $Public = Join-Path $Source 'engine-rss-public'
    Require (!(Test-Path -LiteralPath $Public))
    $null = New-Item -ItemType Directory -Path $Raw,$Public
    # Two builds and four batteries at the recorded original timings already
    # consume about 77 minutes, before toolchain, cloning, and probe overhead.
    # The extra orchestration time does not change any benchmark threshold.
    $Deadline = [DateTime]::UtcNow.AddSeconds($TimeLimits.diagnosticSeconds)
    $Sequence = 0
    $identities = @{}
    $Packet = [ordered]@{
        schemaVersion=1; collectorRevision=$env:GITHUB_SHA; status='partial'; failure=$null; fixture=[ordered]@{
            revision=$Candidate; tree=$FixtureTree; id=$FixtureId; files=1318; commits=688; root=$FixtureRoot
            verified=$false
        }
        probeSha256=$ProbeHash; hostClass=$HostClass; targetTriple=$TargetTriple
        dependencies='fresh-job-no-actions-cache'; workloads=$Workloads; runs=$Runs
        timeLimits=$TimeLimits
        plannedOrder=@('main-off','candidate-on','main-on','candidate-off')
        runner=$null; toolchain=[ordered]@{}; builds=[ordered]@{}; arms=[ordered]@{}; comparisons=[ordered]@{}
    }
    Save-Packet
    $Stage = 'provenance'
    Require ((Invoke-Git $Source @('rev-parse','--is-shallow-repository')) -ceq 'false')
    Require ((Invoke-Git $Source @('rev-parse','HEAD')) -ceq $env:GITHUB_SHA)
    Require ((Invoke-Git $Source @('show','-s','--format=%P','HEAD')) -ceq $Candidate)
    Require ((Invoke-Git $Source @('show','-s','--format=%P',$Candidate)) -ceq "$Main $CandidateHead")
    $fixtureSeed = Join-Path $Raw 'fixture-seed'
    New-DetachedClone $Source $fixtureSeed $Candidate
    Assert-Fixture $fixtureSeed
    $Packet.fixture.verified = $true
    Save-Packet

    $Stage = 'overlay'
    $patch = Join-Path $Raw 'probe.patch'
    $patchText = [IO.File]::ReadAllText("$PSScriptRoot/probe.patch").Replace([string][char]13 + [char]10, [string][char]10)
    [IO.File]::WriteAllText($patch, $patchText)
    Require ((Get-Digest $patch) -ceq $ProbeHash)
    $Stage = 'runner-metadata'
    $Packet.runner = Project-RunnerImage $env:ImageOS $env:ImageVersion ([Environment]::OSVersion.Version)
    Save-Packet
    $Stage = 'rust-install'
    $env:CARGO_HOME = Join-Path $Raw 'cargo'
    $install = Invoke-Captured $Raw 'rustup.exe' @('toolchain','install','1.98.1','--profile','minimal') 360
    $Packet.toolchain.install = [ordered]@{exitCode=$install.exitCode; timedOut=$install.timedOut; wallMs=$install.wallMs}
    Save-Packet
    Require (!$install.timedOut -and $install.exitCode -eq 0)
    $Stage = 'toolchain-version'
    foreach ($tool in 'rustc','cargo') {
        $version = Invoke-Captured $Raw "$tool.exe" @('+1.98.1','--version') 30
        Require (!$version.timedOut -and $version.exitCode -eq 0 -and
            (Read-Capture $version.stdout) -cmatch "\A$tool (1\.98\.1) \(([0-9a-f]{9,40}) ([0-9-]{10})\)\s*\z")
        $Packet.toolchain[$tool] = [ordered]@{version=$Matches[1]; commit=$Matches[2]; date=$Matches[3]}
    }
    Save-Packet
    $revisions = [ordered]@{main=$Main; candidate=$Candidate}
    $builds = @{}
    foreach ($label in $revisions.Keys) { $builds[$label] = Invoke-Build $label $revisions[$label] $patch }
    foreach ($arm in $Packet.plannedOrder) {
        if (!(Invoke-Arm $arm)) { $failure = $true }
    }

    $Stage = 'paired-comparison'
    Add-Comparisons
    if (@($Packet.arms.Values | Where-Object { $_.status -ceq 'complete' }).Count -eq 4) {
        $Packet.status = 'complete'
    } else { $Packet.failure = 'incomplete-battery' }
    Save-Packet
}
catch {
    $failure = $true
    if ($null -ne $Packet) {
        $Packet.status = 'partial'
        $Packet.failure = $Stage
        try { Save-Packet } catch {}
    }
    [Console]::Error.WriteLine("Engine RSS diagnostic stopped ($Stage).")
}
finally {
    if ($Raw -and [IO.Path]::GetDirectoryName($Raw) -eq $tempRoot -and
        [IO.Path]::GetFileName($Raw) -cmatch '\Aengine-rss-raw-[0-9a-f]{32}\z') {
        try { Remove-Item -LiteralPath $Raw -Recurse -Force -ErrorAction Stop }
        catch {
            $failure = $true
            [Console]::Error.WriteLine('Engine RSS diagnostic temporary cleanup failed.')
        }
    }
}
if ($failure) { exit 1 }
exit 0
