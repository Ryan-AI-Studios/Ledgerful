#!/usr/bin/env bash
# Gate A — release preflight for a tag cut.
#
# Usage (from repo root):
#   bash scripts/check-release-tag.sh v0.2.3
#
# Asserts against the working tree:
#   (a) Cargo.toml version == tag without leading v
#   (b) CHANGELOG.md has a dated ## [version] - YYYY-MM-DD section
#   (b2) that dated section has a non-empty body (not heading-only / empty)
#   (c) mcp-server/package.json ledgerfulEngineTag == tag
#   (d) docs/api/openapi.json info.version == tag without leading v (0162)
#
# Does NOT require non-empty [Unreleased] — at tag time empty Unreleased is
# healthy (0101 §2.6a). Use scripts/changelog-unreleased.sh before the cut.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/release-changelog.sh
source "${SCRIPT_DIR}/lib/release-changelog.sh"

REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

if [ "$#" -ne 1 ] || [ -z "${1:-}" ]; then
  echo "usage: $0 <tag>" >&2
  echo "  example: $0 v0.2.3" >&2
  exit 2
fi

tag="$1"
# Strip optional leading v for the Cargo/CHANGELOG version form.
if [[ "$tag" == v* ]]; then
  version="${tag#v}"
else
  echo "error: tag must start with 'v' (got '${tag}')" >&2
  exit 1
fi

if [ -z "$version" ]; then
  echo "error: empty version after stripping 'v' from '${tag}'" >&2
  exit 1
fi

echo "check-release-tag: tag=${tag} version=${version}"

# (a) Cargo.toml version
cargo_ver="$(cargo_toml_version "Cargo.toml")"
if [ "$cargo_ver" != "$version" ]; then
  echo "error: Cargo.toml version is '${cargo_ver}', expected '${version}' (tag ${tag})" >&2
  exit 1
fi
echo "ok: Cargo.toml version == ${version}"

# (b) dated CHANGELOG section (shared parser with Gate B)
if ! has_dated_changelog_section "$version" "CHANGELOG.md"; then
  echo "error: CHANGELOG.md has no dated section '## [${version}] - YYYY-MM-DD' for tag ${tag}" >&2
  exit 1
fi
echo "ok: CHANGELOG has dated section for ${version}"

# (b2) dated section body must be non-empty (0101 tag-time half of §2.6a)
if ! changelog_section_has_content "$version" "CHANGELOG.md"; then
  echo "error: CHANGELOG dated section for ${version} has an empty body (no bullets/items)" >&2
  echo "error: Gate A requires real release notes under '## [${version}] - …', not a bare heading" >&2
  exit 1
fi
echo "ok: CHANGELOG dated section for ${version} has content"

# (c) MCP engine pin
mcp_tag="$(mcp_engine_tag "mcp-server/package.json")"
if [ "$mcp_tag" != "$tag" ]; then
  echo "error: mcp-server/package.json ledgerfulEngineTag is '${mcp_tag}', expected '${tag}'" >&2
  exit 1
fi
echo "ok: ledgerfulEngineTag == ${tag}"

# (d) OpenAPI info.version (0162 — fail closed if missing)
if [ ! -f "docs/api/openapi.json" ]; then
  echo "error: docs/api/openapi.json not found — Gate A requires openapi info.version == ${version}" >&2
  exit 1
fi
openapi_ver="$(openapi_info_version "docs/api/openapi.json")"
if [ "$openapi_ver" != "$version" ]; then
  echo "error: docs/api/openapi.json info.version is '${openapi_ver}', expected '${version}' (tag ${tag})" >&2
  exit 1
fi
echo "ok: openapi info.version == ${version}"

echo "check-release-tag: ok"
exit 0
