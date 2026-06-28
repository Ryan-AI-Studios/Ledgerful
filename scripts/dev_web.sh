#!/usr/bin/env bash
# Start the Ledgerful Rust API server and the Next.js dev dashboard together.
# Press Ctrl+C in this terminal to stop both processes.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FRONTEND="$(dirname "$ROOT")/ledgerful-frontend"
SPA_DIR="$FRONTEND/out"

cargo run --features web --bin ledgerful -- web start --port 52001 --spa-dir "$SPA_DIR" &
RUST_PID=$!

npm --prefix "$FRONTEND" run dev -- --port 3001 &
NODE_PID=$!

cleanup() {
    kill "$RUST_PID" "$NODE_PID" 2>/dev/null || true
    wait "$RUST_PID" "$NODE_PID" 2>/dev/null || true
}

trap cleanup INT TERM EXIT

echo "Rust server PID $RUST_PID on http://127.0.0.1:52001"
echo "Next.js dev PID $NODE_PID on http://localhost:3001"
echo "Press Ctrl+C to stop both..."

wait
