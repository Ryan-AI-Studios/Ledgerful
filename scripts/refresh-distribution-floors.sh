#!/usr/bin/env bash
# Refresh dated winget-index / GitHub-Latest claim sites in distribution docs.
#
# Production reads remotes only (R2). --fixtures-dir is test-only.
# Never calls `winget search`. Never reads Cargo.toml or packaging/ as the
# current-claim source.
#
# Usage:
#   scripts/refresh-distribution-floors.sh --write
#   scripts/refresh-distribution-floors.sh --check
#   scripts/refresh-distribution-floors.sh --write --docs-dir DIR --fixtures-dir DIR
set -euo pipefail

MODE=""
DOCS_DIR="docs"
FIXTURES_DIR=""

usage() {
  cat <<'EOF'
Usage: refresh-distribution-floors.sh --write|--check [options]

Options:
  --write                 Rewrite the five claim sites from remotes (or fixtures)
  --check                 Exit 1 if any site disagrees on index / Latest / open PRs
  --docs-dir DIR          Docs root (default: docs)
  --fixtures-dir DIR      Test-only mocks (latest.tag, winget-dirs.txt, open-prs.txt, as-of.txt)
  -h, --help              Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --write) MODE="write"; shift ;;
    --check) MODE="check"; shift ;;
    --docs-dir)
      DOCS_DIR="${2:-}"; shift 2 ;;
    --fixtures-dir)
      FIXTURES_DIR="${2:-}"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2 ;;
  esac
done

if [[ -z "$MODE" ]]; then
  echo "error: --write or --check is required" >&2
  usage >&2
  exit 2
fi

if [[ ! -d "$DOCS_DIR" ]]; then
  echo "error: docs-dir does not exist: $DOCS_DIR" >&2
  exit 1
fi

INSTALL_MD="${DOCS_DIR}/installation.md"
PKG_MD="${DOCS_DIR}/package-distribution.md"
if [[ ! -f "$INSTALL_MD" || ! -f "$PKG_MD" ]]; then
  echo "error: expected ${INSTALL_MD} and ${PKG_MD}" >&2
  exit 1
fi

# --- facts -----------------------------------------------------------------

utc_date() {
  date -u +%Y-%m-%d
}

semver_gt() {
  # return 0 if $1 > $2 (X.Y.Z)
  local a="$1" b="$2"
  local a1 a2 a3 b1 b2 b3
  IFS=. read -r a1 a2 a3 <<EOF
${a}.0.0
EOF
  IFS=. read -r b1 b2 b3 <<EOF
${b}.0.0
EOF
  a1="${a1:-0}"; a2="${a2:-0}"; a3="${a3:-0}"
  b1="${b1:-0}"; b2="${b2:-0}"; b3="${b3:-0}"
  a1="${a1%%[!0-9]*}"; a2="${a2%%[!0-9]*}"; a3="${a3%%[!0-9]*}"
  b1="${b1%%[!0-9]*}"; b2="${b2%%[!0-9]*}"; b3="${b3%%[!0-9]*}"
  if [[ "$a1" -gt "$b1" ]]; then return 0; fi
  if [[ "$a1" -lt "$b1" ]]; then return 1; fi
  if [[ "$a2" -gt "$b2" ]]; then return 0; fi
  if [[ "$a2" -lt "$b2" ]]; then return 1; fi
  if [[ "$a3" -gt "$b3" ]]; then return 0; fi
  return 1
}

newest_semver() {
  local newest="0.0.0" v
  while IFS= read -r v || [[ -n "$v" ]]; do
    v="$(printf '%s' "$v" | tr -d '\r' | awk 'NF { print $1; exit }')"
    [[ -z "$v" ]] && continue
    if semver_gt "$v" "$newest"; then
      newest="$v"
    fi
  done
  printf '%s' "$newest"
}

sorted_unique_nums() {
  awk 'NF { print $1 }' | sort -n -u
}

gh_cmd() {
  # Prefer GH_TOKEN, then GITHUB_TOKEN. Unauthenticated pagination burns the 60/hr limit.
  if [[ -z "${GH_TOKEN:-}" && -n "${GITHUB_TOKEN:-}" ]]; then
    GH_TOKEN="$GITHUB_TOKEN"
    export GH_TOKEN
  fi
  gh "$@"
}

load_facts() {
  if [[ -n "$FIXTURES_DIR" ]]; then
    if [[ ! -d "$FIXTURES_DIR" ]]; then
      echo "error: fixtures-dir does not exist: $FIXTURES_DIR" >&2
      exit 1
    fi
    FACT_TAG="$(tr -d '\r' <"${FIXTURES_DIR}/latest.tag" | awk 'NF { print $1; exit }')"
    FACT_INDEX="$(newest_semver <"${FIXTURES_DIR}/winget-dirs.txt")"
    FACT_PRS="$(tr -d '\r' <"${FIXTURES_DIR}/open-prs.txt" | sorted_unique_nums | paste -sd, -)"
    if [[ -f "${FIXTURES_DIR}/as-of.txt" ]]; then
      FACT_DATE="$(tr -d '\r' <"${FIXTURES_DIR}/as-of.txt" | awk 'NF { print $1; exit }')"
    else
      FACT_DATE="$(utc_date)"
    fi
  else
    FACT_TAG="$(gh_cmd api repos/Ryan-AI-Studios/Ledgerful/releases/latest --jq .tag_name)"
    FACT_INDEX="$(gh_cmd api --paginate 'repos/microsoft/winget-pkgs/contents/manifests/l/Ledgerful/Ledgerful' --jq '.[].name' | newest_semver)"
    FACT_PRS="$(gh_cmd pr list --repo microsoft/winget-pkgs --search 'Ledgerful.Ledgerful' --state open --limit 50 --json number --jq '.[].number' | sorted_unique_nums | paste -sd, -)"
    FACT_DATE="$(utc_date)"
  fi
  FACT_TAG="${FACT_TAG#v}"
  FACT_TAG="${FACT_TAG#V}"
  FACT_TAG_V="v${FACT_TAG}"
  if [[ -z "$FACT_TAG" || -z "$FACT_INDEX" || "$FACT_INDEX" == "0.0.0" ]]; then
    echo "error: failed to load Latest tag or winget-pkgs index" >&2
    exit 1
  fi
}

pr_clause() {
  local prs="${1:-}"
  if [[ -z "$prs" ]]; then
    printf ''
    return
  fi
  local n
  n="$(printf '%s' "$prs" | awk -F, '{ print NF }')"
  if [[ "$n" -eq 1 ]]; then
    printf ' Open version PR #%s.' "$prs"
    return
  fi
  local out="" first=1 p
  IFS=, read -r -a arr <<EOF
${prs}
EOF
  # bash 3.2: no mapfile required; IFS split above may not work on 3.2 with -a in all shells.
  # Rebuild from comma list.
  out=""
  first=1
  oldifs="$IFS"
  IFS=,
  # shellcheck disable=SC2086
  set -- $prs
  IFS="$oldifs"
  for p in "$@"; do
    p="$(printf '%s' "$p" | tr -d ' ')"
    [[ -z "$p" ]] && continue
    if [[ "$first" -eq 1 ]]; then
      out="#${p}"
      first=0
    else
      out="${out}, #${p}"
    fi
  done
  printf ' Open version PRs %s.' "$out"
}

lag_clause() {
  local index="$1" latest_ver="$2"
  if [[ "$index" == "$latest_ver" ]]; then
    printf ''
    return
  fi
  printf ' do not claim winget %s until that directory exists.' "$latest_ver"
}

interior_i57() {
  local index="$1" tag_v="$2" date="$3" prs="$4" latest_ver="$5"
  printf 'community index is live at **%s** (`microsoft/winget-pkgs` `manifests/l/Ledgerful/Ledgerful`, %s). GitHub Release **%s** is published;%s%s' \
    "$index" "$date" "$tag_v" "$(lag_clause "$index" "$latest_ver")" "$(pr_clause "$prs")"
}

interior_p13() {
  local index="$1" tag_v="$2" date="$3" prs="$4"
  local extra=""
  if [[ -n "$prs" ]]; then
    extra="$(pr_clause "$prs")"
  fi
  printf 'Live at **%s** (manifests %s); GitHub **%s** published, index may lag.%s' \
    "$index" "$date" "$tag_v" "$extra"
}

interior_p191() {
  local index="$1" tag_v="$2" date="$3" prs="$4" latest_ver="$5"
  printf 'live on winget at **%s** (`microsoft/winget-pkgs` `manifests/l/Ledgerful/Ledgerful`, %s). GitHub Release **%s** is published;%s%s' \
    "$index" "$date" "$tag_v" "$(lag_clause "$index" "$latest_ver")" "$(pr_clause "$prs")"
}

interior_p215() {
  local index="$1" _tag="$2" date="$3"
  printf 'live on winget at **%s**, manifests %s' "$index" "$date"
}

interior_p217() {
  local index="$1" tag_v="$2" date="$3" prs="$4" latest_ver="$5"
  local lag=""
  if [[ "$index" != "$latest_ver" ]]; then
    lag=" do not claim ${latest_ver} live until that directory exists."
  fi
  printf 'community index **%s**. GitHub Release **%s** published %s;%s%s' \
    "$index" "$tag_v" "$date" "$lag" "$(pr_clause "$prs")"
}

# --- file rewrite ----------------------------------------------------------

# Replace between <!-- lf-floor:SITE --> and <!-- /lf-floor:SITE --> (same line or span).
# If markers are missing, wrap the first regex match of UNMARKED on one line.
apply_site() {
  local file="$1"
  local site="$2"
  local interior="$3"
  local unmarked_ere="$4"
  local tmp
  tmp="$(mktemp)"
  if grep -Fq "<!-- lf-floor:${site} -->" "$file"; then
    awk -v site="$site" -v interior="$interior" '
      BEGIN { start = "<!-- lf-floor:" site " -->"; stop = "<!-- /lf-floor:" site " -->" }
      {
        line = $0
        s = index(line, start)
        e = index(line, stop)
        if (s > 0 && e > s) {
          prefix = substr(line, 1, s + length(start) - 1)
          suffix = substr(line, e)
          print prefix interior suffix
          next
        }
        print
      }
    ' "$file" >"$tmp"
  else
    UNMARKED_ERE="$unmarked_ere" awk -v site="$site" -v interior="$interior" '
      BEGIN {
        start = "<!-- lf-floor:" site " -->"
        stop = "<!-- /lf-floor:" site " -->"
        ere = ENVIRON["UNMARKED_ERE"]
      }
      {
        if (match($0, ere)) {
          prefix = substr($0, 1, RSTART - 1)
          suffix = substr($0, RSTART + RLENGTH)
          print prefix start interior stop suffix
          next
        }
        print
      }
    ' "$file" >"$tmp"
  fi
  if ! grep -Fq "<!-- lf-floor:${site} -->" "$tmp"; then
    echo "error: failed to apply floor site ${site} in ${file}" >&2
    rm -f "$tmp"
    exit 1
  fi
  mv "$tmp" "$file"
}

extract_site() {
  local file="$1"
  local site="$2"
  awk -v site="$site" '
    BEGIN { start = "<!-- lf-floor:" site " -->"; stop = "<!-- /lf-floor:" site " -->" }
    {
      s = index($0, start)
      e = index($0, stop)
      if (s > 0 && e > s) {
        print substr($0, s + length(start), e - (s + length(start)))
      }
    }
  ' "$file"
}

parse_index() {
  printf '%s' "$1" | grep -oE '\*\*[0-9]+\.[0-9]+\.[0-9]+\*\*' | head -n1 | tr -d '*'
}

parse_tag() {
  printf '%s' "$1" | grep -oE '\*\*v[0-9]+\.[0-9]+\.[0-9]+\*\*' | head -n1 | tr -d '*'
}

parse_prs() {
  printf '%s' "$1" | grep -oE '#[0-9]+' | tr -d '#' | sorted_unique_nums | paste -sd, -
}

check_site() {
  local site="$1"
  local file="$2"
  local body
  body="$(extract_site "$file" "$site")"
  if [[ -z "$body" ]]; then
    echo "error: --check: missing markers for ${site} in ${file} (run --write first)" >&2
    return 1
  fi
  local got_index got_tag got_prs
  got_index="$(parse_index "$body")"
  got_tag="$(parse_tag "$body")"
  got_prs="$(parse_prs "$body")"
  local rc=0
  case "$site" in
    I-57|P-13|P-191|P-217)
      if [[ "$got_index" != "$FACT_INDEX" ]]; then
        echo "error: ${site} index '${got_index}' != '${FACT_INDEX}'" >&2
        rc=1
      fi
      if [[ "$got_tag" != "$FACT_TAG_V" ]]; then
        echo "error: ${site} tag '${got_tag}' != '${FACT_TAG_V}'" >&2
        rc=1
      fi
      if [[ "${got_prs:-}" != "${FACT_PRS:-}" ]]; then
        echo "error: ${site} open PRs '${got_prs:-}' != '${FACT_PRS:-}'" >&2
        rc=1
      fi
      ;;
    P-215)
      if [[ "$got_index" != "$FACT_INDEX" ]]; then
        echo "error: ${site} index '${got_index}' != '${FACT_INDEX}'" >&2
        rc=1
      fi
      ;;
  esac
  if ! printf '%s' "$body" | grep -Eq '[0-9]{4}-[0-9]{2}-[0-9]{2}'; then
    echo "error: ${site} missing YYYY-MM-DD date token" >&2
    rc=1
  fi
  return "$rc"
}

# --- main ------------------------------------------------------------------

load_facts

I57="$(interior_i57 "$FACT_INDEX" "$FACT_TAG_V" "$FACT_DATE" "$FACT_PRS" "$FACT_TAG")"
P13="$(interior_p13 "$FACT_INDEX" "$FACT_TAG_V" "$FACT_DATE" "$FACT_PRS")"
P191="$(interior_p191 "$FACT_INDEX" "$FACT_TAG_V" "$FACT_DATE" "$FACT_PRS" "$FACT_TAG")"
P215="$(interior_p215 "$FACT_INDEX" "$FACT_TAG_V" "$FACT_DATE")"
P217="$(interior_p217 "$FACT_INDEX" "$FACT_TAG_V" "$FACT_DATE" "$FACT_PRS" "$FACT_TAG")"

if [[ "$MODE" == "write" ]]; then
  apply_site "$INSTALL_MD" "I-57" "$I57" 'community index is live at [*][*][^*]+[*][*] [(][^)]+[)][.] GitHub Release [*][*]v[^*]+[*][*] is published; do not claim winget [0-9.]+ until [^.]*[.]'
  apply_site "$PKG_MD" "P-13" "$P13" 'Live at [*][*][^*]+[*][*] [(][^)]+[)]; GitHub [*][*]v[^*]+[*][*] published, index may lag'
  apply_site "$PKG_MD" "P-191" "$P191" 'live on winget at [*][*][^*]+[*][*] [(][^)]+[)][.] GitHub Release [*][*]v[^*]+[*][*] is published; do not claim [a-z0-9. -]+ until [^.]*[.]'
  apply_site "$PKG_MD" "P-215" "$P215" 'live on winget at [*][*][^*]+[*][*], `winget search` [0-9-]+|live on winget at [*][*][^*]+[*][*], manifests [0-9-]+'
  apply_site "$PKG_MD" "P-217" "$P217" 'community index [*][*][^*]+[*][*][.] GitHub Release [*][*]v[^*]+[*][*] published [0-9-]+; do not claim [0-9.]+ live until [^.]*[.]'
  echo "refresh-distribution-floors: write index=${FACT_INDEX} latest=${FACT_TAG_V} date=${FACT_DATE} prs=${FACT_PRS:-none}"
  exit 0
fi

fail=0
check_site "I-57" "$INSTALL_MD" || fail=1
check_site "P-13" "$PKG_MD" || fail=1
check_site "P-191" "$PKG_MD" || fail=1
check_site "P-215" "$PKG_MD" || fail=1
check_site "P-217" "$PKG_MD" || fail=1
if [[ "$fail" -ne 0 ]]; then
  echo "refresh-distribution-floors: check FAILED" >&2
  exit 1
fi
echo "refresh-distribution-floors: check ok index=${FACT_INDEX} latest=${FACT_TAG_V}"
exit 0
