#!/usr/bin/env bash
# Unit matrix for scripts/npm-publish-decision.sh three-state policy (DoD-8 / 0101).
# Run from repo root: bash scripts/test-npm-publish-decision.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DECISION="${SCRIPT_DIR}/npm-publish-decision.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0

# Fixture package.json with fixed version/pin (not the live tree).
PKG="$TMP/package.json"
cat >"$PKG" <<'EOF'
{
  "name": "@ledgerful/mcp-server",
  "version": "9.9.9",
  "ledgerfulEngineTag": "v0.9.9"
}
EOF

run_case() {
  local name="$1"
  local expect_exit="$2"
  local expect_out="$3"
  shift 3
  local out=""
  local rc=0
  set +e
  out="$(
    env "$@" bash "$DECISION" "$PKG" 2>/dev/null
  )"
  rc=$?
  set -e
  out="$(printf '%s' "$out" | tr -d '\r' | head -n1 | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"

  if [ "$rc" -eq "$expect_exit" ] && [ "$out" = "$expect_out" ]; then
    echo "PASS: ${name} (exit ${rc}, out=${out})"
    pass=$((pass + 1))
  else
    echo "FAIL: ${name} — expected exit ${expect_exit} out=${expect_out}; got exit ${rc} out=${out}" >&2
    fail=$((fail + 1))
  fi
}

# 1. Version not on registry → publish (view fails)
run_case "absent version (view rc nonzero)" 0 "publish" \
  LEDGERFUL_NPM_VIEW_VERSION_RC=1 \
  LEDGERFUL_NPM_VIEW_VERSION_OUTPUT=

# 2. Version not on registry → publish (empty output, rc 0)
run_case "absent version (empty output)" 0 "publish" \
  LEDGERFUL_NPM_VIEW_VERSION_RC=0 \
  LEDGERFUL_NPM_VIEW_VERSION_OUTPUT=

# 3. Version on registry, pin matches → skip
run_case "present version pin matches → skip" 2 "skip" \
  LEDGERFUL_NPM_VIEW_VERSION_RC=0 \
  LEDGERFUL_NPM_VIEW_VERSION_OUTPUT=9.9.9 \
  LEDGERFUL_NPM_VIEW_PIN_RC=0 \
  LEDGERFUL_NPM_VIEW_PIN_OUTPUT=v0.9.9

# 4. Version on registry, pin differs → fail DoD-8
run_case "present version pin differs → fail DoD-8" 1 "fail" \
  LEDGERFUL_NPM_VIEW_VERSION_RC=0 \
  LEDGERFUL_NPM_VIEW_VERSION_OUTPUT=9.9.9 \
  LEDGERFUL_NPM_VIEW_PIN_RC=0 \
  LEDGERFUL_NPM_VIEW_PIN_OUTPUT=v0.1.0

# 5. Version on registry, pin unreadable → exit 3
run_case "present version pin unreadable → error" 3 "fail" \
  LEDGERFUL_NPM_VIEW_VERSION_RC=0 \
  LEDGERFUL_NPM_VIEW_VERSION_OUTPUT=9.9.9 \
  LEDGERFUL_NPM_VIEW_PIN_RC=1 \
  LEDGERFUL_NPM_VIEW_PIN_OUTPUT=

# 6. Version on registry, pin empty → exit 3
run_case "present version pin empty → error" 3 "fail" \
  LEDGERFUL_NPM_VIEW_VERSION_RC=0 \
  LEDGERFUL_NPM_VIEW_VERSION_OUTPUT=9.9.9 \
  LEDGERFUL_NPM_VIEW_PIN_RC=0 \
  LEDGERFUL_NPM_VIEW_PIN_OUTPUT=

echo ""
echo "npm-publish-decision matrix: ${pass} passed, ${fail} failed"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
exit 0
