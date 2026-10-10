#!/usr/bin/env bash
# Run a CI command; if it fails, annotate the rustc/clippy/dylint errors in
# its output with tools/scripts/rustc_annotate.py (titled annotations on the
# failing line, plus a plain list at the end of the log). The command's exit
# code is passed through unchanged.
#
# Usage: tools/scripts/with_rust_annotations.sh <command> [args...]
set -uo pipefail

log=$(mktemp)
trap 'rm -f "$log"' EXIT

"$@" 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
if [ "$rc" -ne 0 ]; then
    python3 "$(dirname "$0")/rustc_annotate.py" "$log" || true
fi
exit "$rc"
