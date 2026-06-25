#!/usr/bin/env bash
# validate.sh — run the full Starlark validation toolchain over a path.
#
#   1. buildifier  (format + lint)   — if installed; skipped with a notice if not
#   2. starcheck   (parse + resolve, no execution) — always
#
# Usage:
#   validate.sh <file-or-dir> [extra starcheck flags...]
#
# Examples:
#   validate.sh scripts/                       # whole tree, spec-strict
#   validate.sh enrich.star -predeclared=http,secret   # simulate a 'query' level
#
# Exit non-zero if either tool reports a problem.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target="${1:?usage: validate.sh <file-or-dir> [starcheck flags...]}"
shift || true

# Resolve target to an absolute path: starcheck is its own Go module, so we run
# it from its own directory and cannot rely on the caller's cwd.
target="$(cd "$(dirname "$target")" && pwd)/$(basename "$target")"

status=0

echo "==> buildifier (format + lint)"
if command -v buildifier >/dev/null 2>&1; then
  # -type=default treats inputs as generic Starlark (not BUILD/WORKSPACE).
  # -mode=check fails on unformatted files; -lint=warn surfaces lint findings.
  if [ -d "$target" ]; then
    buildifier -r -type=default -mode=check -lint=warn "$target" || status=1
  else
    buildifier -type=default -mode=check -lint=warn "$target" || status=1
  fi
else
  echo "    buildifier not found — skipping (install: go install github.com/bazelbuild/buildtools/buildifier@latest)"
fi

echo "==> starcheck (parse + resolve, no execution)"
recurse=()
[ -d "$target" ] && recurse=(-r)
( cd "$here/starcheck" && go run . "${recurse[@]}" "$@" "$target" ) || status=1

exit "$status"
