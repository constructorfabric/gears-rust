#!/usr/bin/env bash
# ci-cache-buildx-args <image name>: the --cache-from (and, on a trusted job,
# --cache-to) arguments for `docker buildx build`, or nothing when the job has
# no BuildKit cache. Put by the ci-cache action on PATH:
#
#   docker buildx build $(ci-cache-buildx-args api) -t … .
set -euo pipefail
[ $# -eq 1 ] && [ -n "$1" ] || { echo "usage: ci-cache-buildx-args <image name>" >&2; exit 2; }
[ -n "${CI_CACHE_BUILDKIT:-}" ] || exit 0
printf -- '--cache-from %s,name=%s\n' "$CI_CACHE_BUILDKIT" "$1"
if [ "${CI_CACHE_BUILDKIT_WRITE:-false}" = true ]; then
  printf -- '--cache-to %s,name=%s,mode=max,ignore-error=true\n' "$CI_CACHE_BUILDKIT" "$1"
fi
