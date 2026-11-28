$Workloads = @('status-warm','search-warm','context-warm','doctor-warm','hotspots-warm','impact-warm','review-warm')
$FixtureId = '1318:a633a5e8055c'
$FixtureRoot = '85ace228532eb523a4840c34d2ae59850b6cb6ae'
$HostClass = 'ci-windows-latest'
$TargetTriple = 'x86_64-pc-windows-msvc'

function Require($Condition) {
    if (!$Condition) { throw 'diagnostic validation failed' }
}

function Read-JsonNode($Node) {
    switch ($Node.ValueKind.ToString()) {
        Object {
            $map = [System.Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
            foreach ($property in $Node.EnumerateObject()) {
                Require (!$map.ContainsKey($property.Name))
                $map.Add($property.Name, (Read-JsonNode $property.Value))
            }
            return ,$map
        }
        Array {
            $items = [System.Collections.Generic.List[object]]::new()
            foreach ($item in $Node.EnumerateArray()) { $items.Add((Read-JsonNode $item)) }
            return ,($items.ToArray())
        }
        Number { return $Node.GetUInt64() }
        String { return $Node.GetString() }
        True { return $true }
        False { return $false }
        Null { return $null }
        default { throw 'unsupported JSON value' }
    }
}

function Read-StrictJson([string]$Json) {
    Require ($Json.Length -le 1048576)
    $document = [System.Text.Json.JsonDocument]::Parse($Json, [System.Text.Json.JsonDocumentOptions]@{MaxDepth=12})
    try { return ,(Read-JsonNode $document.RootElement) }
    finally { $document.Dispose() }
}

function Require-Keys($Object, [string]$Keys) {
    $names = $Keys.Split(' ')
    Require ($Object -is [System.Collections.Generic.Dictionary[string,object]] -and $Object.Count -eq $names.Count)
    foreach ($name in $names) { Require ($Object.ContainsKey($name)) }
}

function Read-UInt($Value, [uint64]$Min = 0, [uint64]$Max = [uint64]::MaxValue) {
    Require ($Value -is [uint64] -and $Value -ge $Min -and $Value -le $Max)
    return $Value
}

function Read-OptionalUInt($Value) {
    if ($null -ne $Value) { return (Read-UInt $Value) }
}

function Read-Enum($Value, [string[]]$Allowed) {
    Require ($Value -is [string] -and $Allowed -ccontains $Value)
    return $Value
}

function Project-RunnerImage([string]$Image, [string]$Version, [version]$OsVersion) {
    $imageName = Read-Enum $Image @('win22','win25','win25-vs2026')
    Require ($Version -cmatch '\A[0-9]{4,8}(?:\.[0-9]{1,8}){1,4}\z' -and $null -ne $OsVersion)
    return [ordered]@{
        image=$imageName; imageVersion=$Version
        osVersion=@($OsVersion.Major,$OsVersion.Minor,$OsVersion.Build,$OsVersion.Revision)
    }
}

function Project-Receipt($Raw, [string]$Workload, [string]$Revision, [int]$Runs) {
    Require-Keys $Raw 'schemaVersion kind workload buildSha gitHead n samplesMs observedMs p50Ms p95Ms startState fixtureId fixtureFiles fixtureCommits fixtureRootCommit childArgs hostClass peakRssBytes rssTicks rssMeasuredSamples samplerWallMs setupMs warmupMs os targetTriple comparable'
    $null = Read-UInt $Raw.schemaVersion 1 1
    $null = Read-Enum $Raw.kind 'workloadBench'
    $null = Read-Enum $Raw.workload $Workload
    $null = Read-Enum $Raw.buildSha $Revision.Substring(0,12)
    $null = Read-Enum $Raw.gitHead $Revision.Substring(0,12)
    $null = Read-Enum $Raw.startState 'warm'
    $null = Read-Enum $Raw.fixtureId $FixtureId
    $null = Read-UInt $Raw.fixtureFiles 1318 1318
    $null = Read-UInt $Raw.fixtureCommits 688 688
    $null = Read-Enum $Raw.fixtureRootCommit $FixtureRoot
    $null = Read-Enum $Raw.hostClass $HostClass
    $null = Read-Enum $Raw.os 'windows'
    $null = Read-Enum $Raw.targetTriple $TargetTriple
    $null = Read-UInt $Raw.n $Runs $Runs
    Require ($Raw.comparable -is [bool] -and $Raw.comparable -eq ($Runs -ge 3))
    Require ($Raw.childArgs -is [array] -and $Raw.childArgs.Count -le 32)
    foreach ($arg in $Raw.childArgs) { Require ($arg -is [string] -and $arg.Length -le 256) }
    foreach ($series in 'samplesMs','observedMs') {
        Require ($Raw[$series] -is [array] -and $Raw[$series].Count -eq $Runs)
        foreach ($item in $Raw[$series]) { $null = Read-UInt $item 0 180000 }
    }
    Require (($Raw.samplesMs -join ',') -ceq (($Raw.observedMs | Sort-Object) -join ','))
    # Rust percentile_sorted rounds nonnegative ranks away from zero.
    $rank50 = [int][Math]::Round(0.50 * ($Runs - 1), [MidpointRounding]::AwayFromZero)
    $rank95 = [int][Math]::Round(0.95 * ($Runs - 1), [MidpointRounding]::AwayFromZero)
    $p50 = Read-UInt $Raw.p50Ms 0 180000
    $p95 = Read-UInt $Raw.p95Ms 0 180000
    Require ($p50 -eq $Raw.samplesMs[$rank50] -and $p95 -eq $Raw.samplesMs[$rank95])
    $peak = Read-OptionalUInt $Raw.peakRssBytes
    $ticks = Read-UInt $Raw.rssTicks 0 ($Runs * 1000)
    $measured = Read-UInt $Raw.rssMeasuredSamples 0 $Runs
    Require ($ticks -ge $measured -and ($ticks -eq 0) -eq ($measured -eq 0) -and
        ($null -eq $peak) -eq ($measured -eq 0) -and ($null -eq $peak -or $peak -gt 0))
    return [ordered]@{
        workload = $Workload; n = $Runs; observedMs = $Raw.observedMs
        p50Ms = $p50
        p95Ms = $p95
        peakRssBytes = $peak; rssTicks = $ticks; rssMeasuredSamples = $measured
        samplerWallMs = Read-UInt $Raw.samplerWallMs 0 ($Runs * 180000)
        setupMs = Read-UInt $Raw.setupMs 0 180000
        warmupMs = Read-UInt $Raw.warmupMs 0 180000
    }
}

function Get-ProcessIdToken($Identities, [uint64]$PidValue, $Creation) {
    # Sequential labels are scoped to the diagnostic run. An unknown creation
    # identity is never shared across invocations.
    $key = "$PidValue/$Creation"
    if (!$Identities.ContainsKey($key)) { $Identities[$key] = 1 + $Identities.Count }
    return $Identities[$key]
}

function Project-Frames([string]$Stderr, [bool]$Enabled, $Invocation, $Receipt, [int]$Runs, $Identities) {
    Require ($Stderr.Length -le 4194304)
    $prefix = 'LEDGERFUL_RSS_ATTRIBUTION_V1 '
    $frames = [System.Collections.Generic.List[object]]::new()
    $lastSample = 0
    $lastTimestamp = [long]$Invocation.startedMs
    $totalTicks = 0
    $maxPeak = $null
    foreach ($line in ($Stderr -split '\r?\n')) {
        if (!$line.Contains('LEDGERFUL_RSS_ATTRIBUTION')) { continue }
        Require ($Enabled -and $line.StartsWith($prefix, [StringComparison]::Ordinal))
        $raw = Read-StrictJson $line.Substring($prefix.Length)
        Require-Keys $raw 'sample phase tickIntervalMs refreshWallUs attributionWallUs nonzeroTicks sampleMs peak'
        $sample = Read-UInt $raw.sample ($lastSample + 1) $Runs
        $null = Read-Enum $raw.phase 'timed'
        $null = Read-UInt $raw.tickIntervalMs 250 250
        $ticks = Read-UInt $raw.nonzeroTicks 1 1000
        $sampleMs = Read-UInt $raw.sampleMs 0 180000
        $refresh = Read-UInt $raw.refreshWallUs 0 180000000
        $attribution = Read-UInt $raw.attributionWallUs 0 180000000
        $peak = $raw.peak
        Require-Keys $peak 'utcUnixMs nonzeroTick rootPid rootCreationFiletime sampledTreeWorkingSetBytes unattributedWorkingSetBytes missedReads rejectedIdentity attributionWallUs processes'
        $timestamp = Read-UInt $peak.utcUnixMs $lastTimestamp $Invocation.endedMs
        $tick = Read-UInt $peak.nonzeroTick 1 $ticks
        $null = Read-UInt $peak.rootPid $Invocation.processId $Invocation.processId
        $epoch = [uint64]116444736000000000
        $maxCreation = $epoch + ([uint64]$timestamp * [uint64]10000) + [uint64]9999
        $rootCreation = $null
        if ($null -ne $peak.rootCreationFiletime) {
            $rootCreation = Read-UInt $peak.rootCreationFiletime $epoch $maxCreation
            Require ($rootCreation -eq $Invocation.creationFiletime)
        }
        $rootId = Get-ProcessIdToken $Identities $peak.rootPid ($rootCreation ?? "unknown/$($Invocation.sequence)")
        $peakBytes = Read-UInt $peak.sampledTreeWorkingSetBytes 1
        $residual = Read-UInt $peak.unattributedWorkingSetBytes 0 $peakBytes
        $peakAttribution = Read-UInt $peak.attributionWallUs 0 $attribution
        $missed = Read-UInt $peak.missedReads 0 65536
        $rejected = Read-UInt $peak.rejectedIdentity 0 65536
        Require ($peak.processes -is [array] -and $peak.processes.Count -le 512)
        Require (($residual -eq 0 -or ($missed + $rejected) -gt 0) -and
            ($null -ne $rootCreation -or ($missed -gt 0 -and $peak.processes.Count -eq 0)))
        $rows = @{}
        [decimal]$sum = $residual
        foreach ($row in $peak.processes) {
            Require-Keys $row 'pid parentPid creationFiletime parentCreationFiletime image sampledWorkingSetBytes rereadWorkingSetBytes rereadPrivateCommitBytes'
            $rowPid = Read-UInt $row.pid 1 ([uint32]::MaxValue)
            $null = Read-UInt $row.parentPid 1 ([uint32]::MaxValue)
            Require ($rowPid -ne $peak.rootPid -and !$rows.ContainsKey($rowPid))
            $null = Read-UInt $row.parentCreationFiletime $rootCreation $maxCreation
            $null = Read-UInt $row.creationFiletime $row.parentCreationFiletime $maxCreation
            Require ($row.image -is [string] -and $row.image.Length -le 80)
            $sum += Read-UInt $row.sampledWorkingSetBytes 0 $peakBytes
            $rows[$rowPid] = $row
        }
        Require ($sum -eq [decimal]$peakBytes)
        $projectedRows = foreach ($row in $peak.processes) {
            $ancestor = $row
            $depth = 1
            while ($ancestor.parentPid -ne $peak.rootPid) {
                Require ($depth -lt 32 -and $rows.ContainsKey($ancestor.parentPid))
                $parent = $rows[$ancestor.parentPid]
                Require ($ancestor.parentCreationFiletime -eq $parent.creationFiletime)
                $ancestor = $parent
                $depth++
            }
            Require ($ancestor.parentCreationFiletime -eq $rootCreation)
            $imageClass = 'OTHER'
            if ($row.image -ieq 'ledgerful.exe') { $imageClass = 'LEDGERFUL' }
            if ($row.image -ieq 'git.exe') { $imageClass = 'GIT' }
            [ordered]@{
                id = Get-ProcessIdToken $Identities $row.pid $row.creationFiletime
                parentId = Get-ProcessIdToken $Identities $row.parentPid $row.parentCreationFiletime
                imageClass = $imageClass
                sampledWorkingSetBytes = $row.sampledWorkingSetBytes
                rereadWorkingSetBytes = Read-UInt $row.rereadWorkingSetBytes
                rereadPrivateCommitBytes = Read-UInt $row.rereadPrivateCommitBytes
            }
        }
        if ($null -ne $Receipt) { Require ($sampleMs -eq $Receipt.observedMs[$sample - 1]) }
        $frames.Add([ordered]@{
            sample = $sample; sampleMs = $sampleMs; tickIntervalMs = 250; nonzeroTicks = $ticks
            peakTick = $tick; peakOffsetMs = $timestamp - $Invocation.startedMs; rootId = $rootId
            rootIdentityKnown = $null -ne $rootCreation
            refreshWallUs = $refresh; attributionWallUs = $attribution; peakAttributionWallUs = $peakAttribution
            sampledTreeWorkingSetBytes = $peakBytes; unattributedWorkingSetBytes = $residual
            missedReads = $missed; rejectedIdentity = $rejected; processes = @($projectedRows)
        })
        $lastSample = $sample
        $lastTimestamp = $timestamp
        $totalTicks += $ticks
        if ($null -eq $maxPeak -or $peakBytes -gt $maxPeak) { $maxPeak = $peakBytes }
    }
    if ($Enabled -and $null -ne $Receipt) {
        Require ($frames.Count -eq $Receipt.rssMeasuredSamples -and $totalTicks -eq $Receipt.rssTicks -and
            $maxPeak -eq $Receipt.peakRssBytes)
    }
    return ,($frames.ToArray())
}

function Project-Verdict($Raw, $Receipts, [int]$ExitCode) {
    Require-Keys $Raw 'schemaVersion kind results status violations unqualified'
    $null = Read-UInt $Raw.schemaVersion 1 1
    $null = Read-Enum $Raw.kind 'perfBudgetCheck'
    $status = Read-Enum $Raw.status @('within-budget','not-within-budget')
    Require ($Raw.results -is [array] -and $Raw.results.Count -eq $Receipts.Count)
    $seen = @{}
    $results = foreach ($result in $Raw.results) {
        Require-Keys $result 'workload hostClass receiptPath n p50Ms p95Ms p50BudgetMs p95BudgetMs interactiveTargetMs peakRssBytes peakRssBudgetBytes targetStatus status reason'
        $workload = Read-Enum $result.workload $Workloads
        Require (!$seen.ContainsKey($workload) -and $Receipts.Contains($workload))
        $seen[$workload] = $true
        $receipt = $Receipts[$workload]
        $null = Read-Enum $result.hostClass $HostClass
        Require ($result.receiptPath -is [string] -and ($null -eq $result.reason -or $result.reason -is [string]))
        foreach ($name in 'n','p50Ms','p95Ms','peakRssBytes') {
            Require ((Read-OptionalUInt $result[$name]) -eq $receipt[$name])
        }
        [ordered]@{
            workload = $workload
            status = Read-Enum $result.status @(
                'within-budget','over-budget','insufficient-samples','fixture-out-of-band','unqualified-host',
                'unqualified-workload','unmeasured-resource','foreign-build','duplicate-receipt','invalid-receipt')
            targetStatus = Read-Enum $result.targetStatus @('met','not-met','no-target')
            p50BudgetMs = Read-OptionalUInt $result.p50BudgetMs
            p95BudgetMs = Read-OptionalUInt $result.p95BudgetMs
            interactiveTargetMs = Read-OptionalUInt $result.interactiveTargetMs
            peakRssBudgetBytes = Read-OptionalUInt $result.peakRssBudgetBytes
        }
    }
    $violations = Read-UInt $Raw.violations 0 $results.Count
    $unqualified = Read-UInt $Raw.unqualified 0 $results.Count
    Require ($violations -eq @($results | Where-Object status -CEQ 'over-budget').Count)
    Require ($unqualified -eq @($results | Where-Object { $_.status -cnotin 'over-budget','within-budget' }).Count)
    Require (($status -ceq 'within-budget') -eq (($violations + $unqualified) -eq 0) -and
        ($status -ceq 'within-budget') -eq ($ExitCode -eq 0))
    return [ordered]@{ status=$status; exitCode=$ExitCode; violations=$violations; unqualified=$unqualified; results=@($results) }
}
