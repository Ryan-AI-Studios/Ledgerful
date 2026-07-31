#!/usr/bin/env bash
# Prepare a Tier-2 release cut (exactly four files).
#
# Edits:
#   CHANGELOG.md              — retitle ## [Unreleased] → ## [X.Y.Z] - YYYY-MM-DD (UTC),
#                               insert a fresh empty ## [Unreleased] above it
#   Cargo.toml                — package version
#   Cargo.lock                — root package ledgerful version (offline-safe edit;
#                               equivalent to cargo update -w for a version-only bump)
#   mcp-server/package.json   — ledgerfulEngineTag → vX.Y.Z AND patch-bump version
#                               (0101 DoD-8: pin move without wrapper bump fails npm publish)
#
# Call from the repo root:
#   bash scripts/prepare-release-cut.sh <version>
# Version: X.Y.Z or vX.Y.Z
#
# Exit non-zero on empty Unreleased, re-run, invalid version, or four-file invariant break.
# Do NOT commit under .github/ (enforced by assertion + PAT scope).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/release-changelog.sh
source "${SCRIPT_DIR}/lib/release-changelog.sh"

REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

usage() {
  echo "usage: $0 <version>" >&2
  echo "  Prepare a Tier-2 cut: retitle Unreleased, bump Cargo.toml/Cargo.lock," >&2
  echo "  bump mcp-server package.json (version patch + ledgerfulEngineTag)." >&2
  echo "  Version: X.Y.Z or vX.Y.Z (must be greater than current Cargo.toml version)." >&2
  exit 2
}

if [ "$#" -ne 1 ]; then
  usage
fi

raw_version="$1"
# Strip optional leading v.
version="${raw_version#v}"

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: version must match X.Y.Z (got: ${raw_version})" >&2
  exit 2
fi

# --- semver helpers ----------------------------------------------------------

# is_version_greater A B — exit 0 if A > B (strict), both X.Y.Z
is_version_greater() {
  local a="$1" b="$2"
  if [ "$a" = "$b" ]; then
    return 1
  fi
  # sort -V: if the first line is B, then A is greater (or equal, already excluded).
  [ "$(printf '%s\n%s\n' "$a" "$b" | sort -V | head -n1)" = "$b" ]
}

bump_patch() {
  local v="$1"
  local major minor patch
  IFS=. read -r major minor patch <<<"$v"
  if [ -z "$major" ] || [ -z "$minor" ] || [ -z "$patch" ]; then
    echo "error: cannot parse patch version from: ${v}" >&2
    return 1
  fi
  printf '%s.%s.%s\n' "$major" "$minor" "$((patch + 1))"
}

# --- guards ------------------------------------------------------------------

# Pre-bump: refuse empty Unreleased. Distinguish exit 1 (empty) from 2 (hard fail).
set +e
bash "${SCRIPT_DIR}/changelog-unreleased.sh" "CHANGELOG.md"
unrel_rc=$?
set -e
if [ "$unrel_rc" -eq 1 ]; then
  echo "error: [Unreleased] is effectively empty — refuse to cut (pre-bump guard)" >&2
  exit 1
fi
if [ "$unrel_rc" -ne 0 ]; then
  echo "error: changelog-unreleased.sh failed with exit ${unrel_rc}" >&2
  exit 2
fi

current_version="$(cargo_toml_version "Cargo.toml")"
if [ -z "$current_version" ]; then
  echo "error: could not read Cargo.toml version" >&2
  exit 2
fi

if ! is_version_greater "$version" "$current_version"; then
  echo "error: version ${version} must be greater than current Cargo.toml version ${current_version}" >&2
  exit 1
fi

# Re-run protection: refuse if a dated section for this version already exists.
if has_dated_changelog_section "$version" "CHANGELOG.md"; then
  echo "error: CHANGELOG already has a dated section for ${version} — refuse re-run / double-cut" >&2
  exit 1
fi

if [ ! -f "Cargo.lock" ]; then
  echo "error: Cargo.lock not found" >&2
  exit 2
fi
if [ ! -f "mcp-server/package.json" ]; then
  echo "error: mcp-server/package.json not found" >&2
  exit 2
fi

# --- edits -------------------------------------------------------------------

utc_date="$(date -u +%Y-%m-%d)"

# 1. CHANGELOG: insert fresh empty Unreleased above retitled dated section.
changelog_tmp="$(mktemp)"
awk -v ver="$version" -v d="$utc_date" '
  BEGIN { done = 0 }
  !done && index($0, "## [Unreleased]") == 1 {
    after = substr($0, length("## [Unreleased]") + 1)
    if (after == "" || substr(after, 1, 1) == " ") {
      print "## [Unreleased]"
      print ""
      print "## [" ver "] - " d
      done = 1
      next
    }
  }
  { print }
  END {
    if (!done) {
      print "error: no ## [Unreleased] heading found in CHANGELOG.md" > "/dev/stderr"
      exit 1
    }
  }
' "CHANGELOG.md" >"$changelog_tmp"
mv "$changelog_tmp" "CHANGELOG.md"

# 2. Cargo.toml: first ^version = "…" line (package version).
cargo_tmp="$(mktemp)"
awk -v ver="$version" '
  BEGIN { done = 0 }
  !done && /^version = "/ {
    print "version = \"" ver "\""
    done = 1
    next
  }
  { print }
  END {
    if (!done) {
      print "error: no version = line in Cargo.toml" > "/dev/stderr"
      exit 1
    }
  }
' "Cargo.toml" >"$cargo_tmp"
mv "$cargo_tmp" "Cargo.toml"

# 3. Cargo.lock: version field of package name = "ledgerful" only.
# Offline-safe; for a pure version-only bump this matches cargo update -w
# (0098 Phase 0). No silent cargo fallback — fail loud if the package block
# is missing. Do not reintroduce `|| true` cargo paths (codex 0104 P2).
lock_tmp="$(mktemp)"
awk -v ver="$version" '
  BEGIN { in_pkg = 0; done = 0 }
  /^\[\[package\]\]/ { in_pkg = 0 }
  /^name = "ledgerful"$/ { in_pkg = 1; print; next }
  in_pkg && /^version = "/ {
    print "version = \"" ver "\""
    in_pkg = 0
    done = 1
    next
  }
  { print }
  END {
    if (!done) {
      print "error: package ledgerful not found in Cargo.lock" > "/dev/stderr"
      exit 1
    }
  }
' "Cargo.lock" >"$lock_tmp"
mv "$lock_tmp" "Cargo.lock"

# 4. mcp-server/package.json: both ledgerfulEngineTag and version (patch bump).
mcp_path="mcp-server/package.json"
mcp_version_line="$(grep -E '"version"' "$mcp_path" | head -n1 || true)"
if [ -z "$mcp_version_line" ]; then
  echo "error: no version field in ${mcp_path}" >&2
  exit 2
fi
mcp_old_version="$(printf '%s' "$mcp_version_line" | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
if ! [[ "$mcp_old_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: mcp package version is not X.Y.Z: ${mcp_old_version}" >&2
  exit 2
fi
mcp_new_version="$(bump_patch "$mcp_old_version")"
engine_tag="v${version}"

mcp_tmp="$(mktemp)"
# Replace only the first "version" and the ledgerfulEngineTag line (unique in this package).
awk -v new_ver="$mcp_new_version" -v tag="$engine_tag" '
  BEGIN { ver_done = 0; tag_done = 0 }
  !ver_done && /"version"/ {
    sub(/"version"[[:space:]]*:[[:space:]]*"[^"]+"/, "\"version\": \"" new_ver "\"")
    ver_done = 1
    print
    next
  }
  /"ledgerfulEngineTag"/ {
    sub(/"ledgerfulEngineTag"[[:space:]]*:[[:space:]]*"[^"]+"/, "\"ledgerfulEngineTag\": \"" tag "\"")
    tag_done = 1
    print
    next
  }
  { print }
  END {
    if (!ver_done) {
      print "error: failed to rewrite mcp version" > "/dev/stderr"
      exit 1
    }
    if (!tag_done) {
      print "error: failed to rewrite ledgerfulEngineTag" > "/dev/stderr"
      exit 1
    }
  }
' "$mcp_path" >"$mcp_tmp"
mv "$mcp_tmp" "$mcp_path"

# --- four-file invariant -----------------------------------------------------

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "error: not a git work tree — cannot assert four-file invariant" >&2
  exit 2
fi

# Content-bearing paths only. Mode-only noise (Linux CI `chmod +x` on scripts
# still stored as 100644) must not break the invariant — observed failure
# deferred.md 2026-07-31 / release-cut run 30559428279.
# -G. selects diffs that add/remove any line content; pure filemode changes
# have no matching patch text and are excluded.
actual_sorted="$(git diff --name-only -G. | sed 's#\\#/#g' | sort -u)"
expected_sorted="$(printf '%s\n' "CHANGELOG.md" "Cargo.lock" "Cargo.toml" "mcp-server/package.json" | sort)"

if [ -z "$actual_sorted" ]; then
  echo "error: no content files changed after prepare — unexpected" >&2
  exit 1
fi

if [ "$actual_sorted" != "$expected_sorted" ]; then
  echo "error: four-file invariant broken — expected exactly (content changes):" >&2
  printf '%s\n' "$expected_sorted" | sed 's/^/  /' >&2
  echo "error: got:" >&2
  printf '%s\n' "$actual_sorted" | sed 's/^/  /' >&2
  # Helpful when mode-only paths are present (should not fail above, but show).
  mode_only="$(git diff --name-only | sed 's#\\#/#g' | sort -u)"
  if [ "$mode_only" != "$actual_sorted" ]; then
    echo "error: note — full name-only list (includes mode-only):" >&2
    printf '%s\n' "$mode_only" | sed 's/^/  /' >&2
  fi
  exit 1
fi

if printf '%s\n' "$actual_sorted" | grep -E '(^|/)\.github/' >/dev/null; then
  echo "error: prepare must not touch .github/" >&2
  printf '%s\n' "$actual_sorted" | sed 's/^/  /' >&2
  exit 1
fi

echo "ok: prepared release cut v${version} (${utc_date} UTC)"
echo "ok: files: CHANGELOG.md Cargo.toml Cargo.lock mcp-server/package.json"
echo "ok: mcp-server version ${mcp_old_version} → ${mcp_new_version}; ledgerfulEngineTag → ${engine_tag}"
exit 0
