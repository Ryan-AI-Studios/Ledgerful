#!/usr/bin/env bash
set -e

if ! command -v ledgerful >/dev/null 2>&1; then
    echo "ledgerful not found on PATH. Please run 'cargo install --path . --features mcp' first."
    exit 1
fi

PAYLOAD1='{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "1.0.0"}},"id":1}'
LEN1=${#PAYLOAD1}

PAYLOAD2='{"jsonrpc":"2.0","method":"tools/list","params":{},"id":2}'
LEN2=${#PAYLOAD2}

PAYLOAD3='{"jsonrpc":"2.0","method":"tools/call","params":{"name":"ledger_status","arguments":{}},"id":3}'
LEN3=${#PAYLOAD3}

OUTPUT=$(
{
  printf "Content-Length: %d\r\n\r\n%s" "$LEN1" "$PAYLOAD1"
  printf "Content-Length: %d\r\n\r\n%s" "$LEN2" "$PAYLOAD2"
  printf "Content-Length: %d\r\n\r\n%s" "$LEN3" "$PAYLOAD3"
} | ledgerful mcp
)

if ! echo "$OUTPUT" | grep -q "protocolVersion"; then
    echo "Smoke test failed: missing initialize response."
    echo "Output: $OUTPUT"
    exit 1
fi

if ! echo "$OUTPUT" | grep -q "\"method\":\"tools/list\"" && ! echo "$OUTPUT" | grep -q "\"tools\""; then
    echo "Smoke test failed: missing tools/list response."
    echo "Output: $OUTPUT"
    exit 1
fi

if ! echo "$OUTPUT" | grep -q "\"id\":3"; then
    echo "Smoke test failed: missing ledger_status response."
    echo "Output: $OUTPUT"
    exit 1
fi

FRAME_COUNT=$(echo "$OUTPUT" | grep -c "Content-Length:")
if [ "$FRAME_COUNT" -lt 3 ]; then
    echo "Smoke test failed: expected at least 3 Content-Length frames, found $FRAME_COUNT"
    exit 1
fi

echo -e "\nMCP Smoke Test Passed"
