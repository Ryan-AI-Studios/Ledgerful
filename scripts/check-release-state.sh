#!/usr/bin/env bash
# Gate B — release-state drift detector.
#
# Fails when:
#   (1) Cargo.toml's version X has a dated, closed CHANGELOG section
#       ## [X] - YYYY-MM-DD but no refs/tags/vX exists on the remote.
#   (2) published @ledgerful/mcp-server ledgerfulEngineTag (latest dist-tag)
#       does not equal the newest remote vX.Y.Z tag (0101 npm channel gate).
# Exit 0 when (1) is still under ## [Unreleased] (normal in-dev) and (2) matches
# (or the registry is unreachable — warn-not-fail, see below).
#
# Report-only (non-failing) warnings when ci.yml / smoke.yml frontend ref:
# pins are behind ledgerful-frontend main. Do NOT refresh those pins here
# (0098 §4.7 — blocked on Node 24 + frontend-owned Linux CSP gap).
#
# Optional env for deterministic red-path tests (DoD-4):
#   LEDGERFUL_GATE_B_NPM_EXPECTED=v0.0.0  — force expected pin (compare against
#     live registry pin; set to a value that is not published to see exit 1).
#
# Usage (from repo root):
#   bash scripts/check-release-state.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/release-changelog.sh
source "${SCRIPT_DIR}/lib/release-changelog.sh"

REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

# Extract first 40-hex frontend pin from a workflow file (ref: <sha>).
_workflow_frontend_ref() {
  local file="$1"
  if [ ! -f "$file" ]; then
    return 1
  fi
  grep -E 'ref: [0-9a-f]{40}' "$file" | head -n1 \
    | sed -E 's/.*ref: ([0-9a-f]{40}).*/\1/' || true
}

# Report-only: name when ci.yml / smoke.yml frontend pins lag frontend main.
# Offline / ls-remote failure → skip with a warning (does not fail the gate).
# Reason this is non-failing: refreshing those pins is blocked on Node 24 +
# the frontend-owned Linux CSP-manifest gap (0098 §4.7 / deferred.md). A gate
# that cannot be satisfied would get disabled; detection without enforcement
# keeps the drift visible.
warn_stale_frontend_pins() {
  local frontend_main=""
  local ls_out=""
  if ! ls_out="$(git ls-remote https://github.com/Ryan-AI-Studios/ledgerful-frontend.git refs/heads/main 2>/dev/null)"; then
    echo "warn: git ls-remote failed for ledgerful-frontend; skipping pin drift checks (offline?)" >&2
    return 0
  fi
  frontend_main="$(printf '%s\n' "$ls_out" | awk '{print $1; exit}')"
  if [ -z "$frontend_main" ]; then
    echo "warn: could not resolve ledgerful-frontend main; skipping pin drift checks" >&2
    return 0
  fi

  local wf pin
  for wf in .github/workflows/ci.yml .github/workflows/smoke.yml; do
    pin="$(_workflow_frontend_ref "$wf" || true)"
    if [ -z "$pin" ]; then
      echo "warn: no frontend ref pin found in ${wf}" >&2
      continue
    fi
    if [ "$pin" != "$frontend_main" ]; then
      # Report-only — do not affect exit code (0098 §4.7).
      echo "warn: ${wf} frontend ref: ${pin} is behind ledgerful-frontend main ${frontend_main} (detection-only; not failing — Node 24 + Linux CSP gap blocks refresh; see 0098 §4.7)" >&2
    else
      echo "ok: ${wf} frontend ref matches ledgerful-frontend main"
    fi
  done
}

version="$(cargo_toml_version "Cargo.toml")"
echo "Cargo.toml version: ${version}"

if has_dated_changelog_section "$version" "CHANGELOG.md"; then
  tag="v${version}"
  remote="$(resolve_git_remote)"
  echo "CHANGELOG has dated section for ${version}; requiring remote tag ${tag} (remote: ${remote})"
  if remote_has_tag "$tag" "$remote"; then
    echo "ok: remote tag ${tag} exists"
  else
    echo "error: dated CHANGELOG section for ${version} exists but remote tag ${tag} is missing" >&2
    echo "error: this is a half-executed release state (bumped without tagging)" >&2
    echo "error: cut the tag or roll the version into the next release (see 0098-ReleaseCutAutomation)" >&2
    # Still emit pin warnings before failing (report-only; do not mask the exit).
    warn_stale_frontend_pins || true
    exit 1
  fi
else
  echo "ok: version ${version} has no dated closed CHANGELOG section (still under [Unreleased] or not closed)"
fi

warn_stale_frontend_pins || true

# Published npm engine pin vs newest remote tag (0101 Gate B / DoD-4).
# Resolves the *latest* dist-tag deliberately — assertion is about what users
# get today, not a specific version number.
#
# Three states (spec §3.6):
#   pin ≠ expected tag     → exit 1 (hard fail; this is the defect)
#   registry unreachable   → ::warning:: + continue exit 0 (unverified third state)
#   pin == expected tag    → ok
#
# WHY warn-not-fail on outage (do not "tighten" later): a check that goes red on
# someone else's outage gets disabled (0098 frontend-pin + 0103 precedent). An
# annotated ::warning:: on the run summary is distinguishable from a silent pass;
# a bare stderr warn: is not. Prefer a visible unverified state over a gate that
# operators learn to ignore or switch off.
assert_published_npm_engine_pin() {
  local expected="${LEDGERFUL_GATE_B_NPM_EXPECTED:-}"
  local remote
  local published=""
  local query_rc=0

  if [ -z "$expected" ]; then
    remote="$(resolve_git_remote)"
    if ! expected="$(newest_remote_v_tag "$remote")"; then
      echo "error: could not resolve newest remote vX.Y.Z tag for npm pin check" >&2
      return 1
    fi
  fi
  echo "npm Gate B: expecting published ledgerfulEngineTag == ${expected}"

  # Capture stdout only; failures (network, missing package) → unverified.
  # Short fetch timeout so an unreachable registry cannot hang the scheduled job
  # (default npm retries can take minutes against a black-hole host).
  set +e
  published="$(
    npm_config_fetch_retries=0 \
    npm_config_fetch_retry_mintimeout=1 \
    npm_config_fetch_retry_maxtimeout=1 \
    npm_config_fetch_timeout=5000 \
      npm view @ledgerful/mcp-server ledgerfulEngineTag 2>/dev/null
  )"
  query_rc=$?
  set -e
  published="$(printf '%s' "$published" | tr -d '\r' | head -n1 | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"

  if [ "$query_rc" -ne 0 ] || [ -z "$published" ]; then
    echo "::warning::Gate B could not verify npm ledgerfulEngineTag (registry unreachable or query failed); published pin unverified against ${expected}"
    return 0
  fi

  if [ "$published" != "$expected" ]; then
    echo "error: published @ledgerful/mcp-server ledgerfulEngineTag is '${published}', expected '${expected}' (newest release tag / override)" >&2
    echo "error: npm channel is stale — users install the wrong engine pin (0101)" >&2
    echo "error: republish @ledgerful/mcp-server with the current pin, or fix the pin and bump the wrapper version" >&2
    return 1
  fi
  echo "ok: published @ledgerful/mcp-server ledgerfulEngineTag == ${expected}"
  return 0
}

assert_published_npm_engine_pin

echo "check-release-state: ok"
exit 0
