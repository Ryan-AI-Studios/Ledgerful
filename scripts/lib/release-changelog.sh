# Shared release-changelog helpers (sourced by check-release-state.sh,
# check-release-tag.sh, and changelog-unreleased.sh). Not executable standalone.
#
# Version-section matcher anchors on ^## \[ so:
# - tombstone prose lines ("> Note: 0.2.2 was prepared…") do not count as a section
# - ### Known limitations / ### Added headings do not count as a version section
# - Keep-a-Changelog link footers ([0.2.3]: https://…) do not count as a section
#   (forward defence — no such footers exist in this repo today)
#
# Section *body* content (changelog_section_has_content) is stricter than section
# *existence*. A body is effectively empty when it contains only:
# - whitespace lines
# - HTML comments (<!-- … -->) or markdown comment-style HTML blocks
# - ### heading lines with no bullet/item content under them
# Effective content requires at least one non-empty, non-comment line that is not
# a ### heading alone (e.g. a "- item" bullet counts).

# cargo_toml_version [path]
# Prints the package version from Cargo.toml (first ^version = "…" line).
cargo_toml_version() {
  local path="${1:-Cargo.toml}"
  local line
  line="$(grep -E '^version = "' "$path" | head -n1 || true)"
  if [ -z "$line" ]; then
    echo "error: no version line in ${path}" >&2
    return 1
  fi
  printf '%s\n' "$line" | sed -E 's/^version = "([^"]+)".*/\1/'
}

# has_dated_changelog_section VERSION [changelog_path]
# Exit 0 if CHANGELOG has a dated closed section ## [VERSION] - YYYY-MM-DD.
# Anchors on ^## \[ so footers, ### headings, and tombstone prose cannot match.
has_dated_changelog_section() {
  local version="$1"
  local changelog="${2:-CHANGELOG.md}"
  local escaped
  if [ -z "$version" ]; then
    echo "error: has_dated_changelog_section requires a version" >&2
    return 2
  fi
  if [ ! -f "$changelog" ]; then
    echo "error: changelog not found: ${changelog}" >&2
    return 2
  fi
  # Escape dots for basic/ERE (version is digits + dots only in practice).
  escaped="$(printf '%s' "$version" | sed 's/\./\\./g')"
  grep -Eq "^## \[${escaped}\] - [0-9]{4}-[0-9]{2}-[0-9]{2}" "$changelog"
}

# mcp_engine_tag [package_json_path]
# Prints ledgerfulEngineTag value from mcp-server/package.json.
mcp_engine_tag() {
  local path="${1:-mcp-server/package.json}"
  local line
  if [ ! -f "$path" ]; then
    echo "error: package.json not found: ${path}" >&2
    return 1
  fi
  line="$(grep -E '"ledgerfulEngineTag"' "$path" | head -n1 || true)"
  if [ -z "$line" ]; then
    echo "error: no ledgerfulEngineTag in ${path}" >&2
    return 1
  fi
  printf '%s\n' "$line" | sed -E 's/.*"ledgerfulEngineTag"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/'
}

# resolve_git_remote — prefer origin (CI), then ledgerful (local clone).
resolve_git_remote() {
  if git remote get-url origin >/dev/null 2>&1; then
    printf '%s\n' origin
  elif git remote get-url ledgerful >/dev/null 2>&1; then
    printf '%s\n' ledgerful
  else
    git remote | head -n1
  fi
}

# remote_has_tag TAG [remote]
# TAG is e.g. v0.2.2. Exit 0 if the remote advertises refs/tags/TAG.
remote_has_tag() {
  local tag="$1"
  local remote="${2:-}"
  if [ -z "$remote" ]; then
    remote="$(resolve_git_remote)"
  fi
  if [ -z "$remote" ]; then
    echo "error: no git remote configured" >&2
    return 2
  fi
  # Match the exact tag ref (ignore peeled ^{} companions as separate rows;
  # presence of either means the tag exists). Fixed-string match so dots in
  # version tags are not treated as regex wildcards.
  git ls-remote --tags "$remote" "refs/tags/${tag}" 2>/dev/null \
    | grep -F "refs/tags/${tag}" >/dev/null
}

# changelog_section_body VERSION|Unreleased [changelog_path]
# Prints the body between the matching ^## \[…\] heading and the next ^## \[
# heading (or EOF). VERSION may be "Unreleased" (literal) or a semver without
# leading v; dated headings ## [X] - YYYY-MM-DD match on the bracketed token.
# Uses string prefix matching (not regex) so dots in versions need no escaping
# and gawk/mawk escape quirks do not apply.
changelog_section_body() {
  local key="$1"
  local changelog="${2:-CHANGELOG.md}"
  if [ -z "$key" ]; then
    echo "error: changelog_section_body requires VERSION or Unreleased" >&2
    return 2
  fi
  if [ ! -f "$changelog" ]; then
    echo "error: changelog not found: ${changelog}" >&2
    return 2
  fi
  # awk: start after matching heading; stop before next ## [ version heading.
  awk -v key="$key" '
    BEGIN {
      prefix = "## [" key "]"
      in_section = 0
    }
    {
      if (index($0, "## [") == 1) {
        if (in_section) {
          exit
        }
        if (index($0, prefix) == 1) {
          after = substr($0, length(prefix) + 1)
          # Exact heading, or space then rest (e.g. " - 2026-07-29").
          if (after == "" || substr(after, 1, 1) == " ") {
            in_section = 1
            next
          }
        }
        next
      }
      if (in_section) {
        print
      }
    }
  ' "$changelog"
}

# changelog_section_has_content VERSION|Unreleased [changelog_path]
# Exit 0 if the section body has at least one effective content line; exit 1 if
# effectively empty. Effective empty = only whitespace, HTML/markdown comments,
# and/or ### heading lines with no bullet/item content. See header comment.
changelog_section_has_content() {
  local key="$1"
  local changelog="${2:-CHANGELOG.md}"
  local body
  local line
  local trimmed
  # Track multi-line HTML comments so middle lines do not count as content.
  local in_html_comment=0
  if [ -z "$key" ]; then
    echo "error: changelog_section_has_content requires VERSION or Unreleased" >&2
    return 2
  fi
  # Capture body (may be empty). Do not fail the pipeline on empty body.
  body="$(changelog_section_body "$key" "$changelog" || true)"
  while IFS= read -r line || [ -n "$line" ]; do
    # Trim leading/trailing whitespace for classification.
    trimmed="$(printf '%s' "$line" | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"
    if [ -z "$trimmed" ]; then
      continue
    fi
    # HTML comments: single-line fully wrapped, multi-line open/close, and all
    # interior lines while open (codex P2 — middle lines must not count).
    if [ "$in_html_comment" -eq 1 ]; then
      if [[ "$trimmed" == *'-->'* ]]; then
        in_html_comment=0
      fi
      continue
    fi
    if [[ "$trimmed" =~ ^\<!--.*--\>$ ]]; then
      continue
    fi
    if [[ "$trimmed" =~ ^\<!-- ]]; then
      # Open without close on same line → enter multi-line comment.
      if [[ "$trimmed" != *'-->'* ]]; then
        in_html_comment=1
      fi
      continue
    fi
    # ### heading-only lines do not count as content.
    if [[ "$trimmed" =~ ^###([[:space:]]|$) ]]; then
      continue
    fi
    # Any other non-empty line is content (bullets, prose, blockquotes, etc.).
    return 0
  done <<<"$body"
  return 1
}

# newest_remote_v_tag [remote]
# Prints the newest semver tag matching refs/tags/v* on the remote (e.g. v0.2.3).
# Selection: list remote tags, keep vMAJOR.MINOR.PATCH (optional prerelease
# suffix ignored — only plain vX.Y.Z), sort by version (sort -V), take last.
# Exit 1 if none found.
newest_remote_v_tag() {
  local remote="${1:-}"
  local tags
  local newest
  if [ -z "$remote" ]; then
    remote="$(resolve_git_remote)"
  fi
  if [ -z "$remote" ]; then
    echo "error: no git remote configured" >&2
    return 2
  fi
  # Strip peeled ^{} lines; keep only refs/tags/vX.Y.Z (no pre-release / build).
  tags="$(
    git ls-remote --tags "$remote" 2>/dev/null \
      | awk '{print $2}' \
      | sed 's#^refs/tags/##' \
      | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' \
      | sort -V \
      || true
  )"
  if [ -z "$tags" ]; then
    echo "error: no vX.Y.Z tags found on remote ${remote}" >&2
    return 1
  fi
  newest="$(printf '%s\n' "$tags" | tail -n1)"
  printf '%s\n' "$newest"
}
