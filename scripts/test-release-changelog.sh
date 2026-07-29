#!/usr/bin/env bash
# Parser matrix for has_dated_changelog_section (plan step 9 / 0098 Gate B).
# Run from repo root: bash scripts/test-release-changelog.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/release-changelog.sh
source "${SCRIPT_DIR}/lib/release-changelog.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0

assert_has() {
  local name="$1"
  local version="$2"
  local file="$3"
  if has_dated_changelog_section "$version" "$file"; then
    echo "PASS: ${name} (dated section detected)"
    pass=$((pass + 1))
  else
    echo "FAIL: ${name} — expected dated section for ${version}" >&2
    fail=$((fail + 1))
  fi
}

assert_missing() {
  local name="$1"
  local version="$2"
  local file="$3"
  if has_dated_changelog_section "$version" "$file"; then
    echo "FAIL: ${name} — must NOT treat as dated version section for ${version}" >&2
    fail=$((fail + 1))
  else
    echo "PASS: ${name} (correctly ignored)"
    pass=$((pass + 1))
  fi
}

# 1. Dated closed section → tag required
cat >"$TMP/dated.md" <<'EOF'
# Changelog

## [Unreleased]

## [0.2.3] - 2026-07-29

### Fixed
- something
EOF
assert_has "dated ## [0.2.3] - 2026-07-29" "0.2.3" "$TMP/dated.md"

# 2. Unreleased only → green (no dated section for 0.2.3)
cat >"$TMP/unreleased.md" <<'EOF'
# Changelog

## [Unreleased]

### Added
- work in progress

## [0.2.1] - 2026-07-26

### Fixed
- older
EOF
assert_missing "Unreleased only for 0.2.3" "0.2.3" "$TMP/unreleased.md"

# 3. Tombstone prose line mentioning 0.2.2 (NOT a heading) — must not demand tag
#    Phase 1 of 0098 writes exactly this shape.
cat >"$TMP/tombstone.md" <<'EOF'
# Changelog

## [Unreleased]

## [0.2.3] - 2026-07-29

> Note: 0.2.2 was prepared on 2026-07-27 but never tagged; rolled into 0.2.3.

### Fixed
- rolled-in 0.2.2 fix

## [0.2.1] - 2026-07-26
EOF
assert_missing "tombstone prose line for 0.2.2" "0.2.2" "$TMP/tombstone.md"
assert_has "tombstone file still has dated 0.2.3" "0.2.3" "$TMP/tombstone.md"

# 4. ### Known limitations is not a version section
cat >"$TMP/known.md" <<'EOF'
# Changelog

## [0.1.6] - 2026-06-28

### Known limitations

- something about 0.2.3
EOF
assert_missing "### Known limitations for 0.2.3" "0.2.3" "$TMP/known.md"
assert_has "### Known limitations file has dated 0.1.6" "0.1.6" "$TMP/known.md"

# 5. Markdown link-reference footer [0.2.3]: https://… — forward defence
#    (grep -n '^\[0\.' CHANGELOG.md returns nothing today; keep the case).
cat >"$TMP/footer.md" <<'EOF'
# Changelog

## [Unreleased]

## [0.2.1] - 2026-07-26

[0.2.3]: https://github.com/Ryan-AI-Studios/Ledgerful/compare/v0.2.2...v0.2.3
[0.2.1]: https://github.com/Ryan-AI-Studios/Ledgerful/compare/v0.2.0...v0.2.1
EOF
assert_missing "Keep-a-Changelog link footer for 0.2.3 (forward defence)" "0.2.3" "$TMP/footer.md"
assert_has "footer file still has dated 0.2.1" "0.2.1" "$TMP/footer.md"

echo ""
echo "release-changelog parser matrix: ${pass} passed, ${fail} failed"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
exit 0
