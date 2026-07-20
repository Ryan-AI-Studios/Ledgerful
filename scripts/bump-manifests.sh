#!/usr/bin/env bash
# Bump Homebrew formula + Scoop manifest version/hashes from published .sha256 files.
#
# Bash 3.2+ compatible (macOS /bin/bash). On Windows, prefer:
#   pwsh -File scripts/bump-manifests.ps1 …
#
# Reads checksums ONLY from published/fixture *.sha256 files (never recomputes).
# Required assets:
#   ledgerful-x86_64-pc-windows-msvc.zip
#   ledgerful-x86_64-unknown-linux-gnu.tar.gz
#   ledgerful-x86_64-apple-darwin.tar.gz
#   ledgerful-aarch64-apple-darwin.tar.gz
#
# Usage:
#   scripts/bump-manifests.sh --version 0.1.8 --checksums-dir path/to/sha256s \
#     [--packaging-dir packaging] [--out-dir DIR] [--dry-run]
set -euo pipefail

VERSION=""
CHECKSUMS_DIR=""
PACKAGING_DIR="packaging"
OUT_DIR=""
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: bump-manifests.sh --version <v> --checksums-dir <dir> [options]

Options:
  --version <v>           Version with or without leading v (required)
  --checksums-dir <dir>   Directory of *.sha256 files (required)
  --packaging-dir <dir>   Source packaging templates (default: packaging)
  --out-dir <dir>         Write outputs here (default: overwrite packaging-dir)
  --dry-run               Validate + print summary; do not write
  -h, --help              Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"; shift 2 ;;
    --checksums-dir)
      CHECKSUMS_DIR="${2:-}"; shift 2 ;;
    --packaging-dir)
      PACKAGING_DIR="${2:-}"; shift 2 ;;
    --out-dir)
      OUT_DIR="${2:-}"; shift 2 ;;
    --dry-run)
      DRY_RUN=1; shift ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2 ;;
  esac
done

if [[ -z "$VERSION" || -z "$CHECKSUMS_DIR" ]]; then
  echo "error: --version and --checksums-dir are required" >&2
  usage >&2
  exit 2
fi

VERSION_NORM="${VERSION#v}"
VERSION_NORM="${VERSION_NORM#V}"
if [[ ! "$VERSION_NORM" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; then
  echo "error: version must look like X.Y.Z (got '$VERSION' → '$VERSION_NORM')" >&2
  exit 1
fi

if [[ ! -d "$CHECKSUMS_DIR" ]]; then
  echo "error: checksums-dir does not exist: $CHECKSUMS_DIR" >&2
  exit 1
fi
if [[ ! -d "$PACKAGING_DIR" ]]; then
  echo "error: packaging-dir does not exist: $PACKAGING_DIR" >&2
  exit 1
fi

CHECKSUMS_DIR="$(cd "$CHECKSUMS_DIR" && pwd)"
PACKAGING_DIR="$(cd "$PACKAGING_DIR" && pwd)"

# Sorted required assets for deterministic processing
REQUIRED_ASSETS=(
  "ledgerful-aarch64-apple-darwin.tar.gz"
  "ledgerful-x86_64-apple-darwin.tar.gz"
  "ledgerful-x86_64-pc-windows-msvc.zip"
  "ledgerful-x86_64-unknown-linux-gnu.tar.gz"
)

read_sha256() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "error: missing checksum file: $path" >&2
    return 1
  fi
  local token
  token="$(tr -d '\r' <"$path" | awk 'NF { print tolower($1); exit }')"
  if [[ -z "$token" ]]; then
    echo "error: empty checksum file: $path" >&2
    return 1
  fi
  if [[ ! "$token" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: invalid sha256 token in $path: '$token'" >&2
    return 1
  fi
  printf '%s' "$token"
}

# Bash 3.2 compatible: store hashes in flat KEY=value file, no associative arrays.
HASH_TABLE="$(mktemp)"
trap 'rm -f "$HASH_TABLE"' EXIT
for asset in "${REQUIRED_ASSETS[@]}"; do
  sha_path="${CHECKSUMS_DIR}/${asset}.sha256"
  printf '%s=%s\n' "$asset" "$(read_sha256 "$sha_path")" >>"$HASH_TABLE"
done

hash_for() {
  # Usage: hash_for <asset-name>
  local asset="$1"
  local line
  line="$(grep -F "${asset}=" "$HASH_TABLE" | head -n 1)" || true
  if [[ -z "$line" ]]; then
    echo "error: no hash recorded for asset: $asset" >&2
    return 1
  fi
  printf '%s' "${line#*=}"
}

HOMEBREW_SRC="${PACKAGING_DIR}/homebrew/ledgerful.rb"
SCOOP_SRC="${PACKAGING_DIR}/scoop/ledgerful.json"
if [[ ! -f "$HOMEBREW_SRC" ]]; then
  echo "error: Homebrew formula missing: $HOMEBREW_SRC" >&2
  exit 1
fi
if [[ ! -f "$SCOOP_SRC" ]]; then
  echo "error: Scoop manifest missing: $SCOOP_SRC" >&2
  exit 1
fi

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"; rm -f "$HASH_TABLE"' EXIT

# Normalize source to LF for deterministic rewrites
tr -d '\r' <"$HOMEBREW_SRC" >"${WORKDIR}/formula.in"
tr -d '\r' <"$SCOOP_SRC" >"${WORKDIR}/scoop.in"
for f in formula.in scoop.in; do
  if [[ -s "${WORKDIR}/$f" ]] && [[ "$(tail -c1 "${WORKDIR}/$f" | wc -l)" -eq 0 ]]; then
    printf '\n' >>"${WORKDIR}/$f"
  fi
done

# Extract previous version (portable sed)
PREV_VERSION="$(
  sed -n 's/.*version[[:space:]]*"\([^"]*\)".*/\1/p' "${WORKDIR}/formula.in" | head -n 1
)"
if [[ -z "$PREV_VERSION" ]]; then
  PREV_VERSION="$(
    sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "${WORKDIR}/scoop.in" | head -n 1
  )"
fi

# --- Homebrew: version field + URL version segments ---
# When PREV_VERSION is known, rewrite exact /v{prev}/ segments. Otherwise fall
# back to any /vX.Y.Z/ segment (matches PowerShell generic rewrite).
awk -v ver="$VERSION_NORM" -v prev="${PREV_VERSION:-}" '
{
  line = $0
  if (line ~ /version[[:space:]]+"/) {
    sub(/version[[:space:]]+"[^"]+"/, "version \"" ver "\"", line)
  }
  if (prev != "") {
    gsub("/v" prev "/", "/v" ver "/", line)
  } else {
    while (match(line, /\/v[0-9]+\.[0-9]+\.[0-9]+(-[^\/"]+)?\//)) {
      line = substr(line, 1, RSTART - 1) "/v" ver "/" substr(line, RSTART + RLENGTH)
    }
  }
  print line
}
' "${WORKDIR}/formula.in" >"${WORKDIR}/formula.work"

# Per-asset sha256 after matching url line (unix archives only, sorted)
for asset in \
  "ledgerful-aarch64-apple-darwin.tar.gz" \
  "ledgerful-x86_64-apple-darwin.tar.gz" \
  "ledgerful-x86_64-unknown-linux-gnu.tar.gz"
do
  hash="$(hash_for "$asset")"
  if ! grep -Fq "$asset" "${WORKDIR}/formula.work"; then
    echo "error: Homebrew formula has no url for asset: $asset" >&2
    exit 1
  fi
  awk -v asset="$asset" -v hash="$hash" '
  BEGIN { pending = 0; found = 0 }
  {
    if (pending) {
      if ($0 ~ /sha256[[:space:]]+"[0-9a-fA-F]+"/) {
        sub(/sha256[[:space:]]+"[0-9a-fA-F]+"/, "sha256 \"" hash "\"")
        found = 1
      }
      print
      pending = 0
      next
    }
    print
    if (index($0, asset) > 0 && $0 ~ /url/) {
      pending = 1
    }
  }
  END {
    if (!found) {
      print "error: Homebrew formula has no url/sha256 pair for asset: " asset > "/dev/stderr"
      exit 1
    }
  }
  ' "${WORKDIR}/formula.work" >"${WORKDIR}/formula.next"
  mv "${WORKDIR}/formula.next" "${WORKDIR}/formula.work"
done
cp "${WORKDIR}/formula.work" "${WORKDIR}/formula.out"

# --- Scoop ---
WIN_ASSET="ledgerful-x86_64-pc-windows-msvc.zip"
WIN_HASH="$(hash_for "$WIN_ASSET")"

awk -v ver="$VERSION_NORM" -v prev="${PREV_VERSION:-}" -v win_asset="$WIN_ASSET" -v win_hash="$WIN_HASH" '
BEGIN { seen_url = 0; hash_done = 0 }
{
  line = $0
  if (line ~ /"version"[[:space:]]*:/) {
    sub(/"version"[[:space:]]*:[[:space:]]*"[^"]+"/, "\"version\": \"" ver "\"", line)
  }
  if (prev != "") {
    gsub("/v" prev "/", "/v" ver "/", line)
  } else {
    while (match(line, /\/v[0-9]+\.[0-9]+\.[0-9]+(-[^\/"]+)?\//)) {
      line = substr(line, 1, RSTART - 1) "/v" ver "/" substr(line, RSTART + RLENGTH)
    }
  }
  if (index(line, win_asset) > 0 && line ~ /"url"/) {
    seen_url = 1
  }
  if (seen_url && !hash_done && line ~ /"hash"[[:space:]]*:[[:space:]]*"[0-9a-fA-F]+"/) {
    sub(/"hash"[[:space:]]*:[[:space:]]*"[0-9a-fA-F]+"/, "\"hash\": \"" win_hash "\"", line)
    hash_done = 1
    seen_url = 0
  }
  print line
}
END {
  if (!hash_done) {
    print "error: Scoop manifest: failed to locate hash for " win_asset > "/dev/stderr"
    exit 1
  }
}
' "${WORKDIR}/scoop.in" >"${WORKDIR}/scoop.out"

for f in formula.out scoop.out; do
  if [[ -s "${WORKDIR}/$f" ]] && [[ "$(tail -c1 "${WORKDIR}/$f" | wc -l)" -eq 0 ]]; then
    printf '\n' >>"${WORKDIR}/$f"
  fi
done

echo "bump-manifests: version=${VERSION_NORM}"
echo "checksums-dir=${CHECKSUMS_DIR}"
for asset in "${REQUIRED_ASSETS[@]}"; do
  echo "  ${asset} = $(hash_for "$asset")"
done

changed_list=""
if ! cmp -s "${WORKDIR}/formula.out" "${WORKDIR}/formula.in"; then
  changed_list="homebrew/ledgerful.rb"
fi
if ! cmp -s "${WORKDIR}/scoop.out" "${WORKDIR}/scoop.in"; then
  if [[ -n "$changed_list" ]]; then
    changed_list="${changed_list} scoop/ledgerful.json"
  else
    changed_list="scoop/ledgerful.json"
  fi
fi
if [[ -z "$changed_list" ]]; then
  echo "No content changes (already at target version/hashes)."
else
  echo "Changed: ${changed_list}"
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "DryRun: not writing files."
  exit 0
fi

DEST_ROOT="${OUT_DIR:-$PACKAGING_DIR}"
mkdir -p "${DEST_ROOT}/homebrew" "${DEST_ROOT}/scoop"

HB_DEST="${DEST_ROOT}/homebrew/ledgerful.rb"
SC_DEST="${DEST_ROOT}/scoop/ledgerful.json"

cp "${WORKDIR}/formula.out" "$HB_DEST"
cp "${WORKDIR}/scoop.out" "$SC_DEST"

echo "Wrote: $HB_DEST"
echo "Wrote: $SC_DEST"
exit 0
