#!/usr/bin/env bash
# Three-state npm publish decision for @ledgerful/mcp-server (DoD-8 / 0101).
#
# Usage (from repo root, or pass package.json path):
#   bash scripts/npm-publish-decision.sh [mcp-server/package.json]
#
# Reads local version + ledgerfulEngineTag from package.json, queries the
# registry (or env mocks), and prints a decision line on stdout:
#   publish | skip | fail
#
# Exit codes:
#   0 — version not on registry → should publish
#   2 — version on registry, pin matches local → skip (re-dispatch safe)
#   1 — version on registry, pin differs → fail DoD-8 (forgot wrapper bump)
#   3 — version on registry but pin unreadable, or bad inputs
#
# Env overrides (for unit tests — skip real npm when VERSION_RC is set):
#   LEDGERFUL_NPM_VIEW_VERSION_RC      — mock exit code of `npm view … version`
#   LEDGERFUL_NPM_VIEW_VERSION_OUTPUT  — mock stdout (trimmed like real path)
#   LEDGERFUL_NPM_VIEW_PIN_RC          — mock exit code of `npm view … pin`
#   LEDGERFUL_NPM_VIEW_PIN_OUTPUT      — mock pin stdout
#
# Does NOT run `npm publish`. Callers (release.yml) publish only on exit 0.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

PACKAGE_JSON="${1:-${REPO_ROOT}/mcp-server/package.json}"
if [ ! -f "$PACKAGE_JSON" ]; then
  echo "error: package.json not found: ${PACKAGE_JSON}" >&2
  exit 3
fi

# Prefer node when available (same as release job); fall back to grep/sed.
_read_pkg_field() {
  local field="$1"
  local path="$2"
  local value=""
  if command -v node >/dev/null 2>&1; then
    value="$(
      node -e "const p=require(process.argv[1]); const v=p[process.argv[2]]; process.stdout.write(v==null?'':String(v))" \
        "$path" "$field" 2>/dev/null || true
    )"
  fi
  if [ -z "$value" ]; then
    local line
    line="$(grep -E "\"${field}\"" "$path" | head -n1 || true)"
    if [ -n "$line" ]; then
      value="$(printf '%s' "$line" | sed -E "s/.*\"${field}\"[[:space:]]*:[[:space:]]*\"([^\"]+)\".*/\1/")"
    fi
  fi
  printf '%s' "$value"
}

_trim_one_line() {
  printf '%s' "$1" | tr -d '\r' | head -n1 | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//'
}

version="$(_read_pkg_field version "$PACKAGE_JSON")"
local_pin="$(_read_pkg_field ledgerfulEngineTag "$PACKAGE_JSON")"

if [ -z "$version" ]; then
  echo "error: could not read version from ${PACKAGE_JSON}" >&2
  exit 3
fi
if [ -z "$local_pin" ]; then
  echo "error: could not read ledgerfulEngineTag from ${PACKAGE_JSON}" >&2
  exit 3
fi

echo "local version=${version} ledgerfulEngineTag=${local_pin}" >&2

PKG_SPEC="@ledgerful/mcp-server@${version}"
mock_mode=0
if [ -n "${LEDGERFUL_NPM_VIEW_VERSION_RC+x}" ]; then
  mock_mode=1
fi

# --- registry query (or mock) ---
published_version=""
view_rc=0
if [ "$mock_mode" -eq 1 ]; then
  view_rc="${LEDGERFUL_NPM_VIEW_VERSION_RC}"
  published_version="${LEDGERFUL_NPM_VIEW_VERSION_OUTPUT:-}"
else
  set +e
  published_version="$(npm view "${PKG_SPEC}" version 2>/dev/null)"
  view_rc=$?
  set -e
fi
published_version="$(_trim_one_line "$published_version")"

if [ "$view_rc" -ne 0 ] || [ -z "$published_version" ]; then
  echo "version ${version} not on registry — should publish" >&2
  printf '%s\n' "publish"
  exit 0
fi

# Version already on registry: skip if pin matches; fail if pin moved (DoD-8).
published_pin=""
pin_rc=0
if [ "$mock_mode" -eq 1 ]; then
  # Default pin_rc=1 if not set so tests must opt into a successful pin read.
  pin_rc="${LEDGERFUL_NPM_VIEW_PIN_RC:-1}"
  published_pin="${LEDGERFUL_NPM_VIEW_PIN_OUTPUT:-}"
else
  set +e
  published_pin="$(npm view "${PKG_SPEC}" ledgerfulEngineTag 2>/dev/null)"
  pin_rc=$?
  set -e
fi
published_pin="$(_trim_one_line "$published_pin")"

if [ "$pin_rc" -ne 0 ] || [ -z "$published_pin" ]; then
  echo "error: version ${version} exists on registry but ledgerfulEngineTag could not be read" >&2
  printf '%s\n' "fail"
  exit 3
fi

if [ "$published_pin" = "$local_pin" ]; then
  echo "already published @ledgerful/mcp-server@${version} with ledgerfulEngineTag=${published_pin} — skip" >&2
  printf '%s\n' "skip"
  exit 2
fi

echo "error: @ledgerful/mcp-server@${version} is already on the registry with ledgerfulEngineTag=${published_pin}" >&2
echo "error: local pin is ${local_pin} — the engine pin moved but the wrapper version was not bumped (DoD-8)" >&2
echo "error: bump mcp-server/package.json version and re-cut; do not reuse ${version}" >&2
printf '%s\n' "fail"
exit 1
