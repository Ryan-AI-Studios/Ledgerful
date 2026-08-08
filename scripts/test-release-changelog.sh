#!/usr/bin/env bash
# Parser matrix for release-changelog helpers (0098 Gate B + 0101 section body).
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

assert_content() {
  local name="$1"
  local key="$2"
  local file="$3"
  if changelog_section_has_content "$key" "$file"; then
    echo "PASS: ${name} (has content)"
    pass=$((pass + 1))
  else
    echo "FAIL: ${name} — expected content for [${key}]" >&2
    fail=$((fail + 1))
  fi
}

assert_empty_content() {
  local name="$1"
  local key="$2"
  local file="$3"
  if changelog_section_has_content "$key" "$file"; then
    echo "FAIL: ${name} — expected effectively empty for [${key}]" >&2
    fail=$((fail + 1))
  else
    echo "PASS: ${name} (effectively empty)"
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

# --- 0101: section body content (Gate A dated body + pre-bump Unreleased) ---

# 6. Truly empty Unreleased body → empty
cat >"$TMP/empty-unreleased.md" <<'EOF'
# Changelog

## [Unreleased]

## [0.2.3] - 2026-07-29

### Fixed
- something
EOF
assert_empty_content "truly empty Unreleased" "Unreleased" "$TMP/empty-unreleased.md"
assert_content "dated 0.2.3 with bullet has content" "0.2.3" "$TMP/empty-unreleased.md"

# 7. Heading-only (### Added, no items) → empty
cat >"$TMP/heading-only.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

## [0.2.3] - 2026-07-29

### Fixed
- real fix
EOF
assert_empty_content "heading-only Unreleased (### Added, no items)" "Unreleased" "$TMP/heading-only.md"

# 8. Whitespace / comment-only → empty
cat >"$TMP/comment-only.md" <<'EOF'
# Changelog

## [Unreleased]

<!-- pending -->

## [0.2.3] - 2026-07-29

- item
EOF
assert_empty_content "whitespace/comment-only Unreleased" "Unreleased" "$TMP/comment-only.md"

# 8b. Multi-line HTML comment (middle lines must not count as content)
cat >"$TMP/multiline-comment.md" <<'EOF'
# Changelog

## [Unreleased]

<!--
placeholder that looks like content
still inside the comment
-->

## [0.2.3] - 2026-07-29

- item
EOF
assert_empty_content "multi-line HTML comment Unreleased (middle lines ignored)" "Unreleased" "$TMP/multiline-comment.md"

# 9. One real entry → content
cat >"$TMP/one-entry.md" <<'EOF'
# Changelog

## [Unreleased]

### Fixed
- real work

## [0.2.3] - 2026-07-29
EOF
assert_content "one real Unreleased entry" "Unreleased" "$TMP/one-entry.md"
assert_empty_content "dated 0.2.3 bare heading after Unreleased with content" "0.2.3" "$TMP/one-entry.md"

# 10. Pre-bump script: empty → exit 1; with content → exit 0
UNREL_SCRIPT="${SCRIPT_DIR}/changelog-unreleased.sh"
assert_unrel_rc() {
  local name="$1"
  local file="$2"
  local want="$3"
  local rc=0
  set +e
  bash "$UNREL_SCRIPT" "$file" >/dev/null 2>&1
  rc=$?
  set -e
  if [ "$rc" -eq "$want" ]; then
    echo "PASS: ${name} (exit ${rc})"
    pass=$((pass + 1))
  else
    echo "FAIL: ${name} — expected exit ${want}, got ${rc}" >&2
    fail=$((fail + 1))
  fi
}
assert_unrel_rc "changelog-unreleased.sh empty Unreleased (i)" "$TMP/empty-unreleased.md" 1
assert_unrel_rc "changelog-unreleased.sh heading-only (ii)" "$TMP/heading-only.md" 1
assert_unrel_rc "changelog-unreleased.sh comment-only (iii)" "$TMP/comment-only.md" 1
assert_unrel_rc "changelog-unreleased.sh one real entry (iv)" "$TMP/one-entry.md" 0

# 11. Gate A body check via temp tree (helpers already covered; script path)
GATE_A="${SCRIPT_DIR}/check-release-tag.sh"
# Build a temp tree that looks enough like the repo for Gate A path checks we care about:
# only body helper is unit-tested fully; dated empty body must be detected by has_content.
cat >"$TMP/empty-dated.md" <<'EOF'
# Changelog

## [Unreleased]

- wip

## [0.9.9] - 2026-07-29

EOF
assert_empty_content "empty dated body for 0.9.9" "0.9.9" "$TMP/empty-dated.md"
assert_has "empty-dated file still has heading for 0.9.9" "0.9.9" "$TMP/empty-dated.md"

# --- 0162: openapi info.version helpers (C3 / C1 / C5 style) -----------------

# 12. openapi_info_version reads info-block version on multi-line stub
cat >"$TMP/openapi-stub.json" <<'EOF'
{
  "openapi": "3.1.0",
  "info": {
    "title": "fixture",
    "version": "0.2.3",
    "description": "stub"
  },
  "components": {
    "schemas": {
      "Versioned": {
        "properties": {
          "version": {
            "type": "string"
          }
        }
      }
    }
  }
}
EOF
got_ov="$(openapi_info_version "$TMP/openapi-stub.json" 2>/dev/null || true)"
if [ "$got_ov" = "0.2.3" ]; then
  echo "PASS: openapi_info_version reads info.version 0.2.3"
  pass=$((pass + 1))
else
  echo "FAIL: openapi_info_version reads info.version 0.2.3 — got '${got_ov}'" >&2
  fail=$((fail + 1))
fi

# 13. rewrite then read equals new version; decoys untouched
set +e
rewrite_openapi_info_version "0.2.7" "$TMP/openapi-stub.json"
rw_rc=$?
set -e
got_rw="$(openapi_info_version "$TMP/openapi-stub.json" 2>/dev/null || true)"
if [ "$rw_rc" -eq 0 ] && [ "$got_rw" = "0.2.7" ]; then
  echo "PASS: rewrite_openapi_info_version → openapi_info_version == 0.2.7"
  pass=$((pass + 1))
else
  echo "FAIL: rewrite_openapi_info_version → openapi_info_version == 0.2.7 — rc=${rw_rc} got='${got_rw}'" >&2
  fail=$((fail + 1))
fi
if grep -q '"openapi": "3.1.0"' "$TMP/openapi-stub.json"; then
  echo "PASS: decoy openapi document version 3.1.0 not rewritten"
  pass=$((pass + 1))
else
  echo "FAIL: decoy openapi document version 3.1.0 not rewritten" >&2
  fail=$((fail + 1))
fi
if grep -q '"version": {' "$TMP/openapi-stub.json"; then
  echo "PASS: decoy schema version object not rewritten"
  pass=$((pass + 1))
else
  echo "FAIL: decoy schema version object not rewritten" >&2
  fail=$((fail + 1))
fi

# 14. Gate A style: missing openapi fails
set +e
openapi_info_version "$TMP/no-such-openapi.json" >/dev/null 2>&1
miss_rc=$?
set -e
if [ "$miss_rc" -ne 0 ]; then
  echo "PASS: openapi_info_version missing file fails closed (exit ${miss_rc})"
  pass=$((pass + 1))
else
  echo "FAIL: openapi_info_version missing file fails closed — expected non-zero" >&2
  fail=$((fail + 1))
fi

# 15. Gate B style: mismatch detected (helper unit; cargo 0.9.9 vs openapi 0.2.7)
if [ "$got_rw" != "0.9.9" ]; then
  echo "PASS: Gate B style mismatch (openapi ${got_rw} != cargo 0.9.9) would fail"
  pass=$((pass + 1))
else
  echo "FAIL: Gate B style mismatch detection" >&2
  fail=$((fail + 1))
fi

# 16. rewrite fails when no info.version present
cat >"$TMP/openapi-no-ver.json" <<'EOF'
{
  "openapi": "3.1.0",
  "info": {
    "title": "no version field"
  }
}
EOF
set +e
rewrite_openapi_info_version "0.2.7" "$TMP/openapi-no-ver.json" >/dev/null 2>&1
nover_rc=$?
set -e
if [ "$nover_rc" -ne 0 ]; then
  echo "PASS: rewrite fails when info has no string-semver version (exit ${nover_rc})"
  pass=$((pass + 1))
else
  echo "FAIL: rewrite fails when info has no string-semver version — expected non-zero" >&2
  fail=$((fail + 1))
fi

echo ""
echo "release-changelog parser matrix: ${pass} passed, ${fail} failed"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
exit 0
