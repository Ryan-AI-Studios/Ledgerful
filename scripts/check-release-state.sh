#!/usr/bin/env bash
# Gate B — release-state drift detector.
#
# Fails when Cargo.toml's version X has a dated, closed CHANGELOG section
# ## [X] - YYYY-MM-DD but no refs/tags/vX exists on the remote.
# Exit 0 when the version is still under ## [Unreleased] (normal in-dev).
#
# Report-only (non-failing) warnings when ci.yml / smoke.yml frontend ref:
# pins are behind ledgerful-frontend main. Do NOT refresh those pins here
# (0098 §4.7 — blocked on Node 24 + frontend-owned Linux CSP gap).
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
echo "check-release-state: ok"
exit 0
