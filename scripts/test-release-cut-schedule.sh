#!/usr/bin/env bash
# Assert release-cut.yml Thursday cadence + Gate B weekday unchanged (0218).
# Run from repo root: bash scripts/test-release-cut-schedule.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CUT_YML="${REPO_ROOT}/.github/workflows/release-cut.yml"
STATE_YML="${REPO_ROOT}/.github/workflows/release-state.yml"

pass=0
fail=0

assert_pass() {
  local name="$1"
  echo "PASS: ${name}"
  pass=$((pass + 1))
}

assert_fail() {
  local name="$1"
  local detail="${2:-}"
  echo "FAIL: ${name}${detail:+ — ${detail}}" >&2
  fail=$((fail + 1))
}

if [[ ! -f "$CUT_YML" ]]; then
  assert_fail "release-cut.yml present" "missing ${CUT_YML}"
else
  assert_pass "release-cut.yml present"
fi

if [[ ! -f "$STATE_YML" ]]; then
  assert_fail "release-state.yml present" "missing ${STATE_YML}"
else
  assert_pass "release-state.yml present"
fi

# Thursday cut schedule (0218): 10:17 America/New_York on day-of-week 4.
if grep -qE 'cron:[[:space:]]*"17 10 \* \* 4"' "$CUT_YML"; then
  assert_pass "release-cut.yml cron is Thursday 17 10 * * 4"
else
  assert_fail "release-cut.yml cron is Thursday 17 10 * * 4" \
    "expected cron: \"17 10 * * 4\""
fi

if grep -qE 'timezone:[[:space:]]*"America/New_York"' "$CUT_YML"; then
  assert_pass "release-cut.yml timezone America/New_York"
else
  assert_fail "release-cut.yml timezone America/New_York"
fi

# Cut schedule must not retain weekday 1-5 cron (Gate B may still use it).
if grep -qE 'cron:[[:space:]]*"17 10 \* \* 1-5"' "$CUT_YML"; then
  assert_fail "release-cut.yml has no weekday cut cron" \
    "found cron: \"17 10 * * 1-5\""
elif grep -qE 'cron:[[:space:]]*"[^"]* \* \* 1-5"' "$CUT_YML"; then
  assert_fail "release-cut.yml has no weekday cut cron" \
    "found a * * 1-5 cron in release-cut.yml"
else
  assert_pass "release-cut.yml has no weekday cut cron"
fi

# Gate B stays weekday (drift watch, not the cut PR).
if grep -qE 'cron:[[:space:]]*"40 15 \* \* 1-5"' "$STATE_YML"; then
  assert_pass "release-state.yml Gate B weekday cron 40 15 * * 1-5"
else
  assert_fail "release-state.yml Gate B weekday cron 40 15 * * 1-5"
fi

if grep -qE 'timezone:[[:space:]]*"America/New_York"' "$STATE_YML"; then
  assert_pass "release-state.yml timezone America/New_York"
else
  assert_fail "release-state.yml timezone America/New_York"
fi

echo
echo "release-cut schedule: ${pass} passed, ${fail} failed"
if [[ "$fail" -ne 0 ]]; then
  exit 1
fi
exit 0
