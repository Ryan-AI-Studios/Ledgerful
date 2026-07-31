#!/usr/bin/env bash
# Fail loudly when a required secret environment variable is empty/unset.
#
# Usage:
#   MANIFEST_PUSH_TOKEN=… bash scripts/require-secret.sh MANIFEST_PUSH_TOKEN
#
# Exit 0 when the named env var is non-empty; non-zero naming the secret and
# pointing at docs/package-distribution.md when empty.
set -euo pipefail

if [ "$#" -ne 1 ] || [ -z "${1:-}" ]; then
  echo "usage: $0 <SECRET_NAME>" >&2
  exit 2
fi

name="$1"
# Indirect expansion without nounset exploding on missing vars.
value=""
if [ -n "${!name+x}" ]; then
  value="${!name}"
fi

if [ -n "$value" ]; then
  echo "ok: secret ${name} is set"
  exit 0
fi

echo "error: required secret ${name} is empty or unset" >&2
echo "error: configure it for this repository, then re-run the release workflow" >&2
echo "error: see docs/package-distribution.md (Bump automation / Secrets checklist)" >&2
exit 1
