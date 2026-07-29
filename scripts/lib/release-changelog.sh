# Shared release-changelog helpers (sourced by check-release-state.sh and
# check-release-tag.sh). Not executable standalone.
#
# Version-section matcher anchors on ^## \[ so:
# - tombstone prose lines ("> Note: 0.2.2 was prepared…") do not count
# - ### Known limitations headings do not count
# - Keep-a-Changelog link footers ([0.2.3]: https://…) do not count
#   (forward defence — no such footers exist in this repo today)

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
