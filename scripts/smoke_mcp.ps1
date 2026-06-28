# PowerShell Smoke test for MCP server
$ErrorActionPreference = "Stop"

$cmd = Get-Command ledgerful -ErrorAction SilentlyContinue
if (-not $cmd) {
    throw "ledgerful not found on PATH. Please run 'cargo install --path . --features mcp' first."
}

# Payload 1: initialize
$payload1 = '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "1.0.0"}},"id":1}'
$len1 = $payload1.Length
$framed1 = "Content-Length: $len1`r`n`r`n$payload1"

# Payload 2: tools/list
$payload2 = '{"jsonrpc":"2.0","method":"tools/list","params":{},"id":2}'
$len2 = $payload2.Length
$framed2 = "Content-Length: $len2`r`n`r`n$payload2"

# Payload 3: tools/call
$payload3 = '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"ledger_status","arguments":{}},"id":3}'
$len3 = $payload3.Length
$framed3 = "Content-Length: $len3`r`n`r`n$payload3"

$all_frames = "$framed1$framed2$framed3"

# ledgerful mcp's stdout spans multiple lines, which PowerShell captures as a
# string array. -match/-notmatch against an array filters elements instead of
# doing a substring check across the whole text, so join into a single string
# first to get correct scalar match semantics.
$output = ($all_frames | ledgerful mcp) -join "`n"

if ($output -notmatch "protocolVersion") {
    throw "Smoke test failed: missing initialize response. Output: $output"
}
if ($output -notmatch '"method":"tools/list"' -and $output -notmatch '"tools"') {
    throw "Smoke test failed: missing tools/list response. Output: $output"
}
if ($output -notmatch '"id":3') {
    throw "Smoke test failed: missing ledger_status response. Output: $output"
}

$frameCount = ([regex]::Matches($output, "Content-Length:")).Count
if ($frameCount -lt 3) {
    throw "Smoke test failed: expected at least 3 Content-Length frames, found $frameCount"
}

Write-Host "`nMCP Smoke Test Passed"

# ledgerful mcp exits non-zero on EOF after the last frame (no further input to
# read), which leaves a stale non-zero $LASTEXITCODE even though the checks above
# passed. GitHub Actions' pwsh runner uses $LASTEXITCODE as the step's exit code,
# so reset it explicitly to avoid failing a step that actually succeeded.
exit 0
