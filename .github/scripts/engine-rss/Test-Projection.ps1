$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/Projection.ps1"

function Assert-Rejected([scriptblock]$Operation) {
    try { $null = & $Operation }
    catch { return }
    throw 'invalid diagnostic input was accepted'
}

$runner = Project-RunnerImage 'win25-vs2026' '20261123.321.1' ([version]'10.0.26100.1')
Require ($runner.image -ceq 'win25-vs2026' -and $runner.imageVersion -ceq '20261123.321.1')
foreach ($image in 'win22','win25') {
    Require ((Project-RunnerImage $image '20261123.321.1' ([version]'10.0.26100.1')).image -ceq $image)
}
Assert-Rejected { Project-RunnerImage "win25-vs2026`nprivate" '20261123.321.1' ([version]'10.0.26100.1') }
Assert-Rejected { Project-RunnerImage 'win25-vs2026' '20261123.321.1-private' ([version]'10.0.26100.1') }

$rawFrame = @'
{"sample":1,"phase":"timed","tickIntervalMs":250,"refreshWallUs":200,"attributionWallUs":20,"nonzeroTicks":2,"sampleMs":550,"peak":{"utcUnixMs":1700000000520,"nonzeroTick":2,"rootPid":421234,"rootCreationFiletime":133444736000000001,"sampledTreeWorkingSetBytes":630,"unattributedWorkingSetBytes":600,"missedReads":1,"rejectedIdentity":3,"attributionWallUs":10,"processes":[{"pid":421235,"parentPid":421234,"creationFiletime":133444736000000002,"parentCreationFiletime":133444736000000001,"image":"Ledgerful.exe","sampledWorkingSetBytes":10,"rereadWorkingSetBytes":1000,"rereadPrivateCommitBytes":1111},{"pid":421236,"parentPid":421235,"creationFiletime":133444736000000003,"parentCreationFiletime":133444736000000002,"image":"C:\\private\\secret.exe","sampledWorkingSetBytes":20,"rereadWorkingSetBytes":2000,"rereadPrivateCommitBytes":2222}]}}
'@
$prefix = 'LEDGERFUL_RSS_ATTRIBUTION_V1 '
$invocation = @{
    processId=421234; creationFiletime=[uint64]133444736000000001; sequence=1
    startedMs=[long]1700000000000; endedMs=[long]1700000000600
}
$revision = '90a3a0a072f791495607a4e77c188b512a223092'
$rawReceipt = @'
{"schemaVersion":1,"kind":"workloadBench","workload":"context-warm","buildSha":"90a3a0a072f7","gitHead":"90a3a0a072f7","n":1,"samplesMs":[550],"observedMs":[550],"p50Ms":550,"p95Ms":550,"startState":"warm","fixtureId":"1318:a633a5e8055c","fixtureFiles":1318,"fixtureCommits":688,"fixtureRootCommit":"85ace228532eb523a4840c34d2ae59850b6cb6ae","childArgs":["C:\\private\\secret"],"hostClass":"ci-windows-latest","peakRssBytes":630,"rssTicks":2,"rssMeasuredSamples":1,"samplerWallMs":3,"setupMs":100,"warmupMs":200,"os":"windows","targetTriple":"x86_64-pc-windows-msvc","comparable":false}
'@
$receipt = Project-Receipt (Read-StrictJson $rawReceipt) 'context-warm' $revision 1
$frames = Project-Frames ($prefix + $rawFrame) $true $invocation $receipt 1 @{}
Require ($frames.Count -eq 1 -and $frames[0].processes.Count -eq 2)
Require ($frames[0].processes[0].imageClass -ceq 'LEDGERFUL' -and $frames[0].processes[1].imageClass -ceq 'OTHER')
Require ($frames[0].processes[0].parentId -eq $frames[0].rootId -and
    $frames[0].processes[1].parentId -eq $frames[0].processes[0].id)
Require ($frames[0].unattributedWorkingSetBytes -eq 600 -and $frames[0].peakOffsetMs -eq 520)
Require ((Project-Frames 'private stderr' $false $invocation $receipt 1 @{}).Count -eq 0)
Require ((Project-Frames ($prefix + $rawFrame) $true $invocation $null 1 @{}).Count -eq 1)
Assert-Rejected { Project-Frames ($prefix + $rawFrame) $false $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames '' $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + '{') $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + $rawFrame.Replace('"sample":1','"sample":1,"unknown":1')) $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + $rawFrame.Replace('"sample":1','"sample":1,"sample":1')) $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + $rawFrame.Replace(':600',':601')) $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + $rawFrame.Replace(':630',':631').Replace(':600',':601')) $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + $rawFrame.Replace('"parentCreationFiletime":133444736000000002','"parentCreationFiletime":133444736000000001')) $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + $rawFrame.Replace(':1700000000520',':1700000000601')) $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + $rawFrame.Replace(':421236',':421235')) $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Frames ($prefix + $rawFrame.Replace('"nonzeroTicks":2','"nonzeroTicks":3')) $true $invocation $receipt 1 @{} }
Assert-Rejected { Project-Receipt (Read-StrictJson $rawReceipt.Replace('"n":1','"n":"1"')) 'context-warm' $revision 1 }
Assert-Rejected { Project-Receipt (Read-StrictJson $rawReceipt.Replace('"n":1','"n":1,"unknown":1')) 'context-warm' $revision 1 }
Assert-Rejected { Project-Receipt (Read-StrictJson $rawReceipt.Replace('"p95Ms":550','"p95Ms":1')) 'context-warm' $revision 1 }
Assert-Rejected { Project-Receipt (Read-StrictJson $rawReceipt.Replace('"p50Ms":550','"p50Ms":1')) 'context-warm' $revision 1 }
$pair = $rawReceipt.Replace('"n":1','"n":2').Replace('"samplesMs":[550]','"samplesMs":[300,550]').Replace('"observedMs":[550]','"observedMs":[550,300]')
$null = Project-Receipt (Read-StrictJson $pair) 'context-warm' $revision 2
Assert-Rejected { Project-Receipt (Read-StrictJson $pair.Replace('"p50Ms":550','"p50Ms":300')) 'context-warm' $revision 2 }

$rawVerdict = @'
{"schemaVersion":1,"kind":"perfBudgetCheck","results":[{"workload":"context-warm","hostClass":"ci-windows-latest","receiptPath":"C:\\private\\secret","n":1,"p50Ms":550,"p95Ms":550,"p50BudgetMs":800,"p95BudgetMs":900,"interactiveTargetMs":null,"peakRssBytes":630,"peakRssBudgetBytes":620,"targetStatus":"no-target","status":"over-budget","reason":"secret"}],"status":"not-within-budget","violations":1,"unqualified":0}
'@
$receipts = [ordered]@{'context-warm'=$receipt}
$verdict = Project-Verdict (Read-StrictJson $rawVerdict) $receipts 1
Require ($verdict.exitCode -eq 1 -and $verdict.status -ceq 'not-within-budget' -and $verdict.results[0].status -ceq 'over-budget')
Assert-Rejected { Project-Verdict (Read-StrictJson $rawVerdict) $receipts 0 }
Assert-Rejected { Project-Verdict (Read-StrictJson $rawVerdict.Replace('"violations":1','"violations":0')) $receipts 1 }
$public = ConvertTo-Json -InputObject @{frames=$frames; verdict=$verdict; receipt=$receipt} -Depth 16 -Compress
Require ($public -cnotmatch 'secret|private\\|42123[456]|13344473600000000[123]|receiptPath|childArgs|creationFiletime|utcUnixMs')
Require ((ConvertFrom-Json $public -AsHashtable).frames[0].processes[1].imageClass -ceq 'OTHER')
Write-Output 'Engine RSS projection tests passed.'
