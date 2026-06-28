#!/usr/bin/env pwsh
# Start the Ledgerful Rust API server and the Next.js dev dashboard together.
# Press Ctrl+C in this window to stop both processes.

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path $PSScriptRoot
$frontend = Join-Path (Split-Path $repoRoot) "ledgerful-frontend"
$spaDir = Join-Path $frontend "out"

$rustProc = Start-Process -FilePath "cargo" `
    -ArgumentList @("run", "--features", "web", "--bin", "ledgerful", "--", "web", "start", "--port", "52001", "--spa-dir", $spaDir) `
    -WorkingDirectory $repoRoot `
    -NoNewWindow `
    -PassThru

$nodeProc = Start-Process -FilePath "npm" `
    -ArgumentList @("--prefix", $frontend, "run", "dev", "--", "--port", "3001") `
    -WorkingDirectory $frontend `
    -NoNewWindow `
    -PassThru

Write-Host "Rust server PID $($rustProc.Id) on http://127.0.0.1:52001"
Write-Host "Next.js dev PID $($nodeProc.Id) on http://localhost:3001"
Write-Host "Press Ctrl+C to stop both..."

try {
    while ($true) {
        Start-Sleep -Seconds 1
    }
}
finally {
    Stop-Process -Id $rustProc.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $nodeProc.Id -Force -ErrorAction SilentlyContinue
    Write-Host "Stopped both servers."
}
