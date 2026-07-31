#!/usr/bin/env bash
# Pre-bump guard: [Unreleased] must have effective content before retitling.
#
# Call this *before* moving [Unreleased] into a dated section (human cut
# checklist and 0104 scheduler). Do NOT run at Gate A / tag time — an empty
# [Unreleased] is the healthy post-cut state (spec 0101 §2.6a).
#
# Exit codes:
#   0  — Unreleased has at least one effective content line
#   1  — Unreleased is effectively empty (no completed work ⇒ no cut)
#   2  — usage / missing file
#
# Usage (from repo root):
#   bash scripts/changelog-unreleased.sh [CHANGELOG.md]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/release-changelog.sh
source "${SCRIPT_DIR}/lib/release-changelog.sh"

if [ "$#" -gt 1 ]; then
  echo "usage: $0 [CHANGELOG.md]" >&2
  echo "  Pre-bump check: [Unreleased] must have effective content before a cut." >&2
  exit 2
fi

changelog="${1:-CHANGELOG.md}"
if [ ! -f "$changelog" ]; then
  echo "error: changelog not found: ${changelog}" >&2
  exit 2
fi

if changelog_section_has_content "Unreleased" "$changelog"; then
  echo "ok: CHANGELOG [Unreleased] has content (pre-bump guard)"
  exit 0
fi

echo "error: CHANGELOG [Unreleased] is effectively empty — no completed work to cut" >&2
echo "error: this is the pre-bump guard (0101): refuse to retitle/bump with nothing under [Unreleased]" >&2
echo "error: add a real entry (not only ### headings / comments / whitespace) before cutting" >&2
exit 1
