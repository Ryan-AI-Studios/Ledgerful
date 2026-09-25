#!/usr/bin/env bash
# Hermetic matrix for scripts/refresh-distribution-floors.sh (0434).
# Run from repo root: bash scripts/test-refresh-distribution-floors.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SCRIPT="${SCRIPT_DIR}/refresh-distribution-floors.sh"
FIXTURES="${REPO_ROOT}/tests/fixtures/distribution-floors"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

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

chmod +x "$SCRIPT"

seed_docs() {
  local dest="$1"
  mkdir -p "$dest"
  cp "${REPO_ROOT}/docs/installation.md" "$dest/installation.md"
  cp "${REPO_ROOT}/docs/package-distribution.md" "$dest/package-distribution.md"
}

# --- newest semver from alphabetical-and-paged listing ---------------------
seed_docs "${TMP}/semver"
if ! bash "$SCRIPT" --write --docs-dir "${TMP}/semver" --fixtures-dir "$FIXTURES"; then
  assert_fail "write with paged listing"
else
  if grep -Fq '**0.2.14**' "${TMP}/semver/installation.md" && ! grep -Fq '**0.2.8**' "${TMP}/semver/installation.md"; then
    assert_pass "paged listing picks newest semver 0.2.14 not 0.2.8"
  else
    assert_fail "paged listing picks newest semver 0.2.14 not 0.2.8"
  fi
fi

# --- write + check round-trip ---------------------------------------------
seed_docs "${TMP}/round"
if bash "$SCRIPT" --write --docs-dir "${TMP}/round" --fixtures-dir "$FIXTURES" \
  && bash "$SCRIPT" --check --docs-dir "${TMP}/round" --fixtures-dir "$FIXTURES"; then
  assert_pass "write then check round-trip"
else
  assert_fail "write then check round-trip"
fi

# idempotent second write
cp "${TMP}/round/installation.md" "${TMP}/round/installation.md.before"
cp "${TMP}/round/package-distribution.md" "${TMP}/round/package-distribution.md.before"
bash "$SCRIPT" --write --docs-dir "${TMP}/round" --fixtures-dir "$FIXTURES"
if cmp -s "${TMP}/round/installation.md" "${TMP}/round/installation.md.before" \
  && cmp -s "${TMP}/round/package-distribution.md" "${TMP}/round/package-distribution.md.before"; then
  assert_pass "second write is a no-op"
else
  assert_fail "second write is a no-op"
fi

# five sites present
for site in I-57 P-13 P-191 P-215 P-217; do
  if grep -Fq "<!-- lf-floor:${site} -->" "${TMP}/round/installation.md" \
    || grep -Fq "<!-- lf-floor:${site} -->" "${TMP}/round/package-distribution.md"; then
    assert_pass "marker ${site} present"
  else
    assert_fail "marker ${site} present"
  fi
done

if grep -Fq 'Open version PR #440829' "${TMP}/round/installation.md"; then
  assert_pass "one open PR is named"
else
  assert_fail "one open PR is named"
fi

# --- one of five sites stale → check exit 1 --------------------------------
seed_docs "${TMP}/stale"
bash "$SCRIPT" --write --docs-dir "${TMP}/stale" --fixtures-dir "$FIXTURES"
# Corrupt only I-57 interior (leave the other four sites current).
awk '
  BEGIN { start = "<!-- lf-floor:I-57 -->"; stop = "<!-- /lf-floor:I-57 -->" }
  {
    s = index($0, start)
    e = index($0, stop)
    if (s > 0 && e > s) {
      prefix = substr($0, 1, s + length(start) - 1)
      suffix = substr($0, e)
      print prefix "community index is live at **0.2.13** (`microsoft/winget-pkgs` `manifests/l/Ledgerful/Ledgerful`, 2026-09-18). GitHub Release **v0.2.14** is published;" suffix
      next
    }
    print
  }
' "${TMP}/stale/installation.md" >"${TMP}/stale/installation.md.stale"
mv "${TMP}/stale/installation.md.stale" "${TMP}/stale/installation.md"
set +e
bash "$SCRIPT" --check --docs-dir "${TMP}/stale" --fixtures-dir "$FIXTURES"
stale_rc=$?
set -e
if [[ "$stale_rc" -eq 1 ]]; then
  assert_pass "one-site stale check exits 1"
else
  assert_fail "one-site stale check exits 1" "rc=${stale_rc}"
fi

# --- zero open PRs omits the sentence; date token any ISO date -------------
seed_docs "${TMP}/zero"
if bash "$SCRIPT" --write --docs-dir "${TMP}/zero" --fixtures-dir "${FIXTURES}/zero-prs" \
  && bash "$SCRIPT" --check --docs-dir "${TMP}/zero" --fixtures-dir "${FIXTURES}/zero-prs"; then
  if grep -Fq 'Open version PR' "${TMP}/zero/installation.md" \
    || grep -Fq 'Open version PR' "${TMP}/zero/package-distribution.md"; then
    assert_fail "zero open PRs omits Open version PR"
  else
    assert_pass "zero open PRs omits Open version PR"
  fi
  if grep -Fq '2026-01-01' "${TMP}/zero/installation.md"; then
    assert_pass "as-of date is written"
  else
    assert_fail "as-of date is written"
  fi
else
  assert_fail "zero-pr write+check"
fi

# --- several open PRs are sorted ------------------------------------------------
seed_docs "${TMP}/multi"
if bash "$SCRIPT" --write --docs-dir "${TMP}/multi" --fixtures-dir "${FIXTURES}/multi-prs"; then
  if grep -Fq 'Open version PRs #440829, #440830' "${TMP}/multi/installation.md"; then
    assert_pass "several open PRs are sorted"
  else
    assert_fail "several open PRs are sorted"
  fi
else
  assert_fail "multi-pr write"
fi

echo ""
echo "refresh-distribution-floors tests: ${pass} passed, ${fail} failed"
if [[ "$fail" -ne 0 ]]; then
  exit 1
fi
exit 0
