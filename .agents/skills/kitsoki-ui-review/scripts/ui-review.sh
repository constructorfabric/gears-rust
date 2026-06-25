#!/usr/bin/env bash
# kitsoki-ui-review · one-shot: capture → multi-agent review → gated report.
#
# Drives the three stages end-to-end and exits with the gate code:
#   0 pass · 1 blocking finding · 2 pipeline error
#
# Artifacts land in .artifacts/ui-review/ (see CLAUDE.md / feedback_artifacts_dir):
#   frames/  audit.json  vision.json  verdict.json  review-report.md
#
# Usage:
#   ui-review.sh [--no-build] [--no-capture] [--viewports a,b,c]
#                [--design-intent F] [--model M] [--jobs N]
#                [--shard step|viewport] [--no-adversary] [--strict]
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
art="$repo/.artifacts/ui-review"
heur="$here/../heuristics.yaml"

do_build=1 do_capture=1 viewports="" intent="" model="claude-opus-4-8"
jobs=4 shard="step" adversary="" strict=""
while [ $# -gt 0 ]; do
  case "$1" in
    --no-build)      do_build=0; shift ;;
    --no-capture)    do_capture=0; shift ;;
    --viewports)     viewports="$2"; shift 2 ;;
    --design-intent) intent="$2"; shift 2 ;;
    --model)         model="$2"; shift 2 ;;
    --jobs)          jobs="$2"; shift 2 ;;
    --shard)         shard="$2"; shift 2 ;;
    --no-adversary)  adversary="--no-adversary"; shift ;;
    --strict)        strict="--strict"; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [ "$do_capture" -eq 1 ]; then
  cap_args=()
  [ "$do_build" -eq 0 ] && cap_args+=(--no-build)
  [ -n "$viewports" ] && cap_args+=(--viewports "$viewports")
  "$here/capture.sh" "${cap_args[@]}" || { echo "capture failed" >&2; exit 2; }
fi
[ -f "$art/audit.json" ] || { echo "no audit.json — run capture first (drop --no-capture)" >&2; exit 2; }

intent_args=()
[ -n "$intent" ] && intent_args+=(--design-intent "$intent")

"$here/review.sh" \
  --audit "$art/audit.json" --frames "$art/frames" --heuristics "$heur" \
  --out "$art/vision.json" --model "$model" --jobs "$jobs" --shard "$shard" \
  $adversary "${intent_args[@]}" || { echo "review failed" >&2; exit 2; }

"$here/report.sh" \
  --audit "$art/audit.json" --vision "$art/vision.json" \
  --out "$art/review-report.md" --verdict "$art/verdict.json" $strict
gate=$?

echo "── review-report.md ──" >&2
cat "$art/review-report.md" >&2 || true
exit $gate
